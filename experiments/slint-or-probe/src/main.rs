// M-P1 Slint override-redirect 候选窗 PoC
//
// 目的（docs/ROUTE-P.md §6 风险 1）：打穿「Slint 软件渲染 → 自建无 WM 干预 X 窗口」
// 完整链路，验证三件事：
//   P1 override-redirect 窗口可正常显示 Slint 渲染内容（绕过 WM，天然置顶不抢焦点）
//   P2 鼠标事件进入 Slint（悬浮/点击），且悬浮高亮语义由我们定义（对比 classicui 的硬编码 hoverIndex_）
//   P3 点击候选不改变 X 输入焦点（GetInputFocus 前后一致）
//
// 渲染：SoftwareRenderer 画进 Rgb8 缓冲 → blit 到 X (ZPixmap 24bpp)。
// 运行（在仓库根）：cd experiments/slint-or-probe && cargo run

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RepaintBufferType, SoftwareRenderer, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};
use x11rb::protocol::Event;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

const WIDTH: u32 = 340;
const HEIGHT: u32 = 260;

// ---- Slint UI：heng_blue 观感的候选窗（白底、蓝块白字选中） ----

slint::slint! {
    export component CandWindow inherits Window {
        in property <string> preedit;
        in property <int> highlight;       // 引擎侧高亮（确认键的语义）
        in property <int> hover;           // 悬浮指示（视觉提示，不改变 highlight）
        in property <[string]> labels;
        in property <[string]> candidates;
        in property <bool> show-hover;     // PoC 开关：演示悬浮提示可选
        width: 340px;
        height: 260px;
        background: #ffffff;
        Text {
            x: 12px; y: 8px;
            text: root.preedit;
            color: #333333;
            font-size: 15px;
        }
        for c[idx] in root.candidates: Rectangle {
            x: 6px;
            y: 36px + idx * 40px;
            width: parent.width - 12px;
            height: 36px;
            border-radius: 4px;
            background: idx == root.highlight ? #2164f1 : transparent;
            border-width: 1px;
            border-color: (idx == root.hover && root.show-hover && idx != root.highlight)
                ? #2164f1 : transparent;
            HorizontalLayout {
                padding-left: 10px;
                padding-right: 10px;
                spacing: 8px;
                alignment: center;
                Text {
                    text: root.labels[idx];
                    color: idx == root.highlight ? #ffffff : #888888;
                    font-size: 14px;
                }
                Text {
                    text: c;
                    color: idx == root.highlight ? #ffffff : #222222;
                    font-size: 16px;
                }
            }
            TouchArea {
                mouse-cursor: pointer;
                clicked => { root.candidate_clicked(idx); }
            }
        }
        callback candidate_clicked(int);
    }
}

// ---- 自定义 Platform：窗口适配器 = MinimalSoftwareWindow，事件循环 = X 轮询 ----

struct XPlatform;

impl Platform for XPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(MSW.with(|w| w.clone()))
    }

    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        unreachable!("由 main 驱动，不走这里")
    }

    fn duration_since_start(&self) -> core::time::Duration {
        // PoC 不做动画，固定值即可
        Duration::ZERO
    }
}

thread_local! {
    static MSW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
}

// 自定义 3 字节 RGB 像素（renderer 经 TargetPixel 写入；blit 时转 X 24bpp）
#[derive(Clone, Copy, Default)]
struct Rgb8 {
    r: u8,
    g: u8,
    b: u8,
}
impl TargetPixel for Rgb8 {
    fn blend(&mut self, color: slint::platform::software_renderer::PremultipliedRgbaColor) {
        let a = (255 - color.alpha) as u16;
        self.r = (self.r as u16 * a / 255) as u8 + color.red;
        self.g = (self.g as u16 * a / 255) as u8 + color.green;
        self.b = (self.b as u16 * a / 255) as u8 + color.blue;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

// ---- X11 侧：override-redirect 窗口 + blit ----

struct XWindow {
    conn: Rc<x11rb::rust_connection::RustConnection>,
    win: u32,
    gc: u32,
    screen: Rc<Screen>,
}

impl XWindow {
    fn new() -> Self {
        let (conn, screen_num) = x11rb::connect(None).expect("X 连接失败");
        let screen = Rc::new(conn.setup().roots[screen_num].clone());
        let win = conn
            .generate_id()
            .unwrap();
        let gc = conn.generate_id().unwrap();
        // override_redirect = 1：绕过窗口管理器，天然不抢焦点、不出现在任务栏
        let aux = CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(screen.white_pixel)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::LEAVE_WINDOW,
            );
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            screen.root,
            200,
            500,
            WIDTH as u16,
            HEIGHT as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .unwrap()
        .check()
        .unwrap();
        conn.create_gc(gc, win, &CreateGCAux::new()).unwrap().check().unwrap();
        conn.map_window(win).unwrap().check().unwrap();
        conn.flush().unwrap();
        Self { conn: Rc::new(conn), win, gc, screen }
    }

    fn blit(&self, buf: &[Rgb8]) {
        // Rgb8(r,g,b) -> X ZPixmap 24bpp：像素 u32 = r<<16 | g<<8 | b（LE，字节序 [b,g,r,x]）
        let mut data = Vec::with_capacity(buf.len() * 4);
        for p in buf {
            data.push(p.b);
            data.push(p.g);
            data.push(p.r);
            data.push(0);
        }
        self.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                self.win,
                self.gc,
                WIDTH as u16,
                HEIGHT as u16,
                0,
                0,
                0,
                24,
                &data,
            )
            .unwrap()
            .check()
            .unwrap();
        self.conn.flush().unwrap();
    }

