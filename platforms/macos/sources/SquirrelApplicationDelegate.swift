//
//  SquirrelApplicationDelegate.swift
//  Squirrel
//
//  Created by Leo Liu on 5/6/24.
//
//  HengIME 换血：librime 生命周期（setup/initialize/maintenance/notification）
//  全部由 heng_core 接管；外壳只负责 heng_create / heng_destroy 时机。
//

import UserNotifications
import Sparkle
import AppKit
import InputMethodKit

final class SquirrelApplicationDelegate: NSObject, NSApplicationDelegate, SPUStandardUserDriverDelegate, UNUserNotificationCenterDelegate {
  static let rimeWikiURL = URL(string: "https://github.com/rime/home/wiki")!
  static let updateNotificationIdentifier = "SquirrelUpdateNotification"
  static let notificationIdentifier = "SquirrelNotification"

  var config: SquirrelConfig?
  var panel: SquirrelPanel?
  var enableNotifications = false
  var showStatusIcon: Bool = true
  var statusItem: NSStatusItem?
  /// dev 包（bundle id 以 .dev 结尾）不启动更新器：
  /// Info.plist 里的 SUFeedURL 指向 rime 官方的 appcast，dev 版不该去查它
  /// —— 既免掉周期性网络请求，也避免「提示更新到上游版本」把人带沟里。
  static let isDevBuild = (Bundle.main.bundleIdentifier ?? "").hasSuffix(".dev")
  let updateController = SPUStandardUpdaterController(
    startingUpdater: !SquirrelApplicationDelegate.isDevBuild,
    updaterDelegate: nil, userDriverDelegate: nil)
  var supportsGentleScheduledUpdateReminders: Bool {
    true
  }

  func standardUserDriverWillHandleShowingUpdate(_ handleShowingUpdate: Bool, forUpdate update: SUAppcastItem, state: SPUUserUpdateState) {
    NSApp.setActivationPolicy(.regular)
    if !state.userInitiated {
      NSApp.dockTile.badgeLabel = "1"
      let content = UNMutableNotificationContent()
      content.title = NSLocalizedString("A new update is available", comment: "Update")
      content.body = NSLocalizedString("Version [version] is now available", comment: "Update").replacingOccurrences(of: "[version]", with: update.displayVersionString)
      let request = UNNotificationRequest(identifier: Self.updateNotificationIdentifier, content: content, trigger: nil)
      UNUserNotificationCenter.current().add(request)
    }
  }

