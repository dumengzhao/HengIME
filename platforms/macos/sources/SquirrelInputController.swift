//
//  SquirrelInputController.swift
//  Squirrel
//
//  Created by Leo Liu on 5/7/24.
//
//  HengIME 换血：数据源从 librime rime_api 换为 heng_core C ABI。
//  外壳职责保持不变：按键转发 + 候选窗渲染 + 会话生命周期挂钩；
//  app_options / 开关传播 / 跨重启记忆由 core v6+ 行为统一层接管。
//

import InputMethodKit

final class SquirrelInputController: IMKInputController {
  private static let keyRollOver = 50
  private static var unknownAppCnt: UInt = 0

  // IMK 只走 init!(server:delegate:client:) 构造 controller（见 IMKInputController.h），
  // override init() 永远不会被调用 —— 观察者注册曾经写在这里，等于从未注册。
  //
  // 这里听的是 core 的 commit 回调（hengCoreCommitReady），不是 mouseUp 的点击通知：
  // 点击从主线程发给 core UI 线程后，core 要跨两轮循环才产出 commit，
  // 比主线程的下一轮晚约 9ms，所以在 mouseUp 里取必定取空（实测 nil）。
  // core 在文本就绪的同一刻回调，时序天然正确。
  /// 软键盘点击：core 转交过来，按真实按键处理（内联预编辑 / 光标锚点同一条链）
  private func observeSoftKeyboard() {
    softKeyObserver = NotificationCenter.default.addObserver(
      forName: .hengSoftKey, object: nil, queue: .main
    ) { [weak self] note in
      guard let self,
            let s = note.userInfo?["session"] as? UInt64,
            let ks = note.userInfo?["ks"] as? Int32,
            let mask = note.userInfo?["mask"] as? Int32 else { return }
      // 会话号对不上（换输入框后旧 session 已销毁、或激活顺序错位）时，
      // 投给最近激活的控制器，避免软键盘整片失效。
      guard s == self.session || self === Self.lastActive else { return }
      // 键盘自带功能键：截屏 / 中英（-6 / -8，由 core 下发）
      if ks == -6 {
        // 自绘覆盖窗：拖拽选区 / 缩放把手 / 回车确认 / Esc·右键·✕ 取消
        ScreenshotOverlay.shared.begin()
        return
      }
      if ks == -8 {
        hengSetOption(self.session, "ascii_mode", !hengGetOption(self.session, "ascii_mode"))
        self.rimeUpdate()
        return
      }
      // 带 ⌃/⌥/⌘ 的按键：合成带修饰的真实按键（⌘C、⌃A 等组合才到得了宿主）
      if SoftKey.postCombo(ks, mask: mask) { return }
      // ⇪ 打开时与物理 CapsLock 同语义：不组字、直接把大写字母送到宿主，
      // 否则 rime 会把它当普通字母起一次组字，候选窗就冒出来了。
      if mask & (1 << 1) != 0, let scalar = UnicodeScalar(UInt32(ks)),
         (65...90).contains(ks) || (97...122).contains(ks) {
        client?.insertText(
          String(Character(scalar)).uppercased(),
          replacementRange: NSRange(location: NSNotFound, length: 0))
        return
      }
      // 功能键（回车/退格/删除/方向/翻页…）合成一次真实按键事件：完整走
      // 「键盘事件 → IME → 宿主」链路，组字、上屏、光标移动与物理键盘一致。
      if SoftKey.post(ks) { return }
      let handled = self.processKey(UInt32(bitPattern: ks), modifiers: UInt32(bitPattern: mask))
      self.rimeUpdate()
      // 软键盘没法像物理键那样「返回 false 交回宿主」，rime 不吃的键要自己转成
      // 标准编辑命令，否则 Del / 方向键 / Tab 这类到了这里就静默消失。
      if !handled && self.preedit.isEmpty {
        self.dispatchUnhandledKey(ks)
      }
    }
  }

  /// rime 没消费的功能键 → 走 IMK 标准编辑命令交给宿主
  private func dispatchUnhandledKey(_ ks: Int32) {
    if !softKeyTrustLogged {
      softKeyTrustLogged = true
      HengWindowRenderer.log("SOFTKEY trusted=\(AXIsProcessTrusted())")
    }
    guard let client else { return }
    let sel: Selector?
    switch ks {
    case 0xFFFF: sel = Selector(("deleteForward:"))
    case 0xFF08: sel = Selector(("deleteBackward:"))
    case 0xFF51: sel = Selector(("moveLeft:"))
    case 0xFF52: sel = Selector(("moveUp:"))
    case 0xFF53: sel = Selector(("moveDown:"))
    case 0xFF54: sel = Selector(("moveRight:"))
    case 0xFF09: sel = Selector(("insertTab:"))
    case 0xFF0D: sel = Selector(("insertNewline:"))
    case 0xFF1B: sel = Selector(("cancelOperation:"))
    case 0xFF50: sel = Selector(("moveToBeginningOfLine:"))
    case 0xFF57: sel = Selector(("moveToEndOfLine:"))
    case 0xFF55: sel = Selector(("pageUp:"))
    case 0xFF56: sel = Selector(("pageDown:"))
    // Ins 在 macOS 上没有对应编辑命令；原来映射成 insertText: 并传 nil，
    // 会把输入上下文搞坏（之后退格、插入全失效），这里直接不处理。
    default: sel = nil
    }
    if sel == nil {
      // 可打印 ASCII（数字行、符号、字母）：rime 没消费就直接插入字符
      let printable = ks >= 0x20 && ks <= 0x7E
      HengWindowRenderer.log("SOFTKEY dispatch ks=\(ks) printable=\(printable) client=\(client != nil)")
      if printable, let scalar = UnicodeScalar(UInt32(ks)) {
        client.insertText(
          String(Character(scalar)),
          replacementRange: NSRange(location: NSNotFound, length: 0))
        HengWindowRenderer.log("SOFTKEY inserted '\(Character(scalar))'")
      }
      return
    }
    guard let sel else { return }
    // AppKit 文本视图：走 NSTextInputClient 的标准命令通路（不需要辅助功能权限）
    if let tic = client as? NSTextInputClient {
      tic.doCommand(by: sel)
      if ks == 0xFF0D {
        client.insertText("\u{000A}", replacementRange: NSRange(location: NSNotFound, length: 0))
      }
      return
    }
    let obj = client as AnyObject
    if obj.responds(to: sel) {
      obj.perform(sel, with: nil)
    }
    // 回车/制表符再补一手纯文本插入：部分宿主只认这个
    let none = NSRange(location: NSNotFound, length: 0)
    if ks == 0xFF0D { client.insertText("\u{000A}", replacementRange: none) }
    if ks == 0xFF09 { client.insertText("\u{0009}", replacementRange: none) }
  }

  private func observeCandidateClick() {
    candidateClickObserver = NotificationCenter.default.addObserver(
      forName: .hengCoreCommitReady, object: nil, queue: .main
    ) { [weak self] note in
      guard let text = note.userInfo?["text"] as? String, !text.isEmpty else { return }
      // 按会话过滤：工具窗上屏的目标会话可能不是本 controller
      if let target = note.userInfo?["session"] as? UInt64, target != 0, target != self?.session {
        return
      }
      self?.commit(string: text)
    }
  }

  private weak var client: IMKTextInput?
  /// CGEventTap 回调里需要拿到当前 controller 才能用 IMK client 上屏。
  /// tap 回调是 C 函数指针，捕获不到实例，只能走这个静态弱引用。
  static weak var lastActive: SquirrelInputController?

