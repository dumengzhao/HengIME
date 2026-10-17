//! C ABI 导出层 —— 对应 `include/heng.h`，供各端外壳（C++/Swift/Kotlin/ArkTS）调用。
//!
//! 设计要点（docs/ARCHITECTURE.md 第 10.2 节）：
//! - 每个跨边界结构体首字段 `data_size`，配 `heng_free_context` 做调用方侧释放
//! - C99 类型 + C 调用约定；字符串由 core 分配，调用方用 `heng_free_string` / `heng_free_context` 释放
//! - `heng_describe` 返回静态 JSON（Keyman Core 式元数据自省）
//! - 所有导出经 `ffi_guard` 拦截 panic，禁止 Rust panic 穿过 FFI 边界
//!
//! M0 约束：每进程一个引擎；`heng_destroy` 之后的 librime 调用属未定义行为。

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};

use crate::global::{default_config, engine, engine_with, SESSIONS};

pub type HengSession = u64; // 对应 heng.h 的 heng_session_t（uint64_t），0 表示无效

pub const HENG_TRUE: c_int = 1;
pub const HENG_FALSE: c_int = 0;

/// 当前 C ABI 版本（v11：新增 heng_sync_configure / heng_sync_set_enabled /
/// heng_sync_config / heng_sync_now / heng_sync_status / heng_installation_id）
pub const HENG_ABI_VERSION: c_int = 11;
/// 仍兼容的最低调用方 ABI（v4 起有 config API；更早调用方未验证）
pub const HENG_MIN_ABI_VERSION: c_int = 4;

// ---- 错误传递：最后一次错误的线程安全缓存 ----

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

pub(crate) fn set_err<E: std::fmt::Display>(e: E) {
    let text = e.to_string();
    *LAST_ERROR.lock().unwrap() = Some(CString::new(text).unwrap_or_default());
}

fn clear_err() {
    *LAST_ERROR.lock().unwrap() = None;
}

/// 拦截 panic 的 FFI 守卫。`fallback` 在 panic 时作为返回值。
macro_rules! ffi_guard {
    ($fallback:expr, $body:block) => {
        match catch_unwind(AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(_) => {
                set_err("heng-core 内部 panic（已拦截，未越过 FFI 边界）");
                $fallback
            }
        }
    };
}

// ---- 对外结构体：字段顺序与 include/heng.h 严格一致 ----

/// 与 heng.h 的 `HengContext` 一致。内存归 core，调用方用 `heng_free_context` 释放。
#[repr(C)]
pub struct HengContext {
    pub data_size: c_int,
    pub preedit: *mut c_char,
    pub cursor_pos: c_int,
    pub candidates: *mut *mut c_char, // 与 comments / labels 平行
    pub comments: *mut *mut c_char,   // 无注释的槽位为 NULL
    pub candidate_count: c_int,
    pub highlighted: c_int, // 当前页内高亮
    pub page_no: c_int,
    pub is_last_page: c_int,
    pub commit_text_preview: *mut c_char,
    /// v2 新增：选键标签（select_labels；缺省槽位为 NULL，调用方回退 select_keys/序号）
    pub labels: *mut *mut c_char,
    /// v3 新增：组合串高亮区间（已转换部分；sel_start == sel_end 表示无区间）
    pub sel_start: c_int,
    pub sel_end: c_int,
    /// v3 新增：选键序列（select_keys，如 "1234567890"；labels 缺省时的回退）
    pub select_keys: *mut c_char,
}

impl HengContext {
    fn zeroed() -> Self {
        Self {
            data_size: 0,
            preedit: ptr::null_mut(),
            cursor_pos: 0,
            candidates: ptr::null_mut(),
            comments: ptr::null_mut(),
            candidate_count: 0,
            highlighted: 0,
            page_no: 0,
            is_last_page: 0,
            commit_text_preview: ptr::null_mut(),
            labels: ptr::null_mut(),
            sel_start: 0,
            sel_end: 0,
            select_keys: ptr::null_mut(),
        }
    }
}

/// 与 heng.h 的 `HengStatus` 一致（v3 新增）。字符串内存归 core，用 `heng_free_status` 释放。
#[repr(C)]
pub struct HengStatus {
    pub data_size: c_int,
    pub schema_id: *mut c_char,
    pub schema_name: *mut c_char,
    pub is_ascii_mode: c_int,
    pub is_composing: c_int,
    pub is_disabled: c_int,
    pub is_full_shape: c_int,
}

impl HengStatus {
    fn zeroed() -> Self {
        Self {
            data_size: 0,
            schema_id: ptr::null_mut(),
            schema_name: ptr::null_mut(),
            is_ascii_mode: 0,
            is_composing: 0,
            is_disabled: 0,
            is_full_shape: 0,
        }
    }
}

/// 与 heng.h 的 `HengHello` 一致（v5 新增）。纯值结构，无堆分配。
#[repr(C)]
pub struct HengHello {
    pub data_size: c_int,
    pub abi_version: c_int,
    pub min_abi_version: c_int,
    pub reserved: [u64; 2],
}

/// 拿一个 ContextSnapshot 填充调用方结构体；失败时保持全 NULL 并返回 FALSE。
fn fill_context(out: *mut HengContext, snapshot: crate::engine::ContextSnapshot) -> c_int {
    if out.is_null() {
        return HENG_FALSE;
    }
    let count = snapshot.candidates.len();
    unsafe {
        (*out) = HengContext::zeroed();
        (*out).data_size = std::mem::size_of::<HengContext>() as c_int;
        (*out).cursor_pos = snapshot.cursor_pos;
        (*out).candidate_count = count as c_int;
        (*out).highlighted = snapshot.highlighted;
        (*out).page_no = snapshot.page_no;
        (*out).is_last_page = snapshot.is_last_page as c_int;

        (*out).preedit = match CString::new(snapshot.preedit) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        };
        (*out).commit_text_preview = match snapshot.commit_text_preview {
            Some(s) => CString::new(s).map_or(ptr::null_mut(), |c| c.into_raw()),
            None => ptr::null_mut(),
        };
        (*out).sel_start = snapshot.sel_start;
        (*out).sel_end = snapshot.sel_end;
        (*out).select_keys = CString::new(snapshot.select_keys)
            .map_or(ptr::null_mut(), |c| c.into_raw());

        if count == 0 {
            (*out).candidates = ptr::null_mut();
            (*out).comments = ptr::null_mut();
            return HENG_TRUE;
        }

        let mut texts: Vec<*mut c_char> = Vec::with_capacity(count);
        let mut notes: Vec<*mut c_char> = Vec::with_capacity(count);
        let mut labels: Vec<*mut c_char> = Vec::with_capacity(count);
        for c in &snapshot.candidates {
            texts.push(CString::new(c.text.clone()).map_or(ptr::null_mut(), |s| s.into_raw()));
            notes.push(match &c.comment {
                Some(s) => CString::new(s.clone()).map_or(ptr::null_mut(), |s| s.into_raw()),
                None => ptr::null_mut(),
            });
        }
        // labels 数量与 candidates 对齐，缺省槽位 NULL
        for i in 0..count {
            labels.push(
                snapshot
                    .select_labels
                    .get(i)
                    .and_then(|s| CString::new(s.clone()).ok())
                    .map_or(ptr::null_mut(), |s| s.into_raw()),
            );
        }
        (*out).candidates = texts.as_mut_ptr();
        (*out).comments = notes.as_mut_ptr();
        (*out).labels = labels.as_mut_ptr();
        std::mem::forget(texts);
        std::mem::forget(notes);
        std::mem::forget(labels);
    }
    HENG_TRUE
}

// ---- 生命周期 ----

