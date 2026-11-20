//
//  BridgingFunctions.swift
//  Squirrel
//
//  Created by Leo Liu on 5/11/24.
//

import ApplicationServices
import CoreGraphics
//  HengIME 换血：数据源从 librime rime_api 换为 heng_core C ABI。
//  本文件保留与数据源无关的通用助手，并新增 heng_* 的 Swift 便捷封装。
//

import Foundation

/// heng_session_t（uint64_t）。0 表示无效会话。
typealias RimeSessionId = UInt64

// MARK: - heng_* 便捷封装

/// 读取会话选项（heng_get_option）。
func hengGetOption(_ session: RimeSessionId, _ option: String) -> Bool {
  guard session != 0 else { return false }
  return option.withCString { heng_get_option(session, $0) != 0 }
}

/// 设置会话选项（heng_set_option；传播/持久化策略由 core 统一处理）。
func hengSetOption(_ session: RimeSessionId, _ option: String, _ value: Bool) {
  guard session != 0 else { return }
  _ = option.withCString { heng_set_option(session, $0, value ? 1 : 0) }
}

/// 上报 inline_preedit 开关给 core（heng_set_inline_preedit）。仅 macOS 有效。
func hengSetInlinePreedit(_ on: Bool) {
  heng_set_inline_preedit(on ? 1 : 0)
}

/// 读 weasel.custom.yaml 的 style/<key>（heng_style_flag，源补丁直读，不过部署缓存）。
/// 返回调用方传入的 defaultValue = 键不存在（无覆盖）；0/1 = 有显式值。
func hengStyleFlag(_ key: String, _ defaultValue: Int32) -> Int32 {
  return key.withCString { heng_style_flag($0, defaultValue) }
}

/// 取待上屏文本（消费语义）。
func hengTakeCommit(_ session: RimeSessionId) -> String? {
  guard session != 0 else { return nil }
  var ptr: UnsafeMutablePointer<CChar>?
  guard heng_commit_text(session, &ptr) != 0, let text = ptr else { return nil }
  defer { heng_free_string(text) }
  return String(cString: text)
}

/// 取「鼠标点选候选」产生的上屏文本（消费语义）。
///
/// 与 hengTakeCommit 分开：按键路径的 commit 由 rimeUpdate 开头统一取，
/// 而点选候选没有任何按键事件伴随，必须在点击处理里显式取一次，
/// 否则候选窗关了但字不上屏。
func hengTakeUiCommit(_ session: RimeSessionId) -> String? {
  guard session != 0 else { return nil }
  var ptr: UnsafeMutablePointer<CChar>?
  guard heng_take_ui_commit(session, &ptr) != 0, let text = ptr else { return nil }
  defer { heng_free_string(text) }
  return String(cString: text)
}

/// 取组合串原始输入（如拼音串）。nil = 无组合。
func hengGetInput(_ session: RimeSessionId) -> String? {
  guard session != 0 else { return nil }
  var ptr: UnsafeMutablePointer<CChar>?
  guard heng_get_input(session, &ptr) != 0, let text = ptr else { return nil }
  defer { heng_free_string(text) }
  return String(cString: text)
}

/// 选项状态标签（短/长），菜单栏图标与浮窗用。nil = 无标签。
func hengStateLabel(_ session: RimeSessionId, _ option: String, _ state: Bool, abbreviated: Bool) -> String? {
  guard session != 0 else { return nil }
  var buf = [CChar](repeating: 0, count: 256)
  let rc = option.withCString {
    heng_get_state_label_abbreviated(session, $0, state ? 1 : 0, abbreviated ? 1 : 0, &buf, Int32(buf.count))
  }
  guard rc > 0, rc < Int32(buf.count) else { return nil }
  return String(cString: buf)
}

/// 把 UTF-8 字节偏移安全映射为 String.Index（越界收敛到边界）。
func stringIndex(utf8Offset offset: Int, in string: String) -> String.Index {
  let limit = string.utf8.index(string.utf8.startIndex, offsetBy: offset, limitedBy: string.utf8.endIndex) ?? string.utf8.endIndex
  return String.Index(limit, within: string) ?? string.endIndex
}