  /// 结束当前组合态（丢弃零宽占位，不落文档）。
  ///
  /// **必须用 setMarkedText("")，不能用 insertText("")。**
  /// `insertText("")` 是**空操作**：Qt 的 `-[QNSView insertText:replacementRange:]`
  /// 在 commitString 为空时直接 `return`，既不发 QInputMethodEvent 也不清
  /// `m_composingText`，于是「清组合态」这件事从未真正发生 —— 宿主那边组合态
  /// 长期挂着，编辑器 JS 把带 `isComposing` 的 keydown 一律忽略。
  /// setMarkedText 是把 `m_composingText` 直接赋成空串，没有早退分支。
  private func endComposition(_ client: IMKTextInput?) {
    guard let client else { return }
    client.setMarkedText(
      NSAttributedString(string: ""),
      selectionRange: NSRange(location: 0, length: 0),
      replacementRange: .empty)
  }

  /// 上一次在事件层补键盘布局的时间（节流用，避免每个 ⌘ 键都来一次 IPC）。
  private static var lastKbOverrideAt: TimeInterval = 0

  /// 让宿主用真实键盘布局解析快捷键（⌘F / ⌘S 这类宿主菜单快捷键的前提）。
  ///
  /// Qt 的 `QShortcutMap` 靠「当前输入源」的 `kTISPropertyUnicodeKeyLayoutData`
  /// 构建候选序列；第三方输入法、以及系统简体拼音都不带这份数据 → 表为空 →
  /// 宿主快捷键匹配不出来（Qt 日志里每个键都报
  /// `Mismatch between Cocoa 'x' and Carbon '\x0'`）。
  /// `overrideKeyboard(ABC)` 把宿主侧布局换成真实布局，表随之重建。
  ///
  /// activateServer 里已经调过一次，但 2026-11-07 实测：删掉零宽占位后
  /// ⌘F/⌘S 又失效了 —— 说明那次调用的生效时机不够稳。这里在 ⌘ 键真正
  /// 到来的那一刻再补一次（1 秒节流），把「布局什么时候生效」这件事钉死在
  /// 快捷键本身发生的时刻。
  @discardableResult
  func applyKeyboardOverride(_ target: IMKTextInput?) -> String {
    guard let target else { return "" }
    var layout = NSApp.squirrelAppDelegate.config?.getString("keyboard_layout") ?? ""
    if layout == "last" || layout == "" {
      return ""
    } else if layout == "default" {
      layout = "com.apple.keylayout.ABC"
    } else if !layout.hasPrefix("com.apple.keylayout.") {
      layout = "com.apple.keylayout.\(layout)"
    }
    target.overrideKeyboard(withKeyboardNamed: layout)
    return layout
  }

  /// 鼠标按下之前先把「组合中」状态撤掉（由事件层在 leftMouseDown 时调用）。
  ///
  /// 宿主在 marked text 非空时会把**第一次鼠标点击**当作「结束这次组合」
  /// 消费掉，事件不落到 widget 上（表现为文件树点两次只生效一次）。
  /// 点击前清一次组合态就能保证这次点击是干净的、宿主正常处理。
  /// **不判断是哪个应用** —— 这是所有宿主都成立的行为。
  ///
  /// 真实组字（preedit 非空）时不动 —— 那是用户正在输入的内容。
  func endCompositionBeforeClick() {
    guard let client, preedit.isEmpty else { return }
    endComposition(client)
  }

  /// 上一次观测到的 ascii_mode。nil = 尚未建立基线（激活/新建会话后的第一轮不弹提示）。
  private var lastAsciiMode: Bool?

  /// 中英切换瞬态提示：ascii_mode 发生变化时，通知 core 弹「中 / A」胶囊气泡。
  ///
  /// Shift 切换发生在 core 内部（librime 的 ascii_composer 在 Shift 单独按下时
  /// 翻转 ascii_mode），外壳没有对应回调，只能靠**前后对比 option 值**来感知。
  /// Linux(fcitx5) 与 Windows(weasel) 外壳都是显式调 `heng_ui_mode_hint_ex`，
  /// macOS 侧此前漏了这一环 —— 表现就是切中英文没有任何提示。
  ///
  /// 首次只记基线：激活输入法时不该凭空冒出一个气泡。
  private func syncModeHint() {
    guard session != 0 else { return }
    let ascii = hengGetOption(session, "ascii_mode")
    if let last = lastAsciiMode, last != ascii {
      updateCaretAnchor()
      _ = heng_ui_mode_hint_ex(
        ascii ? 1 : 0, caretAnchorX, caretAnchorBottom, caretAnchorTop)
    }
    lastAsciiMode = ascii
  }

  private var preedit: String = ""
  private var preeditDirty: Bool = false
  private var selRange: NSRange = .empty
  private var caretPos: Int = 0
  private var lastModifiers: NSEvent.ModifierFlags = .init()
  private var session: RimeSessionId = 0
  private var schemaId: String = ""
  private var inlinePreedit = false
  /// app_options/<bundle>/no_marked_text：跳过 setMarkedText 会话
  /// （HBuilderX 编辑器缺陷专用，见 show() 内注释）
  private var noMarkedText = false
  private var inlineCandidate = false
  private var chordKeyCodes: [UInt32] = .init(repeating: 0, count: SquirrelInputController.keyRollOver)
  private var chordModifiers: [UInt32] = .init(repeating: 0, count: SquirrelInputController.keyRollOver)
  private var chordKeyCount: Int = 0
  private var chordTimer: Timer?
  private var chordDuration: TimeInterval = 0
  private var currentApp: String = ""
  // 自绘候选窗（core slint 渲染，M-P4 起在 mac 上启用）：
  // 光标矩形锚点。core 无有效区域时沿用上次位置（对齐 fcitx5 端 last_x_/last_y_ 语义）。
  private var caretAnchorX: Int32 = -1
  private var caretAnchorBottom: Int32 = -1
  private var caretAnchorTop: Int32 = -1

  /// 点选候选后由 HengWindowRenderer 派发：core UI 线程已处理完点击，
  /// 此时取走上屏文本。点击路径没有按键事件伴随，不靠 rimeUpdate 触发。
  private var candidateClickObserver: NSObjectProtocol?
  private var softKeyObserver: NSObjectProtocol?
  private var softKeyTrustLogged = false

  // swiftlint:disable:next cyclomatic_complexity
  override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
    guard let event = event else { return false }
    let modifiers = event.modifierFlags
    let changes = lastModifiers.symmetricDifference(modifiers)

    // Return true to consume the key event; return false to pass it to the client app.
    var handled = false

    if session == 0 {
      createSession()
      if session == 0 {
        return false
      }
    }

    self.client ?= sender as? IMKTextInput
    if let app = client?.bundleIdentifier(), currentApp != app {
      currentApp = app
      updateAppOptions()
    }