    fn input_focus(&self) -> u32 {
        self.conn.get_input_focus().unwrap().reply().unwrap().focus
    }
}

fn main() {
    // 1. 平台先行：slint 组件创建前必须注册自定义 Platform
    slint::platform::set_platform(Box::new(XPlatform)).expect("set_platform 失败");

    // 2. X 窗口（override-redirect）
    let xw = Rc::new(XWindow::new());
    let focus_before = xw.input_focus();

    // 3. Slint 组件
    let msw = MSW.with(|w| w.clone());
    let ui = Rc::new(CandWindow::new().unwrap());
    ui.set_preedit("ni hao".into());
    let cands: Vec<SharedString> = ["你好", "拟好", "逆号", "你", "擬"]
        .iter()
        .map(|s| SharedString::from(*s))
        .collect();
    let labels: Vec<SharedString> = (1..=5)
        .map(|i| SharedString::from(i.to_string()))
        .collect();
    ui.set_candidates(ModelRc::new(VecModel::from(cands)));
    ui.set_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_highlight(0);
    ui.set_show_hover(true);
    ui.on_candidate_clicked({
        let ui = ui.clone();
        let xw = xw.clone();
        move |idx| {
            let focus_now = xw.input_focus();
            println!(
                "[点击] 候选 {idx}   X 焦点={focus_now:#x}  焦点未变={}",
                focus_now == focus_before
            );
            ui.set_highlight(idx); // PoC 里点击仅改显示；生产版走 heng_select_candidate_on_current_page
        }
    });
    msw.set_size(PhysicalSize { width: WIDTH, height: HEIGHT });
    ui.show().unwrap();

    // 4. 主循环：X 事件 → Slint；渲染 → blit
    let render_buf: RefCell<Vec<Rgb8>> =
        RefCell::new(vec![Rgb8::default(); (WIDTH * HEIGHT) as usize]);
    let hover: RefCell<Option<u32>> = RefCell::new(None);

    loop {
        // 4.1 X 事件（非阻塞）
        while let Some(event) = xw.conn.poll_for_event().unwrap() {
            match event {
                Event::MotionNotify(m) => {
                    let pos = LogicalPosition::new(m.event_x as f32, m.event_y as f32);
                    msw.window().dispatch_event(WindowEvent::PointerMoved { position: pos });
                    // 悬浮提示：算出行号（视觉层，不影响 highlight 语义）
                    let row = if m.event_y > 36 {
                        Some(((m.event_y - 36) as u32 / 40).min(4))
                    } else {
                        None
                    };
                    if hover.borrow_mut().as_ref() != row.as_ref() {
                        *hover.borrow_mut() = row;
                        if let Some(i) = row {
                            ui.set_hover(i as i32);
                        }
                    }
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
                    ui.set_hover(-1);
                }
                _ => {}
            }
        }

        // 4.2 Slint 动画/定时器推进
        slint::platform::update_timers_and_animations();

        // 4.3 渲染并 blit
        msw.draw_if_needed(|renderer: &SoftwareRenderer| {
            let mut buf = render_buf.borrow_mut();
            let region = renderer.render(&mut buf, WIDTH as usize);
            let _ = region; // PoC 全量 blit；生产版按 DirtyRegion 局部 PutImage
            xw.blit(&buf);
        });

        std::thread::sleep(Duration::from_millis(8));
    }
}