// MARK: - 通用助手

infix operator ?= : AssignmentPrecedence
// swiftlint:disable:next operator_whitespace
func ?=<T>(left: inout T, right: T?) {
  if let right = right {
    left = right
  }
}
// swiftlint:disable:next operator_whitespace
func ?=<T>(left: inout T?, right: T?) {
  if let right = right {
    left = right
  }
}

extension NSRange {
  static let empty = NSRange(location: NSNotFound, length: 0)
}

extension NSPoint {
  static func += (lhs: inout Self, rhs: Self) {
    lhs.x += rhs.x
    lhs.y += rhs.y
  }
  static func - (lhs: Self, rhs: Self) -> Self {
    Self.init(x: lhs.x - rhs.x, y: lhs.y - rhs.y)
  }
  static func -= (lhs: inout Self, rhs: Self) {
    lhs.x -= rhs.x
    lhs.y -= rhs.y
  }
  static func * (lhs: Self, rhs: CGFloat) -> Self {
    Self.init(x: lhs.x * rhs, y: lhs.y * rhs)
  }
  static func / (lhs: Self, rhs: CGFloat) -> Self {
    Self.init(x: lhs.x / rhs, y: lhs.y / rhs)
  }
  var length: CGFloat {
    sqrt(pow(self.x, 2) + pow(self.y, 2))
  }
}

// MARK: - 上屏回调（点击选词）

/// core 选词完成后回调这个 C 函数（经 heng_set_commit_handler 注册）。
///
/// 为什么必须用回调而不是轮询：点击事件从 Swift 主线程发给 core UI 线程，
/// core 要跨两轮循环（poll_events 收点击 → 下一轮 try_recv 才拿到 BarSelect）
/// 才处理完并产出 commit，实测比主线程的下一轮**晚约 9ms**。
/// 外壳在 mouseUp 里 DispatchQueue.main.async 取的话必定取空
/// —— 实测 takeUiCommit 返回 nil，而 9ms 后 map 里才出现文本。
/// 回调则由 core 在文本就绪的同一刻发起，时序天然正确。
private let commitCallback: @convention(c) (UnsafeMutableRawPointer?, UInt64, UnsafePointer<CChar>?) -> Void = {
  _, session, text in
  guard let text else { return }
  let s = String(cString: text)
  // 带上会话句柄：工具窗（表情/符号）的上屏可能投给「最近用过的会话」，
  // 而同一进程里每个宿主都挂着一个 controller。只有 session 相符的那个
  // 才该把字插进自己的 client，否则一次点击会往多个应用里各插一份。
  DispatchQueue.main.async {
    NotificationCenter.default.post(
      name: .hengCoreCommitReady, object: nil,
      userInfo: ["text": s, "session": session])
  }
}

extension Notification.Name {
  static let hengCoreCommitReady = Notification.Name("HengCoreCommitReady")
}

/// 注册上屏回调（进程级，只需一次）。幂等。
func hengRegisterCommitHandler() {
  heng_set_commit_handler(commitCallback, nil)
}

// MARK: - 软键盘按键转交

/// core 收到软键盘点击后回调这里。外壳必须按「真实按键」处理
/// （processKey + rimeUpdate），内联预编辑、光标锚点、候选窗定位都挂在那条链上。
private let softKeyCallback: @convention(c) (UnsafeMutableRawPointer?, UInt64, Int32, Int32) -> Void = {
  _, session, ks, mask in
  DispatchQueue.main.async {
    NotificationCenter.default.post(
      name: .hengSoftKey, object: nil,
      userInfo: ["session": session, "ks": ks, "mask": mask])
  }
}

extension Notification.Name {
  static let hengSoftKey = Notification.Name("HengSoftKey")
}