/// 初始化引擎。传 NULL 使用默认运行时目录（`HENG_RUNTIME` 或 ./runtime）。
/// 返回 0 成功，-1 失败（`heng_last_error` 可取详情）。
#[no_mangle]
pub extern "C" fn heng_create(
    shared_data_dir: *const c_char,
    user_data_dir: *const c_char,
) -> c_int {
    ffi_guard!(-1, {
        clear_err();
        let mut config = default_config();
        unsafe {
            if !shared_data_dir.is_null() {
                if let Some(s) = cstr_to_string(shared_data_dir) {
                    config.shared_data_dir = Some(s.into());
                }
            }
            if !user_data_dir.is_null() {
                if let Some(s) = cstr_to_string(user_data_dir) {
                    config.user_data_dir = s.into();
                }
            }
        }
        match engine_with(config) {
            Ok(_) => 0,
            Err(e) => {
                set_err(e);
                -1
            }
        }
    })
}

/// 销毁全部会话并 finalize librime。之后不得再调用任何 heng_*（M0 约束）。
#[no_mangle]
pub extern "C" fn heng_destroy() {
    ffi_guard!((), {
        clear_err();
        if let Ok(engine) = engine_with(default_config()) {
            // ids() 快照后再逐个移除，避免遍历中修改自身；gapped ID 也不会漏
            for id in SESSIONS.ids() {
                SESSIONS.remove(engine, id);
            }
            engine.finalize();
        }
    })
}

#[no_mangle]
pub extern "C" fn heng_version() -> *const c_char {
    static VERSION: LazyLock<CString> =
        LazyLock::new(|| CString::new("heng-core 0.1.0").unwrap());
    ffi_guard!(ptr::null(), {
        let _ = &*VERSION; // 静态字符串进程生命周期有效
        match crate::global::engine() {
            Ok(e) => e.version().and_then(|v| CString::new(v).ok()).map_or_else(
                || VERSION.as_ptr(),
                |c| {
                    // 每次泄露一个小字符串以返回有效指针；version 调用频率极低，可接受
                    c.into_raw() as *const c_char
                },
            ),
            Err(_) => VERSION.as_ptr(),
        }
    })
}

/// 元数据自省（Keyman Core 式）：返回静态 JSON，调用方不得释放。
#[no_mangle]
pub extern "C" fn heng_describe() -> *const c_char {
    static DESCRIBE: LazyLock<CString> = LazyLock::new(|| {
        let json = serde_json::json!({
            "name": "heng-core",
            "abi_version": HENG_ABI_VERSION,
            "min_abi_version": HENG_MIN_ABI_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "engine": "librime",
            "commands": [
                "heng_create", "heng_destroy", "heng_version", "heng_describe",
                "heng_hello",
                "heng_start_session", "heng_end_session", "heng_set_session_owner",
                "heng_process_key", "heng_process_key_ex", "heng_simulate_key_sequence",
                "heng_get_context", "heng_free_context",
                "heng_commit_text", "heng_select_candidate_on_current_page",
                "heng_highlight_candidate_on_current_page", "heng_change_page",
                "heng_commit_composition", "heng_set_option", "heng_get_option",
                "heng_get_status", "heng_free_status",
                "heng_config_open", "heng_config_close",
                "heng_config_get_string", "heng_config_get_int", "heng_config_get_bool",
                "heng_config_begin_map", "heng_config_next", "heng_config_end",
                "heng_set_propagation_policy", "heng_get_propagation_policy",
                "heng_ui_sync", "heng_ui_hide", "heng_take_ui_commit", "heng_ui_mode_hint",
                "heng_get_input", "heng_get_caret_pos", "heng_set_caret_pos",
                "heng_sync_user_data", "heng_get_state_label_abbreviated",
                "heng_config_open_schema", "heng_config_get_double",
                "heng_clear", "heng_free_string", "heng_last_error",
                "heng_settings_show", "heng_settings_hide",
                "heng_sync_configure", "heng_sync_set_enabled", "heng_sync_config",
                "heng_sync_now", "heng_sync_status", "heng_installation_id"
            ]
        });
        CString::new(json.to_string()).unwrap()
    });
    DESCRIBE.as_ptr()
}

// ---- 会话 ----