    switch event.type {
    case .flagsChanged:
      if lastModifiers == modifiers {
        handled = true
        break
      }
      var rimeModifiers: UInt32 = SquirrelKeycode.osxModifiersToRime(modifiers: modifiers)
      // Some remote desktop tools send flagsChanged with keyCode 0; infer the real modifier key when needed.
      var keyCode = event.keyCode
      if !SquirrelKeycode.modifierKeycodes.contains(keyCode) {
        guard let inferred = SquirrelKeycode.inferModifierKeycode(from: changes) else {
          lastModifiers = modifiers
          rimeUpdate()
          handled = true
          break
        }
        keyCode = inferred
      }
      let rimeKeycode: UInt32 = SquirrelKeycode.osxKeycodeToRime(keycode: keyCode, keychar: nil, shift: false, caps: false)

      if changes.contains(.capsLock) {
        // Rime expects XK_Caps_Lock before the lock mask changes; NSFlagsChanged has already applied it.
        rimeModifiers ^= kLockMask.rawValue
        _ = processKey(rimeKeycode, modifiers: rimeModifiers)
      }

      // Process releases first because some modifier releases arrive with the next keydown.
      var buffer = [(keycode: UInt32, modifier: UInt32)]()
      for flag in [NSEvent.ModifierFlags.shift, .control, .option, .command] where changes.contains(flag) {
        if modifiers.contains(flag) {
          buffer.append((keycode: rimeKeycode, modifier: rimeModifiers))
        } else {
          buffer.insert((keycode: rimeKeycode, modifier: rimeModifiers | kReleaseMask.rawValue), at: 0)
        }
      }
      for (keycode, modifier) in buffer {
        _ = processKey(keycode, modifiers: modifier)
      }

      lastModifiers = modifiers
      rimeUpdate()

    case .keyDown:
      // ⌘ 组合键一律交回宿主。输入法只负责一件事：组字中被 ⌘ 打断时把组合态收干净。
      //
      // ⌘F/⌘S 依赖宿主用「当前输入源」的键盘布局数据建快捷键映射表，第三方输入法
      // 不带这份数据 → 映射表为空（Qt 日志里每个键都报
      // `Mismatch between Cocoa 'x' and Carbon '\x0'`）→ 快捷键序列匹配不出来。
      // 修法：squirrel.yaml 的 `keyboard_layout: default` 让
      // `client.overrideKeyboard(withKeyboardNamed: "com.apple.keylayout.ABC")`
      // 把宿主侧布局换成真实布局，映射表随之重建。
      if modifiers.contains(.command) {
        if !preedit.isEmpty {
          endComposition(client)
        }
        // 把「布局什么时候生效」钉在快捷键即将发生的这一刻，而不是指望
        // activateServer 那次调用的效果一直有效。
        applyKeyboardOverride(client)
        break
      }

      // 回车/退格/Tab/Esc/方向键这类控制键必须交回宿主自己
      // 执行（换行、删字、移光标）。用 insertText 硬插只会把 \r、\b、\u{1B}
      // 这类控制码写进文档 —— 编辑器把它们渲染成「BS」「ESC」这种占位字样，
      // 复制出去又不可见。
      //
      // 判定「不该当文本插入的键」：控制码（回车 \r、退格 \u{7F}、Tab \t、
      // Esc \u{1B}、小键盘回车 \u{3}）与 macOS 功能键私用区码点
      // （0xF700–0xF8FF：方向键 / F1-F35 / PageUp / Home…）。
      // 比写死 keyCode 表稳：键盘布局、Fn 层、外接键盘都覆盖得到。
      // 空串也必须算非文本键：Qt 下 Esc 的 charactersIgnoringModifiers 就是空的。
      let rawChars = event.charactersIgnoringModifiers ?? ""
      let isNonTextKey = rawChars.isEmpty || rawChars.unicodeScalars.contains {
        $0.value < 0x20 || $0.value == 0x7F || (0xF700...0xF8FF).contains($0.value)
      }
      if preedit.isEmpty && isNonTextKey {
        // 真组字时不动（preedit 非空走下面交 rime）；这里只是把可能残留的
        // marked text 收掉，保证宿主拿到的是一个干净的组合态。
        endComposition(client)
        break
      }

      let keyCode = event.keyCode
      var keyChars = event.charactersIgnoringModifiers
      let capitalModifiers = modifiers.isSubset(of: [.shift, .capsLock])
      if let code = keyChars?.first,
         (capitalModifiers && !code.isLetter) || (!capitalModifiers && !code.isASCII) {
        keyChars = event.characters
      }
      if let char = keyChars?.first {
        let rimeKeycode = SquirrelKeycode.osxKeycodeToRime(keycode: keyCode, keychar: char,
                                                           shift: modifiers.contains(.shift),
                                                           caps: modifiers.contains(.capsLock))
        if rimeKeycode != 0 {
          let rimeModifiers = SquirrelKeycode.osxModifiersToRime(modifiers: modifiers)
          handled = processKey(rimeKeycode, modifiers: rimeModifiers)
          rimeUpdate()
        }
      }

    default:
      break
    }