  func standardUserDriverDidReceiveUserAttention(forUpdate update: SUAppcastItem) {
    NSApp.dockTile.badgeLabel = ""
    UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [Self.updateNotificationIdentifier])
  }

  func standardUserDriverWillFinishUpdateSession() {
    NSApp.setActivationPolicy(.accessory)
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
    if response.notification.request.identifier == Self.updateNotificationIdentifier && response.actionIdentifier == UNNotificationDefaultActionIdentifier {
      updateController.updater.checkForUpdates()
    }

    completionHandler()
  }

  func applicationWillFinishLaunching(_ notification: Notification) {
    panel = SquirrelPanel(position: .zero)
    refreshStatusItem()
    addObservers()
  }

  func applicationWillTerminate(_ notification: Notification) {
    // swiftlint:disable:next notification_center_detachment
    NotificationCenter.default.removeObserver(self)
    DistributedNotificationCenter.default().removeObserver(self)
    panel?.hide()
    if let item = statusItem {
      NSStatusBar.system.removeStatusItem(item)
      statusItem = nil
    }
  }

  func updateStatusIcon(asciiMode: Bool, schemaLabel: String?) {
    DispatchQueue.main.async { [weak self] in
      self?.applyStatusIcon(asciiMode: asciiMode, schemaLabel: schemaLabel)
    }
  }

  func deploy() {
    print("Start maintenance...")
    self.shutdownRime()
    self.startRime(fullCheck: true)
    self.loadSettings()
  }

  func syncUserData() {
    print("Sync user data")
    _ = heng_sync_user_data() != 0
  }

  func openLogFolder() {
    NSWorkspace.shared.open(SquirrelApp.logDir)
  }

  func openRimeFolder() {
    NSWorkspace.shared.open(SquirrelApp.userDir)
  }

  func checkForUpdates() {
    if updateController.updater.canCheckForUpdates {
      print("Checking for updates")
      updateController.updater.checkForUpdates()
    } else {
      print("Cannot check for updates")
    }
  }

  func openWiki() {
    NSWorkspace.shared.open(Self.rimeWikiURL)
  }

  static func showMessage(msgText: String?) {
    let center = UNUserNotificationCenter.current()
    center.requestAuthorization(options: [.alert, .provisional]) { _, error in
      if let error = error {
        print("User notification authorization error: \(error.localizedDescription)")
      }
    }
    center.getNotificationSettings { settings in
      if (settings.authorizationStatus == .authorized || settings.authorizationStatus == .provisional) && settings.alertSetting == .enabled {
        let content = UNMutableNotificationContent()
        content.title = NSLocalizedString("Squirrel", comment: "")
        if let msgText = msgText {
          content.subtitle = msgText
        }
        content.interruptionLevel = .active
        let request = UNNotificationRequest(identifier: Self.notificationIdentifier, content: content, trigger: nil)
        center.add(request) { error in
          if let error = error {
            print("User notification request error: \(error.localizedDescription)")
          }
        }
      }
    }
  }

  func setupRime() {
    createDirIfNotExist(path: SquirrelApp.userDir)
    createDirIfNotExist(path: SquirrelApp.logDir)
  }

  /// 系统级键盘事件捕获（CGEventTap）。
  ///
  /// HBuilderX 的 QtWebEngine 不把 keyDown 交给输入法（只在「组合中」才交，
  /// 而组合态会污染文档模型并吞掉页面级快捷键），所以那一类宿主改由这里接管：
  /// 可写 tap 先拿键 → `handleTapKey` 问 Rime → 消费就吞掉、不消费原样放行。
  /// 只有最前台是 HBuilderX 时才会真的介入，其它应用完全不受影响。
  ///
  /// 拿不到「发送事件」授权时自动退回只读监听：此时 HBuilderX 代码区退回
  /// 「只能打英文」，但绝不会出现卡键。
  private var eventTap: CFMachPort?
  /// tap 句柄的静态副本：回调是 C 函数指针，拿不到 self。
  private static var activeTap: CFMachPort?

  private func probeEventTapAccess() {
    let listen = CGPreflightListenEventAccess()
    let post = CGPreflightPostEventAccess()
    let mask = CGEventMask(1 << CGEventType.leftMouseDown.rawValue)
    var mode = "none"
    // 只读即可：tap 现在只做「鼠标点击前清组合态」这一件被动的事，不拦截
    // 也不吞任何事件，所以不需要「发送事件」授权，只要「输入监控」。
    let wanted: CGEventTapOptions = .listenOnly
    if let tap = CGEvent.tapCreate(
      tap: .cgSessionEventTap,
      place: .headInsertEventTap,
      options: wanted,
      eventsOfInterest: mask,
      callback: { _, type, event, _ in
        // 系统会因为我们回调超时/用户强制输入而把 tap 停掉，停了就再也不会投递。
        // 长期运行时这表现为「点击前不再清组合态」，必须自己重新启用。
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
          HengWindowRenderer.log("TAP disabled type=\(type.rawValue)")
          if let t = SquirrelApplicationDelegate.activeTap {
            CGEvent.tapEnable(tap: t, enable: true)
          }
          return Unmanaged.passUnretained(event)
        }
        // 鼠标按下：先把宿主的「组合中」状态撤掉，再原样放行。
        // 宿主在 marked text 非空时会把第一次点击拿去结束组合，点击落不到 widget
        //（HBuilderX 表现为文件树点两次只生效一次）。**不判断是哪个应用**。
        if type == .leftMouseDown {
          SquirrelInputController.lastActive?.endCompositionBeforeClick()
        }
        return Unmanaged.passUnretained(event)
      },
      userInfo: nil
    ) {
      let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
      CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .commonModes)
      CGEvent.tapEnable(tap: tap, enable: true)
      eventTap = tap
      SquirrelApplicationDelegate.activeTap = tap
      mode = "mouseOnly"
    }
    HengWindowRenderer.log("TAP mode=\(mode) listen=\(listen) post=\(post)")
  }

  func startRime(fullCheck: Bool) {
    print("Initializing heng-core...")
    probeEventTapAccess()
    // M-P4：注册窗口后端。core 只做slint 渲染，窗口贴帧由外壳在主线程执行。
    // 必须在 heng_create 之前—— core 启动 UI 线程后会立刻回调要窗口。
    let regRc = HengWindowRenderer.shared.register()
    let msg = "registered=\(regRc > 0) screen=\(HengWindowRenderer.shared.debugScreenInfo())"
    print("HengWindowRenderer \(msg)")
    HengWindowRenderer.log("REGISTER \(msg)")
    // fullCheck 保留入位：heng_create 内部恒为 full_check=TRUE 的完整部署
    let sharedPath = Bundle.main.sharedSupportPath ?? ""
    let userPath = SquirrelApp.userDir.path
    let rc = sharedPath.withCString { shared in
      userPath.withCString { user in
        heng_create(shared, user)
      }
    }
    if rc != 0 {
      let message = heng_last_error().map { String(cString: $0) } ?? "(no error message)"
      print("heng_create failed: \(message)")
      Self.showMessage(msgText: "heng_create failed: \(message)")
    }
  }

  func loadSettings() {
    config = SquirrelConfig()
    if !config!.openBaseConfig() {
      return
    }

    enableNotifications = config!.getString("show_notifications_when") != "never"
    showStatusIcon = config!.getBool("status_icon/show") ?? true
    refreshStatusItem()
    if let panel = panel, let config = self.config {
      panel.load(config: config, forDarkMode: false)
      panel.load(config: config, forDarkMode: true)
    }
  }

  func loadSettings(for schemaID: String) {
    if schemaID.count == 0 || schemaID.first == "." {
      return
    }
    let schema = SquirrelConfig()
    if let panel = panel, let config = self.config {
      if schema.open(schemaID: schemaID, baseConfig: config) && schema.has(section: "style") {
        panel.load(config: schema, forDarkMode: false)
        panel.load(config: schema, forDarkMode: true)
      } else {
        panel.load(config: config, forDarkMode: false)
        panel.load(config: config, forDarkMode: true)
      }
    }
    schema.close()
  }

  // Detect repeated launches that may indicate a bad configuration loop.
  func problematicLaunchDetected() -> Bool {
    var detected = false
    let logFile = FileManager.default.temporaryDirectory.appendingPathComponent("squirrel_launch.json", conformingTo: .json)
    do {
      let archive = try Data(contentsOf: logFile, options: [.uncached])
      let decoder = JSONDecoder()
      decoder.dateDecodingStrategy = .millisecondsSince1970
      let previousLaunch = try decoder.decode(Date.self, from: archive)
      if previousLaunch.timeIntervalSinceNow >= -2 {
        detected = true
      }
    } catch let error as NSError where error.domain == NSCocoaErrorDomain && error.code == NSFileReadNoSuchFileError {

    } catch {
      print("Error occurred during processing launch time archive: \(error.localizedDescription)")
      return detected
    }
    do {
      let encoder = JSONEncoder()
      encoder.dateEncodingStrategy = .millisecondsSince1970
      let record = try encoder.encode(Date.now)
      try record.write(to: logFile)
    } catch {
      print("Error occurred during saving launch time to archive: \(error.localizedDescription)")
    }
    return detected
  }

  func addObservers() {
    let center = NSWorkspace.shared.notificationCenter
    center.addObserver(forName: NSWorkspace.willPowerOffNotification, object: nil, queue: nil, using: workspaceWillPowerOff)

    let notifCenter = DistributedNotificationCenter.default()
    notifCenter.addObserver(forName: .init("SquirrelReloadNotification"), object: nil, queue: nil, using: rimeNeedsReload)
    notifCenter.addObserver(forName: .init("SquirrelSyncNotification"), object: nil, queue: nil, using: rimeNeedsSync)
    notifCenter.addObserver(forName: .init("SquirrelToggleASCIIModeNotification"), object: nil, queue: nil, using: rimeToggleASCIIMode)
    notifCenter.addObserver(forName: .init("SquirrelGetASCIIModeNotification"), object: nil, queue: nil, using: rimeGetASCIIMode)
    // Suspension behavior matters: the default coalescing holds notifications
    // back while the process is inactive, which is exactly the state Squirrel
    // enters when the user switches away — the icon would fail to hide until
    // the next activation. Deliver immediately instead.
    notifCenter.addObserver(self, selector: #selector(inputSourceChanged(_:)),
                            name: .init(kTISNotifySelectedKeyboardInputSourceChanged as String),
                            object: nil, suspensionBehavior: .deliverImmediately)
  }

  func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    print("Squirrel is quitting.")
    heng_destroy()
    return .terminateNow
  }

}