/// 打开会话。app_id 仅用于登记（M0 不做按应用配置），成功返回会话句柄，0 失败。
///
/// v11：全程持 `sync::SESSION_GATE`。librime 的 sync 会 `CleanupAllSessions`
/// 摧毁全部会话，会话创建与一轮 sync 互斥 → 最多等一轮 sync（秒级），
/// 不会出现「会话被摧毁但外壳仍持旧句柄」（docs/SYNC-PLAN.md §6）。
#[no_mangle]
pub extern "C" fn heng_start_session(app_id: *const c_char) -> HengSession {
    ffi_guard!(0, {
        clear_err();
        let app = unsafe { if app_id.is_null() { None } else { cstr_to_string(app_id) } };
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return 0;
            }
        };
        let _gate = crate::sync::SESSION_GATE.lock().unwrap();
        match engine.create_session() {
            Ok(session) => {
                // into_raw 消费 Session 但不触发 Drop 销毁，librime 会话交注册表接管
                let rime_id = session.into_raw();
                let handle = SESSIONS.insert(rime_id, app.filter(|s| !s.is_empty()));
                // v6：新会话应用初始状态（持久化选项 → app_options）
                let owner = SESSIONS.owner(handle);
                let _guard = crate::global::OP_LOCK.lock().unwrap();
                crate::global::apply_session_initial_state(engine, rime_id, owner.as_deref());
                handle
            }
            Err(e) => {
                set_err(e);
                0
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn heng_end_session(session: HengSession) {
    ffi_guard!((), {
        if session == 0 {
            return;
        }
        if let Ok(engine) = engine() {
            SESSIONS.remove(engine, session);
        }
        // v11：最后一个会话结束且留有待合并标记 → 立即补跑一轮同步
        if SESSIONS.count() == 0 {
            crate::sync::notify_sessions_idle();
        }
    })
}

// ---- 热路径 ----

/// 处理一个按键。返回 TRUE=已处理（组合串或上屏有变化），FALSE=未处理。
#[no_mangle]
pub extern "C" fn heng_process_key(session: HengSession, keysym: c_int, mask: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.process_key(rime_id, keysym, mask) {
            Ok(handled) => handled as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 模拟一段按键序列（如 "nihao"、"nihao{space}"）。返回 TRUE=全部按键已处理。
#[no_mangle]
pub extern "C" fn heng_simulate_key_sequence(
    session: HengSession,
    key_sequence: *const c_char,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || key_sequence.is_null() {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let seq = match unsafe { cstr_to_string(key_sequence) } {
            Some(s) => s,
            None => return HENG_FALSE,
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.simulate_key_sequence(rime_id, &seq) {
            Ok(handled) => handled as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 取当前组合串与候选快照。成功后调用方必须用 `heng_free_context` 释放。
#[no_mangle]
pub extern "C" fn heng_get_context(session: HengSession, out: *mut HengContext) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.get_context(rime_id) {
            Ok(snapshot) => fill_context(out, snapshot),
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 取待上屏文本。返回 TRUE 时 *out 为新分配字符串（`heng_free_string` 释放），无文本时 *out 为 NULL。
#[no_mangle]
pub extern "C" fn heng_commit_text(session: HengSession, out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = ptr::null_mut() };
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.get_commit(rime_id) {
            Ok(text) if !text.is_empty() => match CString::new(text) {
                Ok(c) => {
                    unsafe { *out = c.into_raw() };
                    HENG_TRUE
                }
                Err(e) => {
                    set_err(e);
                    HENG_FALSE
                }
            },
            Ok(_) => HENG_TRUE, // 无待上屏文本，*out 保持 NULL
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 选中当前页第 index 个候选（0 基）。
#[no_mangle]
pub extern "C" fn heng_select_candidate_on_current_page(session: HengSession, index: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || index < 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.select_candidate_on_current_page(rime_id, index as usize) {
            Ok(done) => done as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 高亮当前页第 index 个候选（不提交；候选窗移动光标用）。
#[no_mangle]
pub extern "C" fn heng_highlight_candidate_on_current_page(
    session: HengSession,
    index: c_int,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || index < 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.highlight_candidate_on_current_page(rime_id, index as usize) {
            Ok(done) => done as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 翻页。backward 非 0 表示向前翻。
#[no_mangle]
pub extern "C" fn heng_change_page(session: HengSession, backward: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.change_page(rime_id, backward != 0) {
            Ok(done) => done as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 绑定/改绑会话归属应用（焦点切换时调用；app_options 与开关传播的基础）。
/// 返回 0 成功，-1 失败（会话不存在）。
#[no_mangle]
pub extern "C" fn heng_set_session_owner(session: HengSession, app_id: *const c_char) -> c_int {
    ffi_guard!(-1, {
        if session == 0 || app_id.is_null() {
            return -1;
        }
        match unsafe { cstr_to_string(app_id) } {
            Some(app) if !app.is_empty() => {
                let bound = SESSIONS.set_owner(session, app.clone()) as c_int - 1;
                if bound == 0 {
                    // v6：改绑归属应用时应用该应用的初始选项（不重放持久化值，
                    // 避免覆盖用户当前会话里的实时开关状态）
                    if let (Ok(engine), Some(rime_id)) = (engine(), SESSIONS.rime_id(session)) {
                        let _guard = crate::global::OP_LOCK.lock().unwrap();
                        crate::global::apply_session_initial_state(
                            engine,
                            rime_id,
                            Some(app.as_str()),
                        );
                    }
                }
                bound
            }
            _ => -1,
        }
    })
}

#[no_mangle]
pub extern "C" fn heng_clear(session: HengSession) {
    ffi_guard!((), {
        if session == 0 {
            return;
        }
        if let Ok(engine) = engine() {
            if let Some(rime_id) = SESSIONS.rime_id(session) {
                let _guard = crate::global::OP_LOCK.lock().unwrap();
                let _ = engine.clear_composition(rime_id);
            }
        }
    })
}

/// 提交当前组合串（上屏）。返回 TRUE=已产生提交。
#[no_mangle]
pub extern "C" fn heng_commit_composition(session: HengSession) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.commit_composition(rime_id) {
            Ok(done) => done as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 设置会话选项。返回 TRUE=成功。
#[no_mangle]
pub extern "C" fn heng_set_option(
    session: HengSession,
    option: *const c_char,
    value: c_int,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || option.is_null() {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let Some(name) = (unsafe { cstr_to_string(option) }) else {
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.set_option(rime_id, &name, value != 0) {
            Ok(()) => {
                // v6：按策略广播到其它会话 + 名单内选项持久化（跨重启记忆）
                crate::global::propagate_option(engine, rime_id, &name, value != 0);
                crate::global::record_option_change(engine, &name, value != 0);
                HENG_TRUE
            }
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 读取会话选项。返回 0/1；-1 表示失败（会话不存在或 API 缺失）。
#[no_mangle]
pub extern "C" fn heng_get_option(session: HengSession, option: *const c_char) -> c_int {
    ffi_guard!(-1, {
        if session == 0 || option.is_null() {
            return -1;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return -1;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return -1;
        };
        let Some(name) = (unsafe { cstr_to_string(option) }) else {
            return -1;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.get_option(rime_id, &name) {
            Ok(v) => v as c_int,
            Err(e) => {
                set_err(e);
                -1
            }
        }
    })
}

/// 读样式布尔（weasel.custom.yaml style/<key>，源补丁直读）。
/// 返回 0/1；键不存在或读取失败返回 default_value。
/// 外壳用它决定是否内联推送 preedit（如 "inline_preedit"）——
/// 不读部署产物缓存，避免"改了开关但不重部署就读到旧值"。
#[no_mangle]
pub extern "C" fn heng_style_flag(key: *const c_char, default_value: c_int) -> c_int {
    ffi_guard!(default_value, {
        let Some(key) = (unsafe { cstr_to_string(key) }) else {
            return default_value;
        };
        let Ok(engine) = engine() else {
            return default_value;
        };
        let settings = crate::settings::Settings::new(engine);
        match settings.get_path(crate::settings::PATCH_WEASEL, &format!("style/{key}")) {
            Some(serde_yaml::Value::Bool(b)) => {
                if b {
                    1
                } else {
                    0
                }
            }
            _ => default_value,
        }
    })
}

/// 取会话状态快照。返回 TRUE 后调用方必须用 `heng_free_status` 释放。
#[no_mangle]
pub extern "C" fn heng_get_status(session: HengSession, out: *mut HengStatus) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.get_status(rime_id) {
            Ok(st) => unsafe {
                (*out) = HengStatus::zeroed();
                (*out).data_size = std::mem::size_of::<HengStatus>() as c_int;
                (*out).schema_id = CString::new(st.schema_id)
                    .map_or(ptr::null_mut(), |c| c.into_raw());
                (*out).schema_name = CString::new(st.schema_name)
                    .map_or(ptr::null_mut(), |c| c.into_raw());
                (*out).is_ascii_mode = st.is_ascii_mode as c_int;
                (*out).is_composing = st.is_composing as c_int;
                (*out).is_disabled = st.is_disabled as c_int;
                (*out).is_full_shape = st.is_full_shape as c_int;
                HENG_TRUE
            },
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 释放 `heng_get_status` 填充的结构体。
#[no_mangle]
pub extern "C" fn heng_free_status(out: *mut HengStatus) {
    if out.is_null() {
        return;
    }
    unsafe {
        let st = &mut *out;
        if !st.schema_id.is_null() {
            drop(CString::from_raw(st.schema_id));
            st.schema_id = ptr::null_mut();
        }
        if !st.schema_name.is_null() {
            drop(CString::from_raw(st.schema_name));
            st.schema_name = ptr::null_mut();
        }
    }
}

// ---- v5：版本握手与热路径合并调用 ----

/// 版本握手。client_abi_version 当前不校验（预留）。返回 TRUE=已填充。
#[no_mangle]
pub extern "C" fn heng_hello(client_abi_version: c_int, out: *mut HengHello) -> c_int {
    ffi_guard!(HENG_FALSE, {
        let _ = client_abi_version; // 预留：未来 core 收窄兼容范围时据此拒绝
        if out.is_null() {
            return HENG_FALSE;
        }
        unsafe {
            (*out) = HengHello {
                data_size: std::mem::size_of::<HengHello>() as c_int,
                abi_version: HENG_ABI_VERSION,
                min_abi_version: HENG_MIN_ABI_VERSION,
                reserved: [0; 2],
            };
        }
        HENG_TRUE
    })
}

/// 热路径合并调用：process_key → commit_text → get_context 三合一，
/// 单次跨进程往返。out_commit / out_ctx 均可为 NULL。
#[no_mangle]
pub extern "C" fn heng_process_key_ex(
    session: HengSession,
    keysym: c_int,
    mask: c_int,
    out_commit: *mut *mut c_char,
    out_ctx: *mut HengContext,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        if !out_commit.is_null() {
            unsafe { *out_commit = ptr::null_mut() };
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        // 0. 自绘候选窗键盘语义（微信同款）。整段逻辑抽在 ui_nav_key 里，
        //    与 heng_ui_nav_key（macOS 专用入口）共用同一实现，避免两端漂移。
        //    keyup（RELEASE_MASK）不拦截，否则一次按键 = keydown+keyup 两次导航。
        let ui_nav = if mask & (1 << 30) == 0 {
            ui_nav_key(&engine, rime_id, keysym)
        } else {
            false
        };
        // 1. 按键
        let handled = if ui_nav {
            true
        } else {
            match engine.process_key(rime_id, keysym, mask) {
                Ok(h) => h,
                Err(e) => {
                    set_err(e);
                    return HENG_FALSE;
                }
            }
        };
        // 2. 取走待上屏文本（消费语义）
        if !out_commit.is_null() {
            match engine.get_commit(rime_id) {
                Ok(text) if !text.is_empty() => match CString::new(text) {
                    Ok(c) => unsafe { *out_commit = c.into_raw() },
                    Err(e) => {
                        set_err(e);
                        return HENG_FALSE;
                    }
                },
                Ok(_) => {} // 无待上屏，*out_commit 保持 NULL
                Err(e) => {
                    set_err(e);
                    return HENG_FALSE;
                }
            }
        }
        // 3. 组合串与候选快照
        if !out_ctx.is_null() {
            match engine.get_context(rime_id) {
                Ok(snapshot) => {
                    if fill_context(out_ctx, snapshot) != HENG_TRUE {
                        return HENG_FALSE;
                    }
                }
                Err(e) => {
                    set_err(e);
                    return HENG_FALSE;
                }
            }
        }
        handled as c_int
    })
}

// ---- v6：行为统一层（传播策略；app_options 与持久化经既有入口生效） ----

/// 设置开关传播策略。policy: 0=per_session 1=per_app 2=global。
/// 返回 0 成功，-1 非法值。运行时设置优先于 heng.yaml 的初值。
#[no_mangle]
pub extern "C" fn heng_set_propagation_policy(policy: c_int) -> c_int {
    use crate::global::{POLICY_GLOBAL, POLICY_PER_APP, POLICY_PER_SESSION};
    ffi_guard!(-1, {
        match policy {
            POLICY_PER_SESSION | POLICY_PER_APP | POLICY_GLOBAL => {
                crate::global::PROPAGATION_POLICY.store(policy, Ordering::Relaxed);
                0
            }
            _ => -1,
        }
    })
}

/// 读取当前传播策略。返回 0/1/2；-1 引擎未初始化。
#[no_mangle]
pub extern "C" fn heng_get_propagation_policy() -> c_int {
    ffi_guard!(-1, {
        match engine() {
            Ok(_) => crate::global::PROPAGATION_POLICY.load(Ordering::Relaxed),
            Err(e) => {
                set_err(e);
                -1
            }
        }
    })
}

// ---- v6：自绘候选窗（M-P1；运行于 core 内部 UI 线程） ----

/// 同步候选窗：拉取会话 context，有候选则在屏幕 (x,y)（光标左下角）显示并刷新，
/// 无候选则隐藏。x11 不可用时静默失败（外壳可回退宿主候选窗）。
/// 返回 HENG_TRUE=已同步（显示或隐藏），HENG_FALSE=UI 不可用。
#[no_mangle]
pub extern "C" fn heng_ui_sync(session: HengSession, x: c_int, y: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return HENG_FALSE;
        };
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        crate::ui::ui_sync(rime_id, x, y);
        HENG_TRUE
    })
}

/// 同步候选窗（扩展版，v8）：额外传入光标所在文本行的顶边 y（屏幕坐标）。
/// 展开面板向上翻转时以此为准（面板底边贴输入行上方，不遮输入行）。
/// 外壳未升级时仍用 heng_ui_sync（顶边按底边-40 估算）。
#[no_mangle]
pub extern "C" fn heng_ui_sync_ex(
    session: HengSession,
    x: c_int,
    caret_bottom: c_int,
    caret_top: c_int,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return HENG_FALSE;
        };
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        crate::ui::ui_sync_ex(rime_id, x, caret_bottom, caret_top);
        HENG_TRUE
    })
}

/// 打开设置窗口（v9；运行于 core 内部 UI 线程，窗口属于宿主进程）。
/// page = 初始页索引 0-5（0=输入方案 1=外观 2=快捷键 3=标点 4=词库 5=关于）。
#[no_mangle]
pub extern "C" fn heng_settings_show(page: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        crate::ui::ui_settings_show(page);
        HENG_TRUE
    })
}

/// 关闭设置窗口（v9）。
#[no_mangle]
pub extern "C" fn heng_settings_hide() -> c_int {
    ffi_guard!(HENG_FALSE, {
        crate::ui::ui_settings_hide();
        HENG_TRUE
    })
}

/// 注册 macOS 窗口后端回调（M-P4）。cb 为空指针时注销。
///
/// core 只负责 slint 渲染，窗口创建/定位/贴帧交给外壳在主线程执行
/// （AppKit 的窗口调用必须在主线程，而 UI 线程是 core 自己 spawn 的 Rust 线程）。
/// 仅 macOS 有效；另两端由 core 各自的 backend 直连平台，不经过这里。
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn heng_set_window_handler(
    cb: Option<crate::ui::mac_backend::WindowCb>,
    ctx: *mut core::ffi::c_void,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        crate::ui::mac_backend::set_window_handler(cb, ctx);
        HENG_TRUE
    })
}

#[cfg(not(target_os = "macos"))]
#[no_mangle]
pub extern "C" fn heng_set_window_handler(
    _cb: Option<core::ffi::c_void>,
    _ctx: *mut core::ffi::c_void,
) -> c_int {
    HENG_FALSE
}

/// 外壳上报主屏尺寸与缩放（M-P4）。仅 macOS 有效。
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn heng_set_screen_info(width: u32, height: u32, scale: f32) {
    crate::ui::mac_backend::set_screen_info(width as i32, height as i32, scale);
}

#[cfg(not(target_os = "macos"))]
#[no_mangle]
pub extern "C" fn heng_set_screen_info(_width: u32, _height: u32, _scale: f32) {}

/// 外壳上报所在显示器的工作区上下边界（设备像素，**左上原点**，与 core
/// 三端统一坐标系一致）。仅 macOS 有效。
///
/// 用途：展开候选面板时判断「光标下方还放不放下」——放不下就向上翻转
/// （微信同款）。Windows 与 Linux 各自能直接查系统 API
/// （`MonitorFromPoint`+`GetMonitorInfoW` / x11rb randr CRTC），
/// macOS 的等价信息只能在主线程从 AppKit 取，故由外壳上报。
///
/// 取 `NSScreen.visibleFrame` 而非 `frame`：前者已排除 Dock 与菜单栏，
/// 语义与 Windows 的 `rcWork` 一致；用整屏高度会在光标贴底时误判
/// 「下方还有空间」，面板实际会被 Dock 盖住。
///
/// 显示器插拔 / Dock 显隐 / 菜单栏高度变化时应重新上报。
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn heng_set_work_area(top: i32, bottom: i32) {
    crate::ui::mac_backend::set_work_area(top, bottom);
}

#[cfg(not(target_os = "macos"))]
#[no_mangle]
pub extern "C" fn heng_set_work_area(_top: i32, _bottom: i32) {}

/// 外壳上报 inline_preedit 开关（"字母区内嵌到宿主输入区"）。
/// macOS 专用：core 不能自己读 weasel.custom.yaml（Windows 端补丁文件，
/// macOS 上不存在），否则外壳插宿主区、core 条内也画，一份字母显两处。
/// 其他平台继续由 core 直读 weasel.custom.yaml（weasel/fcitx5 正统配置源）。
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn heng_set_inline_preedit(on: c_int) {
    crate::ui::mac_backend::set_inline_preedit(on != 0);
}

#[cfg(not(target_os = "macos"))]
#[no_mangle]
pub extern "C" fn heng_set_inline_preedit(_on: c_int) {}

/// 外壳上报指针事件（M-P4）。仅 macOS 有效。
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn heng_push_pointer_event(
    win: u8,
    kind: u8,
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
) {
    crate::ui::mac_backend::push_pointer(win, kind, x, y, dx, dy);
}

#[cfg(not(target_os = "macos"))]
#[no_mangle]
pub extern "C" fn heng_push_pointer_event(
    _win: u8,
    _kind: u8,
    _x: f32,
    _y: f32,
    _dx: f32,
    _dy: f32,
) {
}

/// 注册「服务级动作」回调（v10）。cb 为空指针时注销。
///
/// 设置页「服务」页的重启服务 / 退出服务 / 打开程序目录这三个动作 core 自己
/// 做不了（core 是宿主加载的 DLL，重启/退出的是宿主进程），只能把意图抛给
/// 外壳。`action` 取值见 `SERVICE_*`。
///
/// 回调在 core UI 线程同步执行，实现方只应做投递（post 消息 / 排队）。
#[no_mangle]
pub extern "C" fn heng_set_service_handler(
    cb: Option<crate::ui::ServiceHandler>,
    ctx: *mut core::ffi::c_void,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        *crate::ui::SERVICE_HANDLER.lock().unwrap() = cb.map(|f| (f, ctx as usize));
        HENG_TRUE
    })
}

/// 在资源管理器中打开某个目录（v10）。
///
/// 供设置页「服务」页的「打开用户目录 / 程序目录 / 日志目录」使用。
/// `dir` 为 UTF-8 路径。目录不存在则返回 FALSE。
#[no_mangle]
pub extern "C" fn heng_open_dir(dir: *const c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if dir.is_null() {
            return HENG_FALSE;
        }
        let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(dir) }).to_str() else {
            return HENG_FALSE;
        };
        let p = std::path::Path::new(s);
        if s.is_empty() || !p.is_dir() {
            return HENG_FALSE;
        }
        crate::ui::open_in_explorer(p);
        HENG_TRUE
    })
}

/// 隐藏候选窗（焦点离开/清空组合串时调用）。始终返回 HENG_TRUE。
#[no_mangle]
pub extern "C" fn heng_ui_hide() -> c_int {
    ffi_guard!(HENG_TRUE, {
        crate::ui::ui_hide();
        HENG_TRUE
    })
}

/// 候选窗键盘导航（**仅 macOS 外壳使用**）。
///
/// 返回 HENG_TRUE 表示该按键已被候选窗导航消费（调用方不应再交给 librime），
/// HENG_FALSE 表示未消费、按键应继续走普通输入流程。
///
/// 为什么不复用 heng_process_key_ex：那个是 Windows/Linux 的热路径，一次调用
/// 就完成「导航 + 按键 + 取commit + 取 context」，签名与调用时机都绑在那套
/// 请求-响应模型上。macOS 的 IMK 是同步回调、没有跨进程往返，且方向键走
/// didCommand(by:) 而非 handle(_:)，塞进 _ex 会让两端时序耦合。
/// 内部逻辑与 _ex 共用 ui_nav_key，两端行为不会漂移。
#[no_mangle]
pub extern "C" fn heng_ui_nav_key(session: HengSession, keysym: c_int, mask: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 {
            return HENG_FALSE;
        }
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        let Ok(engine) = engine() else {
            return HENG_FALSE;
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        // keyup 不消费：否则一次按键 = keydown+keyup 两次导航（跳两格）
        if mask & (1 << 30) != 0 {
            return HENG_FALSE;
        }
        if ui_nav_key(&engine, rime_id, keysym) {
            HENG_TRUE
        } else {
            HENG_FALSE
        }
    })
}

/// 候选格点击选中（**仅 macOS 外壳使用**）。
///
/// 外壳在候选窗上做完自己的命中测试后，把全局候选下标传进来；
/// 返回 HENG_TRUE 表示已提交上屏。外壳负责在返回后调用
/// heng_take_ui_commit 或依赖已注册的 heng_set_commit_handler 取文本。
///
/// 与 heng_select_candidate_on_current_page 的区别：那个按「页内序号」，
/// 本函数按**全局下标**（横条/面板的格子顺序），与 UI 线程的格子模型一致。
#[no_mangle]
pub extern "C" fn heng_ui_click_candidate(session: HengSession, index: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || index < 0 {
            return HENG_FALSE;
        }
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        let Ok(engine) = engine() else {
            return HENG_FALSE;
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.select_candidate_global(rime_id, index as usize) {
            Ok(true) => HENG_TRUE,
            Ok(false) => HENG_FALSE,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 中英切换瞬态提示（Shift 切换后调用）：显示大字「中」/「A」约 1 秒。
#[no_mangle]
pub extern "C" fn heng_ui_mode_hint(ascii: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        crate::ui::ui_mode_hint(ascii != 0);
        HENG_TRUE
    })
}

/// 中英切换瞬态提示（v9 追加，带锚点坐标）：气泡显示在光标行顶上方。
/// x < 0 时回退到内部记忆锚点（与 heng_ui_sync_ex 同坐标系）。
#[no_mangle]
pub extern "C" fn heng_ui_mode_hint_ex(
    ascii: c_int,
    x: c_int,
    caret_bottom: c_int,
    caret_top: c_int,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if !crate::ui::ensure_started() {
            return HENG_FALSE;
        }
        crate::ui::ui_mode_hint_ex(
            ascii != 0,
            if x < 0 { None } else { Some((x, caret_bottom, caret_top)) },
        );
        HENG_TRUE
    })
}

/// 注册「UI 点击上屏」回调。cb 为空指针时注销（退回轮询取语义）。
///
/// 鼠标点击选词这条路径没有任何按键事件伴随，而 weasel 的 IPC 是严格
/// 请求-响应模型（无server→client 推送通道），`_Respond` 只在按键时被
/// 调用 → 点击产生的 commit 永远没人取，候选窗关了但字不上屏。注册本回调
/// 后 core 选词完立即调用，跨端行为一致。
///
/// 回调在 core UI 线程同步执行，实现方只应做投递（post 消息 / 排队），
/// 不要阻塞或反向长时间持有 core 锁。text 为 UTF-8、以 NUL 结尾，
/// 仅在回调期间有效，需自存。
#[no_mangle]
pub extern "C" fn heng_set_commit_handler(
    cb: Option<crate::ui::CommitHandler>,
    ctx: *mut core::ffi::c_void,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        *crate::ui::COMMIT_HANDLER.lock().unwrap() = cb.map(|f| (f, ctx as usize));
        HENG_TRUE
    })
}

/// 取走 UI 点击产生的待上屏文本（消费语义）。返回 HENG_TRUE 且 *out 非 NULL
/// 表示有文本（heng_free_string 释放）；否则 *out 为 NULL。
///
/// 仅在外壳未注册 heng_set_commit_handler 时才有内容（注册后 core 直接回调，
/// 此接口返回空）。保留供轮询式外壳与调试使用。
#[no_mangle]
pub extern "C" fn heng_take_ui_commit(session: HengSession, out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = ptr::null_mut() };
        let mut pend = crate::ui::PENDING_UI_COMMITS.lock().unwrap();
        if let Some(text) = pend.remove(&session) {
            drop(pend);
            if let Ok(c) = CString::new(text) {
                unsafe { *out = c.into_raw() };
                return HENG_TRUE;
            }
        }
        HENG_FALSE
    })
}

