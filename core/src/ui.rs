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
use crate::settings::{
    build_bindings, parse_bindings, Settings, FULL_PRESETS, LR_PRESETS, PAGING_PRESETS,
    PATCH_WEASEL, PUNCT_PRESETS, SIMP_PRESETS,
};
use crate::Engine;

/// 横排候选栏尺寸：宽度按内容估算（上限截断），高度随字号
const BAR_HEIGHT: u32 = 40;
/// 横条与展开面板统一固定宽度（微信同款：宽度不随内容变化，超长词显示省略号）
const PANEL_WIDTH: u32 = 460;
/// 展开面板最大行数
const FLOW_MAX_ROWS: i32 = 40; // 可滚动，上限放宽
const PANEL_VISIBLE_ROWS: i32 = 5; // 展开态可视行数

// ---- 布局 = 14px 设计稿 × 字号缩放 k（DPI 式等比缩放）----
// k = 字号/14：12→0.857、14→1.0（与历史常量严格一致）、18→1.286。
// 所有尺寸（高/留白/圆角/按钮/行距）统一 设计值×k，比例恒定不跑形；
// 与屏幕 DPI 系数（ui_scale）相乘得到物理像素。宽度按规格保持固定。
fn lay_k() -> f32 {
    current_cand_font() as f32 / 14.0
}
/// 设计稿尺寸 → 当前字号下的逻辑px
fn lay_px(design: f32) -> i32 {
    (design * lay_k()).round() as i32
}
/// 候选格高（设计 26：上下各留 6）
fn lay_cell_h() -> i32 {
    lay_px(26.0)
}
/// 流式/网格行距（设计 34 = 格高 26 + 行间 8）
fn lay_row_h() -> i32 {
    lay_px(34.0)
}
/// 收起条高（设计 34 = 首行上边距 2 + 格高 26 + 下留白 6）
fn lay_bar_h() -> i32 {
    lay_px(34.0)
}
/// 候选窗/字母区窗宽（设计 460 × k）——宽度与高度同为等比缩放，
/// 大字号下每行可容纳的候选数不减少
fn lay_panel_w() -> i32 {
    lay_px(PANEL_WIDTH as f32)
}
/// 顶部字母区窗高（设计 18 = 字号行高 16 + 上下各 1：紧贴候选行且不切字）
fn lay_letters_h() -> i32 {
    lay_px(18.0)
}
/// ☰ 菜单高（设计 = 上留白 2 + 3 行 × 34 + 下留白 6 = 110）
fn lay_menu_h() -> i32 {
    lay_px(110.0)
}
/// 首行 y 基线（设计 2：与字母区一起收紧，避免文字间出现宽缝）
fn lay_flow_top() -> i32 {
    lay_px(2.0)
}
/// 序号字号（设计 11）
fn lay_num_font() -> i32 {
    lay_px(11.0)
}

// ---- 对外接口（capi 调用） ----

/// UI 线程命令
enum UiCmd {
    /// 拉取当前会话 context 并刷新显示；有候选则显示在 (x,y)，无候选则隐藏
    Sync { rime_id: RimeSessionId, x: i32, y: i32, top: i32 },
    Hide,
    /// 展开箭头：切换「全部候选」面板（内部命令，来自 Slint 回调）
    ToggleExpand,
    /// 键盘 ↑（第一行）触发的收起（内部命令）
    SetExpanded(bool),
    /// 面板内移动视觉高亮（±1 格，内部命令）
    MoveHL(i32),
    /// 面板内按行移动高亮（+1 下一行 / -1 上一行；-1 在首行 → 收起）
    RowMove(i32),
    /// 中英切换瞬态提示（内部命令）；anchor = Some((x, bottom, top)) 时用
    /// 外壳传入的光标坐标（server 端持续上报、始终最新），None 用内部记忆值
    ModeHint {
        ascii: bool,
        anchor: Option<(i32, i32, i32)>,
    },
    /// 键盘空格/回车：选中面板当前高亮项（内部命令）
    SelectHL,
    /// 收起横条：按全局横条下标选词（鼠标点击 / 数字键）
    BarSelect(usize),
    /// 打开 ☰ 菜单（占位：符号/常用语/设置）
    MenuOpen,
    /// ☰ 菜单项点击（0=符号 1=常用语 2=设置，内部命令）
    MenuItem(i32),
    /// ☰ 菜单空白处点击 / 其它 dismissal：整窗关闭（不回横条，打字才回）
    MenuDismiss,
    /// 关闭菜单回到横条
    MenuClose,
    /// 展开面板：按「行内序号」选词（数字键 1-9）。
    /// 面板每行序号从 1 重新开始，而 items 是全局连续索引 → 需按行换算：
    /// 目标全局索引 = 该行首项索引 + (行内序号 - 1)
    SelectByRowSeq(i32),
    /// 打开设置窗口（参数 = 初始页索引）
    SettingsShow(i32),
    /// 关闭设置窗口
    SettingsHide,
}

static UI_CMD_TX: LazyLock<Mutex<Option<Sender<UiCmd>>>> = LazyLock::new(|| Mutex::new(None));

/// UI 点击产生的待上屏文本：外壳经 heng_take_ui_commit 取走
pub static PENDING_UI_COMMITS: LazyLock<Mutex<std::collections::HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 上屏回调（C ABI 函数指针 + 外壳上下文），由外壳经
/// `heng_set_commit_handler` 注册。UI 点击选词产生 commit 时**立即**回调，
/// 不等外壳发起下一次请求。
///
/// 为什么必须有回调：鼠标点击这条路径没有任何按键事件伴随，而 weasel 的
/// IPC 是严格请求-响应模型（无server→client 推送通道），`_Respond`
/// 只在按键时被调用 → 点击产生的 commit 永远没人取，候选窗关了但字不上屏。
/// fcitx5 侧原本靠在「下次按键前 drain」兜底，行为是延迟生效，跨端不一致。
pub type CommitHandler = extern "C" fn(ctx: *mut core::ffi::c_void, session: u64, text: *const u8);

/// ctx 以 usize 存（不透明地址）：裸指针非 Send，跨线程静态存储需此包装。
pub static COMMIT_HANDLER: LazyLock<Mutex<Option<(CommitHandler, usize)>>> =
    LazyLock::new(|| Mutex::new(None));

/// 上屏一次：优先走回调（即时），无回调时退回 PENDING（外壳轮询取）。
///
/// 回调在 UI 线程同步执行，外壳实现里只应做「投递」而非阻塞操作
/// （Windows 上真正上屏要经 TSF 回到宿主进程，weasel 侧实现为post 消息）。
fn emit_commit(handle: u64, text: &str) {
    let handler = COMMIT_HANDLER.lock().unwrap();
    if let Some((cb, ctx)) = *handler {
        drop(handler); // 解锁后再调：回调里可能反向调 core（重入）
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0); // C 字符串结尾
        cb(ctx as *mut core::ffi::c_void, handle, bytes.as_ptr());
    } else {
        PENDING_UI_COMMITS.lock().unwrap().insert(handle, text.to_string());
    }
}

// ---- 展开面板共享状态（UI 线程写，capi 键盘拦截读） ----
pub static UI_EXPANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static UI_HL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
pub static UI_TOTAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// 收起横条词数（capi 判断 → 是否到达最后一个词 → 直接展开）
pub static UI_BAR_COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// ☰ 菜单是否打开（鼠标点开；←/→ 关闭回横条）
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
    // 旧路径无行顶信息：按一个横条高（40 逻辑px × DPI）估算行高
    let est_line = (40.0 * ui_scale()).round() as i32;
    send_cmd(UiCmd::Sync { rime_id, x, y, top: y - est_line });
}

