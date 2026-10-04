// M0.5 Slint 窗口属性试验（Linux / GNOME Wayland + XWayland）
//
// 对照 docs/M0.5-slint-window-probe.md（Windows 侧已验证）回答同样的三个问题：
//   T1 置顶（always-on-top）
//   T2 基线：默认 show() 是否抢焦点
//   T3 显示不抢前台（hide/show 循环） / T4 点击不激活（manual 模式人工点击）
//
// 判定手段：XGetInputFocus 直查 X 输入焦点。
// 注意：GNOME Wayland 下 XWayland root 的 _NET_ACTIVE_WINDOW 不可靠（实测恒空），
// 与 Windows 试验报告 §4 记录的「退化场景读数不可信」同因，故不用 EWMH active 判定。
//
// 运行（在仓库根）：
//   cd experiments/slint-probe && WINIT_UNIX_BACKEND=x11 cargo run -- auto
//   cd experiments/slint-probe && WINIT_UNIX_BACKEND=x11 cargo run -- manual

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

slint::slint! {
    export component HostWindow inherits Window {
        title: "HENG-HOST";
        width: 480px;
        height: 280px;
        background: #173f1e;
        Text {
            text: "宿主窗口（扮演正在打字的应用）";
            color: white;
            font-size: 18px;
            vertical-alignment: center;
            horizontal-alignment: center;
        }
    }

    export component CandWindow inherits Window {
        title: "HENG-CAND";
        width: 320px;
        height: 200px;
        background: #1e2430;
        always-on-top: true;
        VerticalLayout {
            padding: 8px;
            spacing: 4px;
            Text { text: "1 nihao -> 你好"; color: white; }
            Text { text: "2 nihao -> 拟好"; color: #9aa4b2; }
            Text { text: "3 nihao -> 逆号"; color: #9aa4b2; }
        }
    }
}

// ---- X 判定（x11rb 直连 DISPLAY） ----

fn xconn() -> Option<x11rb::rust_connection::RustConnection> {
    match x11rb::connect(None) {
        Ok((conn, _)) => Some(conn),
        Err(e) => {
            eprintln!("X 连接失败（DISPLAY 环境变量？）：{e}");
            None
        }
    }
}

fn atom(conn: &x11rb::rust_connection::RustConnection, name: &str) -> Option<u32> {
    conn.intern_atom(false, name.as_bytes())
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| r.atom)
}

/// 窗口标题（_NET_WM_NAME 优先，WM_NAME 兜底）；本窗无标题则递归下钻子窗。
/// GNOME/mutter 会把 X11 客户端包进无标题的 frame 窗口，X 焦点常指向 frame。
fn window_title(conn: &x11rb::rust_connection::RustConnection, win: u32) -> String {
    for (name, ty) in [("_NET_WM_NAME", "UTF8_STRING"), ("WM_NAME", "STRING")] {
        let Some(a) = atom(conn, name) else { continue };
        let Some(t) = atom(conn, ty) else { continue };
        let Ok(cookie) = conn.get_property(false, win, a, t, 0, 4096) else { continue };
        let Ok(r) = cookie.reply() else { continue };
        if r.value_len > 0 {
            return String::from_utf8_lossy(&r.value).trim_matches('\0').into();
        }
    }
    // 递归下钻一层（frame 窗口通常只有一个子窗）
    if let Ok(cookie) = conn.query_tree(win) {
        if let Ok(tree) = cookie.reply() {
            for &child in &tree.children {
                let t = window_title(conn, child);
                if !t.is_empty() {
                    return t;
                }
            }
        }
    }
    String::new()
}

/// 当前持有 X 输入焦点的窗口标题（根窗口/无焦点归为 "none"）
fn focused_title(conn: &x11rb::rust_connection::RustConnection) -> String {
    let Ok(cookie) = conn.get_input_focus() else { return "unknown".into() };
    let Ok(focus) = cookie.reply() else { return "unknown".into() };
    let root = conn.setup().roots[0].root;
    if focus.focus == root || focus.focus == 0 {
        return "none".into();
    }
    let title = window_title(conn, focus.focus);
    if title.is_empty() { format!("<id {:#x}>", focus.focus) } else { title }
}

/// frame 窗口 -> 真客户端窗口：沿有标题的子窗下钻，直到拿到自身带标题的窗口
fn resolve_client(conn: &x11rb::rust_connection::RustConnection, win: u32) -> u32 {
    // 自身直接有标题（不靠递归）
    for (name, ty) in [("_NET_WM_NAME", "UTF8_STRING"), ("WM_NAME", "STRING")] {
        if let (Some(a), Some(t)) = (atom(conn, name), atom(conn, ty)) {
            if let Ok(cookie) = conn.get_property(false, win, a, t, 0, 4096) {
                if let Ok(r) = cookie.reply() {
                    if r.value_len > 0 {
                        return win;
                    }
                }
            }
        }
    }
    if let Ok(cookie) = conn.query_tree(win) {
        if let Ok(tree) = cookie.reply() {
            for &child in &tree.children {
                if !window_title(conn, child).is_empty() {
                    return resolve_client(conn, child);
                }
            }
        }
    }
    win
}

/// 枚举根子树，按标题前缀找窗口 id（返回真客户端窗口，非 frame 包裹）
fn find_window(conn: &x11rb::rust_connection::RustConnection, title_prefix: &str) -> Option<u32> {
    let root = conn.setup().roots[0].root;
    let Ok(cookie) = conn.query_tree(root) else { return None };
    let Ok(tree) = cookie.reply() else { return None };
    for &win in &tree.children {
        if window_title(conn, win).starts_with(title_prefix) {
            return Some(resolve_client(conn, win));
        }
    }
    None
}

/// T1：窗口是否带 _NET_WM_STATE_ABOVE（等价 Windows 的 WS_EX_TOPMOST）
fn state_above(conn: &x11rb::rust_connection::RustConnection, win: u32) -> bool {
    let (Some(state_atom), Some(above_atom)) = (
        atom(conn, "_NET_WM_STATE"),
        atom(conn, "_NET_WM_STATE_ABOVE"),
    ) else {
        return false;
    };
    let Ok(cookie) = conn.get_property(false, win, state_atom, AtomEnum::ATOM, 0, 256) else {
        return false;
    };
    cookie
        .reply()
        .map(|r| r.value32().map_or(false, |mut v| v.any(|a| a == above_atom)))
        .unwrap_or(false)
}

fn fg(conn: &x11rb::rust_connection::RustConnection) -> String {
    focused_title(conn)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "auto".into());
    if mode == "manual" {
        eprintln!("manual 模式：两窗口常驻，请人工点击候选窗验证 T4（前台不应离开原程序）");
        run_manual();
        return;
    }
    run_auto();
}