// ---- 配置读取（librime RimeConfig 直通；句柄 = RimeConfig 内部指针） ----

/// 与 librime `RimeConfigIterator` 二进制兼容（前 5 字段同序同型），
/// 尾部 reserved 供前向扩展；key/path 指向 librime 内部内存，close/end 前有效。
#[repr(C)]
pub struct HengConfigIterator {
    pub list: *mut c_void,
    pub map: *mut c_void,
    pub index: c_int,
    pub key: *const c_char,
    pub path: *const c_char,
    pub reserved: [u64; 4],
}

/// 打开配置（如 "weasel"、"default"）。返回句柄，NULL 失败（引擎未初始化或配置不存在）。
#[no_mangle]
pub extern "C" fn heng_config_open(config_id: *const c_char) -> *mut c_void {
    ffi_guard!(ptr::null_mut(), {
        if let Err(e) = engine() {
            set_err(e);
            return ptr::null_mut();
        }
        let Some(id) = (unsafe { cstr_to_string(config_id) }) else {
            return ptr::null_mut();
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine().unwrap().config_open(&id) {
            Ok(config) => config.ptr,
            Err(e) => {
                set_err(e);
                ptr::null_mut()
            }
        }
    })
}

/// 关闭配置。之后句柄失效。
#[no_mangle]
pub extern "C" fn heng_config_close(config: *mut c_void) {
    ffi_guard!((), {
        if config.is_null() {
            return;
        }
        if let Ok(engine) = engine() {
            let _guard = crate::global::OP_LOCK.lock().unwrap();
            let mut cfg = crate::engine::RimeConfig { ptr: config };
            engine.config_close(&mut cfg);
        }
    })
}

/// 读字符串。
/// 返回：>0 且 < buf_len = 已写入 buf 的字节数（不含 NUL）；
///       > buf_len = 键存在但缓冲不足（未写入，返回所需长度含 NUL）；
///       0 = 键不存在 / 参数无效。
#[no_mangle]
pub extern "C" fn heng_config_get_string(
    config: *mut c_void,
    key: *const c_char,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    ffi_guard!(0, {
        if config.is_null() || key.is_null() || buf_len <= 0 {
            return 0;
        }
        let Ok(engine) = engine() else {
            return 0;
        };
        let Some(k) = (unsafe { cstr_to_string(key) }) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let cfg = crate::engine::RimeConfig { ptr: config };
        let mut tmp = vec![0u8; buf_len as usize];
        match engine.config_get_string(&cfg, &k, &mut tmp) {
            // 缓冲足够（含 NUL 后仍放得下）：tmp 已填充，拷给调用方
            Some(len) if (len as c_int) < buf_len => {
                unsafe {
                    std::ptr::copy_nonoverlapping(tmp.as_ptr(), buf as *mut u8, len + 1);
                }
                len as c_int
            }
            // 键存在但缓冲不足：不写入，返回所需长度（含 NUL）
            Some(len) => (len + 1) as c_int,
            None => 0,
        }
    })
}

/// 读整数。返回 1 命中（*out 已填），0 未命中 / 参数无效。
#[no_mangle]
pub extern "C" fn heng_config_get_int(
    config: *mut c_void,
    key: *const c_char,
    out: *mut c_int,
) -> c_int {
    ffi_guard!(0, {
        if config.is_null() || key.is_null() || out.is_null() {
            return 0;
        }
        let Ok(engine) = engine() else {
            return 0;
        };
        let Some(k) = (unsafe { cstr_to_string(key) }) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let cfg = crate::engine::RimeConfig { ptr: config };
        match engine.config_get_int(&cfg, &k) {
            Some(v) => unsafe {
                *out = v;
                1
            },
            None => 0,
        }
    })
}

/// 读布尔。返回 1 命中（*out 已填 0/1），0 未命中 / 参数无效。
#[no_mangle]
pub extern "C" fn heng_config_get_bool(
    config: *mut c_void,
    key: *const c_char,
    out: *mut c_int,
) -> c_int {
    ffi_guard!(0, {
        if config.is_null() || key.is_null() || out.is_null() {
            return 0;
        }
        let Ok(engine) = engine() else {
            return 0;
        };
        let Some(k) = (unsafe { cstr_to_string(key) }) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let cfg = crate::engine::RimeConfig { ptr: config };
        match engine.config_get_bool(&cfg, &k) {
            Some(v) => unsafe {
                *out = v as c_int;
                1
            },
            None => 0,
        }
    })
}

/// 开始遍历 `key` 下直属子键（map）。返回 1 成功（iter 已初始化），0 失败。
#[no_mangle]
pub extern "C" fn heng_config_begin_map(
    config: *mut c_void,
    key: *const c_char,
    iter: *mut HengConfigIterator,
) -> c_int {
    ffi_guard!(0, {
        if config.is_null() || key.is_null() || iter.is_null() {
            return 0;
        }
        let Ok(engine) = engine() else {
            return 0;
        };
        let Some(k) = (unsafe { cstr_to_string(key) }) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let cfg = crate::engine::RimeConfig { ptr: config };
        let rime_iter = iter as *mut crate::engine::RimeConfigIterator;
        unsafe { engine.config_begin_map(&cfg, &k, rime_iter) as c_int }
    })
}

/// 遍历下一项。返回 1 有项（iter.key/iter.path 可读），0 结束。
#[no_mangle]
pub extern "C" fn heng_config_next(iter: *mut HengConfigIterator) -> c_int {
    ffi_guard!(0, {
        if iter.is_null() {
            return 0;
        }
        let Ok(engine) = engine() else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let rime_iter = iter as *mut crate::engine::RimeConfigIterator;
        unsafe { engine.config_next(rime_iter) as c_int }
    })
}

/// 结束遍历（释放迭代器资源）。
#[no_mangle]
pub extern "C" fn heng_config_end(iter: *mut HengConfigIterator) {
    ffi_guard!((), {
        if iter.is_null() {
            return;
        }
        if let Ok(engine) = engine() {
            let _guard = crate::global::OP_LOCK.lock().unwrap();
            let rime_iter = iter as *mut crate::engine::RimeConfigIterator;
            unsafe { engine.config_end(rime_iter) };
        }
    })
}

// ---- v7：Squirrel 外壳所需增量 ----

/// 取当前组合串的原始输入（消费不适用：只读快照）。
/// 返回 TRUE 且 *out 非 NULL 表示有输入（heng_free_string 释放）；否则 *out 为 NULL。
#[no_mangle]
pub extern "C" fn heng_get_input(session: HengSession, out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = std::ptr::null_mut() };
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.get_input(rime_id) {
            Ok(input) => match CString::new(input) {
                Ok(s) => {
                    unsafe { *out = s.into_raw() };
                    HENG_TRUE
                }
                Err(_) => HENG_FALSE,
            },
            Err(_) => HENG_FALSE, // 无组合串属正常情形，不视为错误
        }
    })
}