    return handled
  }

  func selectCandidate(_ index: Int) -> Bool {
    let success = heng_select_candidate_on_current_page(session, Int32(index)) != 0
    if success {
      rimeUpdate()
    }
    return success
  }

  // swiftlint:disable:next identifier_name
  func page(up: Bool) -> Bool {
    var handled = false
    handled = heng_change_page(session, up ? 1 : 0) != 0
    if handled {
      rimeUpdate()
    }
    return handled
  }

  func moveCaret(forward: Bool) -> Bool {
    let currentCaretPos = Int(heng_get_caret_pos(session))
    guard let input = hengGetInput(session) else { return false }
    if forward {
      if currentCaretPos <= 0 {
        return false
      }
      heng_set_caret_pos(session, Int32(currentCaretPos - 1))
    } else {
      if currentCaretPos >= input.utf8.count {
        return false
      }
      heng_set_caret_pos(session, Int32(currentCaretPos + 1))
    }
    rimeUpdate()
    return true
  }

  override func recognizedEvents(_ sender: Any!) -> Int {
    // keyDown + flagsChanged，与上游一致。声明里不要带鼠标事件：
    // 那等于告诉 IMK「本输入法要处理鼠标」，宿主会据此改变会话策略。
    let mask = NSEvent.EventTypeMask.Element(arrayLiteral: .keyDown, .flagsChanged)
    return Int(mask.rawValue)
  }

  // 方向键不走 handle(event)：client 把它们绑定为 NSResponder action（moveLeft: 等）
  // 经 didCommandBySelector:client: 路由。与 Windows 端统一：转交 core，由 key_binder
  // （Left→Up / Right→Down，when: has_menu）实现候选高亮切换；core 不消费时回退默认
  // （无候选时左右键仍为光标编辑，与 Windows 端语义一致）。
  override func didCommand(by aSelector: Selector, client sender: Any!) -> Bool {
    let arrows: [String: UInt32] = [
      "moveLeft:": UInt32(XK_Left),
      "moveRight:": UInt32(XK_Right),
      "moveUp:": UInt32(XK_Up),
      "moveDown:": UInt32(XK_Down)
    ]
    if let rimeKeycode = arrows[aSelector.description], session != 0 {
      if processKey(rimeKeycode, modifiers: 0) {
        rimeUpdate()
        return true
      }
    }
    // 除方向键外一律**不认领**。原来这里 return super.didCommand(...)：
    // IMKInputController 的父类对未知 selector 的返回值不可靠，一旦返回 true
    // 就等于告诉宿主「命令已被输入法处理」，Qt 会直接 bail out。
    return false
  }

  override func activateServer(_ sender: Any!) {
    Self.lastActive = self
    self.client ?= sender as? IMKTextInput
    // 工具窗（表情/符号）上屏按焦点会话投递：core 只认「最近用过的会话」，
    // 焦点换到别的应用后字符会落到上一处。会话句柄只有壳侧有。
    // 会话是惰性创建的（第一次按键才建）。软键盘的上屏依赖「当前焦点会话」，
    // 只点软键盘不打字时这里还是 0 → 键全部落空。激活时就建好。
    if session == 0 { createSession() }
    if session != 0 { heng_ui_set_active_session(session) }
    // 焦点落下时就把前台 app 记好：`recognizedEvents` 不认领按键时
    // handle() 可能一次都不被调用，事件层接管那边只能靠这里维护的 currentApp。
    if let app = (sender as? IMKTextInput)?.bundleIdentifier(), currentApp != app {
      currentApp = app
      updateAppOptions()
    }
    var keyboardLayout = NSApp.squirrelAppDelegate.config?.getString("keyboard_layout") ?? ""
    if keyboardLayout == "last" || keyboardLayout == "" {
      keyboardLayout = ""
    } else if keyboardLayout == "default" {
      keyboardLayout = "com.apple.keylayout.ABC"
    } else if !keyboardLayout.hasPrefix("com.apple.keylayout.") {
      keyboardLayout = "com.apple.keylayout.\(keyboardLayout)"
    }
    if keyboardLayout != "" {
      client?.overrideKeyboard(withKeyboardNamed: keyboardLayout)
    }
    // Activation delivers no flagsChanged event, and NSEvent.modifierFlags
    // only reflects this process's own event stream, so lastModifiers may
    // disagree with the actual Caps Lock state by now. Seed it from the
    // session-wide hardware state; otherwise the next Caps Lock press can
    // compare equal to the stale lastModifiers and be dropped by the
    // early-return in handle().
    if CGEventSource.flagsState(.combinedSessionState).contains(.maskAlphaShift) {
      lastModifiers.insert(.capsLock)
    } else {
      lastModifiers.remove(.capsLock)
    }
    preedit = ""
    SquirrelInputController.lastActive = self
    HengWindowRenderer.log("SESS activate sess=\(session) app=\(currentApp)")
    if session != 0 {
      let state = hengGetOption(session, "ascii_mode")
      let label = hengStateLabel(session, "ascii_mode", state, abbreviated: true)
      NSApp.squirrelAppDelegate.updateStatusIcon(asciiMode: state, schemaLabel: label)
    }
  }

  override init!(server: IMKServer!, delegate: Any!, client: Any!) {
    self.client = client as? IMKTextInput
    super.init(server: server, delegate: delegate, client: client)
    createSession()
    observeCandidateClick()
    hengRegisterCommitHandler()
    hengRegisterKeyHandler()
    observeSoftKeyboard()

    NotificationCenter.default.addObserver(
      forName: .init("SquirrelSetASCIIModeNotification"),
      object: nil,
      queue: nil
    ) { [weak self] notification in
      self?.handleASCIIModeToggle(notification)
    }

    NotificationCenter.default.addObserver(
      forName: .init("SquirrelReportASCIIModeNotification"),
      object: nil,
      queue: nil
    ) { [weak self] notification in
      self?.reportASCIIMode(notification)
    }
  }

  override func deactivateServer(_ sender: Any!) {
    HengWindowRenderer.log("SESS deactivate sess=\(session)")
    // 失活后 client 已不在，工具窗不能再往这个会话投递（0 = 退回最近会话兜底）
    heng_ui_set_active_session(0)
    hidePalettes()
    // 零宽占位不是用户打出来的字，绝不能走 commitComposition ——
    // 基类实现会把 marked text 提交上屏，那个 U+200B 会被插进文档；
    // 而且 Qt 的 m_composingText 之后并不一定被清空，留在那个视图上，
    // 之后该视图的第一次鼠标点击就会被当成「结束组合」吃掉。
    // 只有真实组字（preedit 非空）才需要提交。
    if preedit.isEmpty {
      endComposition(client)
    } else {
      commitComposition(sender)
    }
    client = nil
    // 失活后事件层必须彻底撒手：tap 回调靠 lastActive 找 controller，
    // 置 nil 它就不会再接管任何按键（详见 handleTapKey 的 client 闸）。
    if SquirrelInputController.lastActive === self {
      SquirrelInputController.lastActive = nil
    }
  }

  override func hidePalettes() {
    NSApp.squirrelAppDelegate.panel?.hide()
    // 自绘候选窗也要收：宿主panel 藏了但 core 的win=0/2 还挂着会留残影。
    heng_ui_hide()
    super.hidePalettes()
  }

  /// 光标锚点（core 三端统一**左上原点**的屏幕坐标）。
  ///
  /// 数据源：IMK 官方 API `attributesForCharacterIndex:lineHeightRectangle:`，
  /// SDK 注释原话 "Input methods will call this method to place a candidate
  /// window on the screen"，返回的是**光标处宽 1px、高一个行高**的矩形
  /// （实测日志 `1x18` 印证），坐标系为屏幕坐标 —— 上游 SquirrelPanel 也是
  /// 直接拿它当屏幕坐标用（`panel.position = inputPos`）。
  ///
  /// index 的正确取法，SDK 原文给了两条，缺一不可：
  /// 1. "The index is relative to the inline session." —— 有内联会话时，
  ///    index 是**相对会话起点**的偏移。
  /// 2. "Note that if there is **no inline session** the value for index should be 0,
  ///    which indicates that the information should be taken from the current selection."
  ///    —— 没有内联会话时**必须传 0**，系统自己取当前选区。
  ///
  /// 之前踩的坑（三次位置跑偏都源于此）：
  /// - 传 0（以为=行首）→ 实际是"当前选区"，**这才是对的**；
  /// - 传 `selectedRange().location`（文档绝对偏移，如25）→ 越界，rect 不可靠；
  /// - 传 `sel - marked`（相对偏移）→ 在 marked.length=0 时等于传绝对偏移，同样越界。
  /// 关键：`client.length()` 对 IMK 代理返回 **0**（日志 docLen=0），
  /// 拿它当上界做 clamp 会完全失效。
  /// 光标锚点 → core（左上原点屏幕坐标）。
  ///
  /// ===== 权威结论（Apple 官方文档 + 实测，勿再猜）=====
  /// 1. `NSTextInput.firstRectForCharacterRange:actualRange:`
  ///    文档原文：*"Returns the first frame rectangle for characters in the
  ///    given range, **in screen coordinates**."* → **这就是我们要的屏幕坐标。**
  /// 2. `IMKTextInput.attributesForCharacterIndex:lineHeightRectangle:`
  ///    返回的是**客户端局部坐标**（实测 y 随文档长度递减：
  ///    y = 537 - (字符数/80列)×17，四轮全中），**不能当屏幕坐标用**。
  ///    上游 Squirrel 官方用法是 `index=0` + `NSMinX/NSMinY`。
  ///
  /// `firstRect` 的坑：range 的 **length 必须 > 0**，传length=0（纯插入点）
  /// 这个 legacy client 返回 0×0。传 length=1 即可拿到真实矩形。
  private func updateCaretAnchor() {
    var selSnap = NSRange(location: NSNotFound, length: 0)
    var rect = NSRect.zero
    var src = "fallback"
    let lineHForClamp: CGFloat = 20
    var probeDoc = NSRect.zero

    if let client {
      selSnap = client.selectedRange()
      var sel = (selSnap.location == NSNotFound ? 0 : selSnap.location)
      if sel < 0 { sel = 0 }

      // ===== 用 index=0（上游 Squirrel 官方做法，20 年验证）=====
      // 上游SquirrelPanel.swift:665 就是
      //   `client.attributes(forCharacterIndex: 0, lineHeightRectangle: &inputPos)`
      // 原因：index=0 取的是**内联会话/文档首行**，而首行必然在视口内
      //（终端会滚动到底部显示最新内容），所以它的 y 恒为有效屏幕坐标。
      // 传 sel（光标处）反而在长文档里拿到大负数（实测 sel=10640 -> y=-1724），
      // 因为那个坐标是文档布局坐标，滚出视口就失去意义。
      //
      // x 同样取 index=0 的值 —— 上游也是这么做的，位置跟随的是行首，
      // 不会因为打字长短而左右跳。
      var probe = NSRect.zero
      if client.attributes(forCharacterIndex: 0, lineHeightRectangle: &probe) != nil,
         probe.width > 0 || probe.height > 0 {
        rect = probe
        src = "idx0"
      }
      // 对照：光标处的坐标，仅诊断用
      _ = client.attributes(forCharacterIndex: sel, lineHeightRectangle: &probeDoc)
    }

    if src == "fallback" {
      //拿不到就保持上一次有效锚点（首次用屏幕中心偏上）
      if caretAnchorX >= 0 {
        return
      }
      let sx = NSScreen.screens.first?.frame.width ?? 1440
      let sy = NSScreen.screens.first?.frame.height ?? 900
      rect = NSRect(x: sx * 0.4, y: sy * 0.45, width: 1, height: 20)
      src = "center"
    }

    guard let screen = NSScreen.screens.first(where: {
      NSMouseInRect(NSPoint(x: rect.minX, y: rect.minY), $0.frame, false)
    }) ?? NSScreen.screens.first else { return }
    let screenMaxY = screen.frame.maxY

    // 屏幕边界夹紧（照抄上游 SquirrelPanel.swift:480-486）：
    // 拿不到可靠屏幕坐标时，至少保证窗口不飞出屏幕。
    // 注意夹紧必须在【点】域做，这里 rect 仍是点制。
    if rect.minY < screen.frame.minY { rect.origin.y = screen.frame.minY }
    if rect.minY > screen.frame.maxY - lineHForClamp {
      rect.origin.y = screen.frame.maxY - lineHForClamp
    }
    if rect.minX < screen.frame.minX { rect.origin.x = screen.frame.minX }
    caretAnchorX = Int32(rect.minX.rounded())
    // 左下原点 → 左上原点，**保持点制不乘 scale**。
    // 定位用的 x/y 必须是点：外壳 configure 时会把 x 交给
    // NSPanel.setFrameOrigin（点域），乘 scale 会让窗口整体偏移一个缩放比。
    // 需要设备像素的地方（如 core 的翻转判定）由 core 自己乘 scale。
    caretAnchorBottom = Int32((screenMaxY - rect.minY).rounded())
    caretAnchorTop = Int32((screenMaxY - rect.maxY).rounded())
  }

  /// 驱动 core 自绘候选窗。语义与 fcitx5 端 updateUI 尾部一致：
  /// 把光标锚点交给 core，由它决定显示哪个窗口、画什么内容
  /// （inline_preedit 的字母区内嵌与否由 core 自己读同一份配置）。
  private func syncSelfDrawnUi() {
    updateCaretAnchor()
    _ = heng_ui_sync_ex(session, caretAnchorX, caretAnchorBottom, caretAnchorTop)
  }

  /// inline_preedit 开关计算与推送。squirrel 配置（panel.inlinePreedit）为基准，
  /// weasel.custom.yaml 的 style/inline_preedit 显式值覆盖之 —— 与 rime 的 patch
  /// 语义一致：core 设置窗拨这个开关写的就是该文件，高级用户也可手改。
  ///
  /// 算完立即推送 core（heng_set_inline_preedit）：macOS 上 core 读不到
  /// weasel.custom.yaml（那是 Windows weasel 端补丁），历史上恒 fallback「关」，
  /// 于是 squirrel.yaml 开着时外壳把字母插进宿主区、core 的预览窗条内也画，
  /// 同一份字母显两处（本 BUG）。推送后两侧同源，拨开关两边一起变。
  private func refreshInlinePreedit() {
    guard session != 0, let panel = NSApp.squirrelAppDelegate.panel else { return }
    let base = (panel.inlinePreedit && !hengGetOption(session, "no_inline")) || hengGetOption(session, "inline")
    let override = hengStyleFlag("inline_preedit", -1)
    let on = override >= 0 ? (override != 0) : base
    if on != inlinePreedit {
      inlinePreedit = on
      // 内嵌开 = 光标由宿主 marked text 承载；关 = core 预览窗自己画软光标
      hengSetOption(session, "soft_cursor", !on)
    }
    hengSetInlinePreedit(on)
  }

  /// no_marked_text 开关读取（app_options/<bundle>/no_marked_text）。
  /// 放在每轮 rimeUpdate 而不是 createSession：createSession 时
  /// SquirrelConfig 尚未 openBaseConfig（loadSettings 才打开），
  /// 那时读必为空；且 createSession 自己设 currentApp，handle 的
  /// app 变化分支被跳过。每轮刷一次，config open 后立即生效。
  private func refreshNoMarkedText() {
    guard !currentApp.isEmpty else { return }
    let v = NSApp.squirrelAppDelegate.config?.getAppOptions(currentApp)["no_marked_text"] ?? false
    // 注意：不要在这里给 HBuilderX 强开 no_marked_text。
    // 实测该做法既不能让 Cmd+F 恢复，又会让组字串从文档里消失
    //（微信输入法在 HBuilderX 里是**有**内联组字的，说明内联组字本身不是障碍）。
    if v != noMarkedText {
      noMarkedText = v
      HengWindowRenderer.log("APP=\(currentApp) noMarkedText=\(noMarkedText)")
    }
  }

  override func commitComposition(_ sender: Any!) {
    self.client ?= sender as? IMKTextInput
    if session != 0 {
      if let input = hengGetInput(session) {
        commit(string: input)
        heng_clear(session)
      }
    }
  }

  // core 设置窗口的页号契约（core/src/ui.rs SET_NAV 下标，改那边必须同步这里）
  private enum SettingsPage: Int32 {
    case service = 5   // 服务（重新部署 / 日志目录 / 程序目录）
    case about = 6     // 关于（版本 / 版权）
  }

  override func menu() -> NSMenu! {
    // 工具窗入口：表情/符号和 Windows 托盘菜单对齐。窗口长驻、不夺焦点，
    // 点格子连续上屏；关闭走窗口自己的 ✕。
    let emoji = NSMenuItem(title: NSLocalizedString("Emoji", comment: "Menu item"),
                           action: #selector(openEmoji), keyEquivalent: "")
    emoji.target = self
    let symbols = NSMenuItem(title: NSLocalizedString("Symbols", comment: "Menu item"),
                             action: #selector(openSymbols), keyEquivalent: "")
    symbols.target = self
    let tradItem = NSMenuItem(title: NSLocalizedString("Switch to Traditional", comment: "Menu item"),
                              action: #selector(toggleTraditional), keyEquivalent: "")
    tradItem.target = self
    // rime-ice 的简繁开关是 traditionalization（开 = 繁体）。勾选表示"当前是繁体"
    tradItem.state = hengGetOption(session, "traditionalization") ? .on : .off
    let vkbdItem = NSMenuItem(title: NSLocalizedString("Virtual Keyboard", comment: "Menu item"),
                              action: #selector(toggleVkbd), keyEquivalent: "")
    vkbdItem.target = self
    let shotItem = NSMenuItem(title: NSLocalizedString("Screenshot", comment: "Menu item"),
                              action: #selector(takeScreenshot), keyEquivalent: "")
    shotItem.target = self

    let setting = NSMenuItem(title: NSLocalizedString("Settings...", comment: "Menu item"),
                             action: #selector(openSettings), keyEquivalent: "")
    setting.target = self
    let about = NSMenuItem(title: NSLocalizedString("About...", comment: "Menu item"),
                           action: #selector(openAbout), keyEquivalent: "")
    about.target = self
    let update = NSMenuItem(title: NSLocalizedString("Check for updates...", comment: "Menu item"),
                            action: #selector(checkForUpdates), keyEquivalent: "")
    update.target = self

    // 分组：设置 | 表情/符号 | 繁体 | 软键盘 | 截图 | 关于/检查更新
    let menu = NSMenu()
    menu.addItem(setting)
    menu.addItem(.separator())
    menu.addItem(emoji)
    menu.addItem(symbols)
    menu.addItem(.separator())
    menu.addItem(tradItem)
    menu.addItem(.separator())
    menu.addItem(vkbdItem)
    menu.addItem(.separator())
    menu.addItem(shotItem)
    menu.addItem(.separator())
    menu.addItem(about)
    menu.addItem(update)

    return menu
  }

  @objc func openSettings() {
    _ = heng_settings_show(SettingsPage.service.rawValue)
  }

  @objc func openEmoji() {
    _ = heng_emoji_show()
  }

  @objc func openSymbols() {
    _ = heng_symbols_show()
  }

  /// 简繁切换：翻转 rime-ice 的 traditionalization 开关（core 负责持久化）
  @objc func toggleTraditional() {
    guard session != 0 else { return }
    hengSetOption(session, "traditionalization", !hengGetOption(session, "traditionalization"))
  }

  @objc func toggleVkbd() {
    _ = heng_vkbd_toggle()
  }

  @objc func takeScreenshot() {
    ScreenshotOverlay.shared.begin()
  }

  @objc func openAbout() {
    _ = heng_settings_show(SettingsPage.about.rawValue)
  }

  @objc func checkForUpdates() {
    NSApp.squirrelAppDelegate.checkForUpdates()
  }

  private(set) var specialCommentIndices: [ReservedPropertyKey: Set<Int>] = [:]

  func handleReservedProperty(key rawKey: String, value rawValue: String, for sessionId: RimeSessionId) throws(ReservedPropertyError) {
    guard session == sessionId, session != 0 else { return }
    guard let key = ReservedPropertyKey(rawValue: rawKey) else { throw .unknownInput(rawKey) }
    let parsed = try ReservedPropertyValue.parse(rawValue)
    switch key {
    case .commentHighlight:
      specialCommentIndices[.commentHighlight] = try parsed.indices()
    case .commentWarning:
      specialCommentIndices[.commentWarning] = try parsed.indices()
    case .refreshUI:
      rimeUpdate(clearReservedComments: false)
    }
  }

  deinit {
    if let obs = candidateClickObserver {
      NotificationCenter.default.removeObserver(obs)
    }
    if let obs = softKeyObserver {
      NotificationCenter.default.removeObserver(obs)
    }
    destroySession()
  }
}

private extension SquirrelInputController {

  func onChordTimer(_: Timer) {
    var processedKeys = false
    if chordKeyCount > 0 && session != 0 {
      // Chord typing releases are synthesized after the configured timeout.
      for i in 0..<chordKeyCount {
        let handled = heng_process_key(session, Int32(chordKeyCodes[i]), Int32(chordModifiers[i] | kReleaseMask.rawValue)) != 0
        if handled {
          processedKeys = true
        }
      }
    }
    clearChord()
    if processedKeys {
      rimeUpdate()
    }
  }

  func updateChord(keycode: UInt32, modifiers: UInt32) {
    for i in 0..<chordKeyCount where chordKeyCodes[i] == keycode {
      return
    }
    if chordKeyCount >= Self.keyRollOver {
      return
    }
    chordKeyCodes[chordKeyCount] = keycode
    chordModifiers[chordKeyCount] = modifiers
    chordKeyCount += 1
    if let timer = chordTimer, timer.isValid {
      timer.invalidate()
    }
    chordDuration = 0.1
    if let duration = NSApp.squirrelAppDelegate.config?.getDouble("chord_duration"), duration > 0 {
      chordDuration = duration
    }
    chordTimer = Timer.scheduledTimer(withTimeInterval: chordDuration, repeats: false, block: onChordTimer)
  }

  func clearChord() {
    chordKeyCount = 0
    if let timer = chordTimer {
      if timer.isValid {
        timer.invalidate()
      }
      chordTimer = nil
    }
  }

  func createSession() {
    let app = client?.bundleIdentifier() ?? {
      SquirrelInputController.unknownAppCnt &+= 1
      return "UnknownApp\(SquirrelInputController.unknownAppCnt)"
    }()
    print("createSession: \(app)")
    HengWindowRenderer.log("SESS create app=\(app)")
    currentApp = app
    session = app.withCString { heng_start_session($0) }
    schemaId = ""
    // 新会话的 ascii_mode 可能与本 controller 上一轮不同，重置基线避免激活时误弹提示。
    lastAsciiMode = nil

    if session != 0 {
      updateAppOptions()
      // 会话刚建好也可能是首个焦点会话：立刻登记，别等下一次 activateServer
      heng_ui_set_active_session(session)
    }
  }

  func updateAppOptions() {
    if currentApp == "" {
      return
    }
    // v6 行为统一层：app_options 与选项初值由 core 在 set_session_owner 时统一应用，
    // 外壳不再自行读取/下发（与 Windows/Linux 外壳一致）。
    _ = currentApp.withCString { heng_set_session_owner(session, $0) }
  }

  func destroySession() {
    if session != 0 {
      HengWindowRenderer.log("SESS destroy sess=\(session)")
      heng_end_session(session)
      session = 0
    }
    clearChord()
  }

  func processKey(_ rimeKeycode: UInt32, modifiers rimeModifiers: UInt32) -> Bool {
    // 注：不再同步 _linear/_vertical 到 core。这两个原版 Squirrel 的 UI 提示选项
    // 会让 librime 把方向键路由到组合串光标移动，破坏 key_binder 的
    // Left→Up / Right→Down 候选切换（与 Windows 端行为统一的关键）。

    // 先问候选窗：←/→ 切换预选词、↓ 展开面板、空格/回车/数字选词。
    // 这一步不能省 —— heng_process_key 只调engine.process_key，不含 UI 导航，
    // 方向键会被 librime 当成组合串光标移动吃掉，预选高亮永远不动。
    // 未被消费（返回 false）才继续走普通按键流程。
    let navConsumed = heng_ui_nav_key(session, Int32(rimeKeycode), Int32(rimeModifiers)) != 0
    if navConsumed {
      HengWindowRenderer.log("PROC keycode=\(rimeKeycode) nav=1")
      rimeUpdate()
      return true
    }

    let handled = heng_process_key(session, Int32(rimeKeycode), Int32(rimeModifiers)) != 0
    HengWindowRenderer.log("PROC keycode=\(rimeKeycode) mask=\(rimeModifiers) nav=0 handled=\(handled)")

    if !handled {
      let isVimBackInCommandMode = rimeKeycode == XK_Escape || ((rimeModifiers & kControlMask.rawValue != 0) && (rimeKeycode == XK_c || rimeKeycode == XK_C || rimeKeycode == XK_bracketleft))
      if isVimBackInCommandMode && hengGetOption(session, "vim_mode") &&
          !hengGetOption(session, "ascii_mode") {
        hengSetOption(session, "ascii_mode", true)
      }
    } else {
      let isChordingKey = switch Int32(rimeKeycode) {
      case XK_space...XK_asciitilde, XK_Control_L, XK_Control_R, XK_Alt_L, XK_Alt_R, XK_Shift_L, XK_Shift_R:
        true
      default:
        false
      }
      if isChordingKey && hengGetOption(session, "_chord_typing") {
        updateChord(keycode: rimeKeycode, modifiers: rimeModifiers)
      } else if (rimeModifiers & kReleaseMask.rawValue) == 0 {
        clearChord()
      }
    }

    return handled
  }

  func rimeConsumeCommittedText() {
    if let text = hengTakeCommit(session) {
      commit(string: text)
    }
    // 鼠标点选候选没有按键事件伴随，rimeUpdate 不会因按键触发，
    // 必须在点击处理里显式取一次，否则候选窗关了但字不上屏。
    if let text = hengTakeUiCommit(session) {
      commit(string: text)
    }
  }

  // Preserve reserved comment marks when librime requests a UI-only refresh.
  func rimeUpdate(clearReservedComments: Bool = true) {
    // 中英切换气泡必须发在**本轮所有 core 同步之后**：core 的 UiCmd::Sync 一收到
    // 就执行「气泡显示期间来了输入 → 立即退出气泡态」，排在 Sync 前面就会刚弹即清
    //（表现是气泡闪一下）。这条函数有多处早退，用 defer 保证每条返回路径都补发。
    defer { syncModeHint() }
    if clearReservedComments {
      specialCommentIndices = [:]
    }
    rimeConsumeCommittedText()

    var status = HengStatus()
    status.data_size = Int32(MemoryLayout<HengStatus>.size)
    let statusRet = heng_get_status(session, &status)
    if statusRet != 0 {
      // swiftlint:disable:next identifier_name
      if let schema_id = status.schema_id, schemaId == "" || schemaId != String(cString: schema_id) {
        schemaId = String(cString: schema_id)
        HengWindowRenderer.log("SCHEMA \(schemaId) sess=\(session)")
        NSApp.squirrelAppDelegate.loadSettings(for: schemaId)
        if let panel = NSApp.squirrelAppDelegate.panel {
          inlineCandidate = panel.inlineCandidate && !hengGetOption(session, "no_inline")
        }
      }
      // inline 开关每次刷新都算并推送 core：squirrel.yaml 是用户主开关，
      // weasel.custom.yaml（设置窗 / 高级用户直改）显式值覆盖之。
      // 放在 schema 分支外 —— 不换 schema 也要能热生效（改配置即生效）。
      refreshInlinePreedit()
      refreshNoMarkedText()
      heng_free_status(&status)
    }

    var ctx = HengContext()
    ctx.data_size = Int32(MemoryLayout<HengContext>.size)
    let ctxRet = heng_get_context(session, &ctx)
    if ctxRet != 0 {
      let preedit = ctx.preedit.map({ String(cString: $0) }) ?? ""
      HengWindowRenderer.log("CTX ret=\(ctxRet) sess=\(session) preedit=\(preedit) cand=\(ctx.candidate_count)")

      let start = stringIndex(utf8Offset: Int(ctx.sel_start), in: preedit)
      let end = stringIndex(utf8Offset: Int(ctx.sel_end), in: preedit)
      let caretPos = stringIndex(utf8Offset: Int(ctx.cursor_pos), in: preedit)

      if inlineCandidate {
        var candidatePreview = ctx.commit_text_preview.map { String(cString: $0) } ?? ""
        let endOfCandidatePreview = candidatePreview.endIndex
        if inlinePreedit {
          // 左移光標後的情形：
          // preedit:             ^已選某些字[xiang zuo yi dong]|guangbiao$
          // commit_text_preview: ^已選某些字向左移動$
          // candidate_preview:   ^已選某些字[向左移動]|guangbiao$
          // 繼續翻頁至指定更短字詞的情形：
          // preedit:             ^已選某些字[xiang zuo]yidong|guangbiao$
          // candidate_preview:   ^已選某些字[向左]yidong|guangbiao$
          // 光標移至當前段落最左端的情形：
          // preedit:             ^已選某些字|[xiang zuo yi dong guang biao]$
          // commit_text_preview: ^已選某些字向左移動光標$
          // candidate_preview:   ^已選某些字|[向左移動光標]$
          // 討論：
          // preedit 與 commit_text_preview 中“已選某些字”部分一致
          // 因此，選中範圍即正在翻譯的碼段“向左移動”中，兩者的 start 值一致
          // 光標位置的範圍是 start ..= endOfCandidatePreview
          if caretPos >= end && caretPos < preedit.endIndex {
            // 從 preedit 截取光標後未翻譯的編碼“guangbiao”
            candidatePreview += preedit[caretPos...]
          }
        } else {
          // 翻頁至指定更短字詞的情形：
          // preedit:             ^已選某些字[xiang zuo]yidong|guangbiao$
          // commit_text_preview: ^已選某些字向左yidongguangbiao$
          // candidate_preview:   ^已選某些字[向左???]|$
          // 光標移至當前段落最左端，繼續翻頁至指定更短字詞的情形：
          // preedit:             ^已選某些字|[xiang zuo]yidongguangbiao$
          // commit_text_preview: ^已選某些字向左yidongguangbiao$
          // candidate_preview:   ^已選某些字|[向左]???$
        }
        // preedit can contain additional prompt text before start:
        // ^(prompt)[selection]$
        let start = min(start, candidatePreview.endIndex)
        let caretPos = caretPos <= start ? caretPos : endOfCandidatePreview
        show(preedit: candidatePreview,
             selRange: NSRange(location: start.utf16Offset(in: candidatePreview),
                               length: candidatePreview.utf16.distance(from: start, to: candidatePreview.endIndex)),
             caretPos: caretPos.utf16Offset(in: candidatePreview))
      } else {
        if inlinePreedit {
          show(preedit: preedit, selRange: NSRange(location: start.utf16Offset(in: preedit), length: preedit.utf16.distance(from: start, to: end)), caretPos: caretPos.utf16Offset(in: preedit))
        } else {
          // Use a full-width space placeholder to prevent iTerm2 from echoing raw preedit;
          // half-width placeholders make the Chinese composition baseline unstable.
          show(preedit: preedit.isEmpty ? "" : "　", selRange: NSRange(location: 0, length: 0), caretPos: 0)
        }
      }

      let numCandidates = Int(ctx.candidate_count)
      var candidates = [String]()
      var comments = [String]()
      for i in 0..<numCandidates {
        let candidate = ctx.candidates[i]
        let comment = ctx.comments[i]
        candidates.append(candidate.map { String(cString: $0) } ?? "")
        comments.append(comment.map { String(cString: $0) } ?? "")
      }
      var labels = [String]()
      // swiftlint:disable identifier_name
      if let select_keys = ctx.select_keys {
        labels = String(cString: select_keys).map { String($0) }
      } else if let labelArray = ctx.labels {
        for i in 0..<numCandidates {
          labels.append(labelArray[i].map { String(cString: $0) } ?? "")
        }
      }
      // swiftlint:enable identifier_name
      let page = Int(ctx.page_no)
      let lastPage = ctx.is_last_page != 0

      let selRange = NSRange(location: start.utf16Offset(in: preedit), length: preedit.utf16.distance(from: start, to: end))
      // 自绘候选窗（M-P4）：候选内容由 core 自己从 session 取并渲染，
      // 外壳只负责给光标锚点 —— 与 fcitx5 端 updateUI 尾部同构。
      syncSelfDrawnUi()
      _ = (candidates, comments, labels, page, lastPage, selRange)
      heng_free_context(&ctx)
    } else {
      HengWindowRenderer.log("CTX ret=\(ctxRet) sess=\(session) FAILED")
      // 取不到 context **不等于**没有候选词：只说明这一次快照失败。
      // 旧代码在这里直接 hidePalettes() → 候选窗闪没，而 core 侧随后
      // 又会 sync 回来，于是用户看到「窗口毫无规律的消失又出现」。
      // 正确做法：不碰窗口，等下一次按键的 rimeUpdate 自然重画。
    }
  }

  func commit(string: String) {
    guard let client = client else { return }
    HengWindowRenderer.log("COMMIT \(string)")

    let forceMarkedText =
      session != 0 &&
      hengGetOption(session, "force_marked_text_for_direct_commit")

    // Direct commits such as full-width punctuation do not necessarily have an
    // active marked-text phase. Some NSTextInputClient implementations require
    // one before accepting insertText.
    if forceMarkedText && preedit.isEmpty && !string.isEmpty {
      let markedText = NSMutableAttributedString(string: string)
      client.setMarkedText(
        markedText,
        selectionRange: NSRange(location: markedText.length, length: 0),
        replacementRange: .empty
      )
    }

    client.insertText(string, replacementRange: .empty)
    // preedit 缓存必须清，但它同时是 show() 的早退依据 —— 清了之后若下一个
    // show() 的入参也恰好是 ""，两者相等 → show() 直接 return →
    // setMarkedText 不再调用 → IMK 侧 marked range 停留在旧状态，
    // 而 syncSelfDrawnUi() 照发 SYNC → core 按「无组合串」把候选窗藏掉。
    // 这正是「第一个字母最常丢预览窗」的成因：commit 后 preedit 与
    // 入参同时为空，早退把刷新吞了。置脏标记强制下一次 show() 执行。
    preeditDirty = true
    preedit = ""
    // **不要在这里 hidePalettes()**。
    // commit() 不只由「选词上屏」触发 —— librime 在打字过程中也会产生 commit
    // （自动上屏 / 部分上屏 / 标点直上屏），此时组合串还在、候选窗应该继续显示。
    // 旧代码在这里无条件藏窗 → 窗口毫无规律地消失，打下一个字母才回来，
    // 就是本 BUG 的真凶（探针实测：BLIT 正常显示 → 0.5s 后 commit 藏窗 → 下一字母才回来）。
    // 候选窗显隐统一交给 core：rimeUpdate 尾部会syncSelfDrawnUi()，
    // core 侧按「既无候选又无组合串才藏窗」自己判断。
    //
    // 仍需 host panel 收起（那是宿主 UI，不走 core 的自绘链路）。
    NSApp.squirrelAppDelegate.panel?.hide()
  }

  func show(preedit: String, selRange: NSRange, caretPos: Int) {
    guard let client = client else { return }
    // preeditDirty：commit() 之后强制重刷一次，绕过下面的早退。
    //（早退在「commit 后 preedit 与入参同为 ""」时会吞掉 setMarkedText，
    //  导致 IMK marked range 不刷新 → core 判定无组合串 → 藏窗。）
    if !preeditDirty && self.preedit == preedit && self.caretPos == caretPos && self.selRange == selRange {
      HengWindowRenderer.log("MT dedup-skip text=\(preedit) dirty=\(preeditDirty)")
      return
    }
    preeditDirty = false

    // 空组合串绝不主动 setMarkedText("")。
    // Qt 6.5 的 QNSView 在 setMarkedText 时置 m_composingText，只有 insertText
    // 或再给一段 marked text 才解除；空串 setMarkedText 会让它长期停在
    // 「输入法占用组合态」，此后宿主本该自己插入的裸字符全被 IME 接管、
    // 而 IME 又返回 false → 字符凭空消失。这正是 HBuilderX 里英文模式
    // 按字母毫无反应、切回中文按 d 组字成功却紧接着又失效的根因
    //（15:47:03 日志：三条空 COMMIT 置脏 → show("") 越过早退 → 调了空串）。
    // 仅在宿主确实残留 marked text（Esc 取消组合等）时才清一次。
    if preedit.isEmpty {
      let had = !self.preedit.isEmpty
      self.preedit = preedit
      self.caretPos = caretPos
      self.selRange = selRange
      if had {
        let mr = client.markedRange()
        if mr.location != NSNotFound && mr.length > 0 {
          endComposition(client)
          HengWindowRenderer.log("MT clear resid marked=\(mr.location),\(mr.length)")
        } else {
          HengWindowRenderer.log("MT SKIP empty (no resid)")
        }
      } else {
        HengWindowRenderer.log("MT SKIP empty")
      }
      return
    }

    self.preedit = preedit
    self.caretPos = caretPos
    self.selRange = selRange

    HengWindowRenderer.log("MT text=\(preedit) sel=\(selRange.location),\(selRange.length) caret=\(caretPos)")
    if noMarkedText {
      // HBuilderX 编辑器缺陷（DCloud 社区 ask.dcloud.net.cn/question/216975 等
      // 多例，鼠须管/落格同中招，Intel/ARM 多版本复现）：组字调用
      // setMarkedText 之后，宿主不再给输入法转键，首键之后全部按键无响应。
      // 跳过 marked-text 会话，组字完全走 core 自绘候选窗；光标锚点由
      // updateCaretAnchor 的 fallback 兜底（无会话时光标矩形 API 返零
      // 是已知行为，保持上次锚点/屏幕中心偏上）。
      HengWindowRenderer.log("MT SKIP no_marked_text=1")
      return
    }

    let start = selRange.location
    let attrString = NSMutableAttributedString(string: preedit)
    if start > 0 {
      let attrs = mark(forStyle: kTSMHiliteConvertedText, at: NSRange(location: 0, length: start))! as! [NSAttributedString.Key: Any]
      attrString.setAttributes(attrs, range: NSRange(location: 0, length: start))
    }
    let remainingRange = NSRange(location: start, length: preedit.utf16.count - start)
    let attrs = mark(forStyle: kTSMHiliteSelectedRawText, at: remainingRange)! as! [NSAttributedString.Key: Any]
    attrString.setAttributes(attrs, range: remainingRange)
    client.setMarkedText(attrString, selectionRange: NSRange(location: caretPos, length: 0), replacementRange: .empty)
  }

  // swiftlint:disable:next function_parameter_count
  func showPanel(preedit: String, selRange: NSRange, caretPos: Int, candidates: [String], comments: [String], labels: [String], highlighted: Int, page: Int, lastPage: Bool) {
    guard let client = client else { return }
    var inputPos = NSRect()
    client.attributes(forCharacterIndex: 0, lineHeightRectangle: &inputPos)
    if let panel = NSApp.squirrelAppDelegate.panel {
      panel.position = inputPos
      panel.inputController = self
      panel.update(preedit: preedit, selRange: selRange, caretPos: caretPos, candidates: candidates, comments: comments, labels: labels,
                   highlighted: highlighted, page: page, lastPage: lastPage, update: true)
    }
  }

  private func handleASCIIModeToggle(_ notification: Notification) {
    guard let enableASCII = notification.object as? Bool else { return }
    guard session != 0 else { return }

    hengSetOption(session, "ascii_mode", enableASCII)
    NSApp.squirrelAppDelegate.updateStatusIcon(asciiMode: enableASCII, schemaLabel: nil)
    rimeUpdate()
  }

  private func reportASCIIMode(_: Notification) {
    guard client != nil else { return }
    guard session != 0 else { return }

    let isASCIIMode = hengGetOption(session, "ascii_mode")
    let status = isASCIIMode ? "ascii" : "nascii"

    DistributedNotificationCenter.default().postNotificationName(
      .init("SquirrelASCIIModeResponse"),
      object: status
    )
  }

}
