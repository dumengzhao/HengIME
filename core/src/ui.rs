//! 自绘候选窗（M-P1）：Slint 软件渲染 + override-redirect X 窗口，运行在 core
//! 内部的独立 UI 线程。
//!
//! 职责边界（docs/ROUTE-P.md §2 / M-P1-slint-or-probe.md §5）：
//! - 候选窗行为（悬浮提示、选中语义）完全由 core 定义——classicui 的
//!   hoverIndex_ 歧义在此路线下不存在
//! - 点击候选 → core 内部直接 select + 取走 commit（存入
//!   `PENDING_UI_COMMITS`，外壳经 `heng_take_ui_commit` 取回上屏）
//! - 外壳只调 `heng_ui_sync` / `heng_ui_hide` / `heng_take_ui_commit`
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
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

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

// 预乘 ARGB 像素（32 位视觉，支持窗口透明圆角）
#[derive(Clone, Copy)]
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

struct XWindow {
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
    fn new() -> Option<Self> {
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

    fn configure(&self, x: i32, y: i32, w: u32, h: u32) {
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

    fn blit(&self, buf: &[Argb], w: u32, h: u32) {
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

fn ui_thread_main(rx: Receiver<UiCmd>) {
    // Slint 平台必须先于组件创建注册
    slint::platform::set_platform(Box::new(XPlatform)).expect("heng-ui: set_platform 失败");
    let Some(mut xw) = XWindow::new() else {
        eprintln!("heng-ui: X 连接失败，自绘候选窗不可用（外壳回退宿主候选窗）");
        return;
    };
    let msw = MSW.with(|w| w.clone());
    let ui = Rc::new(CandWindow::new().expect("heng-ui: 组件创建失败"));
    msw.set_size(PhysicalSize { width: 160, height: BAR_HEIGHT });
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
                            xw.set_mapped(false);
                            *visible.borrow_mut() = false;
                        } else {
                            apply_context_to(&ui, &snapshot);
                            let w = estimate_bar_width(&snapshot).min(MAX_WIDTH);
                            ui.set_bar_width(w as i32);
                            msw.set_size(PhysicalSize { width: w, height: BAR_HEIGHT });
                            xw.configure(x, y, w, BAR_HEIGHT);
                            if render_buf.borrow().len() != (w * BAR_HEIGHT) as usize {
                                *render_buf.borrow_mut() =
                                    vec![Argb::default(); (w * BAR_HEIGHT) as usize];
                            }
                            // XWayland：未映射窗口的 PutImage 会被丢弃（无报错），
                            // 必须先 map 再画内容
                            xw.set_mapped(true);
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
                                    let mut ppm = format!("P6\n{w} {BAR_HEIGHT}\n255\n").into_bytes();
                                    for p in buf.iter() {
                                        ppm.push(p.r);
                                        ppm.push(p.g);
                                        ppm.push(p.b);
                                    }
                                    let _ = std::fs::write("/tmp/heng-bar.ppm", &ppm);
                                }
                                xw.blit(&buf, w, BAR_HEIGHT);
                            }
                            *visible.borrow_mut() = true;
                        }
                    }
                }
                UiCmd::Hide => {
                    xw.set_mapped(false);
                    *visible.borrow_mut() = false;
                }
            }
        }

        // 2. X 事件（仅窗口可见时有意义，但轮询保持廉价）
        while let Some(event) = xw.conn.poll_for_event().unwrap_or(None) {
            if !*visible.borrow() {
                continue;
            }
            match event {
                Event::MotionNotify(m) => {
                    let pos = LogicalPosition::new(m.event_x as f32, m.event_y as f32);
                    msw.window().dispatch_event(WindowEvent::PointerMoved { position: pos });
                }
                Event::ButtonPress(b) if b.detail == 1 => {
                    let pos = LogicalPosition::new(b.event_x as f32, b.event_y as f32);
                    msw.window().dispatch_event(WindowEvent::PointerPressed {
                        position: pos,
                        button: slint::platform::PointerEventButton::Left,
                    });
                }
                Event::ButtonRelease(b) if b.detail == 1 => {
                    let pos = LogicalPosition::new(b.event_x as f32, b.event_y as f32);
                    msw.window().dispatch_event(WindowEvent::PointerReleased {
                        position: pos,
                        button: slint::platform::PointerEventButton::Left,
                    });
                }
                Event::LeaveNotify(_) => {
                    msw.window().dispatch_event(WindowEvent::PointerExited);
                }
                _ => {}
            }
        }

        // 3. Slint 推进 + 渲染
        slint::platform::update_timers_and_animations();
        msw.draw_if_needed(|renderer: &SoftwareRenderer| {
            // 首次 Sync 前缓冲尚未按内容宽度分配，跳过渲染
            let w = {
                let buf = render_buf.borrow();
                if buf.is_empty() {
                    return;
                }
                buf.len() as u32 / BAR_HEIGHT
            };
            {
                let mut buf = render_buf.borrow_mut();
                let _ = renderer.render(&mut buf, w as usize);
            }
            let buf = render_buf.borrow();
            xw.blit(&buf, w, BAR_HEIGHT);
        });

        std::thread::sleep(Duration::from_millis(8));
    }
}