/// 组合串内光标位置（UTF-8 字节偏移）。无组合/失败返回 0。
#[no_mangle]
pub extern "C" fn heng_get_caret_pos(session: HengSession) -> c_int {
    ffi_guard!(0, {
        if session == 0 {
            return 0;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        engine.get_caret_pos(rime_id).unwrap_or(0) as c_int
    })
}

/// 设置组合串内光标位置（UTF-8 字节偏移）。返回 TRUE=已设置。
#[no_mangle]
pub extern "C" fn heng_set_caret_pos(session: HengSession, pos: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || pos < 0 {
            return HENG_FALSE;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            set_err(format!("会话 {session} 不存在"));
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.set_caret_pos(rime_id, pos as usize) {
            Ok(()) => HENG_TRUE,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 用户数据同步（Rime sync 机制）。返回 TRUE=成功。
///
/// **注意**：设置页「服务」栏的「用户资料同步」入口已移除 —— 该功能与未来的
/// 「服务端 + 账号」资料同步方案重合，且 librime 的 sync **不含任何网络代码**
/// （未配 `sync_dir` 时只把快照写到 `user_dir\sync\<installation_id>\`，
/// 是本地镜像、零传输）。
///
/// 但**本导出不要删**：「导出快照」正是未来服务端方案的底座 ——
/// 账号登录后上传的就是这份文本快照（带 `#@/tick`、`#@/user_id` 头），
/// 服务端只需存文件 + 合并 tick 回传。届时 UI 入口会在新的「账号」页重建。
#[no_mangle]
pub extern "C" fn heng_sync_user_data() -> c_int {
    ffi_guard!(HENG_FALSE, {
        let engine = match engine() {
            Ok(e) => e,
            Err(e) => {
                set_err(e);
                return HENG_FALSE;
            }
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine.sync_user_data() {
            Ok(done) => done as c_int,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

// ---- v11：用户词库同步（docs/SYNC-PLAN.md） ----

/// 登录同步服务端并保存账号（server_url / username / password）。
/// 成功后启用同步、落盘 `<user_dir>/.heng/sync.yaml` 并立即唤醒定时器。
/// 返回 TRUE 成功；失败原因经 `heng_last_error` 读取。
///
/// 本调用含网络 IO（秒级），不得在输入热路径调用。令牌只落在本机
/// `.heng/sync.yaml`，不回显给调用方（`heng_sync_config` 不含 token）。
#[no_mangle]
pub extern "C" fn heng_sync_configure(
    server_url: *const c_char,
    username: *const c_char,
    password: *const c_char,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        clear_err();
        let (Some(url), Some(user), Some(pass)) = (
            unsafe { cstr_to_string(server_url) },
            unsafe { cstr_to_string(username) },
            unsafe { cstr_to_string(password) },
        ) else {
            set_err("server_url/username/password 有空指针");
            return HENG_FALSE;
        };
        // 引擎未就位时同步模块无处落盘：先确保初始化（幂等）
        if let Err(e) = engine() {
            set_err(e);
            return HENG_FALSE;
        }
        match crate::sync::configure(&url, &user, &pass) {
            Ok(()) => HENG_TRUE,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 同步总开关（设置页拨杆）。返回 TRUE 成功。
#[no_mangle]
pub extern "C" fn heng_sync_set_enabled(enabled: c_int) -> c_int {
    ffi_guard!(HENG_FALSE, {
        clear_err();
        match crate::sync::set_enabled(enabled != 0) {
            Ok(()) => HENG_TRUE,
            Err(e) => {
                set_err(e);
                HENG_FALSE
            }
        }
    })
}

/// 「立即同步」：唤醒定时器线程跑一轮（同步含网络 IO，不卡调用方）。
/// 返回 TRUE = 已唤醒；FALSE = 未配置 / 已停用 / 引擎未初始化。
#[no_mangle]
pub extern "C" fn heng_sync_now() -> c_int {
    ffi_guard!(HENG_FALSE, {
        clear_err();
        crate::sync::request_sync_now() as c_int
    })
}

/// 同步配置快照（JSON，不含令牌）：`{"server_url","username","enabled",
/// "interval_secs","configured"}`。内存归 core，调用方用 `heng_free_string`
/// 释放；*out 恒非 NULL（未配置时为默认值 JSON）。
#[no_mangle]
pub extern "C" fn heng_sync_config(out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = std::ptr::null_mut() };
        let cfg = crate::sync::config_snapshot();
        let json = serde_json::json!({
            "server_url": cfg.server_url,
            "username": cfg.username,
            "enabled": cfg.enabled,
            "interval_secs": cfg.interval_secs,
            "configured": !cfg.server_url.is_empty() && !cfg.token.is_empty(),
        });
        match CString::new(json.to_string()) {
            Ok(s) => {
                unsafe { *out = s.into_raw() };
                HENG_TRUE
            }
            Err(_) => HENG_FALSE,
        }
    })
}

/// 同步状态快照（JSON）：`{"state","last_sync_at","last_error",
/// "last_downloaded","last_uploaded","pending_merge","next_attempt_at",
/// "consecutive_failures","installation_id","configured","enabled"}`。
/// 内存归 core，调用方用 `heng_free_string` 释放。
#[no_mangle]
pub extern "C" fn heng_sync_status(out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = std::ptr::null_mut() };
        let st = crate::sync::status_snapshot();
        match CString::new(serde_json::to_string(&st).unwrap_or_default()) {
            Ok(s) => {
                unsafe { *out = s.into_raw() };
                HENG_TRUE
            }
            Err(_) => HENG_FALSE,
        }
    })
}

/// 本机 installation_id（`<platform>-<hostname>-<6位随机>`，已写回
/// `installation.yaml`）。内存归 core，调用方用 `heng_free_string` 释放；
/// 引擎未初始化时为空串。
#[no_mangle]
pub extern "C" fn heng_installation_id(out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = std::ptr::null_mut() };
        match CString::new(crate::sync::installation_id()) {
            Ok(s) => {
                unsafe { *out = s.into_raw() };
                HENG_TRUE
            }
            Err(_) => HENG_FALSE,
        }
    })
}

/// 选项状态标签（abbreviated 非 0 取短标签）。
/// 返回 >0 且 < buf_len：已写入字节数（不含 NUL）；
/// 返回 > buf_len：缓冲不足（返回所需长度含 NUL）；
/// 返回 0：无标签 / 参数无效。
#[no_mangle]
pub extern "C" fn heng_get_state_label_abbreviated(
    session: HengSession,
    option: *const c_char,
    state: c_int,
    abbreviated: c_int,
    buf: *mut c_char,
    buf_len: c_int,
) -> c_int {
    ffi_guard!(0, {
        if session == 0 || option.is_null() || buf.is_null() || buf_len <= 0 {
            return 0;
        }
        let engine = match engine() {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let Some(rime_id) = SESSIONS.rime_id(session) else {
            return 0;
        };
        let Some(name) = (unsafe { cstr_to_string(option) }) else {
            return 0;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let label = engine.get_state_label_abbreviated(
            rime_id,
            &name,
            state != 0,
            abbreviated != 0,
        );
        let Some(label) = label else {
            return 0;
        };
        let bytes = label.as_bytes();
        if (bytes.len() as c_int) < buf_len {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, bytes.len());
                *buf.add(bytes.len()) = 0;
            }
            bytes.len() as c_int
        } else {
            (bytes.len() + 1) as c_int
        }
    })
}

/// 打开方案的已部署配置（如 "luna_pinyin"）。返回句柄，NULL 失败。
#[no_mangle]
pub extern "C" fn heng_config_open_schema(schema_id: *const c_char) -> *mut c_void {
    ffi_guard!(ptr::null_mut(), {
        if let Err(e) = engine() {
            set_err(e);
            return ptr::null_mut();
        }
        let Some(id) = (unsafe { cstr_to_string(schema_id) }) else {
            return ptr::null_mut();
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        match engine().unwrap().schema_open(&id) {
            Ok(config) => config.ptr,
            Err(e) => {
                set_err(e);
                ptr::null_mut()
            }
        }
    })
}

/// 读浮点。返回 TRUE=命中（*out 已填），FALSE=未命中。
#[no_mangle]
pub extern "C" fn heng_config_get_double(
    config: *mut c_void,
    key: *const c_char,
    out: *mut f64,
) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if config.is_null() || key.is_null() || out.is_null() {
            return HENG_FALSE;
        }
        let Ok(engine) = engine() else {
            return HENG_FALSE;
        };
        let Some(k) = (unsafe { cstr_to_string(key) }) else {
            return HENG_FALSE;
        };
        let _guard = crate::global::OP_LOCK.lock().unwrap();
        let cfg = crate::engine::RimeConfig { ptr: config };
        match engine.config_get_double(&cfg, &k) {
            Some(value) => {
                unsafe { *out = value };
                HENG_TRUE
            }
            None => HENG_FALSE,
        }
    })
}

