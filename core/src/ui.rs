//! 自绘候选窗（M-P1）：Slint 软件渲染 + 平台窗口后端，运行在 core
//! 内部的独立 UI 线程。
//!
//! 职责边界（docs/ROUTE-P.md §2 / M-P1-slint-or-probe.md §5）：
//! - 候选窗行为（悬浮提示、选中语义）完全由 core 定义——classicui 的
//!   hoverIndex_ 歧义在此路线下不存在
//! - 点击候选 → core 内部直接 select + 取走 commit（存入
//!   `PENDING_UI_COMMITS`，外壳经 `heng_take_ui_commit` 取回上屏）
//! - 外壳只调 `heng_ui_sync` / `heng_ui_hide` / `heng_take_ui_commit`
//!
//! 跨端结构：Slint 软渲染产出预乘 ARGB 帧（全端同一份代码），「把帧搬到
//! 屏幕」这一步抽象为 `UiBackend` trait：
//! - Linux   → XWindow（override-redirect X 窗口，x11rb PutImage）
//! - Windows → WinWindow（WS_POPUP 分层窗口，CreateDIBSection + UpdateLayeredWindow）
//!
//! 线程模型：Slint 侧全部对象（platform/window/组件）只在 UI 线程触碰；
//! ABI 线程经 mpsc 命令通道与 UI 线程通信。

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RepaintBufferType, SoftwareRenderer, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

use crate::engine::RimeSessionId;
use crate::global::{engine, SESSIONS, OP_LOCK};

/// 横排候选栏尺寸：宽度按内容估算（上限截断），高度固定单条
const BAR_HEIGHT: u32 = 40;
/// 横条与展开面板统一固定宽度（微信同款：宽度不随内容变化，超长词显示省略号）
const PANEL_WIDTH: u32 = 460;
/// 展开面板行高与最大行数
const FLOW_ROW_H: i32 = 38;
const FLOW_MAX_ROWS: i32 = 40; // 可滚动，上限放宽
const PANEL_VISIBLE_ROWS: i32 = 5; // 展开态可视行数
/// ☰ 菜单高度（占位）：上留白 4 + 3 行 × 38 + 下留白 8
const MENU_HEIGHT: i32 = FLOW_TOP + 3 * FLOW_ROW_H + 8;
/// 首行 y 基线：40px 横条内 28px 格子上下各留 6px（留白要明显）
const FLOW_TOP: i32 = 6;

// ---- 对外接口（capi 调用） ----

/// UI 线程命令
enum UiCmd {
    /// 拉取当前会话 context 并刷新显示；有候选则显示在 (x,y)，无候选则隐藏
    Sync { rime_id: RimeSessionId, x: i32, y: i32 },
    Hide,
    /// 展开箭头：切换「全部候选」面板（内部命令，来自 Slint 回调）
    ToggleExpand,
    /// 键盘 ↑（第一行）触发的收起（内部命令）
    SetExpanded(bool),
    /// 面板内移动视觉高亮（±1 格，内部命令）
    MoveHL(i32),
    /// 面板内按行移动高亮（+1 下一行 / -1 上一行；-1 在首行 → 收起）
    RowMove(i32),
    /// 键盘空格/回车：选中面板当前高亮项（内部命令）
    SelectHL,
    /// 收起横条：选中/取消选中 ☰ 图标（true=选中，词高亮隐藏）
    BarIcon(bool),
    /// 收起横条：按全局横条下标选词（鼠标点击 / 空格确认 / 数字键）
    BarSelect(usize),
    /// 打开 ☰ 菜单（占位：符号/常用语/设置）
    MenuOpen,
    /// 关闭菜单回到横条
    MenuClose,
}

static UI_CMD_TX: LazyLock<Mutex<Option<Sender<UiCmd>>>> = LazyLock::new(|| Mutex::new(None));

/// UI 点击产生的待上屏文本：外壳经 heng_take_ui_commit 取走
pub static PENDING_UI_COMMITS: LazyLock<Mutex<std::collections::HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

// ---- 展开面板共享状态（UI 线程写，capi 键盘拦截读） ----
pub static UI_EXPANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static UI_HL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
pub static UI_TOTAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// 收起横条词数（capi 判断 → 是否到达最后一个词）
pub static UI_BAR_COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// 收起横条：☰（最右图标）是否被选中（→ 到词尾跳过 ▾ 直达 ☰）
pub static UI_ICON_SEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// ☰ 菜单是否打开（打开时 ←/→ 关闭回横条）
pub static UI_MENU_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn send_cmd(cmd: UiCmd) -> bool {
    UI_CMD_TX
        .lock()
        .unwrap()
        .as_ref()
        .map(|tx| tx.send(cmd).is_ok())
        .unwrap_or(false)
}

/// 同步候选窗（外壳在 process_key_ex / 光标移动后调用）。
/// x/y 为屏幕坐标（通常取输入光标左下角）。UI 线程未启动或无显示时静默跳过。
pub fn ui_sync(rime_id: RimeSessionId, x: i32, y: i32) {
    send_cmd(UiCmd::Sync { rime_id, x, y });
}

pub fn ui_hide() {
    send_cmd(UiCmd::Hide);
}

/// 键盘 ↓（capi 拦截）触发的展开/收起切换
pub fn ui_toggle() {
    send_cmd(UiCmd::ToggleExpand);
}

/// 键盘 ↑（capi 拦截，面板第一行）触发的收起
pub fn ui_set_expanded(v: bool) {
    send_cmd(UiCmd::SetExpanded(v));
}

/// 面板内移动视觉高亮（capi 拦截）
pub fn ui_move_hl(delta: i32) {
    send_cmd(UiCmd::MoveHL(delta));
}

/// 面板内按行移动视觉高亮（capi 拦截；-1 在首行时由 UI 线程收起面板）
pub fn ui_row_move(delta: i32) {
    send_cmd(UiCmd::RowMove(delta));
}

/// 选中面板当前高亮项（capi 拦截：空格/回车）
pub fn ui_select_hl() {
    send_cmd(UiCmd::SelectHL);
}

/// 收起横条：☰ 图标选中/取消（capi 拦截：→ 到词尾 / ← 返回）
pub fn ui_bar_icon(on: bool) {
    send_cmd(UiCmd::BarIcon(on));
}

/// 收起横条：按横条下标选词（capi 拦截：空格确认 / 数字键 / 鼠标点击）
pub fn ui_bar_select(idx: usize) {
    send_cmd(UiCmd::BarSelect(idx));
}

/// 打开 ☰ 菜单（capi 拦截：☰ 选中时按空格/回车）
pub fn ui_menu_open() {
    send_cmd(UiCmd::MenuOpen);
}

/// 关闭菜单回横条（capi 拦截：菜单打开时 ←/→）
pub fn ui_menu_close() {
    send_cmd(UiCmd::MenuClose);
}

