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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
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
const MAX_WIDTH: u32 = 780;
const BAR_HEIGHT: u32 = 40;

// ---- 对外接口（capi 调用） ----

/// UI 线程命令
enum UiCmd {
    /// 拉取当前会话 context 并刷新显示；有候选则显示在 (x,y)，无候选则隐藏
    Sync { rime_id: RimeSessionId, x: i32, y: i32 },
    Hide,
}

static UI_CMD_TX: LazyLock<Mutex<Option<Sender<UiCmd>>>> = LazyLock::new(|| Mutex::new(None));

/// UI 点击产生的待上屏文本：外壳经 heng_take_ui_commit 取走
pub static PENDING_UI_COMMITS: LazyLock<Mutex<std::collections::HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

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

/// 惰性启动 UI 线程（首次 ui_sync 时）。启动失败（无 X 显示等）返回 false，
/// 外壳可回退到宿主候选窗。
pub fn ensure_started() -> bool {
    let mut guard = UI_CMD_TX.lock().unwrap();
    if guard.is_some() {
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let ok = std::panic::catch_unwind(|| {
        std::thread::Builder::new()
            .name("heng-ui".into())
            .spawn(move || ui_thread_main(rx))
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
    // 微信输入法式横排候选栏：单条横排，蓝块白字选中，悬浮细蓝边框提示
    export component CandWindow inherits Window {
        in property <int> highlight;
        in property <[string]> labels;
        in property <[string]> candidates;
        in property <int> hover;
        in property <int> bar-width;
        width: root.bar-width * 1px;
        height: 40px;
        background: transparent;
        // 最外层容器：与选中块相同圆角（8px）
        Rectangle {
            width: 100%;
            height: 100%;
            border-radius: 8px;
            background: #f7f8fa;
        HorizontalLayout {
            padding: 4px;
            spacing: 2px;
            for c[idx] in root.candidates: Rectangle {
                height: 32px;
                border-radius: 8px;
                background: idx == root.highlight ? #2164f1 : transparent;
                border-width: 1px;
                border-color: (idx == root.hover && idx != root.highlight)
                    ? #7fa8f5 : transparent;
                HorizontalLayout {
                    padding-left: 7px;
                    padding-right: 7px;
                    spacing: 4px;
                    alignment: center;
                    Text {
                        text: root.labels[idx];
                        color: idx == root.highlight ? #cfe0ff : #999999;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                    Text {
                        text: c;
                        color: idx == root.highlight ? #ffffff : #1f2328;
                        font-size: 15px;
                        vertical-alignment: center;
                    }
                }
                TouchArea {
                    mouse-cursor: pointer;
                    clicked => { root.candidate_clicked(idx); }
                }
            }
        }
        }
        callback candidate_clicked(int);
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

/// 估算横排候选栏宽度：外边距 + 每项（内边距 + 标签 + 候选文本）
fn estimate_bar_width(snapshot: &crate::engine::ContextSnapshot) -> u32 {
    let text_w = |s: &str| -> u32 {
        s.chars()
            .map(|c| if c.is_ascii() { 9 } else { 16 })
            .sum()
    };
    let mut w = 12u32; // 外层 padding
    for (i, c) in snapshot.candidates.iter().enumerate() {
        let label = snapshot.select_labels.get(i).cloned().unwrap_or_else(|| (i + 1).to_string());
        w += 14 + text_w(&label) + 4 + text_w(&c.text) + 2;
    }
    w.max(120)
}

fn apply_context_to(ui: &CandWindow, snapshot: &crate::engine::ContextSnapshot) {
    ui.set_highlight(snapshot.highlighted as i32);
    ui.set_hover(-1);
    let texts: Vec<SharedString> =
        snapshot.candidates.iter().map(|c| SharedString::from(c.text.clone())).collect();
    ui.set_candidates(ModelRc::new(VecModel::from(texts)));
    let labels: Vec<SharedString> = snapshot
        .candidates
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let l = snapshot
                .select_labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| (i + 1).to_string());
            SharedString::from(l)
        })
        .collect();
    ui.set_labels(ModelRc::new(VecModel::from(labels)));
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

fn ui_thread_main(rx: Receiver<UiCmd>) {
    // Slint 平台必须先于组件创建注册
    slint::platform::set_platform(Box::new(XPlatform)).expect("heng-ui: set_platform 失败");
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

    // 点击 → core 内部直接选词 + 取走 commit（不回传外壳）
    ui.on_candidate_clicked({
        let ui = ui.clone();
        let current = current.clone();
        move |idx| {
            let Some(rime_id) = *current.borrow() else { return };
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            if engine.select_candidate_on_current_page(rime_id, idx as usize).unwrap_or(false) {
                if let Ok(text) = engine.get_commit(rime_id) {
                    if !text.is_empty() {
                        // 挂到对应外壳会话句柄（反向查 handle）
                        if let Some(h) = SESSIONS.handle_of(rime_id) {
                            PENDING_UI_COMMITS.lock().unwrap().insert(h, text);
                        }
                    }
                }
                // 刷新窗口（选词后组合串可能仍在：输入下一个字母继续）
                if let Ok(snapshot) = engine.get_context(rime_id) {
                    apply_context_to(&ui, &snapshot);
                }
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
                        if snapshot.candidates.is_empty() {
                            backend.set_mapped(false);
                            *visible.borrow_mut() = false;
                        } else {
                            apply_context_to(&ui, &snapshot);
                            let w_logical = estimate_bar_width(&snapshot).min(MAX_WIDTH);
                            ui.set_bar_width(w_logical as i32);
                            let w = (w_logical as f32 * scale).round() as u32;
                            let h = bar_h;
                            msw.set_size(PhysicalSize { width: w, height: h });
                            backend.configure(x, y, w, h);
                            if render_buf.borrow().len() != (w * h) as usize {
                                *render_buf.borrow_mut() =
                                    vec![Argb::default(); (w * h) as usize];
                            }
                            // 必须先映射再画内容（XWayland：未映射窗口的 PutImage
                            // 会被静默丢弃；Windows 上保证 SetWindowPos 在 blit 前）
                            backend.set_mapped(true);
                            msw.window().request_redraw();
                            // 立即渲染 + blit 一次（不等下一轮 draw_if_needed）
                            {
                                let mut buf = render_buf.borrow_mut();
                                let _ = msw.draw_if_needed(|r: &SoftwareRenderer| {
                                    let _ = r.render(&mut buf, w as usize);
                                });
                            }
                            {
                                let buf = render_buf.borrow();
                                if std::env::var_os("HENG_UI_DUMP").is_some() {
                                    dump_frame(&buf, w, h);
                                }
                                backend.blit(&buf, w, h);
                            }
                            *visible.borrow_mut() = true;
                        }
                    }
                }
                UiCmd::Hide => {
                    backend.set_mapped(false);
                    *visible.borrow_mut() = false;
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
            // 首次 Sync 前缓冲尚未按内容宽度分配，跳过渲染
            let w = {
                let buf = render_buf.borrow();
                if buf.is_empty() {
                    return;
                }
                buf.len() as u32 / bar_h
            };
            {
                let mut buf = render_buf.borrow_mut();
                let _ = renderer.render(&mut buf, w as usize);
            }
            let buf = render_buf.borrow();
            backend.blit(&buf, w, BAR_HEIGHT);
        });

        std::thread::sleep(Duration::from_millis(8));
    }
}
