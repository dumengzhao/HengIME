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

/// 当前 C ABI 版本（v6：传播策略 + app_options 统一 + 选项持久化）
pub const HENG_ABI_VERSION: c_int = 9;
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
                "heng_settings_show", "heng_settings_hide"
            ]
        });
        CString::new(json.to_string()).unwrap()
    });
    DESCRIBE.as_ptr()
}

// ---- 会话 ----

/// 打开会话。app_id 仅用于登记（M0 不做按应用配置），成功返回会话句柄，0 失败。
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
        // 0. 自绘候选窗键盘语义（微信同款）：
        //    ↓ 未展开 → 打开面板；面板内 ↑↓ = 行间移动、←→ = 格间移动
        //    （纯面板视觉高亮，不碰 librime）；↑ 在第一行 → 收起；
        //    空格/回车 → 选中面板当前高亮项（真实候选翻页选中 / 同音词清串直上）
        const XK_UP: i32 = 0xff52;
        const XK_DOWN: i32 = 0xff54;
        const XK_LEFT: i32 = 0xff51;
        const XK_RIGHT: i32 = 0xff53;
        const XK_RETURN: i32 = 0xff0b;
        let mut ui_nav = false;
        let expanded = crate::ui::UI_EXPANDED.load(Ordering::Relaxed);
        // ibus RELEASE_MASK（weasel 的 bit14 经 expand_ibus_modifier 映到 bit30；
        // fcitx5 原生 bit30）：keyup 不拦截不选中，否则一次按键 = keydown+keyup
        // 两次导航（跳两格）/ 两次选中
        if mask & (1 << 30) == 0 {
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
                    _ => {}
                }
            } else {
                // 收起单行：不分页，←→/空格/回车/数字 全部本地处理（↑ 吞掉）。
                // 图标导航目标 = 最右图标（现为 ☰ 菜单）；将来若在 ☰ 右侧新增
                // 图标，把"最后一个图标"目标改指新图标即可，跳过逻辑不变
                let has_menu = engine
                    .get_context(rime_id)
                    .map(|s| !s.candidates.is_empty())
                    .unwrap_or(false);
                // UI_BAR_COUNT>0 = 横条状态已建立（UI 线程在跑且 sync 过）；
                // 否则（HTTP/CLI/测试等无 UI 线程场景）全部回落 librime 原行为
                let bar_ready = crate::ui::UI_BAR_COUNT.load(Ordering::Relaxed) > 0;
                if has_menu && bar_ready {
                    let icon_sel = crate::ui::UI_ICON_SEL.load(Ordering::Relaxed);
                    let menu_open = crate::ui::UI_MENU_OPEN.load(Ordering::Relaxed);
                    let hl = crate::ui::UI_HL.load(Ordering::Relaxed);
                    let bar_last = crate::ui::UI_BAR_COUNT.load(Ordering::Relaxed) - 1;
                    if menu_open {
                        // ☰ 菜单打开：←/→ 关闭回横条；空格/回车暂吞（菜单功能未做）
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
                                if icon_sel {
                                    // 最后图标上再按 → = 展开面板
                                    crate::ui::ui_toggle();
                                } else if hl >= bar_last {
                                    // 最后一个词 → 跳过 ▾ 直达最右图标 ☰
                                    crate::ui::ui_bar_icon(true);
                                } else {
                                    crate::ui::ui_move_hl(1);
                                }
                                ui_nav = true;
                            }
                            XK_LEFT => {
                                if icon_sel {
                                    // ☰ ← 回到最后一个词（跳过 ▾）
                                    crate::ui::ui_bar_icon(false);
                                } else if hl > 0 {
                                    crate::ui::ui_move_hl(-1);
                                }
                                // 首词再 ←：吞掉（不分页）
                                ui_nav = true;
                            }
                            0x20 | XK_RETURN => {
                                if icon_sel {
                                    // ☰ 上确认 = 打开菜单（占位：符号/常用语/设置）
                                    crate::ui::ui_menu_open();
                                } else {
                                    // 内联同步选词（横条锚定菜单第一页，高亮序号
                                    // 即全局序号）：commit 由本次 _Respond 直接取走，
                                    // 不能走异步 BarSelect（会错过本轮 _Respond）
                                    let _ = engine.select_candidate_global(
                                        rime_id,
                                        hl.max(0) as usize,
                                    );
                                }
                                ui_nav = true;
                            }
                            c if (0x30..=0x39).contains(&c) => {
                                // 数字键按横条序号选词：'1'-'9' → 0-8，'0' → 9
                                let n = if c == 0x30 { 9 } else { (c - 0x31) as i32 };
                                if n <= bar_last {
                                    let _ = engine.select_candidate_global(
                                        rime_id,
                                        n.max(0) as usize,
                                    );
                                    ui_nav = true;
                                }
                                // 超出横条词数：不拦，交给 librime（页内选词）
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
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
/// page = 初始页索引 0-5（0=输入方案 1=候选窗样式 2=快捷键 3=标点 4=词库 5=关于）。
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

/// 隐藏候选窗（焦点离开/清空组合串时调用）。始终返回 HENG_TRUE。
#[no_mangle]
pub extern "C" fn heng_ui_hide() -> c_int {
    ffi_guard!(HENG_TRUE, {
        crate::ui::ui_hide();
        HENG_TRUE
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

/// 取走 UI 点击产生的待上屏文本（消费语义）。返回 HENG_TRUE 且 *out 非 NULL
/// 表示有文本（heng_free_string 释放）；否则 *out 为 NULL。
#[no_mangle]
pub extern "C" fn heng_take_ui_commit(session: HengSession, out: *mut *mut c_char) -> c_int {
    ffi_guard!(HENG_FALSE, {
        if session == 0 || out.is_null() {
            return HENG_FALSE;
        }
        unsafe { *out = ptr::null_mut() };
        if let Some(text) = crate::ui::PENDING_UI_COMMITS.lock().unwrap().remove(&session) {
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