/// 惰性启动 UI 线程（首次 ui_sync 时）。启动失败（无 X 显示等）返回 false，
/// 外壳可回退到宿主候选窗。
pub fn ensure_started() -> bool {
    let mut guard = UI_CMD_TX.lock().unwrap();
    if guard.is_some() {
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let tx_for_thread = tx.clone();
    let ok = std::panic::catch_unwind(|| {
        std::thread::Builder::new()
            .name("heng-ui".into())
            .spawn(move || ui_thread_main(rx, tx_for_thread))
    })
    .is_ok();
    if ok {
        *guard = Some(tx);
        // 给 UI 线程一点初始化时间（X 连接失败会让它退出，下次 sync 会重试）
        std::thread::sleep(Duration::from_millis(50));
        return true;
    }
    false
}

// ---- UI 线程内部 ----

slint::slint! {
    export struct CandCell {
        num: string,
        text: string,
        hl: bool,
        x: int,
        y: int,
        w: int,
    }
    // 微信输入法式候选窗（单窗口两态，仅宽高与内容不同）：
    // 收起 = 高 40px，只露出流式格子第一行（页 1 候选），右上 ▾ + 菜单（预留）
    // 展开 = 变高，整窗一行行候选词（第一行原内容 + 真实候选 + 同音词），
    //        Flickable 滚动 + 自绘滚动条，按钮消失
    export component CandWindow inherits Window {
        in property <bool> expanded;
        in property <int> panel-width;
        in property <int> panel-height;   // 展开态可视高度
        in property <int> content-height; // 展开态内容总高（滚动范围）
        in property <[CandCell]> all-cells;
        in property <bool> icon-sel;  // ☰ 被键盘选中（单行 → 到词尾跳过 ▾）
        in property <bool> menu-open; // ☰ 菜单打开（覆盖层显示 符号/常用语/设置）
        in property <int> hl-y;       // 高亮格 y（逻辑 px），变化时滚动跟随
        width: root.panel-width * 1px;
        height: root.expanded ? root.panel-height * 1px : 40px;
        background: transparent;
        // 外层圆角容器：窗口本身透明，8px 圆角靠这层裁出（两态共用）
        round := Rectangle {
        x: 0;
        y: 0;
        width: parent.width;
        height: parent.height;
        background: #f7f8fa;
        border-radius: 8px;
        // 候选词流式格子（两态共用；收起时 40px 窗口只露出第一行）
        flick := Flickable {
            x: 0;
            y: 0;
            width: parent.width;
            height: parent.height;
            content-height: root.expanded ? root.content-height * 1px : parent.height;
            Rectangle {
                width: parent.width;
                height: root.expanded ? root.content-height * 1px : parent.height;
                for cell[idx] in root.all-cells: Rectangle {
                    x: cell.x * 1px;
                    y: cell.y * 1px;
                    width: cell.w * 1px;
                    height: 28px;
                    border-radius: 8px;
                    // 只有选中项有背景药丸，其余纯文字不加底色区分边界
                    background: cell.hl ? #2164f1 : transparent;
                    HorizontalLayout {
                        padding-left: 8px;
                        padding-right: 4px;
                        spacing: 4px;
                        alignment: center;
                        Text {
                            text: cell.num;
                            // 序号固定占宽：空串时也占 7px，序号出现/隐藏不引起正文横移
                            width: 7px;
                            color: cell.hl ? #cfe0ff : #999999;
                            font-size: 11px;
                            vertical-alignment: center;
                        }
                        Text {
                            text: cell.text;
                            color: cell.hl ? #ffffff : #1f2328;
                            font-size: 14px;
                            vertical-alignment: center;
                            overflow: elide;
                        }
                    }
                    TouchArea {
                        mouse-cursor: pointer;
                        clicked => { root.candidate_clicked(idx); }
                    }
                }
            }
        }
        // 展开态自绘滚动条（thumb 高 = 轨道高 × 可视/内容；位置 clamp 防负坐标）
        property <length> track-h: Math.max(1px, flick.height - 12px);
        property <length> scroll-range: Math.max(1px, root.content-height * 1px - flick.height);
        property <length> thumb-h: Math.min(track-h,
            Math.max(30px, track-h * flick.height
                / Math.max(1px, root.content-height * 1px)));
        if root.expanded: Rectangle {
            x: parent.width - 5px;
            y: 6px;
            width: 3px;
            height: track-h;
            border-radius: 2px;
            background: #d8dce2;
            Rectangle {
                width: parent.width;
                border-radius: 2px;
                background: #aeb4bd;
                y: Math.min(track-h - thumb-h,
                    Math.max(0px, (track-h - thumb-h)
                        * ((0px - flick.content-y) / scroll-range)));
                height: thumb-h;
            }
        }
        // 收起态右上按钮（换位后：▾ 在左、☰ 在最右 = 键盘导航的"最后一个图标"）：
        // → 到词尾跳过 ▾ 直达 ☰，☰ 上再按 → 展开面板、按空格/回车打开菜单
        // 图标用 Path/色块矢量绘制而非字符 "▾"/"☰"：Windows 默认字体缺
        // 这两个码位的字形，软件渲染器不做逐字形回退，会渲染成空白
        if !root.expanded: Rectangle {
            x: parent.width - 58px;
            y: 6px;
            width: 26px;
            height: 28px;
            border-radius: 8px;
            background: ta_arrow.has-hover ? #e8ebef : transparent;
            Path {
                width: 8px;
                height: 5px;
                x: (parent.width - 8px) / 2;
                y: (parent.height - 5px) / 2;
                viewbox-width: 8;
                viewbox-height: 5;
                commands: "M 0 0 L 8 0 L 4 5 Z";
                fill: #666666;
            }
            ta_arrow := TouchArea {
                mouse-cursor: pointer;
                clicked => { root.toggle_expand(); }
            }
        }
        if !root.expanded: Rectangle {
            x: parent.width - 30px;
            y: 6px;
            width: 26px;
            height: 28px;
            border-radius: 8px;
            background: ta_menu.has-hover || root.icon-sel ? #2164f1 : transparent;
            Rectangle {
                width: 12px;
                height: 8px;
                x: (parent.width - 12px) / 2;
                y: (parent.height - 8px) / 2;
                Rectangle { y: 0; width: parent.width; height: 1.5px; background: root.icon-sel ? #ffffff : #666666; }
                Rectangle { y: 3.25px; width: parent.width; height: 1.5px; background: root.icon-sel ? #ffffff : #666666; }
                Rectangle { y: 6.5px; width: parent.width; height: 1.5px; background: root.icon-sel ? #ffffff : #666666; }
            }
            ta_menu := TouchArea {
                mouse-cursor: pointer;
                clicked => { root.menu_clicked(); }
            }
        }
        // ☰ 菜单覆盖层（占位：符号/常用语/设置，功能后续迭代）
        if root.menu-open: Rectangle {
            x: 0;
            y: 0;
            width: parent.width;
            height: parent.height;
            background: #f7f8fa;
            border-radius: 8px;
            VerticalLayout {
                padding: 4px;
                spacing: 2px;
                for name in ["符号", "常用语", "设置"]: Rectangle {
                    height: 34px;
                    border-radius: 6px;
                    HorizontalLayout {
                        padding-left: 12px;
                        alignment: center;
                        Text {
                            text: name;
                            color: #1f2328;
                            font-size: 14px;
                            vertical-alignment: center;
                        }
                    }
                }
            }
            // 吞掉点击，避免穿透到下层按钮
            TouchArea { }
        }
        // ↓/↑ 移动高亮后把高亮行滚进可视区（自绘滚动条由 flick.content-y
        // 驱动，自动跟随）。28px = 格子高，6px = 视区上下呼吸边距。
        // 注意：Slint Flickable 的 content-y ≤ 0（向下滚动为负），
        // 可视区顶偏移 = -content-y，底 = -content-y + flick.height。
        // changed 只能监听本元素属性，故用镜像属性间接观察 root.hl-y
        property <int> hl-y-obs: root.hl-y;
        changed hl-y-obs => {
            if (((hl-y-obs * 1px) + 28px) > ((0px - flick.content-y) + flick.height)) {
                // 高亮在视区下方 → 滚到刚好露出（留 6px 呼吸）
                flick.content-y = Math.max(0px - scroll-range,
                    flick.height - ((hl-y-obs * 1px) + 28px + 6px));
            } else if ((hl-y-obs * 1px) < (0px - flick.content-y)) {
                // 高亮在视区上方
                flick.content-y = Math.min(0px, 6px - (hl-y-obs * 1px));
            }
        }
        }
        callback candidate_clicked(int);
        callback toggle_expand();
        callback expanded_clicked(int);
        callback menu_clicked();
    }
}

// 预乘 ARGB 像素（32 位视觉，支持窗口透明圆角）。
// #[repr(C)] 保证 b,g,r,a 内存序 = BGRA：Windows DIB 32bpp top-down 的
// 字节布局与之完全一致，blit 时可整块 memcpy（Linux X PutImage 也按
// BGRA 字节序拼包）。
#[derive(Clone, Copy)]
#[repr(C)]
struct Argb {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
}
impl Argb {
    const TRANSPARENT: Self = Self { b: 0, g: 0, r: 0, a: 0 };
}
impl Default for Argb {
    fn default() -> Self {
        Self::TRANSPARENT
    }
}
impl TargetPixel for Argb {
    fn blend(&mut self, color: slint::platform::software_renderer::PremultipliedRgbaColor) {
        let a = (255 - color.alpha) as u16;
        self.b = ((self.b as u16 * a) / 255) as u8 + color.blue;
        self.g = ((self.g as u16 * a) / 255) as u8 + color.green;
        self.r = ((self.r as u16 * a) / 255) as u8 + color.red;
        self.a = ((self.a as u16 + color.alpha as u16).min(255)) as u8;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { b, g, r, a: 255 }
    }
    /// 覆写默认实现（默认 = 不透明黑，会导致透明圆角外留黑角）：
    /// 窗口背景填充用预乘透明，圆角外保持 alpha=0。
    fn background() -> Self {
        Self::TRANSPARENT
    }
}

struct XPlatform;
impl Platform for XPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MSW.with(|w| w.clone()))
    }
    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        unreachable!("由 ui_thread_main 驱动")
    }
    fn duration_since_start(&self) -> core::time::Duration {
        Duration::ZERO
    }
}

thread_local! {
    static MSW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
}

/// 平台窗口后端：把预乘 ARGB 帧呈现到屏幕并回传指针事件
trait UiBackend {
    /// 定位 + 调整窗口尺寸（屏幕坐标）
    fn configure(&mut self, x: i32, y: i32, w: u32, h: u32);
    /// 显示 / 隐藏（显示不得夺取前台焦点）
    fn set_mapped(&mut self, mapped: bool);
    /// 呈现一帧预乘 ARGB（buf 长度 = w*h，字节序 BGRA）
    fn blit(&mut self, buf: &[Argb], w: u32, h: u32);
    /// 非阻塞泵平台事件，逐个交给 sink（PointerMoved/Pressed/Released/Exited）
    fn poll_events(&mut self, sink: &mut dyn FnMut(WindowEvent));
}

#[cfg(target_os = "linux")]
fn create_backend() -> Option<Box<dyn UiBackend>> {
    Some(Box::new(x11_backend::XWindow::new()?))
}

#[cfg(target_os = "windows")]
fn create_backend() -> Option<Box<dyn UiBackend>> {
    Some(Box::new(win_backend::WinWindow::new()?))
}