// ---- 内存释放 ----

#[no_mangle]
pub extern "C" fn heng_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}

#[no_mangle]
pub extern "C" fn heng_free_context(out: *mut HengContext) {
    if out.is_null() {
        return;
    }
    unsafe {
        let ctx = &mut *out;
        heng_free_string(ctx.preedit);
        heng_free_string(ctx.commit_text_preview);
        heng_free_string(ctx.select_keys);
        let count = ctx.candidate_count as usize;
        // 三组数组独立判空释放：兼容 labels 未填充的旧调用方
        if count > 0 && !ctx.candidates.is_null() {
            for i in 0..count {
                let p = *ctx.candidates.add(i);
                if !p.is_null() {
                    drop(CString::from_raw(p));
                }
            }
            // len == capacity == count，与 fill_context 的 forget 配对
            drop(Vec::from_raw_parts(ctx.candidates, count, count));
        }
        if count > 0 && !ctx.comments.is_null() {
            for i in 0..count {
                let p = *ctx.comments.add(i);
                if !p.is_null() {
                    drop(CString::from_raw(p));
                }
            }
            drop(Vec::from_raw_parts(ctx.comments, count, count));
        }
        if count > 0 && !ctx.labels.is_null() {
            for i in 0..count {
                let p = *ctx.labels.add(i);
                if !p.is_null() {
                    drop(CString::from_raw(p));
                }
            }
            drop(Vec::from_raw_parts(ctx.labels, count, count));
        }
        *ctx = HengContext::zeroed();
    }
}