/// 软键盘功能键 → 合成真实按键事件。字符键仍走 processKey，不经过这里。
enum SoftKey {
  /// X keysym → macOS 虚拟键码
  private static let keycodes: [Int32: CGKeyCode] = [
    0xFF0D: 36,   // return
    0xFF8D: 76,   // 小键盘 enter
    0xFF08: 51,   // delete（退格）
    0xFFFF: 117,  // forward delete
    0xFF51: 123,  // ←
    0xFF52: 126,  // ↑
    0xFF53: 125,  // ↓
    0xFF54: 124,  // →
    0xFF09: 48,   // tab
    0xFF1B: 53,   // esc
    0xFF50: 115,  // home
    0xFF57: 119,  // end
    0xFF55: 116,  // page up
    0xFF56: 121,  // page down
  ]

  /// 可打印 ASCII → 键码（US 布局），组合键要靠它合成带修饰的按键
  private static let charKeycodes: [Int32: CGKeyCode] = {
    var m: [Int32: CGKeyCode] = [:]
    let table: [(Character, CGKeyCode)] = [
      ("a", 0), ("s", 1), ("d", 2), ("f", 3), ("h", 4), ("g", 5), ("z", 6),
      ("x", 7), ("c", 8), ("v", 9), ("b", 11), ("q", 12), ("w", 13), ("e", 14),
      ("r", 15), ("y", 16), ("t", 17), ("1", 18), ("2", 19), ("3", 20), ("4", 21),
      ("6", 22), ("5", 23), ("=", 24), ("9", 25), ("7", 26), ("-", 27), ("8", 28),
      ("0", 29), ("]", 30), ("o", 31), ("u", 32), ("[", 33), ("i", 34), ("p", 35),
      ("l", 37), ("j", 38), ("'", 39), ("k", 40), (";", 41), (",", 43),
      ("/", 44), ("n", 45), ("m", 46), (".", 47), (" ", 49),
    ]
    for (c, code) in table {
      if let ascii = c.asciiValue { m[Int32(ascii)] = code }
    }
    return m
  }()

  /// 组合键（⌃/⌥/⌘ + 字符）→ 合成带修饰的真实按键；成功返回 true
  static func postCombo(_ ks: Int32, mask: Int32) -> Bool {
    let mods = mask & ((1 << 2) | (1 << 3) | (1 << 6))
    guard mods != 0, AXIsProcessTrusted() else { return false }
    var lower = ks
    if lower >= 65 && lower <= 90 { lower += 32 }
    guard let code = charKeycodes[lower] ?? keycodes[ks] else { return false }
    HengWindowRenderer.log("COMBO ks=\(ks) mask=\(mask) code=\(code)")
    var flags: CGEventFlags = []
    if mods & (1 << 2) != 0 { flags.insert(.maskControl) }
    if mods & (1 << 3) != 0 { flags.insert(.maskAlternate) }
    if mods & (1 << 6) != 0 { flags.insert(.maskCommand) }
    let src = CGEventSource(stateID: .combinedSessionState)
    guard let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)
    else { return false }
    down.flags = flags
    up.flags = flags
    down.post(tap: .cgAnnotatedSessionEventTap)
    up.post(tap: .cgAnnotatedSessionEventTap)
    return true
  }

  /// 固定组合键（如 ⇧⌘4）
  static func postShortcut(keycode: CGKeyCode, flags: CGEventFlags) {
    let src = CGEventSource(stateID: .combinedSessionState)
    guard let down = CGEvent(keyboardEventSource: src, virtualKey: keycode, keyDown: true),
          let up = CGEvent(keyboardEventSource: src, virtualKey: keycode, keyDown: false)
    else { return }
    down.flags = flags
    up.flags = flags
    down.post(tap: .cgAnnotatedSessionEventTap)
    up.post(tap: .cgAnnotatedSessionEventTap)
  }

  /// 返回 true = 已投递（调用方不要再处理这个键）
  static func post(_ ks: Int32) -> Bool {
    guard let code = keycodes[ks], AXIsProcessTrusted() else { return false }
    let src = CGEventSource(stateID: .combinedSessionState)
    guard let down = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)
    else { return false }
    down.post(tap: .cgAnnotatedSessionEventTap)
    up.post(tap: .cgAnnotatedSessionEventTap)
    return true
  }
}

/// 注册软键盘按键回调（进程级，只需一次）。幂等。
func hengRegisterKeyHandler() {
  heng_set_key_handler(softKeyCallback, nil)
}