fn run_manual() {
    let host = HostWindow::new().unwrap();
    let cand = CandWindow::new().unwrap();
    host.show().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    cand.show().unwrap();
    // 每 2s 采样打印 X 输入焦点归属，Ctrl+C 结束
    let stop = slint::Timer::default();
    stop.start(slint::TimerMode::Repeated, Duration::from_millis(2000), move || {
        if let Some(conn) = xconn() {
            eprintln!("[采样] X 输入焦点 = {}", fg(&conn));
        }
    });
    slint::run_event_loop().unwrap();
}

fn run_auto() {
    use std::cell::Cell;
    use std::rc::Rc;

    let host = Rc::new(HostWindow::new().unwrap());
    let cand = Rc::new(CandWindow::new().unwrap());

    // tick 状态机：所有窗口操作必须发生在运行中的事件循环内，
    // 否则 show()/hide() 只排队不落地（事件循环未启动时窗口不会被 map）。
    // 每 tick 300ms。
    let step = Rc::new(Cell::new(0u32));
    let timer = Rc::new(slint::Timer::default());
    let finish = |msg: &str| {
        println!("{msg}");
        eprintln!("auto 完成（T4 点击不激活请跑 manual 模式人工点击）");
        std::process::exit(0);
    };

    {
        let host = host.clone();
        let cand = cand.clone();
        let step = step.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(300), move || {
            let t = step.get();
            step.set(t + 1);
            let Some(conn) = xconn() else { std::process::exit(1) };
            match t {
                0 => host.show().unwrap(),
                5 => println!("阶段0 宿主已显示     X焦点 = {}", fg(&conn)),
                6 => {
                    let id = find_window(&conn, "HENG-CAND");
                    let above = id.map_or(false, |id| state_above(&conn, id));
                    println!(
                        "阶段1 候选窗(未显示) 找窗 = {}   ABOVE = {above}",
                        id.map_or("未找到".into(), |i| format!("{i:#x}"))
                    );
                }
                7 => cand.show().unwrap(),
                11 => {
                    let f = fg(&conn);
                    let above = find_window(&conn, "HENG-CAND")
                        .map_or(false, |id| state_above(&conn, id));
                    println!(
                        "阶段2 候选窗默认show X焦点 = {f}   T2 抢焦点 = {}   ABOVE = {above}",
                        f.contains("HENG-CAND")
                    );
                }
                12 => host.hide().unwrap(),
                14 => host.show().unwrap(),
                17 => println!("阶段3 宿主重新聚焦   X焦点 = {}", fg(&conn)),
                // 三轮 hide/show 循环：hide / show / (0.6s) 打印
                18 | 24 | 30 => cand.hide().unwrap(),
                19 | 25 | 31 => cand.show().unwrap(),
                21 | 27 | 33 => {
                    let f = fg(&conn);
                    println!(
                        "阶段3 循环{}         X焦点 = {f}   被候选窗夺走 = {}",
                        (t - 21) / 6,
                        f.contains("HENG-CAND")
                    );
                }
                34 => finish("auto 各阶段结束"),
                _ => {}
            }
        });
    }
    slint::run_event_loop().unwrap();
}