// macOS 后端（NSPanel + nonactivating panel，M-P4）尚未实现；
// 无后端时自绘候选窗不可用，引擎/会话/配置等其余能力不受影响。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn create_backend() -> Option<Box<dyn UiBackend>> {
    None
}

/// 系统 DPI 缩放因子（96dpi = 1.0）。渲染与窗口尺寸均按此放大；
/// 指针事件坐标反向除回逻辑像素。
#[cfg(target_os = "windows")]
fn ui_scale() -> f32 {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        if dc.is_null() {
            return 1.0;
        }
        let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
        ReleaseDC(std::ptr::null_mut(), dc);
        (dpi.max(96) as f32) / 96.0
    }
}

#[cfg(not(target_os = "windows"))]
fn ui_scale() -> f32 {
    1.0
}

// =====================================================================
// Linux 后端：override-redirect X 窗口（x11rb）
// =====================================================================
#[cfg(target_os = "linux")]
mod x11_backend {
    use super::{Argb, UiBackend, BAR_HEIGHT};
    use slint::platform::WindowEvent;
    use slint::LogicalPosition;
    use std::rc::Rc;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;

    pub struct XWindow {
        conn: Rc<x11rb::rust_connection::RustConnection>,
        win: u32,
        gc: u32,
        mapped: bool,
    }

    // void 请求统一校验：失败打印并返回 None（XWindow::new 用 ? 传导）
    macro_rules! check_void {
        ($req:expr, $what:expr) => {
            match $req {
                Ok(cookie) => match cookie.check() {
                    Ok(()) => Some(()),
                    Err(e) => {
                        eprintln!("heng-ui: {} 失败: {e}", $what);
                        None
                    }
                },
                Err(e) => {
                    eprintln!("heng-ui: {} 失败: {e}", $what);
                    None
                }
            }
        };
    }

    impl XWindow {
        pub fn new() -> Option<Self> {
            let (conn, screen_num) = x11rb::connect(None).ok()?;
            let screen = conn.setup().roots[screen_num].clone();
            // 找 32 位 TrueColor 视觉（ARGB，支持透明圆角）
            let mut visual = None;
            for d in &screen.allowed_depths {
                if d.depth == 32 {
                    for v in &d.visuals {
                        if v.class == VisualClass::TRUE_COLOR {
                            visual = Some(*v);
                            break;
                        }
                    }
                }
            }
            let Some(visual) = visual else {
                eprintln!("heng-ui: 未找到 32 位 TrueColor 视觉");
                return None;
            };
            let win = match conn.generate_id() {
                Ok(v) => v,
                Err(e) => { eprintln!("heng-ui: generate_id win 失败: {e}"); return None; }
            };
            let gc = match conn.generate_id() {
                Ok(v) => v,
                Err(e) => { eprintln!("heng-ui: generate_id gc 失败: {e}"); return None; }
            };
            let cmap = match conn.generate_id() {
                Ok(v) => v,
                Err(e) => { eprintln!("heng-ui: generate_id cmap 失败: {e}"); return None; }
            };
            check_void!(
                conn.create_colormap(ColormapAlloc::NONE, cmap, screen.root, visual.visual_id),
                "create_colormap"
            )?;
            let aux = CreateWindowAux::new()
                .override_redirect(1)
                .colormap(cmap)
                // 深度与父窗口不同时，X 协议要求显式指定 border_pixel（否则 BadMatch）
                .border_pixel(0)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION
                        | EventMask::LEAVE_WINDOW,
                );
            check_void!(
                conn.create_window(
                    32,
                    win,
                    screen.root,
                    200,
                    500,
                    160,
                    BAR_HEIGHT as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    visual.visual_id,
                    &aux,
                ),
                "create_window"
            )?;
            check_void!(conn.create_gc(gc, win, &CreateGCAux::new()), "create_gc")?;
            conn.flush().ok()?;
            Some(Self { conn: Rc::new(conn), win, gc, mapped: false })
        }

        fn configure(&mut self, x: i32, y: i32, w: u32, h: u32) {
            let aux = ConfigureWindowAux::new().x(x).y(y).width(w as u32).height(h as u32);
            let _ = match self.conn.configure_window(self.win, &aux) {
                Ok(c) => c.check(),
                Err(_) => Err(x11rb::errors::ReplyError::ConnectionError(
                    x11rb::errors::ConnectionError::UnknownError,
                )),
            };
            self.conn.flush().ok();
        }

        fn set_mapped(&mut self, mapped: bool) {
            if self.mapped == mapped {
                return;
            }
            let result = if mapped {
                self.conn.map_window(self.win).map(|c| c.check()).unwrap_or_else(|e| Err(e.into()))
            } else {
                self.conn.unmap_window(self.win).map(|c| c.check()).unwrap_or_else(|e| Err(e.into()))
            };
            if result.is_ok() {
                self.mapped = mapped;
            }
            self.conn.flush().ok();
        }

        fn blit(&mut self, buf: &[Argb], w: u32, h: u32) {
            let mut data = Vec::with_capacity(buf.len() * 4);
            for p in buf {
                data.push(p.b);
                data.push(p.g);
                data.push(p.r);
                data.push(p.a);
            }
            let result = self
                .conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    self.win,
                    self.gc,
                    w as u16,
                    h as u16,
                    0,
                    0,
                    0,
                    32,
                    &data,
                )
                .map(|c| c.check())
                .unwrap_or_else(|e| Err(e.into()));
            if let Err(e) = &result {
                eprintln!("heng-ui: put_image 失败: {e:?}");
            }
            // 调试：回读服务器端窗口首行像素，验证 put_image 是否生效
            match self.conn.get_image(
                ImageFormat::Z_PIXMAP,
                self.win,
                0,
                0,
                w as u16,
                1,
                0x00ffffff,
            ) {
                Ok(cookie) => match cookie.reply() {
                    Ok(img) => {
                        let p0 = u32::from_le_bytes([img.data[0], img.data[1], img.data[2], 0]);
                        eprintln!("heng-ui: 回读首像素={p0:#08x} data长度={}", img.data.len());
                    }
                    Err(e) => eprintln!("heng-ui: get_image reply 失败: {e}"),
                },
                Err(e) => eprintln!("heng-ui: get_image 请求失败: {e}"),
            }
            self.conn.flush().ok();
        }

        fn poll_events(&mut self, sink: &mut dyn FnMut(WindowEvent)) {
            while let Some(event) = self.conn.poll_for_event().unwrap_or(None) {
                match event {
                    Event::MotionNotify(m) => {
                        sink(WindowEvent::PointerMoved {
                            position: LogicalPosition::new(m.event_x as f32, m.event_y as f32),
                        });
                    }
                    Event::ButtonPress(b) if b.detail == 1 => {
                        sink(WindowEvent::PointerPressed {
                            position: LogicalPosition::new(b.event_x as f32, b.event_y as f32),
                            button: slint::platform::PointerEventButton::Left,
                        });
                    }
                    Event::ButtonRelease(b) if b.detail == 1 => {
                        sink(WindowEvent::PointerReleased {
                            position: LogicalPosition::new(b.event_x as f32, b.event_y as f32),
                            button: slint::platform::PointerEventButton::Left,
                        });
                    }
                    Event::LeaveNotify(_) => {
                        sink(WindowEvent::PointerExited);
                    }
                    _ => {}
                }
            }
        }
    }

    impl UiBackend for XWindow {
        fn configure(&mut self, x: i32, y: i32, w: u32, h: u32) {
            XWindow::configure(self, x, y, w, h)
        }
        fn set_mapped(&mut self, mapped: bool) {
            XWindow::set_mapped(self, mapped)
        }
        fn blit(&mut self, buf: &[Argb], w: u32, h: u32) {
            XWindow::blit(self, buf, w, h)
        }
        fn poll_events(&mut self, sink: &mut dyn FnMut(WindowEvent)) {
            XWindow::poll_events(self, sink)
        }
    }
}