/// 同步候选窗（v8 扩展版）：额外传入光标所在文本行顶边（屏幕坐标），
/// 向上展开时面板底边贴输入行上方、不遮输入行。
pub fn ui_sync_ex(rime_id: RimeSessionId, x: i32, y: i32, top: i32) {
    send_cmd(UiCmd::Sync { rime_id, x, y, top });
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

/// 中英切换瞬态提示（capi：Shift 切换后调用）。无坐标版本回退内部记忆锚点。
pub fn ui_mode_hint(ascii: bool) {
    send_cmd(UiCmd::ModeHint { ascii, anchor: None });
}

/// 中英切换瞬态提示（capi ex）：气泡锚在光标行顶上方。
pub fn ui_mode_hint_ex(ascii: bool, anchor: Option<(i32, i32, i32)>) {
    send_cmd(UiCmd::ModeHint { ascii, anchor });
}

/// 收起横条：按横条下标选词（capi 拦截：数字键 / 鼠标点击）
pub fn ui_bar_select(idx: usize) {
    send_cmd(UiCmd::BarSelect(idx));
}

/// 展开面板按行内序号选词（数字键 1-9）。seq 从 1 起。
pub fn ui_select_by_row_seq(seq: i32) {
    send_cmd(UiCmd::SelectByRowSeq(seq));
}

/// 打开 ☰ 菜单（鼠标点击 ☰）
pub fn ui_menu_open() {
    send_cmd(UiCmd::MenuOpen);
}

/// 关闭菜单回横条（capi 拦截：菜单打开时 ←/→）
pub fn ui_menu_close() {
    send_cmd(UiCmd::MenuClose);
}

/// 打开设置窗口（capi：托盘菜单/快捷键；page = 初始页索引 0-5）
pub fn ui_settings_show(page: i32) {
    send_cmd(UiCmd::SettingsShow(page));
}

/// 关闭设置窗口
pub fn ui_settings_hide() {
    send_cmd(UiCmd::SettingsHide);
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
        nw: int,        // 序号实际宽度（实测，避免固定列宽把正文推远）
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
        in property <int> menu-height;    // ☰ 菜单态窗口高度
        in property <int> content-height; // 展开态内容总高（滚动范围）
        in property <[CandCell]> all-cells;
        in property <bool> menu-open; // ☰ 菜单打开（覆盖层显示 符号/常用语/设置）
        in property <int> hl-y;       // 高亮格 y（逻辑 px），变化时滚动跟随
        in property <bool> ascii;     // true = 英文模式（切换提示用）
        in property <int> hint;       // 中英切换瞬态提示：0=无 1=中 2=英
        in property <bool> hint-only; // hint 独占气泡形态：窗口临时缩成胶囊，候选内容全部隐藏
        in property <string> input-text; // 内嵌关时有值（独立字母窗口显示用）
        // 字母区是否显示在候选窗上方（无字母区时收起条需自己垂直居中）
        in property <bool> show-letters: root.input-text != "" && !root.hint-only;
        // 收起态首行下移量：条高 34、格高 26，垂直居中需 y=4 而非设计稿的 2，
        // 即补 2×k。有字母区时不加（保持与字母窗紧贴的收紧间距）。
        in property <length> bar-y-offset: (!root.expanded && !root.show-letters)
            ? root.font-scale * 2px : 0px;
        in property <int> cand-font;  // 候选字号（7 档刻度：14-20，默认 16 = 第 3 档）
        in property <float> font-scale; // 字号缩放系数 k = 字号/14（DPI 式等比）
        // —— 自定义配色（源补丁 style/heng_* 直读，不走 librime 部署语义）——
        // 主预览容器背景色
        in property <color> col-container;
        // 选中区域背景色（alpha=0 = 不填充，露出容器底色）
        in property <color> col-hilite-back;
        // 选中区域字体色
        in property <color> col-hilite-text;
        // 未选中候选词字体色（容器转深色时必需，否则深底深字）
        in property <color> col-cand-text;
        // —— 文本测宽探针 ——
        // 候选格宽度必须用"真实排版宽度"而不是估算表：估算系数随字号漂移，
        // 字号一改就全员省略号（历史事故）。探针 Text 与候选格同字号、
        // 置于窗口外不参与渲染；preferred-width 即该文本在此字号下的精确宽。
        in property <string> probe-text;
        in property <int> probe-font;
        out property <length> probe-width: ptx.preferred-width;
        ptx := Text {
            x: 0;
            y: -100px;
            text: root.probe-text;
            font-size: root.probe-font * 1px;
        }
        width: root.hint-only ? 36px : root.panel-width * 1px;
        height: root.hint-only ? 36px
            : (root.menu-open ? root.menu-height * 1px
            : (root.expanded ? root.panel-height * 1px
            : root.font-scale * 34px));
        background: transparent;
        // 外层圆角容器：窗口本身透明，圆角靠这层裁出（三形态共用：候选/展开/气泡）
        // 气泡态 = 深色半透明胶囊（白字大字，明暗背景均可读），与候选浅底区分开
        round := Rectangle {
        x: 0;
        y: 0;
        width: parent.width;
        height: parent.height;
        background: root.hint-only ? #404048E6 : root.col-container;
        // 字母区显示时取消顶部两角圆角（与上方字母窗连成一体）；内嵌态恢复
        property <bool> letters-above: root.show-letters;
        border-top-left-radius: letters-above ? 0px : (root.hint-only ? 10px : root.font-scale * 8px);
        border-top-right-radius: letters-above ? 0px : (root.hint-only ? 10px : root.font-scale * 8px);
        border-bottom-left-radius: root.hint-only ? 10px : root.font-scale * 8px;
        border-bottom-right-radius: root.hint-only ? 10px : root.font-scale * 8px;
        // 候选词流式格子（两态共用；收起时 40px 窗口只露出第一行）
        flick := Flickable {
            visible: !root.hint-only;
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
                    // 收起态无字母区时补半格下移：设计稿首行 y=2 是为与字母窗
                    // 收紧拼接用的，独立字母区没起来时条内就只剩下 6px 下留白，
                    // 文字重心偏上、与右上角按 (height-h)/2 居中的 ▾/☰ 对不齐。
                    y: cell.y * 1px + root.bar-y-offset;
                    width: cell.w * 1px;
                    height: root.font-scale * 26px;
                    border-radius: root.font-scale * 8px;
                    // 只有选中项有背景药丸，其余纯文字不加底色区分边界
                    background: cell.hl ? root.col-hilite-back : transparent;
                    // 绝对定位（不用 HorizontalLayout：其对齐语义不好控）。
                    // 序号与正文各自占满格高、垂直居中；间距由实测序号宽 + gap 控制
                    property <length> pad-l: root.font-scale * 6px;
                    property <length> pad-r: root.font-scale * 6px;
                    property <length> gap-in: root.font-scale * 3px;
                    Text {
                        // 序号：宽度按本格实际数字实测，空串即不占宽（正文不被推远）
                        x: pad-l;
                        y: 0;
                        width: cell.nw * 1px;
                        height: parent.height;
                        text: cell.num;
                        // 与正文同色（原来浅灰/浅蓝在浅底上不显眼）
                        color: cell.hl ? root.col-hilite-text : root.col-cand-text;
                        font-size: root.font-scale * 11px;
                        vertical-alignment: center;
                    }
                    Text {
                        // 正文：右边界对称留白
                        x: pad-l + cell.nw * 1px + gap-in;
                        y: 0;
                        width: parent.width - pad-l - pad-r - cell.nw * 1px - gap-in;
                        height: parent.height;
                        text: cell.text;
                        color: cell.hl ? root.col-hilite-text : root.col-cand-text;
                        font-size: root.cand-font * 1px;
                        vertical-alignment: center;
                        overflow: elide;
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
        if root.expanded && !root.hint-only: Rectangle {
            x: parent.width - root.font-scale * 5px;
            y: root.font-scale * 6px;
            width: root.font-scale * 3px;
            height: track-h;
            border-radius: root.font-scale * 2px;
            background: #d8dce2;
            Rectangle {
                width: parent.width;
                border-radius: root.font-scale * 2px;
                background: #aeb4bd;
                y: Math.min(track-h - thumb-h,
                    Math.max(0px, (track-h - thumb-h)
                        * ((0px - flick.content-y) / scroll-range)));
                height: thumb-h;
            }
        }
        // 收起态右上按钮（▾ 展开 / ☰ 菜单）：均只响应鼠标点击，键盘不进入
        //（→ 到最后一个预选词即展开，不经过图标焦点）
        // 图标用 Path/色块矢量绘制而非字符 "▾"/"☰"：Windows 默认字体缺
        // 这两个码位的字形，软件渲染器不做逐字形回退，会渲染成空白
        if !root.expanded && !root.hint-only: Rectangle {
            x: parent.width - root.font-scale * 58px;
            y: (parent.height - root.font-scale * 28px) / 2;
            width: root.font-scale * 26px;
            height: root.font-scale * 28px;
            border-radius: root.font-scale * 8px;
            background: ta_arrow.has-hover ? #e8ebef : transparent;
            // 图形本身也随字号等比缩放（此前固定 8×5，大字号下显得很小）
            Path {
                width: root.font-scale * 8px;
                height: root.font-scale * 5px;
                x: (parent.width - root.font-scale * 8px) / 2;
                y: (parent.height - root.font-scale * 5px) / 2;
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
        if !root.expanded && !root.hint-only: Rectangle {
            x: parent.width - root.font-scale * 30px;
            y: (parent.height - root.font-scale * 28px) / 2;
            width: root.font-scale * 26px;
            height: root.font-scale * 28px;
            border-radius: root.font-scale * 8px;
            background: ta_menu.has-hover ? #e8ebef : transparent;
            Rectangle {
                width: root.font-scale * 12px;
                height: root.font-scale * 8px;
                x: (parent.width - root.font-scale * 12px) / 2;
                y: (parent.height - root.font-scale * 8px) / 2;
                Rectangle { y: 0; width: parent.width; height: root.font-scale * 1.5px; background: #666666; }
                Rectangle { y: root.font-scale * 3.25px; width: parent.width; height: root.font-scale * 1.5px; background: #666666; }
                Rectangle { y: root.font-scale * 6.5px; width: parent.width; height: root.font-scale * 1.5px; background: #666666; }
            }
            ta_menu := TouchArea {
                mouse-cursor: pointer;
                clicked => { root.menu_clicked(); }
            }
        }
        // ☰ 菜单覆盖层（占位：符号/常用语/设置，功能后续迭代）
        if root.menu-open && !root.hint-only: Rectangle {
            x: 0;
            y: 0;
            width: parent.width;
            height: parent.height;
            background: root.col-container;
            border-radius: 8px;
            // 空白处点击 → 整窗关闭；垫底（后声明的元素在上层，放在菜单项
            // 之后会把整窗点击吞掉、菜单项永远点不到）
            TouchArea {
                clicked => { root.menu_dismissed(); }
            }
            VerticalLayout {
                padding: 4px;
                spacing: 2px;
                for name[mi] in ["符号", "常用语", "设置"]: Rectangle {
                    height: 34px;
                    border-radius: 6px;
                    background: ta-mi.has-hover ? #e8ebef : transparent;
                    HorizontalLayout {
                        padding-left: 12px;
                        alignment: center;
                        Text {
                            text: name;
                            color: root.col-cand-text;
                            font-size: 14px;
                            vertical-alignment: center;
                        }
                    }
                    ta-mi := TouchArea {
                        mouse-cursor: pointer;
                        clicked => { root.menu_item_clicked(mi); }
                    }
                }
            }
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
        // 中英切换瞬态提示：气泡态铺满全窗（大字居中），兼容旧横条内嵌形态
        if root.hint > 0: Rectangle {
            x: root.hint-only ? 0 : (parent.width - 120px) / 2;
            y: root.hint-only ? 0 : 4px;
            width: root.hint-only ? parent.width : 120px;
            height: root.hint-only ? parent.height : 32px;
            border-radius: root.hint-only ? 10px : 8px;
            background: root.hint-only ? transparent : (root.hint == 1 ? #2164f1 : #9aa3ad);
            Text {
                text: root.hint == 1 ? "中" : "英";
                color: #ffffff;
                font-size: root.hint-only ? 24px : 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
        callback candidate_clicked(int);
        callback menu_item_clicked(int);
        callback menu_dismissed();
        callback toggle_expand();
        callback expanded_clicked(int);
        callback menu_clicked();
    }

    // ============================ 设置窗口（S0-S3） ============================
    // 普通可聚焦窗口（与候选窗不同：可激活、带原生标题栏、不透明）。
    // 无键盘需求：全部交互 = 鼠标点击（行选择/拨杆/步进/按钮）。
    export struct SetRow {
        title: string,  // 左侧标题
        sub: string,    // 右侧当前值/说明
        active: bool,   // 当前选中
    }

    component SetBtn {
        in property <string> text;
        callback clicked();
        min-width: 104px;
        height: 32px;
        Rectangle {
            border-radius: 8px;
            background: ta.has-hover ? #dfe6f5 : #e9ebef;
            Text { text: root.text; font-size: 13px; color: #1f2328; }
            ta := TouchArea { mouse-cursor: pointer; clicked => { root.clicked(); } }
        }
    }

    export component SettingsWindow inherits Window {
        in property <int> page;
        in property <[string]> nav-names;
        in property <string> head-text;
        in property <[SetRow]> page-rows;
        in property <int> font-size;
        in property <bool> inline-preedit;
        in property <bool> ascii-punct;
        in property <string> about-lines;
        // —— 自定义配色页 ——
        in property <[color]> color-vals;   // 4 项当前色（顺序：容器/选中底/选中字/候选字）
        in property <[string]> color-labels;
        in property <bool> picker-open;     // 取色面板是否展开
        in property <int> picker-target;     // 正在编辑第几项（0-3）
        in property <bool> picker-allow-transparent; // 该项是否允许"不填充"
        // HSV 取色器：色相 0-360，饱和/明度 0-100
        in property <float> pick-h;
        in property <float> pick-s;
        in property <float> pick-v;
        in property <[color]> hue-bar;      // 色相条 24 段（固定，不随 s/v 变）
        in property <[color]> sv-grid;      // SV 方块 COLS×ROWS 格（随色相重算）
        in property <int> sv-cols;
        in property <int> sv-rows;
        in property <color> pick-preview;    // 当前 hsv 合成的预览色
    in property <string> pick-preview-hex; // 同色hex 文本（面板右下角显示）
    in property <bool> picker-is-transparent; // 当前项是否已处于「不填」态
        callback nav_clicked(int);
        callback row_clicked(int);
        callback toggle_clicked(string);
        callback scale_clicked(float); // 参数 = 点击位置占轨道比例 0-1
        callback action_clicked(string);
        callback close_clicked();
        callback color_row_clicked(int);   // 点某个色块行 → 开取色器
        callback pick_hsv(float, float, float); // 拖拽取色（h/s/v），实时应用
        callback pick_transparent();             // 点「不填充」
        callback color_reset(int);               // 参数 = 行号，恢复该项默认

        width: 640px;
        height: 480px;
        background: #f7f8fa;

        // 左侧导航
        Rectangle {
            x: 0; y: 0; width: 148px; height: parent.height;
            background: #eef0f4;
            VerticalLayout {
                padding: 12px;
                spacing: 6px;
                for nav[n] in root.nav-names: Rectangle {
                    height: 38px;
                    border-radius: 8px;
                    background: root.page == n ? #2164f1 : (nta.has-hover ? #e2e5ea : transparent);
                    HorizontalLayout {
                        padding-left: 14px;
                        alignment: center;
                        Text { text: nav; font-size: 14px; color: root.page == n ? #ffffff : #1f2328; vertical-alignment: center; }
                    }
                    nta := TouchArea { mouse-cursor: pointer; clicked => { root.nav_clicked(n); } }
                }
            }
        }
        // 页头说明
        Text {
            x: 164px; y: 16px; width: parent.width - 216px;
            text: root.head-text; font-size: 13px; color: #888d95;
            overflow: elide; vertical-alignment: center;
        }
        // 关闭（× 为 U+00D7，中文字体必有字形）
        Rectangle {
            x: parent.width - 40px; y: 10px; width: 28px; height: 28px;
            border-radius: 14px;
            background: cta.has-hover ? #e0e3e8 : transparent;
            Text { text: "×"; font-size: 18px; color: #666666; }
            cta := TouchArea { mouse-cursor: pointer; clicked => { root.close_clicked(); } }
        }
        // 内容区（可滚动）
        Flickable {
            x: 164px; y: 46px;
            width: parent.width - 182px;
            height: parent.height - 62px;
            VerticalLayout {
                // 钉死 = 视口宽：否则内容宽度被最宽子行的 preferred 撑宽，
                // 所有行跟着变宽，右侧胶囊被视口裁掉
                width: parent.width;
                spacing: 6px;
                // —— 页2 外观：面板 → 预览 → 四项色块 → 拨杆 → 字号 ——
                // 顺序按「视口高度倒排」：内容区只有 ~418px，取色面板 + 预览 +
                // 四行色块正好放得下，拨杆与字号在下方（可滚动看到）。
                // 反过来排（面板在末位）会溢出到滚动区外 —— 表现为「点改色后
                // 内容错位一下，面板看不到」。
                if root.page == 1 && root.picker-open: Rectangle {
                    // 高度 = 头部 24 + SV 方块 150 + 间隙 8 + 色相条 22 + 间隙 10
                    // + 预览行 28 + 内边距 = 274
                    height: 274px; border-radius: 8px; background: #ffffff;
                    // 头部
                    Text {
                        x: 12px; y: 10px; width: 200px; height: 20px;
                        text: root.color-labels[root.picker-target];
                        font-size: 13px; color: #1f2328;
                        vertical-alignment: center; overflow: elide;
                    }
                    Text {
                        x: parent.width - 148px; y: 10px; width: 112px; height: 20px;
                        text: "拖动取色"; font-size: 12px; color: #999999;
                        horizontal-alignment: right; vertical-alignment: center;
                    }
                    Rectangle {
                        x: parent.width - 32px; y: 10px;
                        width: 20px; height: 20px; border-radius: 10px;
                        background: pct.has-hover ? #e0e3e8 : transparent;
                        Text { text: "×"; font-size: 15px; color: #666666; }
                        pct := TouchArea { clicked => { root.color_row_clicked(-1); } }
                    }
                    // SV 方块：横向 = 饱和度 0→100，纵向 = 明度 100→0。
                    // 格子颜色由 Rust 侧按当前色相算好塞进 model（色相变了整块重算）。
                    Rectangle {
                        x: 12px; y: 36px; width: 348px; height: 150px;
                        border-radius: 6px; border-width: 1px; border-color: #d8dce2;
                        clip: true;
                        for sc[k] in root.sv-grid: Rectangle {
                            x: Math.mod(k, root.sv-cols) * (348px / root.sv-cols);
                            y: Math.floor(k / root.sv-cols) * (150px / root.sv-rows);
                            width: 348px / root.sv-cols + 1px;
                            height: 150px / root.sv-rows + 1px;
                            background: sc;
                        }
                        // 当前位置指示圈（白描边 + 黑描边双层，任何底色上都看得见）
                        Rectangle {
                            x: root.pick-s * 3.48px - 5px;
                            y: (100.0 - root.pick-v) * 1.5px - 5px;
                            width: 10px; height: 10px; border-radius: 5px;
                            border-width: 2px; border-color: #ffffff;
                            background: #00000000;
                        }
                        Rectangle {
                            x: root.pick-s * 3.48px - 6px;
                            y: (100.0 - root.pick-v) * 1.5px - 6px;
                            width: 12px; height: 12px; border-radius: 6px;
                            background: #00000000;
                            border-width: 1px; border-color: #00000060;
                        }
                        sva := TouchArea {
                            mouse-cursor: crosshair;
                            moved => {
                                root.pick_hsv(root.pick-h,
                                    self.mouse-x / self.width * 100.0,
                                    100.0 - self.mouse-y / self.height * 100.0);
                            }
                            clicked => {
                                root.pick_hsv(root.pick-h,
                                    self.mouse-x / self.width * 100.0,
                                    100.0 - self.mouse-y / self.height * 100.0);
                            }
                        }
                    }
                    // 色相条：24 段固定色相
                    Rectangle {
                        x: 12px; y: 194px; width: 348px; height: 22px;
                        border-radius: 5px; border-width: 1px; border-color: #d8dce2;
                        clip: true;
                        for hc[k] in root.hue-bar: Rectangle {
                            x: k * (348px / 36);
                            y: 0;
                            width: 348px / 36 + 1px;
                            height: 22px;
                            background: hc;
                        }
                        // 指示条
                        Rectangle {
                            x: root.pick-h / 360.0 * 348px - 2px; y: 0;
                            width: 4px; height: 22px;
                            border-width: 1px; border-color: #ffffff;
                            background: #00000000;
                        }
                        Rectangle {
                            x: root.pick-h / 360.0 * 348px - 3px; y: 0;
                            width: 6px; height: 22px;
                            border-width: 1px; border-color: #00000080;
                            background: #00000000;
                        }
                        hua := TouchArea {
                            mouse-cursor: crosshair;
                            moved => {
                                root.pick_hsv(self.mouse-x / self.width * 360.0,
                                    root.pick-s, root.pick-v);
                            }
                            clicked => {
                                root.pick_hsv(self.mouse-x / self.width * 360.0,
                                    root.pick-s, root.pick-v);
                            }
                        }
                    }
                    // 底部：当前色预览 + 不填充 + 常用色快捷
                    Rectangle {
                        x: 12px; y: 226px; width: 48px; height: 28px;
                        border-radius: 6px;
                        background: root.pick-preview;
                        border-width: 1px; border-color: #d8dce2;
                    }
                    // 恒为 true（四项都允许透明）—— 保留属性是因为「不填」需要知道当前项
                    // 是否已处于透明态，以便高亮；将来若再收窄某项，这里不用改 UI
                    if root.picker-allow-transparent: Rectangle {
                        x: 68px; y: 226px; width: 56px; height: 28px;
                        border-radius: 6px;
                        background: root.picker-is-transparent ? #2164f1 : #ffffff;
                        border-width: 1px; border-color: #c9ced6;
                        Path {
                            width: 16px; height: 16px; x: 6px; y: 6px;
                            viewbox-width: 16; viewbox-height: 16;
                            commands: "M 0 16 L 16 0";
                            stroke: root.picker-is-transparent ? #ffffff : #b8bec7;
                            stroke-width: 2px;
                        }
                        Text {
                            x: 24px; y: 0; width: 28px; height: parent.height;
                            text: "不填"; font-size: 11px;
                            color: root.picker-is-transparent ? #ffffff : #6b7280;
                            vertical-alignment: center;
                        }
                        ptt := TouchArea {
                            mouse-cursor: pointer;
                            clicked => { root.pick_transparent(); }
                        }
                    }
                    // 当前取到的 hex —— 比重复「拖动取色」提示有用
                    Text {
                        x: parent.width - 132px; y: 226px; width: 120px; height: 28px;
                        text: root.pick-preview-hex; font-size: 11px; color: #9aa0a8;
                        font-family: "Consolas";
                        horizontal-alignment: right; vertical-alignment: center;
                    }
                }
                // —— 页2 外观：自定义配色（预览 + 四项色块 + 固定色板） ——
                // 预览条：等比缩略候选窗（容器底 + 选中格 + 未选中格），
                // 改色即时可见，不必关窗去候选窗核对
                if root.page == 1: Rectangle {
                    height: 76px; border-radius: 8px; background: #e8ebef;
                    // 迷你候选窗容器
                    Rectangle {
                        x: 14px; y: 12px; width: 220px; height: 52px;
                        border-radius: 8px;
                        background: root.color-vals[0];
                        // 选中格（背景 alpha=0 时自然透出容器底 = 不填充）
                        Rectangle {
                            x: 8px; y: 10px; width: 96px; height: 32px;
                            border-radius: 8px;
                            background: root.color-vals[1];
                            Text {
                                text: "1 你好"; font-size: 14px;
                                color: root.color-vals[2];
                                vertical-alignment: center; horizontal-alignment: center;
                            }
                        }
                        // 未选中格
                        Text {
                            x: 116px; y: 10px; width: 96px; height: 32px;
                            text: "2 您好"; font-size: 14px;
                            color: root.color-vals[3];
                            vertical-alignment: center; horizontal-alignment: center;
                        }
                    }
                    Text {
                        x: 248px; y: 12px; width: parent.width - 262px; height: 52px;
                        text: "实时预览"; font-size: 13px; color: #6b7280;
                        vertical-alignment: center; wrap: word-wrap;
                    }
                }
                // 四个可配色项（点击行 → 展开取色面板）
                for ci[i] in 4: Rectangle {
                    height: 44px; border-radius: 8px;
                    background: cta-ci.has-hover ? #eceef2 : #ffffff;
                    // 绝对定位，不用 HorizontalLayout：布局里Text 的
                    // horizontal-stretch 语义吃掉了显式 width，标题会被推到
                    // 中间（实测 horizontal-stretch:0 + width 仍无效）
                    Text {
                        x: 12px; y: 0; width: 150px; height: parent.height;
                        text: root.color-labels[i];
                        font-size: 14px; color: #1f2328;
                        vertical-alignment: center; overflow: elide;
                    }
                    // 当前色块：透明项画白底（下方无内容 = 不填充）
                    Rectangle {
                        x: parent.width - 116px; y: 10px;
                        width: 40px; height: 24px; border-radius: 6px;
                        background: root.color-vals[i].alpha == 0 ? #ffffff : root.color-vals[i];
                        border-width: 1px; border-color: #d8dce2;
                    }
                    Rectangle {
                        x: parent.width - 66px; y: 10px;
                        width: 44px; height: 24px; border-radius: 6px;
                        background: root.picker-target == i && root.picker-open ? #2164f1 : #e9ebef;
                        Text {
                            text: "改色"; font-size: 12px; color: #ffffff;
                            vertical-alignment: center; horizontal-alignment: center;
                        }
                    }
                    cta-ci := TouchArea { mouse-cursor: pointer; clicked => { root.color_row_clicked(i); } }
                }
                if root.page == 1: Rectangle {
                    height: 44px; border-radius: 8px; background: #ffffff;
                    HorizontalLayout {
                        padding-left: 12px; padding-right: 12px; spacing: 8px;
                        Text { text: "拼音内嵌（编码上屏前显示在输入行）"; font-size: 14px; color: #1f2328; vertical-alignment: center; overflow: elide; }
                        Rectangle { horizontal-stretch: 1; }
                        Rectangle {
                            width: 40px; height: 22px; border-radius: 11px; y: 11px;
                            background: root.inline-preedit ? #2164f1 : #c9ced6;
                            Rectangle {
                                x: root.inline-preedit ? 20px : 2px; y: 2px;
                                width: 18px; height: 18px; border-radius: 9px;
                                background: #ffffff;
                            }
                        }
                    }
                    ita := TouchArea { clicked => { root.toggle_clicked("inline"); } }
                }
                if root.page == 1: Rectangle {
                    height: 68px; border-radius: 8px; background: #ffffff;
                    Text {
                        x: 12px; y: 0; width: 76px; height: parent.height;
                        text: "候选字号"; font-size: 14px; color: #1f2328;
                        vertical-alignment: center;
                    }
                    // 7 档刻度（14-20px），最小/默认/最大标签在 1/3/7 刻度下
                    Rectangle {
                        x: 104px; y: 8px;
                        width: parent.width - 124px; height: 52px;
                        Rectangle { // 轨道
                            y: 12px; width: parent.width; height: 2px;
                            background: #e0e3e8;
                        }
                        for t[i] in 7: Rectangle { // 刻度
                            x: parent.width * i / 6 - 3px;
                            y: 9px; width: 6px; height: 8px; border-radius: 1px;
                            background: #c9ced6;
                        }
                        Rectangle { // 滑块
                            x: parent.width * (root.font-size - 14) / 6 - 8px;
                            y: 5px; width: 16px; height: 16px; border-radius: 8px;
                            background: #2164f1;
                        }
                        Text { x: 0; y: 30px; width: 40px; text: "最小"; font-size: 11px; color: #999999; horizontal-alignment: center; }
                        Text { x: parent.width * 2 / 6 - 20px; y: 30px; width: 40px; text: "默认"; font-size: 11px; color: #999999; horizontal-alignment: center; }
                        Text { x: parent.width - 40px; y: 30px; width: 40px; text: "最大"; font-size: 11px; color: #999999; horizontal-alignment: center; }
                        sta := TouchArea {
                            clicked => { root.scale_clicked(self.mouse-x / self.width); }
                        }
                    }
                }
                for row[r] in root.page-rows: Rectangle {
                    height: 44px;
                    border-radius: 8px;
                    background: row.active ? #dfe9fd : (rta.has-hover ? #eceef2 : #ffffff);
                    HorizontalLayout {
                        padding-left: 12px; padding-right: 12px; spacing: 8px;
                        alignment: center;
                        Text { text: row.title; font-size: 14px; color: #1f2328; vertical-alignment: center; overflow: elide; }
                        Rectangle { horizontal-stretch: 1; }
                        Text { text: row.sub; font-size: 12px; color: #999999; vertical-alignment: center; overflow: elide; }
                    }
                    rta := TouchArea { mouse-cursor: pointer; clicked => { root.row_clicked(r); } }
                }
                // —— 页3 标点：实时切换 ——
                if root.page == 3: Rectangle {
                    height: 44px; border-radius: 8px; background: #ffffff;
                    HorizontalLayout {
                        padding-left: 12px; padding-right: 12px; spacing: 8px;
                        Text { text: "半角标点模式（立即生效）"; font-size: 14px; color: #1f2328; vertical-alignment: center; }
                        Rectangle { horizontal-stretch: 1; }
                        Rectangle {
                            width: 40px; height: 22px; border-radius: 11px; y: 11px;
                            background: root.ascii-punct ? #2164f1 : #c9ced6;
                            Rectangle {
                                x: root.ascii-punct ? 20px : 2px; y: 2px;
                                width: 18px; height: 18px; border-radius: 9px;
                                background: #ffffff;
                            }
                        }
                    }
                    pta3 := TouchArea { clicked => { root.toggle_clicked("punct"); } }
                }
                // —— 页4 词库：动作按钮 ——
                if root.page == 4: HorizontalLayout {
                    height: 40px; spacing: 8px;
                    SetBtn { text: "打开用户目录"; clicked => { root.action_clicked("open-user"); } }
                    SetBtn { text: "备份用户词典"; clicked => { root.action_clicked("backup"); } }
                }
                // —— 页5 关于 ——
                if root.page == 5: Rectangle {
                    min-height: 120px; border-radius: 8px; background: #ffffff;
                    HorizontalLayout {
                        padding: 12px;
                        Text {
                            text: root.about-lines;
                            font-size: 13px; color: #1f2328;
                            wrap: word_wrap;
                            vertical-alignment: center;
                        }
                    }
                }
                if root.page == 5: HorizontalLayout {
                    height: 40px; spacing: 8px;
                    SetBtn { text: "重新部署"; clicked => { root.action_clicked("redeploy"); } }
                    SetBtn { text: "打开日志目录"; clicked => { root.action_clicked("open-log"); } }
                    SetBtn { text: "打开用户目录"; clicked => { root.action_clicked("open-user"); } }
                }
            }
        }
    }

    export component LettersWindow inherits Window {
        in property <string> text;
        in property <int> cand-font;  // 与候选格同字号
        in property <float> font-scale;
        // 与候选窗共用同一套自定义配色（源补丁 style/heng_*）
        in property <color> col-container;
        in property <color> col-cand-text;
        width: root.font-scale * 460px;
        height: root.font-scale * 18px;
        background: transparent;
        // 独立浮窗：与候选窗同色系、同圆角，悬浮于候选窗正上方。
        // 底部两角取消圆角（与下方候选窗顶部方角连成一体）
        Rectangle {
            x: 0; y: 0; width: parent.width; height: parent.height;
            background: root.col-container;
            border-top-left-radius: 8px;
            border-top-right-radius: 8px;
            border-bottom-left-radius: 0px;
            border-bottom-right-radius: 0px;
            Text {
                x: 10px; y: 0;
                width: parent.width - 20px; height: parent.height;
                text: root.text;
                // 与候选词同色（原来浅灰 #888d95 不显眼）
                color: root.col-cand-text;
                font-size: root.cand-font * 1px;
                vertical-alignment: center;
                overflow: elide;
            }
        }
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

struct XPlatform {
    /// 窗口创建计数：第 1 个组件 = 候选窗（MSW），第 2 个 = 设置窗口（MSW2），
    /// 第 3 个 = 顶部字母区（MSW3）
    n: std::cell::Cell<u32>,
}
impl Platform for XPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let n = self.n.get();
        self.n.set(n + 1);
        if n == 0 {
            Ok(MSW.with(|w| w.clone()))
        } else if n == 1 {
            Ok(MSW2.with(|w| w.clone()))
        } else {
            Ok(MSW3.with(|w| w.clone()))
        }
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
    /// 设置窗口（第 2 个组件实例经 XPlatform 计数路由到这里）
    static MSW2: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    /// 顶部字母区（第 3 个组件实例；内嵌关时悬浮于候选窗正上方）
    static MSW3: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
}

/// 平台窗口后端：把预乘 ARGB 帧呈现到屏幕并回传指针事件。
/// win: 0 = 候选窗，1 = 设置窗口。
trait UiBackend {
    /// 定位 + 调整窗口尺寸（屏幕坐标）
    fn configure(&mut self, win: u8, x: i32, y: i32, w: u32, h: u32);
    /// 显示 / 隐藏（win=0 候选窗不得夺取前台焦点；win=1 设置窗口可激活）
    fn set_mapped(&mut self, win: u8, mapped: bool);
    /// 呈现一帧预乘 ARGB（buf 长度 = w*h，字节序 BGRA）
    fn blit(&mut self, win: u8, buf: &[Argb], w: u32, h: u32);
    /// 非阻塞泵平台事件，逐个交给 sink（tag=窗口，PointerMoved/Pressed/Released/Exited/Closed）
    fn poll_events(&mut self, sink: &mut dyn FnMut(u8, WindowEvent));
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
///
/// 为什么不用 `GetDeviceCaps(GetDC(NULL), LOGPIXELSX)`（旧实现）：
/// 该 API 的返回值**取决于进程的 DPI 感知状态**——未声明感知的宿主进程里
/// 它返回虚拟化的 96 而不是真实 DPI，缩放静默退化成 1.0，表现为
/// 「高分辨率屏上界面很小」，且换宿主就变。
///
/// 改用 `GetDpiForWindow(GetDesktopWindow())`：实测该API **完全不受进程
/// 感知状态影响**（本机未声明aware 与 PM aware 下都返回 192=2.0×，而
/// `GetDpiForSystem` 在未声明 aware 时错返回 96）。取桌面窗口的 DPI 而非
/// 具体窗口，是为了让「窗口创建前」也能拿到正确值（设置窗/候选窗的
/// HWND 此时还不存在）。
///
/// 已知局限：多屏异构缩放（两屏缩放不同）下取的是主屏 DPI，副屏上的窗口
/// 会偏大/偏小。彻底解决需处理 `WM_DPICHANGED` + 按窗口取 DPI，暂未做。
#[cfg(target_os = "windows")]
fn ui_scale() -> f32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    let hwnd = unsafe { GetDesktopWindow() };
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi >= 96 {
        return dpi as f32 / 96.0;
    }
    ui_scale_fallback()
}

/// 老系统（< Win10 1607）回落：GetDC(NULL) + LOGPIXELSX
#[cfg(target_os = "windows")]
fn ui_scale_fallback() -> f32 {
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

/// HENG_UI_WORK="top,bottom" 覆盖工作区（测试钩子，优先于平台实现）
fn work_area_env() -> Option<(i32, i32)> {
    let env = std::env::var("HENG_UI_WORK").ok()?;
    let parts: Vec<&str> = env.splitn(2, ',').collect();
    if parts.len() == 2 {
        if let (Ok(top), Ok(bottom)) = (parts[0].trim().parse(), parts[1].trim().parse()) {
            return Some((top, bottom));
        }
    }
    None
}

/// 查询指定点所在显示器的工作区（任务栏除外）的上下边界（设备 px）。
/// 用于展开面板的向上翻转判断：光标下方放不下时朝上展开。
/// 返回 (top, bottom)；查不到返回 None（此时不翻转，维持向下展开）。
/// 测试钩子：HENG_UI_WORK="top,bottom" 可覆盖（沙箱无真实显示器）。
#[cfg(target_os = "windows")]
fn work_area_at(x: i32, y: i32) -> Option<(i32, i32)> {
    if let Some(v) = work_area_env() {
        return Some(v);
    }
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let pt = POINT { x, y };
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() {
            return None;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut mi) == 0 {
            return None;
        }
        Some((mi.rcWork.top, mi.rcWork.bottom))
    }
}

/// Linux：经 x11rb randr 查所在显示器的 CRTC 工作区（对标 Windows 的
/// MonitorFromPoint + GetMonitorInfo）。展开面板放不下时据此向上翻转。
#[cfg(target_os = "linux")]
fn work_area_at(x: i32, y: i32) -> Option<(i32, i32)> {
    if let Some(v) = work_area_env() {
        return Some(v);
    }
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    let conn = X_CONN.with(|c| c.borrow().clone())?;
    let root = conn.setup().roots.first()?.root;
    if let Some(res) = conn
        .randr_get_screen_resources(root)
        .ok()
        .and_then(|c| c.reply().ok())
    {
        for crtc in res.crtcs {
            let Some(info) = res
                .outputs
                .iter()
                .find_map(|&out| {
                    conn.randr_get_crtc_info(out, crtc)
                        .ok()
                        .and_then(|c| c.reply().ok())
                })
            else {
                continue;
            };
            if info.mode == 0 {
                continue; // 该输出未启用
            }
            let (rx, ry) = (info.x as i32, info.y as i32);
            let (cw, ch) = (info.width as i32, info.height as i32);
            if cw > 0 && ch > 0 && x >= rx && x < rx + cw && y >= ry && y < ry + ch {
                return Some((ry, ry + ch));
            }
        }
    }
    // 回退：整个屏幕范围（无 randr / 单屏异常时仍能防止溢出屏幕）
    let screen = conn.setup().roots.first()?;
    Some((0, screen.height_in_pixels as i32))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn work_area_at(_x: i32, _y: i32) -> Option<(i32, i32)> {
    None
}

/// UI 线程持有的 X 连接：供 work_area_at 查显示器工作区（避免另开连接）
#[cfg(target_os = "linux")]
thread_local! {
    static X_CONN: RefCell<Option<Rc<x11rb::rust_connection::RustConnection>>> =
        const { RefCell::new(None) };
}

// =====================================================================
// Linux 后端：override-redirect X 窗口（x11rb）
// =====================================================================
#[cfg(target_os = "linux")]
mod x11_backend {
    use super::{Argb, UiBackend, BAR_HEIGHT, X_CONN};
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
        // 设置窗口（惰性创建；override-redirect 不参与焦点管理）
        win2: Option<u32>,
        gc2: Option<u32>,
        mapped2: bool,
        // 设置窗拖拽状态：(起始root_x, 起始root_y, 窗口x, 窗口y)
        drag2: Option<(i32, i32, i32, i32)>,
        // 设置窗当前宽度（拖动带 × 排除区计算用）
        win2_w: u32,
        // 顶部字母区窗口（惰性创建；无输入事件需求）
        win3: Option<u32>,
        gc3: Option<u32>,
        mapped3: bool,
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
            let conn = Rc::new(conn);
            // 登记给 work_area_at（屏幕工作区查询）
            X_CONN.with(|c| *c.borrow_mut() = Some(conn.clone()));
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
            Some(Self { conn, win, gc, mapped: false, win2: None, gc2: None, mapped2: false, win3: None, gc3: None, mapped3: false, drag2: None, win2_w: 0 })
        }

        /// 惰性创建顶部字母区窗口（override-redirect、不接收输入，纯展示）
        fn ensure_letters(&mut self) -> u32 {
            if let Some(w3) = self.win3 {
                return w3;
            }
            let conn = &self.conn;
            let screen = conn.setup().roots.first().unwrap().clone();
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
                eprintln!("heng-ui: 字母区未找到 32 位视觉");
                return self.win;
            };
            let Ok(win3) = conn.generate_id() else {
                return self.win;
            };
            let Ok(gc3) = conn.generate_id() else {
                return self.win;
            };
            let cmap = match conn.generate_id() {
                Ok(v) => v,
                Err(_) => return self.win,
            };
            if check_void!(
                conn.create_colormap(ColormapAlloc::NONE, cmap, screen.root, visual.visual_id),
                "create_colormap3"
            )
            .is_none()
            {
                return self.win;
            }
            // 无输入需求：仅 EXPOSURE；override-redirect 同候选窗
            let aux = CreateWindowAux::new()
                .override_redirect(1)
                .colormap(cmap)
                .border_pixel(0)
                .event_mask(EventMask::EXPOSURE);
            if check_void!(
                conn.create_window(
                    32,
                    win3,
                    screen.root,
                    200,
                    500,
                    (super::lay_panel_w() as f32 * super::ui_scale()).round() as u16,
                    (24.0 * super::ui_scale()).round() as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    visual.visual_id,
                    &aux,
                ),
                "create_window3"
            )
            .is_none()
            {
                return self.win;
            }
            if check_void!(conn.create_gc(gc3, win3, &CreateGCAux::new()), "create_gc3").is_none() {
                return self.win;
            }
            conn.flush().ok();
            self.win3 = Some(win3);
            self.gc3 = Some(gc3);
            win3
        }

        /// 惰性创建设置窗口（override-redirect，不参与焦点管理；同为 32 位视觉）
        fn ensure_set(&mut self) -> u32 {
            if let Some(w2) = self.win2 {
                return w2;
            }
            let conn = &self.conn;
            let screen_num = conn.setup().roots.len() - 1; // 占位，下面重新取根
            let _ = screen_num;
            // 重新取默认屏幕（new 时保存的 screen 已不可用，这里按 root 视觉重查）
            let screen = self.conn.setup().roots.first().unwrap().clone();
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
                eprintln!("heng-ui: 设置窗口未找到 32 位视觉");
                return self.win; // 兜底：复用候选窗（不理想但可用）
            };
            let win2 = match conn.generate_id() {
                Ok(v) => v,
                Err(_) => return self.win,
            };
            let gc2 = match conn.generate_id() {
                Ok(v) => v,
                Err(_) => return self.win,
            };
            let cmap = match conn.generate_id() {
                Ok(v) => v,
                Err(_) => return self.win,
            };
            if check_void!(
                conn.create_colormap(ColormapAlloc::NONE, cmap, screen.root, visual.visual_id),
                "create_colormap2"
            )
            .is_none()
            {
                return self.win;
            }
            let aux = CreateWindowAux::new()
                .colormap(cmap)
                .border_pixel(0)
                // 必须与候选窗一致：override-redirect 不参与焦点管理——
                // 否则点击设置窗会夺走 X 焦点，关闭后焦点落空（编辑器失焦
                // 打字无反应，且与哪套 IM 无关）
                .override_redirect(1)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION
                        | EventMask::LEAVE_WINDOW,
                );
            if check_void!(
                conn.create_window(
                    32,
                    win2,
                    screen.root,
                    200,
                    500,
                    (640.0 * super::ui_scale()).round() as u16,
                    (480.0 * super::ui_scale()).round() as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    visual.visual_id,
                    &aux,
                ),
                "create_window2"
            )
            .is_none()
            {
                return self.win;
            }
            if check_void!(conn.create_gc(gc2, win2, &CreateGCAux::new()), "create_gc2").is_none() {
                return self.win;
            }
            conn.flush().ok();
            self.win2 = Some(win2);
            self.gc2 = Some(gc2);
            win2
        }

        fn configure(&mut self, win: u32, x: i32, y: i32, w: u32, h: u32) {
            if Some(win) == self.win2 {
                self.win2_w = w;
            }
            let aux = ConfigureWindowAux::new().x(x).y(y).width(w as u32).height(h as u32);
            let _ = match self.conn.configure_window(win, &aux) {
                Ok(c) => c.check(),
                Err(_) => Err(x11rb::errors::ReplyError::ConnectionError(
                    x11rb::errors::ConnectionError::UnknownError,
                )),
            };
            self.conn.flush().ok();
        }

        fn set_mapped(&mut self, win: u32, mapped: bool) {
            let is_set_win = self.win2 == Some(win);
            let is_letters = self.win3 == Some(win);
            let cur = if is_set_win {
                self.mapped2
            } else if is_letters {
                self.mapped3
            } else {
                self.mapped
            };
            if cur == mapped {
                return;
            }
            let result = if mapped {
                self.conn.map_window(win).map(|c| c.check()).unwrap_or_else(|e| Err(e.into()))
            } else {
                self.conn.unmap_window(win).map(|c| c.check()).unwrap_or_else(|e| Err(e.into()))
            };
            if result.is_ok() {
                if is_set_win {
                    self.mapped2 = mapped;
                } else if is_letters {
                    self.mapped3 = mapped;
                } else {
                    self.mapped = mapped;
                }
            }
            self.conn.flush().ok();
        }

        fn blit(&mut self, win: u32, buf: &[Argb], w: u32, h: u32) {
            let gc = if self.win2 == Some(win) {
                self.gc2.unwrap_or(self.gc)
            } else if self.win3 == Some(win) {
                self.gc3.unwrap_or(self.gc)
            } else {
                self.gc
            };
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
                    win,
                    gc,
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
            // 调试：回读服务器端窗口首行像素，验证 put_image 是否生效（仅候选窗）
            if Some(win) == Some(self.win) {
                match self.conn.get_image(
                    ImageFormat::Z_PIXMAP,
                    win,
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
            }
            self.conn.flush().ok();
        }

        fn poll_events(&mut self, sink: &mut dyn FnMut(u8, WindowEvent)) {
            while let Some(event) = self.conn.poll_for_event().unwrap_or(None) {
                // 按接收窗口分流：候选窗 / 设置窗口
                let ev_win = match &event {
                    Event::MotionNotify(m) => m.event,
                    Event::ButtonPress(b) => b.event,
                    Event::ButtonRelease(b) => b.event,
                    Event::LeaveNotify(l) => l.event,
                    _ => 0,
                };
                let tag = if Some(ev_win) == self.win2 {
                    1
                } else if ev_win == self.win {
                    0
                } else {
                    continue;
                };
                match event {
                    Event::MotionNotify(m) => {
                        // 拖拽中：指针事件只用于移动窗口，不透传给 UI
                        if let Some((sx, sy, wx, wy)) = self.drag2 {
                            let nx = wx + (m.root_x as i32 - sx);
                            let ny = wy + (m.root_y as i32 - sy);
                            let aux = ConfigureWindowAux::new().x(nx).y(ny);
                            let _ = self.conn.configure_window(
                                self.win2.unwrap_or(self.win),
                                &aux,
                            );
                            self.conn.flush().ok();
                            continue;
                        }
                        sink(tag, WindowEvent::PointerMoved {
                            position: LogicalPosition::new(m.event_x as f32, m.event_y as f32),
                        });
                    }
                    Event::ButtonPress(b) => {
                        // 滚轮：4/5 垂直（上/下），6/7 水平（左/右），每 notch 50 逻辑px
                        if (4..=7).contains(&b.detail) {
                            let (dx, dy) = match b.detail {
                                4 => (0.0, 50.0),
                                5 => (0.0, -50.0),
                                6 => (-50.0, 0.0),
                                _ => (50.0, 0.0),
                            };
                            sink(tag, WindowEvent::PointerScrolled {
                                position: LogicalPosition::new(
                                    b.event_x as f32,
                                    b.event_y as f32,
                                ),
                                delta_x: dx,
                                delta_y: dy,
                            });
                            continue;
                        }
                        // 设置窗顶部拖动带（镜像 Windows WM_NCHITTEST 语义：
                        // 顶部 48 逻辑px，左导航列与 × 按钮区除外）
                        if b.detail == 1 && tag == 1 && self.drag2.is_none() {
                            let scale = super::ui_scale();
                            let top = (48.0 * scale).round() as i32;
                            let nav_w = (148.0 * scale).round() as i32;
                            let excl_x = self.win2_w as i32 - (40.0 * scale).round() as i32;
                            let (ex, ey) = (b.event_x as i32, b.event_y as i32);
                            if ey < top && ex > nav_w && ex < excl_x {
                                // 抓住指针：拖动期间即使指针移出窗口也持续收到事件
                                let grab = self.conn.grab_pointer(
                                    true,
                                    b.event,
                                    EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                                    GrabMode::ASYNC,
                                    GrabMode::ASYNC,
                                    x11rb::NONE,
                                    x11rb::NONE,
                                    x11rb::CURRENT_TIME,
                                );
                                if let Ok(cookie) = grab {
                                    if let Ok(reply) = cookie.reply() {
                                        if reply.status != GrabStatus::SUCCESS {
                                            eprintln!("heng-ui: 拖动 grab 失败: {:?}", reply.status);
                                        }
                                    }
                                }
                                // 窗口当前位置 = root 指针坐标 - 窗口内坐标
                                self.drag2 = Some((
                                    b.root_x as i32,
                                    b.root_y as i32,
                                    b.root_x as i32 - b.event_x as i32,
                                    b.root_y as i32 - b.event_y as i32,
                                ));
                                continue;
                            }
                        }
                        if b.detail == 1 {
                            sink(tag, WindowEvent::PointerPressed {
                                position: LogicalPosition::new(b.event_x as f32, b.event_y as f32),
                                button: slint::platform::PointerEventButton::Left,
                            });
                        }
                    }
                    Event::ButtonRelease(b) if b.detail == 1 => {
                        if self.drag2.is_some() {
                            self.drag2 = None;
                            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
                            self.conn.flush().ok();
                            continue;
                        }
                        sink(tag, WindowEvent::PointerReleased {
                            position: LogicalPosition::new(b.event_x as f32, b.event_y as f32),
                            button: slint::platform::PointerEventButton::Left,
                        });
                    }
                    Event::LeaveNotify(_) => {
                        sink(tag, WindowEvent::PointerExited);
                    }
                    _ => {}
                }
            }
        }
    }

    impl UiBackend for XWindow {
        fn configure(&mut self, win: u8, x: i32, y: i32, w: u32, h: u32) {
            if win == 2 {
                let win_id = self.ensure_letters();
                return XWindow::configure(self, win_id, x, y, w, h);
            }
            let w = if win == 1 {
                self.ensure_set();
                (640.0 * super::ui_scale()).round() as u32
            } else {
                w
            };
            let h = if win == 1 {
                (480.0 * super::ui_scale()).round() as u32
            } else {
                h
            };
            let win_id = if win == 1 {
                self.win2.unwrap_or(self.win)
            } else {
                self.win
            };
            XWindow::configure(self, win_id, x, y, w, h)
        }
        fn set_mapped(&mut self, win: u8, mapped: bool) {
            let win_id = match win {
                1 => self.ensure_set(),
                2 => self.ensure_letters(),
                _ => self.win,
            };
            XWindow::set_mapped(self, win_id, mapped)
        }
        fn blit(&mut self, win: u8, buf: &[Argb], w: u32, h: u32) {
            let win_id = match win {
                1 => self.ensure_set(),
                2 => self.ensure_letters(),
                _ => self.win,
            };
            XWindow::blit(self, win_id, buf, w, h)
        }
        fn poll_events(&mut self, sink: &mut dyn FnMut(u8, WindowEvent)) {
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
        GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
        GetDC, InvalidateRect, ReleaseDC, ScreenToClient, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
        BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, PAINTSTRUCT,
        BI_RGB, SRCCOPY,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SetFocus, TME_LEAVE, TrackMouseEvent, TRACKMOUSEEVENT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, LoadCursorW,
        PeekMessageW, RegisterClassW, SetForegroundWindow, SetWindowPos, ShowWindow,
        TranslateMessage, UpdateLayeredWindow, HTCAPTION, HTCLIENT, HTTRANSPARENT, IDC_ARROW, MSG,
        PM_REMOVE,
        SW_HIDE, SW_SHOW, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
        ULW_ALPHA, WM_CLOSE, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
        WM_MOUSEWHEEL, WM_NCHITTEST, WM_PAINT,
        WNDCLASSW, WHEEL_DELTA, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT,
        WS_POPUP,
    };

    thread_local! {
        /// WndProc 收集的指针事件，poll_events 时排空（WndProc 与主循环同线程）
        static EVENTS: RefCell<Vec<WindowEvent>> = RefCell::new(Vec::new());
        /// 设置窗口事件队列（独立 WndProc，避免与候选窗混淆）
        static EVENTS2: RefCell<Vec<WindowEvent>> = RefCell::new(Vec::new());
        /// 设置窗口绘制状态：(hwnd, memdc, w, h)，WM_PAINT 时 BitBlt
        static SET_DC: RefCell<Option<(HWND, HDC, u32, u32)>> = const { RefCell::new(None) };
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

    /// 设置窗口 WndProc：鼠标事件入 EVENTS2；非分层窗口，WM_PAINT 从 DIB BitBlt；
    /// WM_CLOSE 转发 WindowClosed（隐藏不销毁，UI 线程收尾）。
    unsafe extern "system" fn set_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let (x, y) = (
            (lparam & 0xffff) as u16 as i16 as f32,
            ((lparam >> 16) & 0xffff) as u16 as i16 as f32,
        );
        match msg {
            WM_MOUSEMOVE => {
                EVENTS2.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerMoved {
                        position: LogicalPosition::new(x, y),
                    })
                });
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
                EVENTS2.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerPressed {
                        position: LogicalPosition::new(x, y),
                        button: PointerEventButton::Left,
                    })
                });
                0
            }
            WM_LBUTTONUP => {
                EVENTS2.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerReleased {
                        position: LogicalPosition::new(x, y),
                        button: PointerEventButton::Left,
                    })
                });
                0
            }
            WM_MOUSELEAVE => {
                EVENTS2.with(|q| q.borrow_mut().push(WindowEvent::PointerExited));
                0
            }
            WM_MOUSEWHEEL => {
                // 滚轮 → Slint PointerScrolled（delta: 每 notch 120 raw → 50 逻辑px）
                let delta = ((wparam >> 16) & 0xffff) as u16 as i16 as f32;
                let mut pt = POINT {
                    x: (lparam & 0xffff) as u16 as i16 as i32,
                    y: ((lparam >> 16) & 0xffff) as u16 as i16 as i32,
                };
                ScreenToClient(hwnd, &mut pt);
                EVENTS2.with(|q| {
                    q.borrow_mut().push(WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(pt.x as f32, pt.y as f32),
                        delta_x: 0.0,
                        delta_y: delta / WHEEL_DELTA as f32 * 50.0,
                    })
                });
                0
            }
            WM_NCHITTEST => {
                // 无边框窗口的标题栏替代：顶部 48 逻辑px 可拖动（× 按钮区除外）
                let mut pt = POINT {
                    x: (lparam & 0xffff) as u16 as i16 as i32,
                    y: ((lparam >> 16) & 0xffff) as u16 as i16 as i32,
                };
                ScreenToClient(hwnd, &mut pt);
                let mut rc = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetClientRect(hwnd, &mut rc);
                let scale = super::ui_scale();
                let top = (48.0 * scale).round() as i32;
                let nav_w = (148.0 * scale).round() as i32; // 左导航列不可拖（首项在拖动带内）
                let excl_x = rc.right - (40.0 * scale).round() as i32; // × 按钮 x 起点
                if pt.y < top && pt.x > nav_w && pt.x < excl_x {
                    HTCAPTION as isize
                } else {
                    HTCLIENT as isize
                }
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                SET_DC.with(|c| {
                    if let Some((h, dc, w, hgt)) = &*c.borrow() {
                        if *h == hwnd {
                            BitBlt(hdc, 0, 0, *w as i32, *hgt as i32, *dc, 0, 0, SRCCOPY);
                        }
                    }
                });
                EndPaint(hwnd, &ps);
                0
            }
            WM_CLOSE => {
                EVENTS2.with(|q| q.borrow_mut().push(WindowEvent::CloseRequested));
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// 单个原生窗口（候选窗 / 设置窗口共用）
    struct SubWin {
        hwnd: HWND,
        memdc: HDC,
        hbmp: HBITMAP,
        bits: *mut u8,
        cur_size: (u32, u32),
        pos: (i32, i32),
        mapped: bool,
    }

    /// 候选窗创建参数（分层 + 置顶 + 不激活 + 工具窗）
    const CAND_EX: u32 = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
    const CAND_STYLE: u32 = WS_POPUP;
    /// 设置窗口创建参数（无边框：标题栏由自绘 UI 提供，顶部区拖动见 WM_NCHITTEST）
    const SET_EX: u32 = 0;
    const SET_STYLE: u32 = WS_POPUP;

    /// 字母区 WndProc：纯显示窗口，不处理指针事件（避免误路由到候选窗的
    /// EVENTS 队列）。WM_NCHITTEST → HTTRANSPARENT 让命中穿透到下层应用，
    /// 该区域覆盖的是用户正在输入的一行文字，点击本应落到应用上。
    /// 注意：这只是命中穿透，与「未响应」无关——系统仍须先派发 WM_NCHITTEST
    /// 才能判定穿透（见 poll_events：漏泵本窗口消息才是未响应的真因）。
    unsafe extern "system" fn letters_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCHITTEST => HTTRANSPARENT as isize,
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn create_subwin(
        class: &str,
        title: &str,
        ex: u32,
        style: u32,
        w: i32,
        h: i32,
    ) -> Option<SubWin> {
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let classw = utf16z(class);
            let is_set_win = class == "heng_ui_set_win";
            let is_letters_win = class == "heng_ui_letters_win";
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: if is_set_win {
                    Some(set_wnd_proc)
                } else if is_letters_win {
                    Some(letters_wnd_proc)
                } else {
                    Some(wnd_proc)
                },
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: classw.as_ptr(),
            };
            // 已注册时返回 0，忽略（同线程每类只建一次）
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                ex,
                classw.as_ptr(),
                utf16z(title).as_ptr(),
                style,
                200,
                500,
                w,
                h,
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
            Some(SubWin {
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

    pub struct WinWindow {
        cand: SubWin,
        set: Option<SubWin>,
        letters: Option<SubWin>,
    }

    impl WinWindow {
        pub fn new() -> Option<Self> {
            Some(Self {
                cand: create_subwin(
                    "heng_ui_win",
                    "",
                    CAND_EX,
                    CAND_STYLE,
                    160,
                    BAR_HEIGHT as i32,
                )?,
                set: None,
                letters: None,
            })
        }

        /// 惰性创建设置窗口（普通可激活窗口，客户区 640×480 逻辑px × scale）
        fn ensure_set(&mut self) -> &mut SubWin {
            if self.set.is_none() {
                let scale = super::ui_scale();
                let w = (640.0 * scale).round() as i32;
                let h = (480.0 * scale).round() as i32;
                let s = create_subwin(
                    "heng_ui_set_win",
                    "衡 · 设置",
                    SET_EX,
                    SET_STYLE,
                    w,
                    h,
                )
                .expect("heng-ui: 设置窗口创建失败");
                SET_DC.with(|c| {
                    *c.borrow_mut() = Some((s.hwnd, s.memdc, 0, 0));
                });
                self.set = Some(s);
            }
            self.set.as_mut().unwrap()
        }

        /// 惰性创建顶部字母区窗口（分层 + 置顶 + 不激活 + 鼠标穿透，同候选窗；
        /// 纯显示无交互，WndProc 事件直通 DefWindowProc 防误路由到候选窗）
        fn ensure_letters(&mut self) -> &mut SubWin {
            if self.letters.is_none() {
                let s = create_subwin(
                    "heng_ui_letters_win",
                    "",
                    CAND_EX | WS_EX_TRANSPARENT,
                    CAND_STYLE,
                    160,
                    40,
                )
                .expect("heng-ui: 字母区窗口创建失败");
                self.letters = Some(s);
            }
            self.letters.as_mut().unwrap()
        }
    }

    impl SubWin {
        /// 按 (w,h) 重建 DIB（尺寸变化时）。DIB 变了窗口尺寸也要跟着变，
        /// 否则 WM_PAINT 的 BitBlt 会按旧客户区尺寸拉伸/截断新帧
        /// （用户中途改系统缩放、或把窗口挪到异构 DPI 屏时必现）。
        fn ensure_bitmap(&mut self, w: u32, h: u32) -> bool {
            unsafe {
                if self.cur_size == (w, h) && !self.bits.is_null() {
                    return true;
                }
                let resized = self.cur_size != (0, 0);
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
                // 已存在的窗口要同步客户区尺寸（首次创建交给 configure，
                // 那时 cur_size 还是 0/0，DIB 与窗口由create_subwin 同步给出）
                if resized {
                    let (x, y) = self.pos;
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
                true
            }
        }
    }

    impl Drop for WinWindow {
        fn drop(&mut self) {
            unsafe {
                let mut subs: Vec<&mut SubWin> = vec![&mut self.cand];
                if let Some(s) = self.set.as_mut() {
                    subs.push(s);
                }
                if let Some(s) = self.letters.as_mut() {
                    subs.push(s);
                }
                for s in subs {
                    if !s.hbmp.is_null() {
                        DeleteObject(s.hbmp);
                    }
                    DeleteDC(s.memdc);
                }
            }
        }
    }

    impl UiBackend for WinWindow {
        fn configure(&mut self, win: u8, x: i32, y: i32, w: u32, h: u32) {
            if win > 2 {
                return;
            }
            let s = match win {
                0 => &mut self.cand,
                1 => {
                    self.ensure_set();
                    self.set.as_mut().unwrap()
                }
                _ => self.ensure_letters(),
            };
            s.pos = (x, y);
            unsafe {
                SetWindowPos(
                    s.hwnd,
                    std::ptr::null_mut(),
                    x,
                    y,
                    w as i32,
                    h as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }

        fn set_mapped(&mut self, win: u8, mapped: bool) {
            if win > 2 {
                return;
            }
            let s = match win {
                0 => &mut self.cand,
                1 => {
                    self.ensure_set();
                    self.set.as_mut().unwrap()
                }
                _ => self.ensure_letters(),
            };
            if s.mapped == mapped {
                return;
            }
            unsafe {
                if win == 1 {
                    // 设置窗口：激活显示 + 置前台 + 设键盘焦点（滚轮立即生效）
                    ShowWindow(s.hwnd, if mapped { SW_SHOW } else { SW_HIDE });
                    SetForegroundWindow(s.hwnd);
                    SetFocus(s.hwnd);
                } else {
                    // 候选窗/字母区：SW_SHOWNA 显示但不激活（不夺前台焦点）
                    ShowWindow(s.hwnd, if mapped { SW_SHOWNA } else { SW_HIDE });
                }
            }
            s.mapped = mapped;
        }

        fn blit(&mut self, win: u8, buf: &[Argb], w: u32, h: u32) {
            if win > 2 {
                return;
            }
            let s = match win {
                0 => &mut self.cand,
                1 => {
                    self.ensure_set();
                    self.set.as_mut().unwrap()
                }
                _ => self.ensure_letters(),
            };
            if !s.ensure_bitmap(w, h) {
                return;
            }
            if win == 1 {
                SET_DC.with(|c| {
                    if let Some((_, _, bw, bh)) = &mut *c.borrow_mut() {
                        *bw = w;
                        *bh = h;
                    }
                });
            }
            unsafe {
                // Argb(#[repr(C)] b,g,r,a) 内存序 == DIB 32bpp top-down BGRA 预乘，直接拷
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr() as *const u8,
                    s.bits,
                    (w * h * 4) as usize,
                );
                if win == 1 {
                    // 非分层窗口：触发 WM_PAINT → BitBlt
                    InvalidateRect(s.hwnd, std::ptr::null(), 0);
                    return;
                }
                // 候选窗/字母区：分层窗口 UpdateLayeredWindow
                let screen_dc = GetDC(std::ptr::null_mut());
                let pt_dst = POINT { x: s.pos.0, y: s.pos.1 };
                let size = SIZE { cx: w as i32, cy: h as i32 };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let ok = UpdateLayeredWindow(
                    s.hwnd,
                    screen_dc,
                    &pt_dst,
                    &size,
                    s.memdc,
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

        fn poll_events(&mut self, sink: &mut dyn FnMut(u8, WindowEvent)) {
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                // 泵整个 UI 线程的消息队列（hwnd 传 null = 不按窗口过滤）。
                //
                // 为什么必须整线程排空：Windows 挂起检测靠 SendMessageTimeout 发
                // WM_NCHITTEST—— 指针移到本线程任一窗口上就会发，若接收线程不
                // 派发，系统判定线程挂起并弹「未响应」。按 hwnd 过滤的 PeekMessage
                // 很容易漏掉某个窗口（原先只有候选窗 + 设置窗，新增的字母区就被
                // 漏掉 → 指针悬停到字母区必现「未响应」）。本线程只拥有这三个
                // 窗口，整线程排空既彻底又不会误吞别人的窗口消息。
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            EVENTS.with(|q| {
                for ev in q.borrow_mut().drain(..) {
                    sink(0, ev);
                }
            });
            EVENTS2.with(|q| {
                for ev in q.borrow_mut().drain(..) {
                    sink(1, ev);
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
    // 内嵌关时字母显示在条上方（对齐 weasel inline_preedit 语义）；
    // 开时字母由 client preedit 内嵌到应用，条内不显示
    let inline_on = crate::settings::Settings::new(engine)
        .get_path(crate::settings::PATCH_WEASEL, "style/inline_preedit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false); // Linux 默认关（字母显示在候选窗上方）
    let input_text = if !inline_on {
        snapshot.preedit.clone()
    } else {
        String::new()
    };
    ui.set_input_text(input_text.into());
    // 只保留首行可见词：40px 横条只显示一行，放不下的词不显示也不参与
    // 键盘导航（否则 → 要穿过一堆看不见的词才能到图标）
    let (cells0, _) = build_flow_cells(&texts, hl);
    let visible = cells0.iter().filter(|c| c.y == lay_flow_top()).count().max(1);
    let visible = visible.min(texts.len());
    texts.truncate(visible);
    panel_hl.set(hl.min(visible as i32 - 1).max(0));
    UI_HL.store(panel_hl.get(), Ordering::Relaxed);
    UI_BAR_COUNT.store(visible as i32, Ordering::Relaxed);
    *bar_items.borrow_mut() = texts.clone();
    let (cells, _rows) = build_flow_cells(&texts, panel_hl.get());
    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
    ui.set_panel_width(lay_panel_w());
    // 中英角标：状态来自 get_status（ContextSnapshot 已不含 ascii 位）
    if let Ok(st) = engine.get_status(rime_id) {
        ui.set_ascii(st.is_ascii_mode);
    }
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

/// 字符宽度估算（宁可估宽绝不估窄）——候选格与条内字母区共用。
/// 基准：14px DejaVu Sans 实测（PIL getlength），系数含 ~15% 余量。
/// 估算偏小的后果：slint Text 在格内 elide 出省略号（用户红线：只有
/// 单词挤满整行才允许省略号）。
/// ① 0x1F000+ / 0x2600-0x27BF / 0x2B00-0x2BFF：原生 emoji 区 → 22px
/// ② 后跟 FE0F 的符号强制 emoji 呈现（⬆️⏰⌚▶️ 等散布各符号区）→ 22px
/// ③ 0x2000 以上全部 → 15px：CJK 实渲染 ≈ 字号宽（14），留 1px 余量
/// ④ 变体选择符 0xFE00-0xFE0F / ZWJ 0x200D → 0；ASCII → 10px
///   （"windows" 实测 60.4px/7 字 ≈ 8.6px/字，10px 覆盖大写与宽字母）
fn estimate_w(text: &str) -> i32 {
    let chs: Vec<char> = text.chars().collect();
    chs.iter()
        .enumerate()
        .map(|(i, &c)| {
            let u = c as u32;
            if (0xFE00..=0xFE0F).contains(&u) || u == 0x200D {
                0
            } else if u >= 0x1F000
                || (0x2600..=0x27BF).contains(&u)
                || (0x2B00..=0x2BFF).contains(&u)
            {
                22
            } else if chs
                .get(i + 1)
                .is_some_and(|&n| (0xFE00..=0xFE0F).contains(&(n as u32)) || n as u32 == 0x200D)
            {
                22
            } else if u >= 0x2000 {
                15
            } else {
                10
            }
        })
        .sum()
}

/// 候选窗探针句柄：UI 线程创建组件后登记，供 measure_w 实测文本宽。
thread_local! {
    static PROBE: RefCell<Option<Rc<CandWindow>>> = const { RefCell::new(None) };
}

/// 实测文本排版宽（逻辑px，含 1px 防舍入余量）：用候选窗里的隐藏探针 Text
/// 按指定字号测量，任何字号/字符集（CJK/ASCII/emoji）都精确——取代随字号
/// 漂移的估算系数表（估算偏窄 = slint elide = 全员省略号事故）。
/// 探针未登记（UI 线程外/启动早期）时退回估算表。
fn measure_w(text: &str, font: i32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    PROBE.with(|p| {
        if let Some(ui) = p.borrow().as_ref() {
            ui.set_probe_text(text.into());
            ui.set_probe_font(font);
            ui.get_probe_width().ceil() as i32 + 1
        } else {
            estimate_w(text)
        }
    })
}

/// 当前候选字号（探针未登记时回默认 16）
fn current_cand_font() -> i32 {
    PROBE.with(|p| {
        p.borrow()
            .as_ref()
            .map(|u| u.get_cand_font())
            .unwrap_or(16)
    })
}

/// 内容自适应流式排布（收起单行同款）：格子宽度随词长走，一行能塞几个塞几个，
/// 不限个数；超长词封顶整行宽并中间省略。序号每行从 1 开始。
/// 返回 (cells, 行数)。hl_global 为当前选中候选的全局序号。
/// 宽度一律实测（measure_w），估算表仅作探针未就绪的兜底。
fn build_flow_cells(all: &[String], hl_global: i32) -> (Vec<CandCell>, i32) {
    let margin = lay_px(6.0);
    let gap = lay_px(3.0);
    let font = current_cand_font();
    let num_font = lay_num_font();
    // 格内水平留白等比且左右对称（与 slint HorizontalLayout 的 padding/spacing 一致）
    let (pad_l, pad_s, pad_r) = (lay_px(6.0), lay_px(3.0), lay_px(6.0));
    let panel_w = lay_panel_w();
    // 首行右侧给 ▾/☰ 两个按钮留位（按钮带设计 58）；展开行给滚动条留位
    let first_row_right = panel_w - margin - lay_px(58.0);
    let row_right = panel_w - margin - lay_px(8.0);
    let mut cells: Vec<CandCell> = Vec::new();
    let (mut x, mut y) = (margin, lay_flow_top());
    let mut num = 0i32;
    for (i, t) in all.iter().enumerate() {
        let disp = t.clone();
        let text_w = measure_w(&disp, font);
        // 序号按实际数字实测宽度（两位数自然更宽，序号与正文不再离太远）
        let num_str = (num + 1).to_string();
        let nw = measure_w(&num_str, num_font);
        let full_w = pad_l + nw + pad_s + text_w + pad_r;
        // 当前装不下完整一词 → 换行（每词完整显示，绝不截断塞边角）
        let top = lay_flow_top();
        let right = if y == top { first_row_right } else { row_right };
        if x + full_w > right && num > 0 {
            y += lay_row_h();
            x = margin;
            num = 0;
        }
        // 只有当一个词连一整行可填宽度都放不下（超长句）才中间省略
        let right = if y == top { first_row_right } else { row_right };
        let max_w = right - margin;
        let full_w = pad_l + nw + pad_s + text_w + pad_r;
        let (w, disp) = if full_w > max_w {
            // 真正放不下一整行才省略；截断后实测收敛，保证任何字号都装得下
            let avail = (max_w - pad_l - nw - pad_s - pad_r).max(0) as usize;
            let mut cap = (avail / 12).max(1);
            let mut d = mid_ellipsis(t, cap);
            while cap > 1 && measure_w(&d, font) > avail as i32 {
                cap -= 1;
                d = mid_ellipsis(t, cap);
            }
            (max_w, d)
        } else {
            (full_w, disp)
        };
        if y > lay_flow_top() + FLOW_MAX_ROWS * lay_row_h() {
            break; // 行数封顶，超出部分后续做滚动
        }
        cells.push(CandCell {
            num: SharedString::from(num_str),
            nw,
            text: SharedString::from(disp),
            hl: (i as i32) == hl_global,
            x,
            y,
            w,
        });
        x += w + gap;
        num += 1;
    }
    let rows = ((y - lay_flow_top()) / lay_row_h()) + 1;
    (cells, rows)
}

/// 固定 6 槽网格排布（展开面板同款）：每槽容纳 3 字 + 序号；
/// 词的槽位跨度 = ceil(字数/3)（4 字占 2 格、7 字占 3 格），占满 6 槽换行；
/// 槽内容纳不下时中间省略。序号每行从 1 开始。返回 (cells, 行数)。
fn build_grid_cells(all: &[String], hl_global: i32) -> (Vec<CandCell>, i32) {
    let margin = lay_px(6.0);
    let gap = lay_px(4.0);
    let panel_w = lay_panel_w();
    // 每行 6 槽（宽度随字号等比放大 → 槽宽自然等比放大，"序号 + 3 字"始终放得下）
    const SLOTS_PER_ROW: i32 = 6;
    let slots_per_row = SLOTS_PER_ROW;
    let slot_w = (panel_w - margin * 2) / slots_per_row;
    let font = current_cand_font();
    let num_font = lay_num_font();
    let (pad_l, pad_s, pad_r) = (lay_px(6.0), lay_px(3.0), lay_px(6.0));
    let mut cells: Vec<CandCell> = Vec::new();
    let (mut x, mut y) = (margin, lay_flow_top());
    let mut used_slots = 0i32;
    let mut num = 0i32;
    let mut hl_row: Option<i32> = None;
    for (i, t) in all.iter().enumerate() {
        let chars = t.chars().count() as i32;
        let spans = (((chars + 2) / 3).max(1)).min(slots_per_row);
        if used_slots + spans > slots_per_row {
            // 换行：序号从 1 重新开始
            y += lay_row_h();
            x = margin;
            used_slots = 0;
            num = 0;
        }
        if y > lay_flow_top() + FLOW_MAX_ROWS * lay_row_h() {
            break; // 行数封顶，超出部分后续做滚动
        }
        let num_str = (num + 1).to_string();
        let nw = measure_w(&num_str, num_font);
        // 正文可用宽（实测收敛）：保证任何字号下槽内都放得下，不出现省略号
        let budget = spans * slot_w - gap - pad_l - pad_r - nw - pad_s;
        let mut cap = (spans * 3) as usize;
        let mut disp = mid_ellipsis(t, cap);
        while cap > 1 && measure_w(&disp, font) > budget {
            cap -= 1;
            disp = mid_ellipsis(t, cap);
        }
        cells.push(CandCell {
            nw,
            num: SharedString::from(num_str),
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
                c.nw = 0;
            }
        }
    }
    let rows = ((y - lay_flow_top()) / lay_row_h()) + 1;
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
/// name 用于区分来源（bar = 候选窗，settings = 设置窗口）。
fn dump_frame(buf: &[Argb], w: u32, h: u32, name: &str) {
    let path = if cfg!(target_os = "windows") {
        std::env::temp_dir().join(format!("heng-{name}.ppm"))
    } else {
        std::path::PathBuf::from(format!("/tmp/heng-{name}.ppm"))
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
    slint::platform::set_platform(Box::new(XPlatform {
        n: std::cell::Cell::new(0),
    }))
    .expect("heng-ui: set_platform 失败");
    let expanded = Rc::new(Cell::new(false));
    // 展开面板快照：内容在展开瞬间固定，导航只动视觉高亮，不碰 rime
    struct PanelSnap {
        items: Vec<String>,
        real_count: usize, // 真实候选数（其后为同音词）
        geo: Vec<(i32, i32, i32)>, // 每格 (x,y,w)，按行导航用
    }
    let panel: Rc<RefCell<Option<PanelSnap>>> = Rc::new(RefCell::new(None));
    let panel_hl = Rc::new(Cell::new(0i32));
    // 收起横条列表首项的全局序号（点击/键盘选词换算用）
    let bar_base = Rc::new(Cell::new(0i32));
    // 收起横条词表缓存（←→ 移动视觉高亮时不重取词）
    let bar_items: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // 收起横条：☰ 菜单打开态
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
    // 登记测宽探针（measure_w 用）；此后所有候选格宽度走实测
    PROBE.with(|p| *p.borrow_mut() = Some(ui.clone()));
    // 第 2 个组件实例 → MSW2（XPlatform 计数路由）
    let msw2 = MSW2.with(|w| w.clone());
    let sui = Rc::new(SettingsWindow::new().expect("heng-ui: 设置组件创建失败"));
    // 第 3 个组件实例 → MSW3：顶部专用字母区（独立窗口悬浮于候选窗正上方，
    // 原候选窗布局零改动）
    let msw3 = MSW3.with(|w| w.clone());
    let lsw = Rc::new(LettersWindow::new().expect("heng-ui: 字母区组件创建失败"));
    // 启动时先应用候选字号（7 档刻度 14-20，默认 16 = 第 3 档）——
    // 布局尺寸全部由此推导，必须在算高度之前
    if let Ok(eng) = engine() {
        let sett = Settings::new(eng);
        let f = sett
            .get_path(crate::settings::PATCH_WEASEL, "style/font_point_size")
            .and_then(|v| v.as_i64())
            .map(|v| v.clamp(14, 20) as i32)
            .unwrap_or(16);
        ui.set_cand_font(f);
        lsw.set_cand_font(f);
        let k = f as f32 / 14.0;
        ui.set_font_scale(k);
        lsw.set_font_scale(k);
        // 自定义配色：启动即应用（不走 redeploy，纯 UI 属性）
        let colors = read_theme_colors(&sett);
        apply_theme_colors(&ui, &lsw, &colors);
    }
    // DPI 缩放：布局用逻辑像素，渲染/窗口尺寸放大到物理像素
    let scale = ui_scale();
    if std::env::var_os("HENG_UI_DUMP").is_some() {
        eprintln!(
            "heng-ui: dpi scale = {scale} → 设置窗物理 {}x{}",
            (640.0 * scale).round() as u32,
            (480.0 * scale).round() as u32
        );
    }
    let bar_h = (lay_bar_h() as f32 * scale).round() as u32;
    // 顶部字母区高度（物理px）：独立窗口，paint 时贴候选窗正上方
    let input_row_h = (lay_letters_h() as f32 * scale).round() as u32;
    msw.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    msw.set_size(PhysicalSize { width: (160.0 * scale) as u32, height: bar_h });
    ui.show().unwrap(); // 不调用则 Slint 认为窗口不可见，永远不渲染
    // 字母区：尺寸/DPI 登记（show 推迟到首次显示时）
    lsw.show().unwrap();
    msw3.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    msw3.set_size(PhysicalSize {
        width: (lay_panel_w() as f32 * scale).round() as u32,
        height: input_row_h,
    });
    // 设置窗口：尺寸/DPI 一次性登记（show 推迟到打开时）
    msw2.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
    msw2.set_size(PhysicalSize {
        width: (640.0 * scale).round() as u32,
        height: (480.0 * scale).round() as u32,
    });
    sui.set_nav_names(
        Rc::new(VecModel::from(
            SET_NAV.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>(),
        ))
        .into(),
    );

    // 当前展示的会话（Sync 时登记；点击选词用）
    let current: Rc<RefCell<Option<RimeSessionId>>> = Rc::new(RefCell::new(None));
    let render_buf: RefCell<Vec<Argb>> = RefCell::new(Vec::new());
    // 顶部字母区独立窗口的帧缓冲
    let lsbuf: RefCell<Vec<Argb>> = RefCell::new(Vec::new());
    let visible = RefCell::new(false);
    // 收起条物理高：随当前字号实时推导（设置页改字号后立即生效）
    let bar_h_now = || (lay_bar_h() as f32 * scale).round() as u32;
    let cur_h = Cell::new(bar_h); // 当前物理高度（展开态会变）
    // 向上展开状态：光标下方放不下时翻转，窗口底边锚定输入行顶边、面板向上长。
    // flip_top = 所在显示器工作区顶（防向上越出屏幕）
    let flip = Cell::new(false);
    let flip_top = Cell::new(0);
    // 光标所在文本行顶边（外壳经 heng_ui_sync_ex 传入；旧路径按底边-40 估算）
    let last_caret_top = Cell::new(0);
    let last_preedit: RefCell<String> = RefCell::new(String::new());
    let hint_mode = Cell::new(0i32);
    // 气泡态自动恢复期限：到期后按输入上下文决定回横条还是藏窗（不误杀候选）
    let hint_deadline: Cell<Option<std::time::Instant>> = Cell::new(None);
    // 渲染并上屏（Sync / 展开 / 收起共用）。w,h 物理像素；map=true 时确保已映射
    // （XWayland：未映射窗口的 PutImage 会被静默丢弃，必须先 map 再画）
    let paint = |backend: &mut Box<dyn UiBackend>,
                 w: u32,
                 h: u32,
                 x: i32,
                 y: i32,
                 map: bool| {
        // 翻转态：面板/菜单底边 = 输入行顶边（整体悬在输入行上面、不遮输入行，
        // 微信同款）。翻转只在展开态存在（收起即复位），故 h 恒为面板/菜单高；
        // flip_top 防向上越出屏幕
        let y_eff = if flip.get() {
            (last_caret_top.get() - h as i32).max(flip_top.get())
        } else {
            y
        };
        cur_h.set(h);
        msw.set_size(PhysicalSize { width: w, height: h });
        backend.configure(0, x, y_eff, w, h);
        if render_buf.borrow().len() != (w * h) as usize {
            *render_buf.borrow_mut() = vec![Argb::default(); (w * h) as usize];
        }
        if map {
            backend.set_mapped(0, true);
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
                dump_frame(&buf, w, h, "bar");
            }
            backend.blit(0, &buf, w, h);
        }
        // 顶部专用字母区：独立窗口悬浮于本窗口正上方（原窗口布局零改动）。
        // hint 气泡态不显示（与气泡形态互斥）
        let has_input = !ui.get_input_text().is_empty() && hint_mode.get() == 0;
        // 字母区贴在主窗/面板顶边之上（用已夹紧的 y_eff，翻转态与面板一致）
        let ly = y_eff - input_row_h as i32;
        // 翻转态面板已贴到工作区顶边 → 上方没有空间，隐藏字母区（否则与面板重叠）
        let letters_ok = has_input && !(flip.get() && ly <= flip_top.get());
        if letters_ok {
            lsw.set_text(ui.get_input_text());
            if lsbuf.borrow().len() != (w * input_row_h) as usize {
                *lsbuf.borrow_mut() = vec![Argb::default(); (w * input_row_h) as usize];
            }
            msw3.set_size(PhysicalSize { width: w, height: input_row_h });
            msw3.window().request_redraw();
            {
                let mut lb = lsbuf.borrow_mut();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    msw3.draw_if_needed(|r: &SoftwareRenderer| {
                        let _ = r.render(&mut lb, w as usize);
                    });
                }));
            }
            backend.configure(2, x, ly, w, input_row_h);
            backend.set_mapped(2, true);
            let lb = lsbuf.borrow();
            backend.blit(2, &lb, w, input_row_h);
        } else {
            backend.set_mapped(2, false);
        }
        *visible.borrow_mut() = map || *visible.borrow();
    };

    // ==================== 面板选词（空格/回车 与 数字键共用） ====================
    // 按面板全局索引选中一项：真实候选走 select_candidate_global + 取引擎
    // commit；同音词走清组合串 + 文本直上屏。之后统一收面板恢复横条。
    let select_panel_index = |backend: &mut Box<dyn UiBackend>, idx: usize| {
        let Some(rime_id) = *current.borrow() else { return };
        let Ok(engine) = engine() else { return };
        let item = panel.borrow().as_ref().and_then(|snap| {
            let real = idx < snap.real_count;
            snap.items.get(idx).cloned().map(|t| (t, real))
        });
        let Some((text, is_real)) = item else { return };
        {
            let _guard = OP_LOCK.lock().unwrap();
            let ok = if is_real {
                let sel_ok = engine.select_candidate_global(rime_id, idx).unwrap_or(false);
                if sel_ok {
                    // select 产生的上屏文本在 librime 的 commit 队列里，
                    // 必须显式取出交给外壳（否则应用收不到——正常按键
                    // 路径由 heng_process_key_ex 返回值携带，此路径没有）
                    match engine.get_commit(rime_id) {
                        Ok(t) if !t.is_empty() => {
                            if let Some(h) = SESSIONS.handle_of(rime_id) {
                                emit_commit(h, &t);
                            }
                        }
                        _ => {}
                    }
                }
                sel_ok
            } else {
                // 同音词：清组合串 + 文本直接上屏
                let _ = engine.clear_composition(rime_id);
                if let Some(h) = SESSIONS.handle_of(rime_id) {
                    emit_commit(h, &text);
                }
                true
            };
            if ok && !is_real {
                // 同音词上屏后组合串已清，直接收起
                ui.set_expanded(false);
                expanded.set(false);
                UI_EXPANDED.store(false, Ordering::Relaxed);
                flip.set(false);
                *panel.borrow_mut() = None;
                backend.set_mapped(0, false);
                backend.set_mapped(2, false);
                *visible.borrow_mut() = false;
            }
        }
        if is_real {
            // 真实候选：可能仍在组句（如首词后继续），收面板恢复横条
            ui.set_expanded(false);
            expanded.set(false);
            UI_EXPANDED.store(false, Ordering::Relaxed);
            flip.set(false);
            *panel.borrow_mut() = None;
            let snapshot = {
                let _guard = OP_LOCK.lock().unwrap();
                engine.get_context(rime_id)
            };
            if let Ok(snapshot) = snapshot {
                if snapshot.candidates.is_empty() {
                    backend.set_mapped(0, false);
                    backend.set_mapped(2, false);
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
                    let w = (lay_panel_w() as f32 * scale).round() as u32;
                    paint(backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
                }
            }
        }
    };

    // ==================== 设置窗口：状态与回调 ====================
    let sstate: Rc<RefCell<Option<SettingsState>>> = Rc::new(RefCell::new(None));
    let settings_visible = Rc::new(Cell::new(false));
    // 设置窗口帧缓冲 + 绘制（每帧循环检查 dirty）
    let sbuf: RefCell<Vec<Argb>> = RefCell::new(Vec::new());
    let draw_settings = |backend: &mut Box<dyn UiBackend>,
                         sbuf: &RefCell<Vec<Argb>>,
                         sui: &Rc<SettingsWindow>,
                         msw2: &Rc<MinimalSoftwareWindow>,
                         open: bool| {
        if !open {
            return;
        }
        // 与 SettingsShow 同理：按当前 DPI 实时算（用户中途改缩放后，
        // 帧缓冲尺寸要跟着变，否则 BitBlt 会拉伸/截断）
        let scale = ui_scale();
        let w = (640.0 * scale).round() as u32;
        let h = (480.0 * scale).round() as u32;
        if sbuf.borrow().len() != (w * h) as usize {
            *sbuf.borrow_mut() = vec![Argb::default(); (w * h) as usize];
        }
        {
            let mut sb = sbuf.borrow_mut();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                msw2.draw_if_needed(|renderer: &SoftwareRenderer| {
                    renderer.render(&mut sb, w as usize);
                });
            }));
        }
        let sb = sbuf.borrow();
        if !sb.is_empty() {
            if std::env::var_os("HENG_UI_DUMP").is_some() {
                dump_frame(&sb, w, h, &format!("settings-p{}", sui.get_page()));
            }
            backend.blit(1, &sb, w, h);
        }
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
    ui.on_menu_item_clicked({
        let tx = tx.clone();
        move |idx| {
            let _ = tx.send(UiCmd::MenuItem(idx as i32));
        }
    });
    ui.on_menu_dismissed({
        let tx = tx.clone();
        move || {
            let _ = tx.send(UiCmd::MenuDismiss);
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

    // ==================== 设置窗口回调（UI 线程内直执行） ====================
    // 模式：写 patch → redeploy（阻塞 1-2s，可接受）→ 重载状态 → 刷新页面
    sui.on_nav_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        move |p| {
            if let Some(st) = sstate.borrow().as_ref() {
                settings_apply_page(&sui, st, p);
            }
        }
    });
    sui.on_row_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        let tx = tx.clone();
        move |r| {
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            let page = sui.get_page();
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            let settings = Settings::new(&engine);
            let mut changed = false;
            match page {
                0 => {
                    if let Some((id, _)) = st.schemas.get(r as usize) {
                        let mut ids: Vec<String> =
                            st.schemas.iter().map(|(i, _)| i.clone()).collect();
                        let id = id.clone();
                        ids.retain(|x| *x != id);
                        ids.insert(0, id);
                        changed = settings.set_schema_list(&ids).is_ok();
                    }
                }
                1 => {
                    // 页2（外观）行由专属块处理，行点击不再切预设配色
                }
                2 => {
                    let (l, p2, si, pu, fu) =
                        (st.lr_on, st.paging, st.simp, st.punct, st.full);
                    let (l, p2, si, pu, fu) = match r {
                        0 => (!l, p2, si, pu, fu),
                        1 => (l, (p2 + 1) % PAGING_PRESETS.len(), si, pu, fu),
                        2 => (l, p2, (si + 1) % SIMP_PRESETS.len(), pu, fu),
                        3 => (l, p2, si, (pu + 1) % PUNCT_PRESETS.len(), fu),
                        _ => (l, p2, si, pu, (fu + 1) % FULL_PRESETS.len()),
                    };
                    changed = settings
                        .set_bindings(build_bindings(l, p2, si, pu, fu))
                        .is_ok();
                }
                _ => {}
            }
            if changed {
                engine.redeploy();
                let ns = load_settings_state(&engine, &settings, None);
                *sstate.borrow_mut() = Some(ns);
                if let Some(ns) = sstate.borrow().as_ref() {
                    settings_apply_page(&sui, ns, page);
                }
                // 会话已重建：候选窗回初始态
                let _ = tx.send(UiCmd::Hide);
            }
        }
    });
    // —— 自定义配色：点行开取色器 / 点色块即生效 ——
    // 不走 redeploy（配色只喂 Slint 属性，librime 不参与），故不杀会话、
    // 不 Hide 候选窗；写 patch 成功即推属性 + 重绘。
    sui.on_color_row_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        move |idx| {
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            let mut ns = st;
            // idx = -1 是面板上的 ×（收起）
            if idx < 0 {
                ns.picker_open = false;
            } else {
                let i = idx as usize;
                if i >= COLOR_KEYS.len() {
                    return;
                }
                ns.picker_open = true;
                ns.picker_target = i;
            }
            *sstate.borrow_mut() = Some(ns.clone());
            settings_apply_page(&sui, &ns, sui.get_page());
        }
    });
    // HSV 拖拽取色：写 patch + 推属性。
    // 色相变 → SV 方块整块重算（280 格，拖动时每帧都做也够快）。
    sui.on_pick_hsv({
        let sui = sui.clone();
        let sstate = sstate.clone();
        let ui = ui.clone();
        let lsw = lsw.clone();
        let tx = tx.clone();
        move |h, s, v| {
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            let settings = Settings::new(&engine);
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            let t = st.picker_target.min(COLOR_KEYS.len() - 1);
            let hex = hex_of(h, s, v);
            if settings.set_custom_color(COLOR_KEYS[t], &hex).is_err() {
                return;
            }
            let mut ns = st;
            ns.colors[t] = hex;
            *sstate.borrow_mut() = Some(ns.clone());
            apply_theme_colors(&ui, &lsw, &ns.colors);
            // 面板自身的 HSV 状态：SV 方块跟色相走，预览块跟指针走
            sui.set_pick_h(h);
            sui.set_pick_s(s);
            sui.set_pick_v(v);
            sui.set_sv_grid(Rc::new(VecModel::from(sv_grid_colors(h))).into());
            sui.set_pick_preview(hsv_color(h, s / 100.0, v / 100.0));
            sui.set_pick_preview_hex(hex_of(h, s, v).into());
            sui.set_picker_is_transparent(false);
            sui.set_color_vals(Rc::new(VecModel::from(
                ns.colors.iter().map(|c| theme_color(c)).collect::<Vec<_>>(),
            ))
            .into());
            // 候选窗需要重绘才能看到新配色
            let _ = tx.send(UiCmd::Hide);
        }
    });
    sui.on_pick_transparent({
        let sui = sui.clone();
        let sstate = sstate.clone();
        let ui = ui.clone();
        let lsw = lsw.clone();
        let tx = tx.clone();
        move || {
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            let settings = Settings::new(&engine);
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            let t = st.picker_target.min(COLOR_KEYS.len() - 1);
            // 「不填充」；仅选中背景允许
            if !COLOR_ALLOW_TRANSPARENT[t] {
                return;
            }
            if settings
                .set_custom_color(COLOR_KEYS[t], TRANSPARENT_MARK)
                .is_err()
            {
                return;
            }
            let mut ns = st;
            ns.colors[t] = TRANSPARENT_MARK.to_string();
            *sstate.borrow_mut() = Some(ns.clone());
            apply_theme_colors(&ui, &lsw, &ns.colors);
            sui.set_picker_is_transparent(true);
            sui.set_pick_preview_hex(TRANSPARENT_MARK.into());
            sui.set_color_vals(Rc::new(VecModel::from(
                ns.colors.iter().map(|c| theme_color(c)).collect::<Vec<_>>(),
            ))
            .into());
            let _ = tx.send(UiCmd::Hide);
        }
    });
    sui.on_toggle_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        let current = current.clone();
        let tx = tx.clone();
        move |tag| {
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            let settings = Settings::new(&engine);
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            match tag.as_str() {
                "inline" => {
                    if settings.set_inline_preedit(!st.inline).is_ok() {
                        // 同上：样式开关不走 redeploy、不重读部署缓存
                        let mut ns = st.clone();
                        ns.inline = !st.inline;
                        *sstate.borrow_mut() = Some(ns.clone());
                        settings_apply_page(&sui, &ns, sui.get_page());
                        let _ = tx.send(UiCmd::Hide);
                    }
                }
                "punct" => {
                    // 会话内即时生效（librime 自行持久化开关状态）
                    if let Some(sid) = *current.borrow() {
                        if engine.set_option(sid, "ascii_punct", !st.ascii_punct).is_ok() {
                            let on = engine.get_option(sid, "ascii_punct").unwrap_or(false);
                            sui.set_ascii_punct(on);
                            let mut ns = sstate.borrow().as_ref().map(|s| s.clone());
                            if let Some(ref mut ns) = ns {
                                ns.ascii_punct = on;
                            }
                            *sstate.borrow_mut() = ns;
                        }
                    }
                }
                _ => {}
            }
        }
    });
    sui.on_scale_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        let ui_c = ui.clone();
        let lsw_c = lsw.clone();
        let tx_c = tx.clone();
        move |ratio: f32| {
            let Ok(engine) = engine() else { return };
            let _guard = OP_LOCK.lock().unwrap();
            let settings = Settings::new(&engine);
            let Some(st) = sstate.borrow().as_ref().map(|s| s.clone()) else { return };
            // 点击位置 → 7 档刻度（0-6），字号 = 14 + 档位（默认 16 = 第 3 档）
            let level = ((ratio * 6.0) + 0.5).floor().clamp(0.0, 6.0) as i32;
            let n = 14 + level;
            if n != st.font && settings.set_font_point_size(n as i64).is_ok() {
                // 样式改动不走 redeploy（会作废活动会话）；字号实时应用到
                // 候选窗与字母区，容器尺寸（格高/条高/字母区高）一起缩放
                ui_c.set_cand_font(n);
                lsw_c.set_cand_font(n);
                let k = n as f32 / 14.0;
                ui_c.set_font_scale(k);
                lsw_c.set_font_scale(k);
                let sc = ui_scale();
                MSW.with(|w| {
                    w.set_size(PhysicalSize {
                        width: (lay_panel_w() as f32 * sc).round() as u32,
                        height: ((lay_bar_h() as f32) * sc).round() as u32,
                    })
                });
                MSW3.with(|w| {
                    w.set_size(PhysicalSize {
                        width: (lay_panel_w() as f32 * sc).round() as u32,
                        height: ((lay_letters_h() as f32) * sc).round() as u32,
                    })
                });
                let mut ns = st.clone();
                ns.font = n;
                *sstate.borrow_mut() = Some(ns.clone());
                settings_apply_page(&sui, &ns, sui.get_page());
                // 收起当前条：下一次按键以新尺寸重新布局绘制
                let _ = tx_c.send(UiCmd::Hide);
            }
        }
    });
    sui.on_action_clicked({
        let sui = sui.clone();
        let sstate = sstate.clone();
        move |tag| {
            let Ok(engine) = engine() else { return };
            let settings = Settings::new(&engine);
            match tag.as_str() {
                "redeploy" => {
                    let _guard = OP_LOCK.lock().unwrap();
                    engine.redeploy();
                    let ns = load_settings_state(&engine, &settings, None);
                    *sstate.borrow_mut() = Some(ns);
                    if let Some(ns) = sstate.borrow().as_ref() {
                        settings_apply_page(&sui, ns, sui.get_page());
                    }
                }
                "open-user" => open_in_explorer(settings.user_dir()),
                "open-log" => open_in_explorer(&std::env::temp_dir()),
                "backup" => {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let dst = settings.user_dir().join(format!("backup-userdb-{secs}"));
                    let _ = std::fs::create_dir_all(&dst);
                    if let Ok(rd) = std::fs::read_dir(settings.user_dir()) {
                        for e in rd.flatten() {
                            let name = e.file_name().to_string_lossy().into_owned();
                            if name.ends_with(".userdb") {
                                let _ = copy_dir(&e.path(), &dst.join(&name));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    });
    sui.on_close_clicked({
        let tx = tx.clone();
        move || {
            let _ = tx.send(UiCmd::SettingsHide);
        }
    });

    loop {
        // 0. 动画/定时器心跳：自定义平台必须显式驱动，否则所有 animate
        //    （设置页开关白点等）永远停在初始值
        slint::platform::update_timers_and_animations();
        // 1. 命令
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                UiCmd::Sync { rime_id, x, y, top } => {
                    // 气泡显示期间来了输入 → 立即退出气泡态，走正常候选渲染
                    if hint_mode.get() != 0 {
                        hint_mode.set(0);
                        ui.set_hint(0);
                        ui.set_hint_only(false);
                        hint_deadline.set(None);
                    }
                    *current.borrow_mut() = Some(rime_id);
                    let Ok(engine) = engine() else { continue };
                    let snapshot = {
                        let _guard = OP_LOCK.lock().unwrap();
                        engine.get_context(rime_id)
                    };
                    if let Ok(snapshot) = snapshot {
                        last_x.set(x);
                        last_y.set(y);
                        // 行顶合法范围：[y-400, y]（行高 ≤400px；防外壳传脏值）
                        last_caret_top.set(top.clamp(y - 400, y));
                        // 输入串变化才收起面板（↓/↑ 导航不改 preedit，面板保持）
                        let input_changed = *last_preedit.borrow() != snapshot.preedit;
                        *last_preedit.borrow_mut() = snapshot.preedit.clone();
                        if snapshot.candidates.is_empty() {
                            backend.set_mapped(0, false);
                            backend.set_mapped(2, false);
                            *visible.borrow_mut() = false;
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            flip.set(false);
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
                            set_bar_cells(
                                &ui,
                                &engine,
                                rime_id,
                                &snapshot,
                                &bar_base,
                                &bar_items,
                                &panel_hl,
                            );
                            let w = (lay_panel_w() as f32 * scale).round() as u32;
                            ui.set_expanded(false);
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            flip.set(false);
                            *panel.borrow_mut() = None;
                            paint(&mut backend, w, bar_h_now(), x, y, true);
                        }
                    }
                }
                UiCmd::Hide => {
                    backend.set_mapped(0, false);
                    backend.set_mapped(2, false);
                    *visible.borrow_mut() = false;
                    hint_mode.set(0);
                    ui.set_hint(0);
                    ui.set_hint_only(false);
                    hint_deadline.set(None);
                }
                UiCmd::MenuItem(index) => {
                    // ☰ 菜单项：点击后整个预览窗关闭（不回横条，打字才回）
                    menu_open.set(false);
                    UI_MENU_OPEN.store(false, Ordering::Relaxed);
                    ui.set_menu_open(false);
                    backend.set_mapped(0, false);
                    backend.set_mapped(2, false);
                    *visible.borrow_mut() = false;
                    if index == 2 {
                        // 设置：弹出设置窗口（独立窗）
                        let _ = tx.send(UiCmd::SettingsShow(1));
                    }
                }
                UiCmd::MenuDismiss => {
                    // 菜单空白处点击：整个预览窗关闭
                    menu_open.set(false);
                    UI_MENU_OPEN.store(false, Ordering::Relaxed);
                    ui.set_menu_open(false);
                    backend.set_mapped(0, false);
                    backend.set_mapped(2, false);
                    *visible.borrow_mut() = false;
                }
                UiCmd::SettingsShow(page) => {
                    let Ok(engine) = engine() else { continue };
                    if !settings_visible.get() {
                        let st = {
                            let _guard = OP_LOCK.lock().unwrap();
                            let cur = *current.borrow();
                            load_settings_state(&engine, &Settings::new(&engine), cur)
                        };
                        *sstate.borrow_mut() = Some(st);
                        let (sw, sh) = screen_size();
                        // 尺寸按**显示这一刻**的 DPI 算，不复用启动时捕获的 scale：
                        // 用户可能在会话期间改过系统缩放，或把输入法跑在缩放不同的屏上。
                        // 候选窗尺寸由每次 Sync 的 paint 重算，无需在此处理。
                        let sc = ui_scale();
                        let w = (640.0 * sc).round() as u32;
                        let h = (480.0 * sc).round() as u32;
                        backend.configure(1, (sw - w as i32) / 2, (sh - h as i32) / 2, w, h);
                        backend.set_mapped(1, true);
                        let _ = sui.show();
                        settings_visible.set(true);
                    }
                    if let Some(st) = sstate.borrow().as_ref() {
                        settings_apply_page(&sui, st, page.clamp(0, 5));
                    }
                }
                UiCmd::SettingsHide => {
                    if settings_visible.get() {
                        backend.set_mapped(1, false);
                        let _ = sui.hide();
                        settings_visible.set(false);
                    }
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
                        let (real, _page_size, snapshot) = {
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
                        ui.set_panel_width(lay_panel_w());
                        let content_h = lay_flow_top() + (rows - 1) * lay_row_h() + lay_cell_h() + lay_px(4.0);
                        let h_logical =
                            (lay_px(4.0) + PANEL_VISIBLE_ROWS * lay_row_h() + lay_px(4.0)).max(lay_bar_h());
                        ui.set_content_height(content_h);
                        ui.set_panel_height(h_logical);
                        last_panel_h.set((h_logical as f32 * scale).round() as u32);
                        // 展开方向：光标下方放不下 → 向上展开（微信同款，
                        // 面板底边贴输入行上方，整体悬在输入行上面）
                        let panel_h_phys = last_panel_h.get();
                        match work_area_at(last_x.get(), last_y.get()) {
                            Some((top, bottom)) if bottom > top && bottom > 0 => {
                                if last_y.get() + panel_h_phys as i32 + 8 > bottom {
                                    flip.set(true);
                                    flip_top.set(top);
                                } else {
                                    flip.set(false);
                                }
                            }
                            _ => flip.set(false),
                        }
                        ui_log(&format!(
                            "expand: flip={} panel_bottom_y={} panel_h={} work_area={:?}",
                            flip.get(),
                            last_y.get(),
                            panel_h_phys,
                            work_area_at(last_x.get(), last_y.get())
                        ));
                        ui.set_expanded(true);
                        UI_EXPANDED.store(true, Ordering::Relaxed);
                        menu_open.set(false);
                        UI_MENU_OPEN.store(false, Ordering::Relaxed);
                        ui.set_menu_open(false);
                        UI_HL.store(hl_global, Ordering::Relaxed);
                        UI_TOTAL.store(total, Ordering::Relaxed);
                        let w = (lay_panel_w() as f32 * scale).round() as u32;
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
                        let w = (lay_panel_w() as f32 * scale).round() as u32;
                        paint(&mut backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
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
                    let (cells, _rows) = if in_panel {
                        build_grid_cells(&items, new_hl)
                    } else {
                        build_flow_cells(&items, new_hl)
                    };
                    let hl_y = cells.iter().find(|c| c.hl).map(|c| c.y).unwrap_or(0);
                    ui.set_all_cells(ModelRc::new(VecModel::from(cells)));
                    ui.set_hl_y(hl_y);
                    let h = if in_panel { last_panel_h.get() } else { bar_h_now() };
                    paint(
                        &mut backend,
                        (lay_panel_w() as f32 * scale).round() as u32,
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
                            flip.set(false);
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
                            let w = (lay_panel_w() as f32 * scale).round() as u32;
                            paint(&mut backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
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
                        (lay_panel_w() as f32 * scale).round() as u32,
                        last_panel_h.get(),
                        last_x.get(),
                        last_y.get(),
                        false,
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
                            // 挂到对应外壳会话句柄（反向查 handle），通知外壳上屏
                            if let Some(h) = SESSIONS.handle_of(rime_id) {
                                emit_commit(h, &text);
                            }
                        }
                    }
                    // 刷新横条（选词后组合串可能仍在：继续输入下一个字母）
                    if let Ok(snapshot) = engine.get_context(rime_id) {
                        if snapshot.candidates.is_empty() {
                            backend.set_mapped(0, false);
                            backend.set_mapped(2, false);
                            *visible.borrow_mut() = false;
                            expanded.set(false);
                            UI_EXPANDED.store(false, Ordering::Relaxed);
                            flip.set(false);
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
                            let w = (lay_panel_w() as f32 * scale).round() as u32;
                            paint(&mut backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
                        }
                    }
                }
                UiCmd::MenuOpen => {
                    // ☰ 菜单（占位）：面板长高显示 符号/常用语/设置，功能后续迭代
                    menu_open.set(true);
                    UI_MENU_OPEN.store(true, Ordering::Relaxed);
                    ui.set_menu_open(true);
                    ui.set_menu_height(lay_menu_h());
                    let h = (lay_menu_h() as f32 * scale).round() as u32;
                    paint(
                        &mut backend,
                        (lay_panel_w() as f32 * scale).round() as u32,
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
                        (lay_panel_w() as f32 * scale).round() as u32,
                        bar_h_now(),
                        last_x.get(),
                        last_y.get(),
                        true,
                    );
                }
                UiCmd::SelectHL => {
                    // 空格/回车：选中面板当前高亮项
                    select_panel_index(&mut backend, panel_hl.get().max(0) as usize);
                }
                UiCmd::SelectByRowSeq(seq) => {
                    // 展开面板数字键：按「行内序号」选词（每行序号从 1 重排，
                    // 而 items 是全局连续索引 → 必须按行换算，否则任何行都选
                    // 到第一行的第 N 个）。
                    let seq = seq.max(1) as usize;
                    // 以当前高亮所在行为锚：用户在第几行按数字，就选那一行
                    let snap_ref = panel.borrow();
                    let Some(snap) = snap_ref.as_ref() else { continue };
                    let hl_idx = panel_hl.get().max(0) as usize;
                    let Some(anchor_y) = snap.geo.get(hl_idx).map(|g| g.1) else {
                        continue;
                    };
                    // 锚点行首项 = 该 y 的第一个出现位置
                    let row_start = snap.geo.iter().position(|g| g.1 == anchor_y).unwrap_or(0);
                    let target = row_start + seq - 1;
                    let total = snap.items.len();
                    let real_count = snap.real_count;
                    drop(snap_ref);
                    if target >= total {
                        continue;  // 该行没这么长
                    }
                    // 同音词（真实候选之后）走清串直上屏，不能按 global 选
                    if target >= real_count {
                        continue;
                    }
                    panel_hl.set(target as i32);
                    UI_HL.store(target as i32, Ordering::Relaxed);
                    select_panel_index(&mut backend, target);
                }
                UiCmd::ModeHint { ascii, anchor } => {
                    // 中英切换瞬态气泡：窗口独占切到胶囊形态（124x48 物理像素由
                    // hint-only 驱动），锚在光标行顶上方；到期恢复，不动候选窗状态
                    let hv = if ascii { 2 } else { 1 };
                    hint_mode.set(hv);
                    ui.set_hint(hv);
                    ui.set_hint_only(true);
                    if let Some((x, _bottom, top)) = anchor {
                        // 外壳传入的坐标始终最新（UpdateInputPos 持续上报），
                        // 顺带刷新记忆值，到期恢复/后续 fallback 也在正确位置
                        last_x.set(x);
                        last_y.set(_bottom);
                        last_caret_top.set(top.clamp(_bottom - 400, _bottom));
                    }
                    let bw = (36.0 * scale).round() as u32;
                    let bh = (36.0 * scale).round() as u32;
                    let (sw, sh) = screen_size();
                    // 从未获得过光标位置（last_caret_top 为 0 = 外壳未上报过）
                    // → 兜底屏幕右下角（通知风格），否则锚光标行顶上方
                    let (hx, hy) = if last_caret_top.get() <= 0 {
                        (sw - bw as i32 - 24, (sh - bh as i32 - 96).max(8))
                    } else {
                        (
                            (last_x.get() + 16).max(8).min(sw - bw as i32 - 8),
                            (last_caret_top.get() - bh as i32 - 8).max(8),
                        )
                    };
                    paint(&mut backend, bw, bh, hx, hy, true);
                    hint_deadline
                        .set(Some(std::time::Instant::now() + Duration::from_millis(900)));
                }
                UiCmd::SetExpanded(v) => {
                    if v || !*visible.borrow() {
                        continue;
                    }
                    // 键盘 ↑（面板第一行）触发的收起
                    ui.set_expanded(false);
                    expanded.set(false);
                    UI_EXPANDED.store(false, Ordering::Relaxed);
                    flip.set(false);
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
                    let w = (lay_panel_w() as f32 * scale).round() as u32;
                    paint(&mut backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
                }
            }
        }

        // 2. 平台事件（仅窗口可见时有意义，但轮询保持廉价）
        //    物理像素坐标 → 逻辑像素（Slint 指针事件约定逻辑坐标）
        let tx_close = tx.clone();
        backend.poll_events(&mut |tag, event| {
            if tag == 1 {
                // 设置窗口事件
                if matches!(event, WindowEvent::CloseRequested) {
                    let _ = tx_close.send(UiCmd::SettingsHide);
                    return;
                }
                if !settings_visible.get() {
                    return;
                }
                let event = scale_pointer_event(event, scale);
                msw2.window().dispatch_event(event);
                return;
            }
            if !*visible.borrow() {
                return;
            }
            let event = scale_pointer_event(event, scale);
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
            backend.blit(0, &buf, w, h);
        });
        // 3b. 设置窗口渲染（dirty 时才实际绘制 + blit）
        draw_settings(&mut backend, &sbuf, &sui, &msw2, settings_visible.get());

        // 3c. 气泡到期恢复：有输入上下文 → 重建横条；否则藏窗（不误杀打字中的候选）
        if let Some(deadline) = hint_deadline.get() {
            if std::time::Instant::now() >= deadline {
                hint_deadline.set(None);
                hint_mode.set(0);
                ui.set_hint(0);
                ui.set_hint_only(false);
                let mut restore = false;
                if let (Some(rime_id), Ok(engine)) = (*current.borrow(), engine()) {
                    let _guard = OP_LOCK.lock().unwrap();
                    if let Ok(snapshot) = engine.get_context(rime_id) {
                        if !snapshot.candidates.is_empty() {
                            restore = true;
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
                if restore {
                    let w = (lay_panel_w() as f32 * scale).round() as u32;
                    paint(&mut backend, w, bar_h_now(), last_x.get(), last_y.get(), true);
                } else {
                    backend.set_mapped(0, false);
                    backend.set_mapped(2, false);
                    *visible.borrow_mut() = false;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(8));
    }
}

/// 指针事件物理像素 → 逻辑像素（其余事件原样）
fn scale_pointer_event(event: WindowEvent, scale: f32) -> WindowEvent {
    match event {
        WindowEvent::PointerMoved { position } => WindowEvent::PointerMoved {
            position: LogicalPosition::new(position.x / scale, position.y / scale),
        },
        WindowEvent::PointerPressed { position, button } => WindowEvent::PointerPressed {
            position: LogicalPosition::new(position.x / scale, position.y / scale),
            button,
        },
        WindowEvent::PointerReleased { position, button } => WindowEvent::PointerReleased {
            position: LogicalPosition::new(position.x / scale, position.y / scale),
            button,
        },
        WindowEvent::PointerScrolled {
            position,
            delta_x,
            delta_y,
        } => WindowEvent::PointerScrolled {
            position: LogicalPosition::new(position.x / scale, position.y / scale),
            delta_x,
            delta_y,
        },
        other => other,
    }
}

// ============================ 设置窗口（S0-S3） ============================

const SET_NAV: [&str; 6] = ["输入方案", "外观", "快捷键", "标点符号", "词库", "关于"];

/// 设置页运行态（打开时从 staging + patch 加载一次，改动后重载）
#[derive(Clone)]
struct SettingsState {
    /// 自定义配色四项，顺序 = [容器背景, 选中背景, 选中字体, 候选字体]；
    /// 每项为 `"#rrggbb"` 或 `"transparent"`（四项都允许透明）
    colors: [String; 4],
    /// 取色面板是否展开 + 正在编辑第几项（纯 UI 态，不持久化）
    picker_open: bool,
    picker_target: usize,
    inline: bool,
    font: i32,
    schemas: Vec<(String, String)>, // id, name（顺序 = 启用序，首个 = 默认）
    lr_on: bool,
    paging: usize,
    simp: usize,
    punct: usize,
    full: usize,
    ascii_punct: bool,
    about: String,
    user_files: Vec<(String, u64)>,
}

// ==================== 自定义配色（替代原 preset_color_schemes 列表） ====================

/// 四项可配色的键名（写 weasel.custom.yaml style/heng_<key>）
const COLOR_KEYS: [&str; 4] = [
    "container_back",
    "hilite_back",
    "hilite_text",
    "cand_text",
];
/// 四项的默认色（与原硬编码一致；未配时回退）
const COLOR_DEFAULTS: [&str; 4] = ["#f7f8fa", "#2164f1", "#ffffff", "#1f2328"];
/// 设置页显示名
const COLOR_LABELS: [&str; 4] = [
    "主预览容器背景色",
    "选中区域背景色",
    "选中区域字体色",
    "候选词字体色",
];
/// 四项全部允许「不填充」。容器背景透明会透出宿主窗口内容（文字叠文字，
/// 看着脏但不崩）；字体透明则是用户自己的选择 —— 配色是自由项，不设闸门。
const COLOR_ALLOW_TRANSPARENT: [bool; 4] = [true; 4];

// ==================== HSV 取色器（任意颜色，纯鼠标拖拽） ====================

/// 色相条分段数（越多越平滑；36 段 = 每段 10°）。
/// 注意：slint! 侧色相条格子写的是硬编码 `348px / 36`，改这里要同步改那里。
const HUE_SEGMENTS: usize = 36;
/// SV 方块列数（横向 = 饱和度）。348px / 28列 ≈ 每格 12px，软件渲染下过渡足够平滑
const SV_COLS: usize = 28;
/// SV 方块行数（纵向 = 明度）。150px / 18 行 ≈ 每格 8px
const SV_ROWS: usize = 18;

/// HSV → RGB。h: 0-360，s/v: 0-1
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

fn hsv_color(h: f32, s: f32, v: f32) -> slint::Color {
    let (r, g, b) = hsv_to_rgb(h, s, v);
    slint::Color::from_argb_u8(
        255,
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

/// hsv（s/v 为 0-100）→ "#rrggbb"
fn hex_of(h: f32, s: f32, v: f32) -> String {
    let (r, g, b) = hsv_to_rgb(h, s / 100.0, v / 100.0);
    format!(
        "#{:02x}{:02x}{:02x}",
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8
    )
}

/// 色相条各段颜色（固定，不随 s/v 变）
fn hue_bar_colors() -> Vec<slint::Color> {
    (0..HUE_SEGMENTS)
        .map(|i| {
            let h = i as f32 * 360.0 / HUE_SEGMENTS as f32 + 360.0 / HUE_SEGMENTS as f32 / 2.0;
            hsv_color(h, 1.0, 1.0)
        })
        .collect()
}

/// SV 方块各格颜色（随色相变 —— 每次色相改变都要整块重算）
fn sv_grid_colors(h: f32) -> Vec<slint::Color> {
    let mut out = Vec::with_capacity(SV_COLS * SV_ROWS);
    for row in 0..SV_ROWS {
        // 行 0 = 明度 100%（顶部），行末 = 明度 0%（黑）
        let v = 1.0 - row as f32 / (SV_ROWS - 1) as f32;
        for col in 0..SV_COLS {
            let s = col as f32 / (SV_COLS - 1) as f32;
            out.push(hsv_color(h, s, v));
        }
    }
    out
}

/// "#rrggbb" → HSV（打开面板时定位指示圈用；非 hex/透明回退蓝）
fn hex_to_hsv(s: &str) -> (f32, f32, f32) {
    let hex = s.trim().strip_prefix('#').unwrap_or("");
    if hex.len() != 6 {
        return (216.0, 0.887, 0.945); // #2164f1
    }
    let v = u32::from_str_radix(hex, 16).unwrap_or(0);
    let (r, g, b) = (
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    let sat = if max <= 0.0 { 0.0 } else { d / max };
    (h, sat * 100.0, max * 100.0)
}

/// 读四项自定义配色（源补丁直读，缺失回退默认）
fn read_theme_colors(settings: &Settings) -> [String; 4] {
    let mut out: [String; 4] = COLOR_DEFAULTS.map(|s| s.to_string());
    for (i, key) in COLOR_KEYS.iter().enumerate() {
        if let Some(v) = settings
            .get_path(PATCH_WEASEL, &format!("style/heng_{key}"))
            .and_then(|v| v.as_str().map(|s| s.to_string()))
        {
            let v = v.trim().to_string();
            if v == TRANSPARENT_MARK || is_hex_color(&v) {
                out[i] = v;
            }
        }
    }
    out
}

/// 「不填充」保留字
const TRANSPARENT_MARK: &str = "transparent";

/// `"#rgb"` / `"#rrggbb"` → true
fn is_hex_color(s: &str) -> bool {
    let b = s.strip_prefix('#').unwrap_or("");
    matches!(b.len(), 3 | 6) && b.chars().all(|c| c.is_ascii_hexdigit())
}

/// 把四项配色推给候选窗 + 字母区（内嵌关时字母窗也要跟，否则字母区仍白底）
fn apply_theme_colors(
    ui: &CandWindow,
    lsw: &LettersWindow,
    colors: &[String; 4],
) {
    let container = theme_color(&colors[0]);
    let hl_back = theme_color(&colors[1]);
    let hl_text = theme_color(&colors[2]);
    let cand_text = theme_color(&colors[3]);
    ui.set_col_container(container);
    ui.set_col_hilite_back(hl_back);
    ui.set_col_hilite_text(hl_text);
    ui.set_col_cand_text(cand_text);
    lsw.set_col_container(container);
    lsw.set_col_cand_text(cand_text);
}

fn load_settings_state(
    engine: &Engine,
    settings: &Settings,
    cur_session: Option<RimeSessionId>,
) -> SettingsState {
    let mut st = SettingsState {
        colors: COLOR_DEFAULTS.map(|s| s.to_string()),
        picker_open: false,
        picker_target: 0,
        inline: true,
        font: 14,
        schemas: vec![],
        lr_on: false,
        paging: 0,
        simp: 0,
        punct: 0,
        full: 0,
        ascii_punct: false,
        about: String::new(),
        user_files: vec![],
    };
    if let Ok(mut cfg) = engine.config_open("weasel") {
        st.inline = engine
            .config_get_bool(&cfg, "style/inline_preedit")
            .unwrap_or(false); // Linux 默认关
        st.font = engine
            .config_get_int(&cfg, "style/font_point_size")
            .unwrap_or(16)
            .clamp(14, 20);
        // 样式开关读源补丁覆盖：config_open 读的是部署产物（user/build/），
        // 直接写 weasel.custom.yaml 后未重部署时仍是旧值；样式不参与
        // librime 部署语义，源补丁即真相（避免为显示而 redeploy 杀会话）
        if let Some(v) = settings
            .get_path(PATCH_WEASEL, "style/inline_preedit")
            .and_then(|v| v.as_bool())
        {
            st.inline = v;
        }
        if let Some(v) = settings
            .get_path(PATCH_WEASEL, "style/font_point_size")
            .and_then(|v| v.as_i64())
        {
            st.font = v.clamp(10, 32) as i32;
        }
        // 自定义配色：源补丁直读（样式不参与 librime 部署语义，同 inline/font）
        st.colors = read_theme_colors(settings);
        engine.config_close(&mut cfg);
    }
    // 调试钩子：HENG_UI_PICKER=<0-3> 让取色面板默认展开（沙箱无交互桌面，
    // 无法模拟点击验证面板视觉；真机调配色时也可用）
    if let Ok(v) = std::env::var("HENG_UI_PICKER") {
        if let Ok(i) = v.trim().parse::<usize>() {
            if i < COLOR_KEYS.len() {
                st.picker_open = true;
                st.picker_target = i;
            }
        }
    }
    st.schemas = engine.schema_list();
    let (lr_on, paging, simp, punct, full) = parse_bindings(settings.get_bindings().as_ref());
    st.lr_on = lr_on;
    st.paging = paging;
    st.simp = simp;
    st.punct = punct;
    st.full = full;
    if let Some(sid) = cur_session {
        st.ascii_punct = engine.get_option(sid, "ascii_punct").unwrap_or(false);
    }
    st.about = format!(
        "HengIME 「衡」 v{}\nABI v{}（本设置窗口运行于 core 内部 UI 线程）\n\n用户目录：{}",
        engine.version().unwrap_or_default(),
        crate::capi::HENG_ABI_VERSION,
        settings.user_dir().display(),
    );
    st.user_files = settings.list_user_files();
    st
}

fn empty_row(title: &str, sub: &str, active: bool) -> SetRow {
    SetRow {
        title: title.into(),
        sub: sub.into(),
        active,
    }
}

/// 配置色值 → slint::Color：`"transparent"` → alpha=0；`"#rgb"`/`"#rrggbb"` → 不透明
fn theme_color(v: &str) -> slint::Color {
    let s = v.trim();
    if s == TRANSPARENT_MARK {
        return slint::Color::from_argb_u8(0, 0, 0, 0);
    }
    let hex = s.strip_prefix('#').unwrap_or("");
    let (r, g, b) = match hex.len() {
        3 => {
            // #abc → #aabbcc
            let d: Vec<u8> = hex
                .chars()
                .filter_map(|c| c.to_digit(16).map(|d| d as u8))
                .collect();
            if d.len() != 3 {
                (0u8, 0u8, 0u8)
            } else {
                (d[0] * 17, d[1] * 17, d[2] * 17)
            }
        }
        6 => {
            let v = u32::from_str_radix(hex, 16).unwrap_or(0);
            (
                ((v >> 16) & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                (v & 0xff) as u8,
            )
        }
        _ => (0, 0, 0),
    };
    slint::Color::from_argb_u8(255, r, g, b)
}

/// 把当前状态 + 页码写进 Slint 属性
fn settings_apply_page(sui: &SettingsWindow, st: &SettingsState, page: i32) {
    sui.set_page(page);
    let mut rows: Vec<SetRow> = Vec::new();
    let head: String;
    match page {
        0 => {
            head = "输入方案 · 点击行设为默认方案（自动重新部署）".into();
            for (i, (id, name)) in st.schemas.iter().enumerate() {
                rows.push(empty_row(name, id, i == 0));
            }
        }
        1 => {
            head = "外观 · 配色点色块即生效（不重启、不重部署）".into();
            // 页2 的配色行由 slint! 里的专属块渲染（预览 + 四项 + 色板），
            // 不走通用 page-rows
        }
        2 => {
            head = "快捷键 · 点击行切换预设（自动重新部署）".into();
            rows.push(empty_row(
                "左右键行为",
                LR_PRESETS[st.lr_on as usize].0,
                false,
            ));
            rows.push(empty_row("翻页键", PAGING_PRESETS[st.paging].0, false));
            rows.push(empty_row("简繁切换键", SIMP_PRESETS[st.simp].0, false));
            rows.push(empty_row("标点切换键", PUNCT_PRESETS[st.punct].0, false));
            rows.push(empty_row("全半角切换键", FULL_PRESETS[st.full].0, false));
        }
        3 => {
            head = "标点符号 · 拨杆立即生效".into();
        }
        4 => {
            head = "词库 · 用户目录文件".into();
            for (name, size) in &st.user_files {
                rows.push(empty_row(
                    name,
                    &format!("{:.1} KB", *size as f64 / 1024.0),
                    false,
                ));
            }
        }
        _ => {
            head = "关于".into();
        }
    }
    sui.set_head_text(head.into());
    sui.set_page_rows(Rc::new(VecModel::from(rows)).into());
    sui.set_font_size(st.font);
    sui.set_inline_preedit(st.inline);
    sui.set_ascii_punct(st.ascii_punct);
    sui.set_about_lines(st.about.clone().into());
    // 配色四项 + 色板 + 取色面板状态
    sui.set_color_vals(Rc::new(VecModel::from(
        st.colors.iter().map(|c| theme_color(c)).collect::<Vec<_>>(),
    ))
    .into());
    sui.set_color_labels(Rc::new(VecModel::from(
        COLOR_LABELS.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>(),
    ))
    .into());
    // —— HSV 取色器状态 ——
    // 色相条固定；SV 方块随色相整块重算（28×18 = 504 格，纯 CPU 计算可忽略）
    sui.set_sv_cols(SV_COLS as i32);
    sui.set_sv_rows(SV_ROWS as i32);
    sui.set_hue_bar(Rc::new(VecModel::from(hue_bar_colors())).into());
    // 指示圈位置由「目标项当前色」反解，不另存状态 —— 面板收起时不会残留脏值
    let t = st.picker_target.min(COLOR_KEYS.len() - 1);
    let is_transparent = st.colors[t] == TRANSPARENT_MARK;
    let (h, s, v) = if is_transparent {
        // 已是透明态：保留上次取色位置（用默认蓝定位），但显示为不填态
        (216.0, 88.7, 94.5)
    } else {
        hex_to_hsv(&st.colors[t])
    };
    sui.set_pick_h(h);
    sui.set_pick_s(s);
    sui.set_pick_v(v);
    sui.set_sv_grid(Rc::new(VecModel::from(sv_grid_colors(h))).into());
    sui.set_pick_preview(hsv_color(h, s / 100.0, v / 100.0));
    sui.set_pick_preview_hex(if is_transparent {
        TRANSPARENT_MARK.into()
    } else {
        hex_of(h, s, v).into()
    });
    sui.set_picker_is_transparent(is_transparent);
    sui.set_picker_open(st.picker_open);
    sui.set_picker_target(st.picker_target as i32);
    sui.set_picker_allow_transparent(COLOR_ALLOW_TRANSPARENT[t]);
}

fn screen_size() -> (i32, i32) {
    #[cfg(target_os = "windows")]
    unsafe {
        (
            windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            ),
            windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows_sys::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
            ),
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        (1920, 1080)
    }
}

fn open_in_explorer(p: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(p).spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    }
}

/// 递归复制目录（备份用户词典用）
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let ty = e.file_type()?;
        let target = dst.join(e.file_name());
        if ty.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_parsing() {
        assert!(is_hex_color("#fff"));
        assert!(is_hex_color("#f7f8fa"));
        assert!(!is_hex_color("#ff"));
        assert!(!is_hex_color("f7f8fa"));
        assert!(!is_hex_color("transparent"));
        // #rgb 展开：f→ff
        let c = theme_color("#abc");
        assert_eq!((c.red(), c.green(), c.blue()), (0xaa, 0xbb, 0xcc));
        assert_eq!(c.alpha(), 255u8);
        // 透明保留字 → alpha=0
        let t = theme_color(TRANSPARENT_MARK);
        assert_eq!(t.alpha(), 0u8);
        // 非法值按黑（不崩）
        let bad = theme_color("nonsense");
        assert_eq!((bad.red(), bad.green(), bad.blue()), (0u8, 0u8, 0u8));
    }

    #[test]
    fn theme_color_read_defaults_and_ignore_garbage() {
        let dir = std::env::temp_dir().join("heng-theme-test");
        std::fs::create_dir_all(&dir).unwrap();
        let s = Settings::with_dir(dir.clone());
        // 无 patch → 全默认
        let c = read_theme_colors(&s);
        assert_eq!(c, COLOR_DEFAULTS.map(|x| x.to_string()));
        // 合法 hex + transparent 都接受
        s.set_custom_color("container_back", "#222222").unwrap();
        s.set_custom_color("hilite_back", TRANSPARENT_MARK).unwrap();
        let c = read_theme_colors(&s);
        assert_eq!(c[0], "#222222");
        assert_eq!(c[1], TRANSPARENT_MARK);
        // 非法值被忽略（回落默认），不崩
        s.set_custom_color("cand_text", "definitely-not-a-color").unwrap();
        let c = read_theme_colors(&s);
        assert_eq!(c[3], COLOR_DEFAULTS[3]);
        std::fs::remove_file(dir.join(PATCH_WEASEL)).unwrap();
    }

    #[test]
    fn all_four_colors_allow_transparent() {
        // 四项都允许「不填」—— 配色是自由项，不设闸门
        assert_eq!(COLOR_ALLOW_TRANSPARENT, [true; 4]);
        // 透明保留字不会被当成非法 hex 拒掉
        assert!(!is_hex_color(TRANSPARENT_MARK));
        assert_eq!(theme_color(TRANSPARENT_MARK).alpha(), 0u8);
        // 每项写「transparent」都要能落盘并读回（模拟四项依次设透明）
        let dir = std::env::temp_dir().join("heng-trans-all-test");
        std::fs::create_dir_all(&dir).unwrap();
        let s = Settings::with_dir(dir.clone());
        for key in COLOR_KEYS {
            s.set_custom_color(key, TRANSPARENT_MARK).unwrap();
        }
        let c = read_theme_colors(&s);
        for v in c.iter() {
            assert_eq!(v, TRANSPARENT_MARK);
        }
        std::fs::remove_file(dir.join(PATCH_WEASEL)).unwrap();
    }

    #[test]
    fn hsv_conversion_roundtrip() {
        // hex → hsv → rgb 必须回到原色（±1/255 容差）
        for hex in ["#000000", "#ffffff", "#ff0000", "#00ff00", "#0000ff", "#2164f1"] {
            let (h, s, v) = hex_to_hsv(hex);
            let (r, g, b) = hsv_to_rgb(h, s / 100.0, v / 100.0);
            let got = (
                (r * 255.0).round() as u8,
                (g * 255.0).round() as u8,
                (b * 255.0).round() as u8,
            );
            let want = (
                u8::from_str_radix(&hex[1..3], 16).unwrap(),
                u8::from_str_radix(&hex[3..5], 16).unwrap(),
                u8::from_str_radix(&hex[5..7], 16).unwrap(),
            );
            assert_eq!(got, want, "hex {}", hex);
        }
    }

    #[test]
    fn hsv_picker_models_are_complete() {
        // 面板一次要画满：色相 24 段、SV 方块 20×14=280 格，一格不能少
        assert_eq!(hue_bar_colors().len(), HUE_SEGMENTS);
        assert_eq!(sv_grid_colors(210.0).len(), SV_COLS * SV_ROWS);
        // 色相变了方块必须真变（否则拖色相时 SV 区不刷新）
        assert_ne!(sv_grid_colors(0.0), sv_grid_colors(180.0));
        // SV 方块四角：左上=白(s=0,v=1)、右上=纯色相(s=1,v=1)、
        // 左下=黑(s=0,v=0)、右下=色相最暗
        let g = sv_grid_colors(0.0);
        assert_eq!((g[0].red(), g[0].green(), g[0].blue()), (255u8, 255, 255));
        assert_eq!(
            (g[SV_COLS - 1].red(), g[SV_COLS - 1].green(), g[SV_COLS - 1].blue()),
            (255u8, 0, 0)
        );
        let bottom = g[(SV_ROWS - 1) * SV_COLS];
        assert_eq!(
            (bottom.red(), bottom.green(), bottom.blue()),
            (0u8, 0, 0)
        );
    }
}
