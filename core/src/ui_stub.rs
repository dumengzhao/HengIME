//! feature `ui` 关闭时的占位实现。
//!
//! 移动三端（Android / iOS / 鸿蒙）用宿主原生框架绘制键盘与候选窗，不需要
//! Slint 软件渲染器；`--no-default-features` 构建时整个 `ui.rs` 与 slint
//! 依赖都不进产物。本模块提供与 `ui.rs` 同名同签名的空实现，让
//! `capi.rs` 的导出函数保持逐字不变——C ABI 的符号集合与平台无关，
//! 各端外壳不需要知道自己链接的是完整实现还是占位实现。
//!
//! 语义：状态量照常读写（外壳可能查询），绘制与窗口操作一律不做事。
//! `ensure_started` 返回 false，`capi.rs` 据此让候选窗相关导出失败。

use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::{LazyLock, Mutex};

use crate::engine::RimeSessionId;

pub type CommitHandler = extern "C" fn(ctx: *mut core::ffi::c_void, session: u64, text: *const u8);
pub type ServiceHandler = extern "C" fn(ctx: *mut core::ffi::c_void, action: i32);

pub static PENDING_UI_COMMITS: LazyLock<Mutex<std::collections::HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
pub static SERVICE_HANDLER: LazyLock<Mutex<Option<(ServiceHandler, usize)>>> =
    LazyLock::new(|| Mutex::new(None));
pub static COMMIT_HANDLER: LazyLock<Mutex<Option<(CommitHandler, usize)>>> =
    LazyLock::new(|| Mutex::new(None));

pub static UI_EXPANDED: AtomicBool = AtomicBool::new(false);
pub static UI_HL: AtomicI32 = AtomicI32::new(-1);
pub static UI_TOTAL: AtomicI32 = AtomicI32::new(0);
pub static UI_BAR_COUNT: AtomicI32 = AtomicI32::new(0);
pub static UI_MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// UI 后端未编入。`capi.rs` 据此让候选窗相关导出返回失败。
pub fn ensure_started() -> bool {
    false
}

pub fn ui_sync(_rime_id: RimeSessionId, _x: i32, _y: i32) {}
pub fn ui_sync_ex(_rime_id: RimeSessionId, _x: i32, _y: i32, _top: i32) {}
pub fn ui_hide() {}
pub fn ui_toggle() {}
pub fn ui_set_expanded(_v: bool) {}
pub fn ui_move_hl(_delta: i32) {}
pub fn ui_row_move(_delta: i32) {}
pub fn ui_select_hl() {}
pub fn ui_mode_hint(_ascii: bool) {}
pub fn ui_mode_hint_ex(_ascii: bool, _anchor: Option<(i32, i32, i32)>) {}
pub fn ui_bar_select(_idx: usize) {}
pub fn ui_select_by_row_seq(_seq: i32) {}
pub fn ui_menu_open() {}
pub fn ui_menu_close() {}
pub fn ui_settings_show(_page: i32) {}
pub fn ui_settings_hide() {}
pub fn fire_service(_action: i32) -> bool {
    false
}

/// 打开资源管理器。桌面端才需要；移动端外壳自行处理。
pub fn open_in_explorer(_p: &std::path::Path) {}

/// macOS 的窗口与屏幕几何回调。桌面 macOS 外壳使用，其余平台是空壳。
pub mod mac_backend {
    use std::sync::Mutex;

    /// 与 `ui.rs` 同签名的窗口回调类型。占位实现存下来但从不调用。
    pub type WindowCb = extern "C" fn(
        ctx: *mut core::ffi::c_void,
        win: u8,
        op: u8,
        buf: *const core::ffi::c_void,
        buf_len: usize,
        logical_w: u32,
        logical_h: u32,
    );

    static WINDOW_CB: Mutex<Option<(WindowCb, usize)>> = Mutex::new(None);
    static WORK_AREA: Mutex<Option<(i32, i32)>> = Mutex::new(None);
    static SCREEN: Mutex<Option<(i32, i32, f32)>> = Mutex::new(None);

    pub fn set_work_area(top: i32, bottom: i32) {
        *WORK_AREA.lock().unwrap() = Some((top, bottom));
    }

    pub fn work_area() -> Option<(i32, i32)> {
        *WORK_AREA.lock().unwrap()
    }

    pub fn set_screen_info(w: i32, h: i32, scale: f32) {
        *SCREEN.lock().unwrap() = Some((w, h, scale));
    }

    pub fn reported_scale() -> Option<f32> {
        SCREEN.lock().unwrap().map(|(_, _, s)| s)
    }

    pub fn set_window_handler(cb: Option<WindowCb>, ctx: *mut core::ffi::c_void) {
        *WINDOW_CB.lock().unwrap() = cb.map(|f| (f, ctx as usize));
    }

    pub fn push_pointer(_win: u8, _kind: u8, _x: f32, _y: f32, _dx: f32, _dy: f32) {}
}