// =====================================================================
// Windows 后端：WS_POPUP 分层窗口（CreateDIBSection + UpdateLayeredWindow）
//
// 关键点：
// - DIB 32bpp top-down（biHeight 取负）的内存字节序 = BGRA 预乘，与 Argb
//   （#[repr(C)]）完全一致，帧可直接 memcpy 进 DIB，零转换。
// - UpdateLayeredWindow(ULW_ALPHA, AC_SRC_OVER + AC_SRC_ALPHA) 呈现透明圆角。
// - WS_EX_NOACTIVATE + SW_SHOWNA：显示/点击都不夺前台焦点（M0.5 已真机验证）。
// - PeekMessage 轮询替代 X poll；WndProc 把鼠标事件转成 Slint WindowEvent。
// =====================================================================
#[cfg(target_os = "windows")]
mod win_backend {
    use super::{Argb, UiBackend, BAR_HEIGHT};
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint::LogicalPosition;
use std::cell::RefCell;
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{
        GetLastError, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        DIB_RGB_COLORS, HBITMAP, HDC, BI_RGB,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TME_LEAVE, TrackMouseEvent, TRACKMOUSEEVENT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, LoadCursorW, PeekMessageW,
        RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow,
        IDC_ARROW, MSG, PM_REMOVE, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
        ULW_ALPHA, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSW, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    thread_local! {
        /// WndProc 收集的指针事件，poll_events 时排空（WndProc 与主循环同线程）
        static EVENTS: RefCell<Vec<WindowEvent>> = RefCell::new(Vec::new());
    }

    fn utf16z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // 客户区坐标在 lparam 低/高 16 位（有符号）
        let (x, y) = (
            (lparam & 0xffff) as u16 as i16 as f32,
            ((lparam >> 16) & 0xffff) as u16 as i16 as f32,
        );
        match msg {
            WM_MOUSEMOVE => {
                EVENTS.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerMoved {
                        position: LogicalPosition::new(x, y),
                    })
                });
                // 注册离开跟踪（一次性，每次进窗口都要重注册才能收到 WM_MOUSELEAVE）
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
                0
            }
            WM_LBUTTONDOWN => {
                EVENTS.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerPressed {
                        position: LogicalPosition::new(x, y),
                        button: PointerEventButton::Left,
                    })
                });
                0
            }
            WM_LBUTTONUP => {
                EVENTS.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerReleased {
                        position: LogicalPosition::new(x, y),
                        button: PointerEventButton::Left,
                    })
                });
                0
            }
            WM_MOUSELEAVE => {
                EVENTS.with(|q| q.borrow_mut().push(WindowEvent::PointerExited));
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub struct WinWindow {
        hwnd: HWND,
        memdc: HDC,
        hbmp: HBITMAP,
        bits: *mut u8,
        cur_size: (u32, u32),
        pos: (i32, i32),
        mapped: bool,
    }

    impl WinWindow {
        pub fn new() -> Option<Self> {
            unsafe {
                let hinstance = GetModuleHandleW(std::ptr::null());
                let class_name = utf16z("heng_ui_win");
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(wnd_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinstance,
                    hIcon: std::ptr::null_mut(),
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    hbrBackground: std::ptr::null_mut(),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class_name.as_ptr(),
                };
                // 已注册时返回 0，忽略（同线程只建一次窗口）
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class_name.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    200,
                    500,
                    160,
                    BAR_HEIGHT as i32,
                    std::ptr::null_mut(), // 无父窗口
                    std::ptr::null_mut(), // 无菜单
                    hinstance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    eprintln!(
                        "heng-ui: CreateWindowExW 失败 (GetLastError={})",
                        GetLastError()
                    );
                    return None;
                }
                let memdc = CreateCompatibleDC(std::ptr::null_mut());
                if memdc.is_null() {
                    eprintln!("heng-ui: CreateCompatibleDC 失败");
                    return None;
                }
                Some(Self {
                    hwnd,
                    memdc,
                    hbmp: std::ptr::null_mut(),
                    bits: std::ptr::null_mut(),
                    cur_size: (0, 0),
                    pos: (200, 500),
                    mapped: false,
                })
            }
        }

        /// 按 (w,h) 重建 DIB（尺寸变化时）
        fn ensure_bitmap(&mut self, w: u32, h: u32) -> bool {
            unsafe {
                if self.cur_size == (w, h) && !self.bits.is_null() {
                    return true;
                }
                if !self.hbmp.is_null() {
                    DeleteObject(self.hbmp);
                    self.hbmp = std::ptr::null_mut();
                    self.bits = std::ptr::null_mut();
                }
                let mut bi: BITMAPINFO = std::mem::zeroed();
                bi.bmiHeader = BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // 负值 = top-down，与 Argb 行序一致
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut pv: *mut core::ffi::c_void = std::ptr::null_mut();
                let hbmp = CreateDIBSection(
                    self.memdc,
                    &bi,
                    DIB_RGB_COLORS,
                    &mut pv,
                    std::ptr::null_mut(),
                    0,
                );
                if hbmp.is_null() || pv.is_null() {
                    eprintln!(
                        "heng-ui: CreateDIBSection 失败 (GetLastError={})",
                        GetLastError()
                    );
                    return false;
                }
                SelectObject(self.memdc, hbmp);
                self.hbmp = hbmp;
                self.bits = pv as *mut u8;
                self.cur_size = (w, h);
                true
            }
        }
    }

    impl Drop for WinWindow {
        fn drop(&mut self) {
            unsafe {
                if !self.hbmp.is_null() {
                    DeleteObject(self.hbmp);
                }
                if !self.memdc.is_null() {
                    DeleteDC(self.memdc);
                }
            }
        }
    }

    impl UiBackend for WinWindow {
        fn configure(&mut self, x: i32, y: i32, w: u32, h: u32) {
            self.pos = (x, y);
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    x,
                    y,
                    w as i32,
                    h as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }

        fn set_mapped(&mut self, mapped: bool) {
            if self.mapped == mapped {
                return;
            }
            unsafe {
                // SW_SHOWNA：显示但不激活（不夺前台焦点，输入焦点留在宿主应用）
                ShowWindow(self.hwnd, if mapped { SW_SHOWNA } else { SW_HIDE });
            }
            self.mapped = mapped;
        }

        fn blit(&mut self, buf: &[Argb], w: u32, h: u32) {
            if !self.ensure_bitmap(w, h) {
                return;
            }
            unsafe {
                // Argb(#[repr(C)] b,g,r,a) 内存序 == DIB 32bpp top-down BGRA 预乘，直接拷
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr() as *const u8,
                    self.bits,
                    (w * h * 4) as usize,
                );
                let screen_dc = GetDC(std::ptr::null_mut());
                let pt_dst = POINT { x: self.pos.0, y: self.pos.1 };
                let size = SIZE { cx: w as i32, cy: h as i32 };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let ok = UpdateLayeredWindow(
                    self.hwnd,
                    screen_dc,
                    &pt_dst,
                    &size,
                    self.memdc,
                    &pt_src,
                    0,
                    &blend,
                    ULW_ALPHA,
                );
                ReleaseDC(std::ptr::null_mut(), screen_dc);
                if ok == 0 {
                    eprintln!(
                        "heng-ui: UpdateLayeredWindow 失败 (GetLastError={})",
                        GetLastError()
                    );
                }
            }
        }

        fn poll_events(&mut self, sink: &mut dyn FnMut(WindowEvent)) {
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE) != 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            EVENTS.with(|q| {
                for ev in q.borrow_mut().drain(..) {
                    sink(ev);
                }
            });
        }
    }
}

/// 收起态横条格子：锚定菜单第一页收集（当前页 + 向后借页凑满一行，微信
/// 同款尽量充满，单字≈9 个），取词需翻页调用方须持 OP_LOCK。
/// bar_base = 列表首项的全局序号，点击/空格/数字键选词换算用。
/// 同时登记 bar_items 缓存（横条导航不重取词）与词数/高亮共享状态。
fn set_bar_cells(
    ui: &CandWindow,
    engine: &crate::engine::Engine,
    rime_id: RimeSessionId,
    snapshot: &crate::engine::ContextSnapshot,
    bar_base: &Cell<i32>,
    bar_items: &RefCell<Vec<String>>,
    panel_hl: &Cell<i32>,
) {
    let (mut texts, hl, base) = match engine.get_bar_candidates(rime_id, 10) {
        Ok((t, hl, base)) => (t, hl, base),
        Err(_) => (
            snapshot
                .candidates
                .iter()
                .filter(|c| !c.text.is_empty())
                .map(|c| c.text.clone())
                .collect(),
            snapshot.highlighted,
            snapshot.page_no * snapshot.page_size.max(1),
        ),
    };
    bar_base.set(base);
    // 只保留首行可见词：40px 横条只显示一行，放不下的词不显示也不参与
    // 键盘导航（否则 → 要穿过一堆看不见的词才能到图标）
    let (cells0, _) = build_flow_cells(&texts, hl);
    let visible = cells0.iter().filter(|c| c.y == FLOW_TOP).count().max(1);
    let visible = visible.min(texts.len());
    texts.truncate(visible);
    panel_hl.set(hl.min(visible as i32 - 1).max(0));
    UI_HL.store(panel_hl.get(), Ordering::Relaxed);
    UI_BAR_COUNT.store(visible as i32, Ordering::Relaxed);
    *bar_items.borrow_mut() = texts.clone();
    let (cells, _rows) = build_flow_cells(&texts, panel_hl.get());
    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
    ui.set_panel_width(PANEL_WIDTH as i32);
}


/// 中间省略：字符数超 cap 时保留前半 + … + 后半（微信同款截断样式）
fn mid_ellipsis(t: &str, cap: usize) -> String {
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= cap {
        return t.to_string();
    }
    if cap < 1 {
        return "…".into();
    }
    let front = (cap - 1 + 1) / 2; // 前半多留一位
    let back = cap - 1 - front;
    let mut s: String = chars[..front].iter().collect();
    s.push('…');
    if back > 0 {
        s.extend(chars[chars.len() - back..].iter());
    }
    s
}