private extension SquirrelApplicationDelegate {
  func refreshStatusItem() {
    if showStatusIcon {
      if statusItem == nil {
        setupStatusItem()
      }
    } else if let item = statusItem {
      NSStatusBar.system.removeStatusItem(item)
      statusItem = nil
    }
  }

  func setupStatusItem() {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    if let button = item.button {
      button.font = NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
      button.toolTip = NSLocalizedString("Squirrel", comment: "")
    }
    statusItem = item
    applyStatusIcon(asciiMode: false, schemaLabel: nil)
    updateStatusItemVisibility()
  }

  @objc func inputSourceChanged(_: Notification) {
    DispatchQueue.main.async { [weak self] in
      self?.updateStatusItemVisibility()
      self?.finalizeStrandedComposition()
    }
  }

  func updateStatusItemVisibility() {
    guard let statusItem = statusItem else { return }
    let currentInputSourceID = SquirrelInstaller.currentInputSourceID() ?? ""
    statusItem.isVisible = currentInputSourceID.hasPrefix("im.rime.inputmethod.Squirrel")
  }

  // macOS 26 does not call deactivateServer when the input source is switched
  // away by another process via TISSelectInputSource() (e.g. macism, Input
  // Source Pro): the pending composition is stranded and the candidate panel
  // is left orphaned on screen (#1140). The input-source-changed notification
  // is still delivered, so finalize the composition here as a fallback.
  // Switching via the menu bar calls deactivateServer first, making this a
  // no-op.
  func finalizeStrandedComposition() {
    let currentInputSourceID = SquirrelInstaller.currentInputSourceID() ?? ""
    guard !currentInputSourceID.hasPrefix("im.rime.inputmethod.Squirrel") else { return }
    if let inputController = panel?.inputController {
      inputController.deactivateServer(inputController.client())
    }
  }