/// 取最后一次错误的描述。返回指针在下次 heng_* 调用前有效，不得释放。
#[no_mangle]
pub extern "C" fn heng_last_error() -> *const c_char {
    LAST_ERROR
        .lock()
        .unwrap()
        .as_ref()
        .map_or(ptr::null(), |c| c.as_ptr())
}

// ---- 内部辅助 ----

unsafe fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

/// 自绘候选窗的键盘导航语义（微信同款）。返回 true = 该按键已被 UI 消费，
/// 调用方**不应**再把它交给 librime。调用方需已持有 OP_LOCK。
///
/// ↓ 未展开 → 打开面板；面板内 ↑↓ = 行间移动、←→ = 格间移动
/// （纯面板视觉高亮，不碰 librime）；↑ 在第一行 → 收起；
/// 空格/回车 → 选中面板当前高亮项（真实候选翻页选中 / 同音词清串直上屏）
///
/// mask 的 keyup 检查由调用方负责：ibus RELEASE_MASK（weasel 的 bit14 经
/// expand_ibus_modifier 映到 bit30，fcitx5 原生 bit30）下必须放行，否则
/// 一次按键 = keydown+keyup 两次导航（跳两格）/ 两次选中。
fn ui_nav_key(
    engine: &crate::engine::Engine,
    rime_id: crate::engine::RimeSessionId,
    keysym: c_int,
) -> bool {
    const XK_UP: i32 = 0xff52;
    const XK_DOWN: i32 = 0xff54;
    const XK_LEFT: i32 = 0xff51;
    const XK_RIGHT: i32 = 0xff53;
    const XK_RETURN: i32 = 0xff0b;

    let mut ui_nav = false;
    let expanded = crate::ui::UI_EXPANDED.load(Ordering::Relaxed);
    if expanded {
        // ↓/↑ 按行移动（行内格数不定，收起/到底的边界判断在 UI 线程，
        // 那里有格子几何）；←→ 按格；空格/回车选中
        match keysym {
            XK_DOWN => {
                crate::ui::ui_row_move(1);
                ui_nav = true;
            }
            XK_UP => {
                crate::ui::ui_row_move(-1);
                ui_nav = true;
            }
            XK_RIGHT => {
                crate::ui::ui_move_hl(1);
                ui_nav = true;
            }
            XK_LEFT => {
                crate::ui::ui_move_hl(-1);
                ui_nav = true;
            }
            0x20 | XK_RETURN => {
                crate::ui::ui_select_hl();
                ui_nav = true;
            }
            // 数字键 1-9：选「当前高亮所在行」的第 N 个。
            // 面板每行序号独立从 1 起排，而候选索引是全局连续的——
            // 不做行换算就会永远选中第一行的第 N 个（已修）。
            0x31..=0x39 => {
                crate::ui::ui_select_by_row_seq((keysym - 0x30) as i32);
                ui_nav = true;
            }
            _ => {}
        }
    } else {
        // 收起单行：不分页，←→/空格/回车/数字 全部本地处理（↑ 吞掉）。
        // 键盘**不进入右侧图标**（☰ 选中态要画硬编码蓝底，自定义配色下
        // 与容器色打架）：→ 到最后一个预选词即展开，☰ 只响应鼠标点击。
        let has_menu = engine
            .get_context(rime_id)
            .map(|s| !s.candidates.is_empty())
            .unwrap_or(false);
        // UI_BAR_COUNT>0 = 横条状态已建立（UI 线程在跑且 sync 过）；
        // 否则（HTTP/CLI/测试等无 UI 线程场景）全部回落 librime 原行为
        let bar_ready = crate::ui::UI_BAR_COUNT.load(Ordering::Relaxed) > 0;
        if has_menu && bar_ready {
            let menu_open = crate::ui::UI_MENU_OPEN.load(Ordering::Relaxed);
            let hl = crate::ui::UI_HL.load(Ordering::Relaxed);
            let bar_last = crate::ui::UI_BAR_COUNT.load(Ordering::Relaxed) - 1;
            if menu_open {
                // ☰ 菜单打开（鼠标点开）：←/→ 关闭回横条；空格/回车暂吞
                //（菜单功能未做）
                match keysym {
                    XK_LEFT | XK_RIGHT => {
                        crate::ui::ui_menu_close();
                        ui_nav = true;
                    }
                    0x20 | XK_RETURN => {
                        ui_nav = true;
                    }
                    _ => {}
                }
            } else {
                match keysym {
                    XK_DOWN => {
                        crate::ui::ui_toggle();
                        ui_nav = true;
                    }
                    XK_UP => {
                        // 单行不分页：↑ 不做任何事（吞掉）
                        ui_nav = true;
                    }
                    XK_RIGHT => {
                        if hl >= bar_last {
                            // 末词再 → = 直接展开面板（不经过图标焦点）
                            crate::ui::ui_toggle();
                        } else {
                            crate::ui::ui_move_hl(1);
                        }
                        ui_nav = true;
                    }
                    XK_LEFT => {
                        if hl > 0 {
                            crate::ui::ui_move_hl(-1);
                        }
                        // 首词再 ←：吞掉（不分页）
                        ui_nav = true;
                    }
                    0x20 | XK_RETURN => {
                        // 内联同步选词（横条锚定菜单第一页，高亮序号
                        // 即全局序号）：commit 由本次调用方直接取走，
                        // 不能走异步 BarSelect（会错过本轮响应）
                        let _ = engine.select_candidate_global(rime_id, hl.max(0) as usize);
                        ui_nav = true;
                    }
                    c if (0x30..=0x39).contains(&c) => {
                        // 数字键按横条序号选词：'1'-'9' → 0-8，'0' → 9
                        let n = if c == 0x30 { 9 } else { (c - 0x31) as i32 };
                        if n <= bar_last {
                            let _ = engine.select_candidate_global(rime_id, n.max(0) as usize);
                            ui_nav = true;
                        }
                        // 超出横条词数：不拦，交给 librime（页内选词）
                    }
                    _ => {}
                }
            }
        }
    }
    ui_nav
}