/// 内容自适应流式排布（收起单行同款）：格子宽度随词长走，一行能塞几个塞几个，
/// 不限个数；超长词封顶 150px 并中间省略。序号每行从 1 开始。
/// 返回 (cells, 行数)。hl_global 为当前选中候选的全局序号。
fn build_flow_cells(all: &[String], hl_global: i32) -> (Vec<CandCell>, i32) {
    let margin = 6i32;
    let gap = 3i32;
    let panel_w = PANEL_WIDTH as i32;
    // 首行右侧给 ▾/☰ 两个按钮留位；展开行给滚动条留位
    let first_row_right = panel_w - margin - 58;
    let row_right = panel_w - margin - 8;
    let mut cells: Vec<CandCell> = Vec::new();
    let (mut x, mut y) = (margin, FLOW_TOP);
    let mut num = 0i32;
    for (i, t) in all.iter().enumerate() {
        let disp = t.clone();
        // 宽度估算：emoji ≈ 20px（Windows 彩色 emoji 实渲染比 14px 字号宽，
        // 估 14px 会被 Text elide 剪成省略号）；零宽字符（变体选择符/ZWJ）算 0；
        // CJK 字 ≈ 14px（字号 14），ASCII ≈ 8px；序号 11px 字号 ≈ 7px/位
        let text_w: i32 = disp
            .chars()
            .map(|c| {
                let u = c as u32;
                if u == 0xFE0F || u == 0x200D {
                    0
                } else if u >= 0x1F000 || (0x2600..=0x27BF).contains(&u) {
                    20
                } else if u > 0x2E80 {
                    14
                } else {
                    8
                }
            })
            .sum();
        let num_w0 = if num < 9 { 7 } else { 14 };
        let full_w = 8 + num_w0 + 4 + text_w + 6;
        // 当前装不下完整一词 → 换行（每词完整显示，绝不截断塞边角）
        let right = if y == FLOW_TOP { first_row_right } else { row_right };
        if x + full_w > right && num > 0 {
            y += FLOW_ROW_H;
            x = margin;
            num = 0;
        }
        // 只有当一个词连一整行可填宽度都放不下（超长句）才中间省略
        let right = if y == FLOW_TOP { first_row_right } else { row_right };
        let max_w = right - margin;
        let num_w = if num < 9 { 7 } else { 14 };
        let full_w = 8 + num_w + 4 + text_w + 6;
        let (w, disp) = if full_w > max_w {
            let cap = ((max_w - 8 - num_w - 4 - 6).max(0) / 14) as usize;
            (max_w, mid_ellipsis(t, cap))
        } else {
            (full_w, disp)
        };
        if y > FLOW_TOP + FLOW_MAX_ROWS * FLOW_ROW_H {
            break; // 行数封顶，超出部分后续做滚动
        }
        cells.push(CandCell {
            num: SharedString::from((num + 1).to_string()),
            text: SharedString::from(disp),
            hl: (i as i32) == hl_global,
            x,
            y,
            w,
        });
        x += w + gap;
        num += 1;
    }
    let rows = ((y - FLOW_TOP) / FLOW_ROW_H) + 1;
    (cells, rows)
}

/// 固定 6 槽网格排布（展开面板同款）：每槽容纳 3 字 + 序号；
/// 词的槽位跨度 = ceil(字数/3)（4 字占 2 格、7 字占 3 格），占满 6 槽换行；
/// 槽内容纳不下时中间省略。序号每行从 1 开始。返回 (cells, 行数)。
fn build_grid_cells(all: &[String], hl_global: i32) -> (Vec<CandCell>, i32) {
    let margin = 6i32;
    let gap = 4i32;
    const SLOTS_PER_ROW: i32 = 6;
    let panel_w = PANEL_WIDTH as i32;
    let slot_w = (panel_w - margin * 2) / SLOTS_PER_ROW;
    let mut cells: Vec<CandCell> = Vec::new();
    let (mut x, mut y) = (margin, FLOW_TOP);
    let mut used_slots = 0i32;
    let mut num = 0i32;
    let mut hl_row: Option<i32> = None;
    for (i, t) in all.iter().enumerate() {
        let chars = t.chars().count() as i32;
        let spans = (((chars + 2) / 3).max(1)).min(SLOTS_PER_ROW);
        if used_slots + spans > SLOTS_PER_ROW {
            // 换行：序号从 1 重新开始
            y += FLOW_ROW_H;
            x = margin;
            used_slots = 0;
            num = 0;
        }
        if y > FLOW_TOP + FLOW_MAX_ROWS * FLOW_ROW_H {
            break; // 行数封顶，超出部分后续做滚动
        }
        let disp = mid_ellipsis(t, (spans * 3) as usize);
        cells.push(CandCell {
            num: SharedString::from((num + 1).to_string()),
            text: SharedString::from(disp),
            hl: (i as i32) == hl_global,
            x,
            y,
            w: spans * slot_w - gap,
        });
        if (i as i32) == hl_global {
            hl_row = Some(y);
        }
        x += spans * slot_w;
        used_slots += spans;
        num += 1;
    }
    // 微信同款：只有选中项所在行显示序号（数字键选词），其他行不显示
    if let Some(hy) = hl_row {
        for c in cells.iter_mut() {
            if c.y != hy {
                c.num = SharedString::from("");
            }
        }
    }
    let rows = ((y - FLOW_TOP) / FLOW_ROW_H) + 1;
    (cells, rows)
}

/// 拼音音节表（声母+韵母组合，贪心最长匹配用）
const SYLLABLES: &[&str] = &[
    "a","ai","an","ang","ao","bai","ban","bang","bao","bei","ben","beng","bi","bian","biao",
    "bie","bin","bing","bo","bu","ca","cai","can","cang","cao","ce","cen","ceng","cha","chai",
    "chan","chang","chao","che","chen","cheng","chi","chong","chou","chu","chua","chuai","chuan",
    "chuang","chui","chun","chuo","ci","cong","cou","cu","cuan","cui","cun","cuo","da","dai",
    "dan","dang","dao","de","deng","di","dia","dian","diao","die","ding","diu","dong","dou",
    "du","duan","dui","dun","duo","e","ei","en","er","fa","fan","fang","fei","fen","feng","fo",
    "fou","fu","ga","gai","gan","gang","gao","ge","gei","gen","geng","gong","gou","gu","gua",
    "guai","guan","guang","gui","gun","guo","ha","hai","han","hang","hao","he","hei","hen",
    "heng","hong","hou","hu","hua","huai","huan","huang","hui","hun","huo","ji","jia","jian",
    "jiang","jiao","jie","jin","jing","jiong","jiu","ju","juan","jue","jun","ka","kai","kan",
    "kang","kao","ke","ken","keng","kong","kou","ku","kua","kuai","kuan","kuang","kui","kun",
    "kuo","la","lai","lan","lang","lao","le","lei","leng","li","lia","lian","liang","liao",
    "lie","lin","ling","liu","lo","long","lou","lu","luan","lun","luo","ma","mai","man","mang",
    "mao","me","mei","men","meng","mi","mian","miao","mie","min","ming","miu","mo","mou","mu",
    "na","nai","nan","nang","nao","ne","nei","nen","neng","ni","nian","niang","niao","nie",
    "nin","ning","niu","nong","nou","nu","nuan","nuo","o","ou","pa","pai","pan","pang","pao",
    "pei","pen","peng","pi","pian","piao","pie","pin","ping","po","pou","pu","qi","qia","qian",
    "qiang","qiao","qie","qin","qing","qiong","qiu","qu","quan","que","qun","ran","rang","rao",
    "re","ren","reng","ri","rong","rou","ru","rua","ruan","rui","run","ruo","sa","sai","san",
    "sang","sao","se","sen","seng","sha","shai","shan","shang","shao","she","shei","shen",
    "sheng","shi","shou","shu","shua","shuai","shuan","shuang","shui","shun","shuo","si","song",
    "sou","su","suan","sui","sun","suo","ta","tai","tan","tang","tao","te","teng","ti","tian",
    "tiao","tie","ting","tong","tou","tu","tuan","tui","tun","tuo","wa","wai","wan","wang",
    "wei","wen","weng","wo","wu","xi","xia","xian","xiang","xiao","xie","xin","xing","xiong",
    "xiu","xu","xuan","xue","xun","ya","yan","yang","yao","ye","yi","yin","ying","yo","yong",
    "you","yu","yuan","yue","yun","za","zai","zan","zang","zao","ze","zei","zen","zeng","zha",
    "zhai","zhan","zhang","zhao","zhe","zhen","zheng","zhi","zhong","zhou","zhu","zhua","zhuai",
    "zhuan","zhuang","zhui","zhun","zhuo","zi","zong","zou","zu","zuan","zui","zun","zuo",
];