  func applyStatusIcon(asciiMode: Bool, schemaLabel: String?) {
    guard let button = statusItem?.button else { return }
    if let schemaLabel = schemaLabel, !schemaLabel.isEmpty {
      button.title = schemaLabel
    } else {
      button.title = asciiMode ? "Ａ" : "中"
    }
  }

  func shutdownRime() {
    config?.close()
    heng_destroy()
  }

  func workspaceWillPowerOff(_: Notification) {
    print("Finalizing before logging out.")
    self.shutdownRime()
  }

  func rimeNeedsReload(_: Notification) {
    print("Reloading rime on demand.")
    self.deploy()
  }

  func rimeNeedsSync(_: Notification) {
    print("Sync rime on demand.")
    self.syncUserData()
  }

  func rimeToggleASCIIMode(_ notification: Notification) {
    guard let mode = notification.object as? String else { return }
    let enableASCII = mode == "ascii"

    if enableASCII {
      NotificationCenter.default.post(name: .init("SquirrelSetASCIIModeNotification"), object: true)
    } else {
      NotificationCenter.default.post(name: .init("SquirrelSetASCIIModeNotification"), object: false)
    }
  }

  func rimeGetASCIIMode(_: Notification) {
    NotificationCenter.default.post(name: .init("SquirrelReportASCIIModeNotification"), object: nil)
  }

  func createDirIfNotExist(path: URL) {
    let fileManager = FileManager.default
    if !fileManager.fileExists(atPath: path.path()) {
      do {
        try fileManager.createDirectory(at: path, withIntermediateDirectories: true)
      } catch {
        print("Error creating user data directory: \(path.path())")
      }
    }
  }
}

extension NSApplication {
  var squirrelAppDelegate: SquirrelApplicationDelegate {
    self.delegate as! SquirrelApplicationDelegate
  }
}