/// 从原始输入提取第一个音节（贪心最长匹配）
fn first_syllable(raw: &str) -> String {
    let letters: String = raw.chars().filter(|c| c.is_ascii_lowercase()).collect();
    let chars: Vec<char> = letters.chars().collect();
    for len in (1..=chars.len().min(6)).rev() {
        let candidate: String = chars[..len].iter().collect();
        if SYLLABLES.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    String::new()
}

/// 轻量诊断日志（%TEMP%\heng-ui.log）：WeaselServer 是 GUI 进程无 stderr，
/// 展开面板内容异常（候选少/同音词空）时靠它定位是哪一环
fn ui_log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("heng-ui.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

/// 同音词填充：临时会话输入首音节，收集其候选（排除已展示的）
fn collect_homophones(engine: &crate::engine::Engine, rime_id: RimeSessionId, existing: &[String]) -> Vec<String> {
    let raw = engine.get_input(rime_id).unwrap_or_default();
    let mut syl = first_syllable(&raw);
    // 简拼（如 "nh"）提不出完整音节 → 回退用首字母作查询，保证展开面板不空
    if syl.is_empty() {
        syl = raw.chars().find(|c| c.is_ascii_lowercase()).map(String::from).unwrap_or_default();
    }
    ui_log(&format!("homophones: raw={raw:?} 音节={syl:?}"));
    if syl.is_empty() {
        return vec![];
    }
    let Ok(session) = engine.create_session() else {
        return vec![];
    };
    let tid = session.into_raw();
    let mut out = Vec::new();
    if engine.simulate_key_sequence(tid, &syl).unwrap_or(false) {
        // 首页 + 多翻几页（微信可滚很久，这里尽量多给）
        for _ in 0..6 {
            let Ok(snap) = engine.get_context(tid) else { break };
            if snap.candidates.is_empty() {
                break;
            }
            for c in &snap.candidates {
                if !c.text.is_empty()
                    && !existing.contains(&c.text)
                    && !out.contains(&c.text)
                {
                    out.push(c.text.clone());
                }
            }
            if snap.is_last_page || !engine.process_key(tid, 0xff56, 0).unwrap_or(false) {
                break;
            }
        }
    }
    engine.destroy_session(tid);
    ui_log(&format!("homophones: 音节={syl:?} 取得={}（去重后）", out.len()));
    out
}

/// 调试用：导出当前帧为 PPM（HENG_UI_DUMP 环境变量开启）。
/// 同时打印角像素 alpha（诊断透明圆角是否生效）。
fn dump_frame(buf: &[Argb], w: u32, h: u32) {
    let path = if cfg!(target_os = "windows") {
        std::env::temp_dir().join("heng-bar.ppm")
    } else {
        std::path::PathBuf::from("/tmp/heng-bar.ppm")
    };
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in buf.iter() {
        ppm.push(p.r);
        ppm.push(p.g);
        ppm.push(p.b);
    }
    let _ = std::fs::write(path, &ppm);
    // 角像素 alpha 诊断：四角应为 0（透明），若为 255 说明透明圆角失效
    let (lw, lh) = (w as usize, h as usize);
    eprintln!(
        "heng-ui: 角像素 alpha = 左上{} 右上{} 左下{} 右下{} | 首行中点 a={} rgb=({},{},{})",
        buf[0].a,
        buf[lw - 1].a,
        buf[lh * lw - lw].a,
        buf[lh * lw - 1].a,
        buf[lw >> 1].a,
        buf[lw >> 1].r,
        buf[lw >> 1].g,
        buf[lw >> 1].b,
    );
}

fn ui_thread_main(rx: Receiver<UiCmd>, tx: Sender<UiCmd>) {
    // Slint 平台必须先于组件创建注册
    slint::platform::set_platform(Box::new(XPlatform)).expect("heng-ui: set_platform 失败");
    let expanded = Rc::new(Cell::new(false));
    // 展开面板快照：内容在展开瞬间固定，导航只动视觉高亮，不碰 rime
    struct PanelSnap {
        items: Vec<String>,
        page_size: i32,
        real_count: usize, // 真实候选数（其后为同音词）
        geo: Vec<(i32, i32, i32)>, // 每格 (x,y,w)，按行导航用
    }
    let panel: Rc<RefCell<Option<PanelSnap>>> = Rc::new(RefCell::new(None));
    let panel_hl = Rc::new(Cell::new(0i32));
    // 收起横条列表首项的全局序号（点击/键盘选词换算用）
    let bar_base = Rc::new(Cell::new(0i32));
    // 收起横条词表缓存（←→ 移动视觉高亮时不重取词）
    let bar_items: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // 收起横条：☰（最右图标）选中态；菜单打开态
    let icon_sel = Rc::new(Cell::new(false));
    let menu_open = Rc::new(Cell::new(false));
    let last_panel_h = Cell::new(0u32);
    let last_x = Cell::new(200);
    let last_y = Cell::new(500);
    let Some(mut backend) = create_backend() else {
        eprintln!("heng-ui: 平台窗口创建失败，自绘候选窗不可用（外壳回退宿主候选窗）");
        return;
    };
    let msw = MSW.with(|w| w.clone());
    let ui = Rc::new(CandWindow::new().expect("heng-ui: 组件创建失败"));
    // DPI 缩放：布局用逻辑像素，渲染/窗口尺寸放大到物理像素
    let scale = ui_scale();
    let bar_h = (BAR_HEIGHT as f32 * scale).round() as u32;
    msw.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    msw.set_size(PhysicalSize { width: (160.0 * scale) as u32, height: bar_h });
    ui.show().unwrap(); // 不调用则 Slint 认为窗口不可见，永远不渲染

    // 当前展示的会话（Sync 时登记；点击选词用）
    let current: Rc<RefCell<Option<RimeSessionId>>> = Rc::new(RefCell::new(None));
    let render_buf: RefCell<Vec<Argb>> = RefCell::new(Vec::new());
    let visible = RefCell::new(false);
    let cur_h = Cell::new(bar_h); // 当前物理高度（展开态会变）
    let last_preedit: RefCell<String> = RefCell::new(String::new());
    // 渲染并上屏（Sync / 展开 / 收起共用）。w,h 物理像素；map=true 时确保已映射
    // （XWayland：未映射窗口的 PutImage 会被静默丢弃，必须先 map 再画）
    let paint = |backend: &mut Box<dyn UiBackend>,
                 w: u32,
                 h: u32,
                 x: i32,
                 y: i32,
                 map: bool| {
        cur_h.set(h);
        msw.set_size(PhysicalSize { width: w, height: h });
        backend.configure(x, y, w, h);
        if render_buf.borrow().len() != (w * h) as usize {
            *render_buf.borrow_mut() = vec![Argb::default(); (w * h) as usize];
        }
        if map {
            backend.set_mapped(true);
        }
        msw.window().request_redraw();
        {
            let mut buf = render_buf.borrow_mut();
            // 渲染器内部对异常内容可能 panic（如负坐标 Overflow），
            // 捕获以保证 UI 线程存活（最坏丢一帧）
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                msw.draw_if_needed(|r: &SoftwareRenderer| {
                    let _ = r.render(&mut buf, w as usize);
                });
            }));
        }
        {
            let buf = render_buf.borrow();
            if std::env::var_os("HENG_UI_DUMP").is_some() {
                dump_frame(&buf, w, h);
            }
            backend.blit(&buf, w, h);
        }
        *visible.borrow_mut() = map || *visible.borrow();
    };


    // 展开箭头 / 展开列表点击 → 内部命令（在循环里统一处理，避免借用冲突）
    ui.on_toggle_expand({
        let tx = tx.clone();
        move || {
            let _ = tx.send(UiCmd::ToggleExpand);
        }
    });
    ui.on_menu_clicked({
        let tx = tx.clone();
        move || {
            let _ = tx.send(UiCmd::MenuOpen); // ☰ 菜单（占位：符号/常用语/设置）
        }
    });
    ui.on_expanded_clicked({
        let tx = tx.clone();
        let panel_hl = panel_hl.clone();
        move |idx| {
            panel_hl.set(idx as i32);
            let _ = tx.send(UiCmd::SelectHL);
        }
    });

    // 点击选词：收起态走 BarSelect（横条跨页换算在命令处理器里统一做），
    // 展开态走 SelectHL（面板快照 + 同音词路径）
    ui.on_candidate_clicked({
        let expanded = expanded.clone();
        let panel_hl = panel_hl.clone();
        let tx = tx.clone();
        move |idx| {
            if expanded.get() {
                panel_hl.set(idx as i32);
                let _ = tx.send(UiCmd::SelectHL);
            } else {
                let _ = tx.send(UiCmd::BarSelect(idx as usize));
            }
        }
    });

    loop {
        // 1. 命令
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                UiCmd::Sync { rime_id, x, y } => {
                    *current.borrow_mut() = Some(rime_id);
                    let Ok(engine) = engine() else { continue };
                    let snapshot = {
                        let _guard = OP_LOCK.lock().unwrap();
                        engine.get_context(rime_id)
                    };
                    if let Ok(snapshot) = snapshot {
                        last_x.set(x);
                        last_y.set(y);
                        // 输入串变化才收起面板（↓/↑ 导航不改 preedit，面板保持）
                        let input_changed = *last_preedit.borrow() != snapshot.preedit;
                        *last_preedit.borrow_mut() = snapshot.preedit.clone();
                        if snapshot.candidates.is_empty() {
                            backend.set_mapped(false);
                            *visible.borrow_mut() = false;
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                        } else if !input_changed {
                            // 视觉态保持，什么都不做：
                            // - 展开面板：内容 = 展开瞬间快照（导航键不碰 rime）
                            // - 收起横条：高亮是视觉态（bar_items + panel_hl），
                            //   ←→/空格/数字键被 core 拦截后外壳仍会 sync，
                            //   重建会把药丸打回 librime 高亮位置（选错词的根源）
                        } else {
                            // 新输入 / 首次显示：重建横条（借页凑满需持锁翻页）
                            let _guard = OP_LOCK.lock().unwrap();
                            menu_open.set(false);
                            UI_MENU_OPEN.store(false, Ordering::Relaxed);
                            ui.set_menu_open(false);
                            icon_sel.set(false);
                            UI_ICON_SEL.store(false, Ordering::Relaxed);
                            ui.set_icon_sel(false);
                            set_bar_cells(
                                &ui,
                                &engine,
                                rime_id,
                                &snapshot,
                                &bar_base,
                                &bar_items,
                                &panel_hl,
                            );
                            let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                            ui.set_expanded(false);
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            *panel.borrow_mut() = None;
                            paint(&mut backend, w, bar_h, x, y, true);
                        }
                    }
                }
                UiCmd::Hide => {
                    backend.set_mapped(false);
                    *visible.borrow_mut() = false;
                }
                UiCmd::ToggleExpand => {
                    if !*visible.borrow() {
                        continue;
                    }
                    let expanding = !expanded.get();
                    if expanding {
                        // 展开：真实候选（跨页）+ 首音节同音词填充。
                        // 面板内容 = 展开瞬间的快照，导航不碰 rime
                        let Some(rime_id) = *current.borrow() else { continue };
                        let Ok(engine) = engine() else { continue };
                        let (real, page_size, snapshot) = {
                            let _guard = OP_LOCK.lock().unwrap();
                            let r = engine.get_all_candidates(rime_id).unwrap_or_default();
                            let snap = engine.get_context(rime_id).ok();
                            (r.0, r.1, snap)
                        };
                        if real.is_empty() {
                            continue;
                        }
                        let real_count = real.len();
                        let homophones = collect_homophones(engine, rime_id, &real);
                        let homo_count = homophones.len();
                        let mut all = real.clone();
                        all.extend(homophones);
                        let total = all.len() as i32;
                        let hl_global = snapshot
                            .as_ref()
                            .map(|s| s.page_no * s.page_size + s.highlighted)
                            .unwrap_or(0);
                        // 诊断日志（%TEMP%\heng-ui.log）：展开内容不足时定位
                        // 是真实候选收集少还是同音词没取到
                        ui_log(&format!(
                            "expand: 真实候选={real_count} 同音词={homo_count} 总={total} hl={hl_global}"
                        ));
                        panel_hl.set(hl_global.max(0));
                        let (cells, rows) = build_grid_cells(&all, hl_global);
                        let hl_y = cells.iter().find(|c| c.hl).map(|c| c.y).unwrap_or(0);
                        *panel.borrow_mut() = Some(PanelSnap {
                            items: all,
                            page_size,
                            real_count,
                            geo: cells
                                .iter()
                                .map(|c| (c.x, c.y, c.w))
                                .collect(),
                        });
                        ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
                        // 滚动跟随：先置 -1 强制触发 changed（顺带把展开前残留的
                        // flick.content-y 归零），再写入真实高亮 y
                        ui.set_hl_y(-1);
                        ui.set_hl_y(hl_y);
                        ui.set_panel_width(PANEL_WIDTH as i32);
                        let content_h = FLOW_TOP + (rows - 1) * FLOW_ROW_H + 28 + 8;
                        let h_logical =
                            (8 + PANEL_VISIBLE_ROWS * FLOW_ROW_H + 8).max(bar_h as i32);
                        ui.set_content_height(content_h);
                        ui.set_panel_height(h_logical);
                        last_panel_h.set((h_logical as f32 * scale).round() as u32);
                        ui.set_expanded(true);
                        UI_EXPANDED.store(true, Ordering::Relaxed);
                        icon_sel.set(false);
                        UI_ICON_SEL.store(false, Ordering::Relaxed);
                        ui.set_icon_sel(false);
                        menu_open.set(false);
                        UI_MENU_OPEN.store(false, Ordering::Relaxed);
                        ui.set_menu_open(false);
                        UI_HL.store(hl_global, Ordering::Relaxed);
                        UI_TOTAL.store(total, Ordering::Relaxed);
                        let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                        paint(&mut backend, w, last_panel_h.get(), last_x.get(), last_y.get(), true);
                        expanded.set(true);
                    } else {
                        // 收起：重建横条（展开网格布局 ≠ 横条流式布局，直接缩窗会错位）
                        ui.set_expanded(false);
                        UI_EXPANDED.store(false, Ordering::Relaxed);
                        *panel.borrow_mut() = None;
                        if let Some(rime_id) = *current.borrow() {
                            if let Ok(engine) = engine() {
                                let _guard = OP_LOCK.lock().unwrap();
                                if let Ok(snapshot) = engine.get_context(rime_id) {
                                    set_bar_cells(
                                        &ui,
                                        &engine,
                                        rime_id,
                                        &snapshot,
                                        &bar_base,
                                        &bar_items,
                                        &panel_hl,
                                    );
                                }
                            }
                        }
                        let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                        paint(&mut backend, w, bar_h, last_x.get(), last_y.get(), true);
                        expanded.set(false);
                    }
                }
                UiCmd::MoveHL(delta) => {
                    // 展开态 = 面板快照（6 槽网格）；收起态 = 横条缓存（流式一行）
                    let in_panel = panel.borrow().as_ref().is_some();
                    let (items, total) = if in_panel {
                        let p = panel.borrow();
                        let snap = p.as_ref().unwrap();
                        (snap.items.clone(), snap.items.len() as i32)
                    } else {
                        let b = bar_items.borrow();
                        (b.clone(), b.len() as i32)
                    };
                    if total == 0 {
                        continue;
                    }
                    let new_hl = (panel_hl.get() + delta).clamp(0, total - 1);
                    if new_hl == panel_hl.get() {
                        continue;
                    }
                    panel_hl.set(new_hl);
                    UI_HL.store(new_hl, Ordering::Relaxed);
                    icon_sel.set(false);
                    UI_ICON_SEL.store(false, Ordering::Relaxed);
                    ui.set_icon_sel(false);
                    let (cells, _rows) = if in_panel {
                        build_grid_cells(&items, new_hl)
                    } else {
                        build_flow_cells(&items, new_hl)
                    };
                    let hl_y = cells.iter().find(|c| c.hl).map(|c| c.y).unwrap_or(0);
                    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
                    ui.set_hl_y(hl_y);
                    let h = if in_panel { last_panel_h.get() } else { bar_h };
                    paint(
                        &mut backend,
                        (PANEL_WIDTH as f32 * scale).round() as u32,
                        h,
                        last_x.get(),
                        last_y.get(),
                        false,
                    );
                }
                UiCmd::RowMove(delta) => {
                    // 行感知导航：↓ 下一行 / ↑ 上一行，列位置取中心最近的一格。
                    // 行内容自适应宽度后每行格数不定，固定 ±6 格会直接跳飞
                    let Some((geo, items)) = panel.borrow().as_ref().map(|s| {
                        (s.geo.clone(), s.items.clone())
                    }) else {
                        continue;
                    };
                    let hl = panel_hl.get();
                    let Some(&(_, cy, _)) = geo.get(hl.max(0) as usize) else {
                        continue;
                    };
                    let target_row = if delta < 0 {
                        let first_row = geo.iter().map(|g| g.1).min().unwrap_or(cy);
                        if cy <= first_row {
                            // 首行 ↑ → 收起面板（重建横条：网格≠流式布局，
                            // 直接缩窗会把网格第一行留在横条里、文字压到图标下）
                            ui.set_expanded(false);
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            *panel.borrow_mut() = None;
                            if let Some(rime_id) = *current.borrow() {
                                if let Ok(engine) = engine() {
                                    let _guard = OP_LOCK.lock().unwrap();
                                    if let Ok(snapshot) = engine.get_context(rime_id) {
                                        if !snapshot.candidates.is_empty() {
                                            set_bar_cells(
                                                &ui,
                                                &engine,
                                                rime_id,
                                                &snapshot,
                                                &bar_base,
                                                &bar_items,
                                                &panel_hl,
                                            );
                                        }
                                    }
                                }
                            }
                            let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                            paint(&mut backend, w, bar_h, last_x.get(), last_y.get(), true);
                            continue;
                        }
                        geo.iter().map(|g| g.1).filter(|&y| y < cy).max()
                    } else {
                        geo.iter().map(|g| g.1).filter(|&y| y > cy).min()
                    };
                    let Some(target_row) = target_row else { continue };
                    // 同序位移动：选中本行第 N 个 → 目标行第 N 个；
                    // 目标行更短（不足 N 个）→ 选该行最后一个
                    let cur_pos = geo
                        .iter()
                        .enumerate()
                        .filter(|(_, &(_, gy, _))| gy == cy)
                        .position(|(i, _)| i == hl.max(0) as usize)
                        .unwrap_or(0);
                    let target_items: Vec<usize> = geo
                        .iter()
                        .enumerate()
                        .filter(|(_, &(_, gy, _))| gy == target_row)
                        .map(|(i, _)| i)
                        .collect();
                    let Some(&new_hl) = target_items
                        .get(cur_pos)
                        .or_else(|| target_items.last())
                    else {
                        continue;
                    };
                    panel_hl.set(new_hl as i32);
                    UI_HL.store(new_hl as i32, Ordering::Relaxed);
                    let (cells, _rows) = build_grid_cells(&items, new_hl as i32);
                    let hl_y = cells.iter().find(|c| c.hl).map(|c| c.y).unwrap_or(0);
                    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
                    ui.set_hl_y(hl_y);
                    paint(
                        &mut backend,
                        (PANEL_WIDTH as f32 * scale).round() as u32,
                        last_panel_h.get(),
                        last_x.get(),
                        last_y.get(),
                        false,
                    );
                }
                UiCmd::BarIcon(on) => {
                    // ☰（最右图标）选中：词高亮隐藏；取消：恢复词高亮
                    // （panel_hl 保持不动，← 回来即原词）
                    icon_sel.set(on);
                    UI_ICON_SEL.store(on, Ordering::Relaxed);
                    ui.set_icon_sel(on);
                    let items = bar_items.borrow().clone();
                    let hl = if on { -1 } else { panel_hl.get() };
                    let (cells, _rows) = build_flow_cells(&items, hl);
                    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
                    paint(
                        &mut backend,
                        (PANEL_WIDTH as f32 * scale).round() as u32,
                        bar_h,
                        last_x.get(),
                        last_y.get(),
                        true,
                    );
                }
                UiCmd::BarSelect(idx) => {
                    // 收起横条选词（鼠标点击 / 空格确认 / 数字键）：
                    // 横条列表锚定菜单第一页，idx + bar_base = 全局序号
                    let Some(rime_id) = *current.borrow() else { continue };
                    let Ok(engine) = engine() else { continue };
                    let _guard = OP_LOCK.lock().unwrap();
                    let selected = engine
                        .select_candidate_global(rime_id, bar_base.get().max(0) as usize + idx)
                        .unwrap_or(false);
                    if !selected {
                        continue;
                    }
                    if let Ok(text) = engine.get_commit(rime_id) {
                        if !text.is_empty() {
                            // 挂到对应外壳会话句柄（反向查 handle），外壳取走上屏
                            if let Some(h) = SESSIONS.handle_of(rime_id) {
                                PENDING_UI_COMMITS.lock().unwrap().insert(h, text);
                            }
                        }
                    }
                    // 刷新横条（选词后组合串可能仍在：继续输入下一个字母）
                    if let Ok(snapshot) = engine.get_context(rime_id) {
                        if snapshot.candidates.is_empty() {
                            backend.set_mapped(false);
                            *visible.borrow_mut() = false;
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                        } else {
                            set_bar_cells(
                                &ui,
                                &engine,
                                rime_id,
                                &snapshot,
                                &bar_base,
                                &bar_items,
                                &panel_hl,
                            );
                            let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                            paint(&mut backend, w, bar_h, last_x.get(), last_y.get(), true);
                        }
                    }
                }
                UiCmd::MenuOpen => {
                    // ☰ 菜单（占位）：面板长高显示 符号/常用语/设置，功能后续迭代
                    menu_open.set(true);
                    UI_MENU_OPEN.store(true, Ordering::Relaxed);
                    ui.set_menu_open(true);
                    let h = (MENU_HEIGHT as f32 * scale).round() as u32;
                    paint(
                        &mut backend,
                        (PANEL_WIDTH as f32 * scale).round() as u32,
                        h,
                        last_x.get(),
                        last_y.get(),
                        true,
                    );
                }
                UiCmd::MenuClose => {
                    menu_open.set(false);
                    UI_MENU_OPEN.store(false, Ordering::Relaxed);
                    ui.set_menu_open(false);
                    paint(
                        &mut backend,
                        (PANEL_WIDTH as f32 * scale).round() as u32,
                        bar_h,
                        last_x.get(),
                        last_y.get(),
                        true,
                    );
                }
                UiCmd::SelectHL => {
                    // 空格/回车：选中面板当前高亮项
                    let idx = panel_hl.get().max(0) as usize;
                    let Some(rime_id) = *current.borrow() else { continue };
                    let Ok(engine) = engine() else { continue };
                    let item = panel.borrow().as_ref().and_then(|snap| {
                        let real = idx < snap.real_count;
                        snap.items.get(idx).cloned().map(|t| (t, real))
                    });
                    let Some((text, is_real)) = item else { continue };
                    {
                        let _guard = OP_LOCK.lock().unwrap();
                        let ok = if is_real {
                            engine.select_candidate_global(rime_id, idx).unwrap_or(false)
                        } else {
                            // 同音词：清组合串 + 文本直接上屏
                            let _ = engine.clear_composition(rime_id);
                            if let Some(h) = SESSIONS.handle_of(rime_id) {
                                PENDING_UI_COMMITS.lock().unwrap().insert(h, text.clone());
                            }
                            true
                        };
                        if ok && !is_real {
                            // 同音词上屏后组合串已清，直接收起
                            ui.set_expanded(false);
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            *panel.borrow_mut() = None;
                            backend.set_mapped(false);
                            *visible.borrow_mut() = false;
                        }
                    }
                    if is_real {
                        // 真实候选：可能仍在组句（如首词后继续），收面板恢复横条
                        ui.set_expanded(false);
                        expanded.set(false);
                        UI_EXPANDED.store(false, Ordering::Relaxed);
                        *panel.borrow_mut() = None;
                        let snapshot = {
                            let _guard = OP_LOCK.lock().unwrap();
                            engine.get_context(rime_id)
                        };
                        if let Ok(snapshot) = snapshot {
                            if snapshot.candidates.is_empty() {
                                backend.set_mapped(false);
                                *visible.borrow_mut() = false;
                            } else {
                                let _guard = OP_LOCK.lock().unwrap();
                                set_bar_cells(
                                    &ui,
                                    &engine,
                                    rime_id,
                                    &snapshot,
                                    &bar_base,
                                    &bar_items,
                                    &panel_hl,
                                );
                                let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                                paint(
                                    &mut backend,
                                    w,
                                    bar_h,
                                    last_x.get(),
                                    last_y.get(),
                                    true,
                                );
                            }
                        }
                    }
                }
                UiCmd::SetExpanded(v) => {
                    if v || !*visible.borrow() {
                        continue;
                    }
                    // 键盘 ↑（面板第一行）触发的收起
                    ui.set_expanded(false);
                    expanded.set(false);
                    UI_EXPANDED.store(false, Ordering::Relaxed);
                    *panel.borrow_mut() = None;
                    // 重建横条（原格子是展开网格布局，直接缩窗会错位）
                    if let Some(rime_id) = *current.borrow() {
                        if let Ok(engine) = engine() {
                            let _guard = OP_LOCK.lock().unwrap();
                            if let Ok(snapshot) = engine.get_context(rime_id) {
                                if !snapshot.candidates.is_empty() {
                                    set_bar_cells(
                                        &ui,
                                        &engine,
                                        rime_id,
                                        &snapshot,
                                        &bar_base,
                                        &bar_items,
                                        &panel_hl,
                                    );
                                }
                            }
                        }
                    }
                    let w = (PANEL_WIDTH as f32 * scale).round() as u32;
                    paint(&mut backend, w, bar_h, last_x.get(), last_y.get(), true);
                }
            }
        }

        // 2. 平台事件（仅窗口可见时有意义，但轮询保持廉价）
        //    物理像素坐标 → 逻辑像素（Slint 指针事件约定逻辑坐标）
        backend.poll_events(&mut |event| {
            if !*visible.borrow() {
                return;
            }
            let event = match event {
                WindowEvent::PointerMoved { position } => WindowEvent::PointerMoved {
                    position: LogicalPosition::new(position.x / scale, position.y / scale),
                },
                WindowEvent::PointerPressed { position, button } => {
                    WindowEvent::PointerPressed {
                        position: LogicalPosition::new(position.x / scale, position.y / scale),
                        button,
                    }
                }
                WindowEvent::PointerReleased { position, button } => {
                    WindowEvent::PointerReleased {
                        position: LogicalPosition::new(position.x / scale, position.y / scale),
                        button,
                    }
                }
                other => other,
            };
            msw.window().dispatch_event(event);
        });

        // 3. Slint 推进 + 渲染
        slint::platform::update_timers_and_animations();
        msw.draw_if_needed(|renderer: &SoftwareRenderer| {
            // 首次 Sync 前缓冲尚未按内容尺寸分配，跳过渲染
            let (w, h) = {
                let buf = render_buf.borrow();
                let h = cur_h.get();
                if buf.is_empty() || buf.len() as u32 % h != 0 {
                    return;
                }
                (buf.len() as u32 / h, h)
            };
            {
                let mut buf = render_buf.borrow_mut();
                // 同 paint：捕获渲染 panic，保证 UI 线程存活
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    renderer.render(&mut buf, w as usize)
                }));
            }
            let buf = render_buf.borrow();
            backend.blit(&buf, w, h);
        });

        std::thread::sleep(Duration::from_millis(8));
    }
}
