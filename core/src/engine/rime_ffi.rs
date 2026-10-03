//! librime 原始 FFI 绑定。
//!
//! 按 `third_party/librime/dist/include/rime_api.h`（librime 1.17.0）逐字段转录。
//! `RimeApi` 的字段顺序必须与 C 头文件完全一致 —— 截取到实际使用的最后一个字段
//! `simulate_key_sequence` 为止；`data_size` 版本化机制保证向后兼容（librime 只读
//! `data_size` 覆盖范围内的成员）。
//!
//! 指针用 `Option<unsafe extern "C" fn>` 表示可空函数指针，`repr(C)` 下与 C 函数指针 ABI 兼容。

use std::os::raw::{c_char, c_int, c_void};

pub type Bool = c_int;
pub const TRUE: Bool = 1;
pub const FALSE: Bool = 0;

/// C: `typedef uintptr_t RimeSessionId;`，0 表示无效会话
pub type RimeSessionId = usize;

pub type RimeNotificationHandler = Option<
    unsafe extern "C" fn(
        context_object: *mut c_void,
        session_id: RimeSessionId,
        message_type: *const c_char,
        message_value: *const c_char,
    ),
>;

/// C: `RimeTraits`（全部 12 个字段，`RIME_STRUCT_INIT` 由 [`init_data_size`](RimeTraits::init_data_size) 等价实现）
#[repr(C)]
pub struct RimeTraits {
    pub data_size: c_int,
    pub shared_data_dir: *const c_char,
    pub user_data_dir: *const c_char,
    pub distribution_name: *const c_char,
    pub distribution_code_name: *const c_char,
    pub distribution_version: *const c_char,
    pub app_name: *const c_char,
    pub modules: *mut *const c_char,
    pub min_log_level: c_int,
    pub log_dir: *const c_char,
    pub prebuilt_data_dir: *const c_char,
    pub staging_dir: *const c_char,
}

#[repr(C)]
pub struct RimeComposition {
    pub length: c_int,
    pub cursor_pos: c_int,
    pub sel_start: c_int,
    pub sel_end: c_int,
    pub preedit: *mut c_char,
}

#[repr(C)]
pub struct RimeCandidate {
    pub text: *mut c_char,
    pub comment: *mut c_char,
    pub reserved: *mut c_void,
}

#[repr(C)]
pub struct RimeMenu {
    pub page_size: c_int,
    pub page_no: c_int,
    pub is_last_page: Bool,
    pub highlighted_candidate_index: c_int,
    pub num_candidates: c_int,
    pub candidates: *mut RimeCandidate,
    pub select_keys: *mut c_char,
}

#[repr(C)]
pub struct RimeCommit {
    pub data_size: c_int,
    pub text: *mut c_char,
}

#[repr(C)]
pub struct RimeContext {
    pub data_size: c_int,
    pub composition: RimeComposition,
    pub menu: RimeMenu,
    pub commit_text_preview: *mut c_char,
    pub select_labels: *mut *mut c_char,
}

#[repr(C)]
pub struct RimeStatus {
    pub data_size: c_int,
    pub schema_id: *mut c_char,
    pub schema_name: *mut c_char,
    pub is_disabled: Bool,
    pub is_composing: Bool,
    pub is_ascii_mode: Bool,
    pub is_full_shape: Bool,
    pub is_simplified: Bool,
    pub is_traditional: Bool,
    pub is_ascii_punct: Bool,
}

#[repr(C)]
pub struct RimeSchemaListItem {
    pub schema_id: *mut c_char,
    pub name: *mut c_char,
    pub reserved: *mut c_void,
}

#[repr(C)]
pub struct RimeSchemaList {
    pub size: usize,
    pub list: *mut RimeSchemaListItem,
}

#[repr(C)]
pub struct RimeConfig {
    pub ptr: *mut c_void,
}

#[repr(C)]
pub struct RimeCustomApi {
    pub data_size: c_int,
}

#[repr(C)]
pub struct RimeModule {
    pub data_size: c_int,
    pub module_name: *const c_char,
    pub initialize: Option<unsafe extern "C" fn()>,
    pub finalize: Option<unsafe extern "C" fn()>,
    pub get_api: Option<unsafe extern "C" fn() -> *mut RimeCustomApi>,
}

#[repr(C)]
pub struct RimeConfigIterator {
    pub list: *mut c_void,
    pub map: *mut c_void,
    pub index: c_int,
    pub key: *const c_char,
    pub path: *const c_char,
}

/// rime_api.h: `RimeCandidateListIterator`（候选列表遍历器）
#[repr(C)]
pub struct RimeCandidateListIterator {
    pub ptr: *mut c_void,
    pub index: c_int,
    pub candidate: RimeCandidate,
}

/// rime_api.h: `RimeStringSlice`（非 NUL 结尾的字符串切片）
#[repr(C)]
pub struct RimeStringSlice {
    pub str: *const c_char,
    pub length: usize,
}

// ---- 函数指针类型（顺序与 rime_api.h 中 RimeApi 成员一致）----

pub type SetupFn = unsafe extern "C" fn(traits: *mut RimeTraits);
pub type SetNotificationHandlerFn =
    unsafe extern "C" fn(handler: RimeNotificationHandler, context_object: *mut c_void);
pub type InitializeFn = unsafe extern "C" fn(traits: *mut RimeTraits);
pub type FinalizeFn = unsafe extern "C" fn();
pub type StartMaintenanceFn = unsafe extern "C" fn(full_check: Bool) -> Bool;
pub type IsMaintenanceModeFn = unsafe extern "C" fn() -> Bool;
pub type JoinMaintenanceThreadFn = unsafe extern "C" fn();
pub type DeployerInitializeFn = unsafe extern "C" fn(traits: *mut RimeTraits);
pub type PrebuildFn = unsafe extern "C" fn() -> Bool;
pub type DeployFn = unsafe extern "C" fn() -> Bool;
pub type DeploySchemaFn = unsafe extern "C" fn(schema_file: *const c_char) -> Bool;
pub type DeployConfigFileFn =
    unsafe extern "C" fn(file_name: *const c_char, version_key: *const c_char) -> Bool;
pub type SyncUserDataFn = unsafe extern "C" fn() -> Bool;
pub type CreateSessionFn = unsafe extern "C" fn() -> RimeSessionId;
pub type FindSessionFn = unsafe extern "C" fn(session_id: RimeSessionId) -> Bool;
pub type DestroySessionFn = unsafe extern "C" fn(session_id: RimeSessionId) -> Bool;
pub type CleanupStaleSessionsFn = unsafe extern "C" fn();
pub type CleanupAllSessionsFn = unsafe extern "C" fn();
pub type ProcessKeyFn =
    unsafe extern "C" fn(session_id: RimeSessionId, keycode: c_int, mask: c_int) -> Bool;
pub type CommitCompositionFn = unsafe extern "C" fn(session_id: RimeSessionId) -> Bool;
pub type ClearCompositionFn = unsafe extern "C" fn(session_id: RimeSessionId);
pub type GetCommitFn =
    unsafe extern "C" fn(session_id: RimeSessionId, commit: *mut RimeCommit) -> Bool;
pub type FreeCommitFn = unsafe extern "C" fn(commit: *mut RimeCommit) -> Bool;
pub type GetContextFn =
    unsafe extern "C" fn(session_id: RimeSessionId, context: *mut RimeContext) -> Bool;
pub type FreeContextFn = unsafe extern "C" fn(ctx: *mut RimeContext) -> Bool;
pub type GetStatusFn =
    unsafe extern "C" fn(session_id: RimeSessionId, status: *mut RimeStatus) -> Bool;
pub type FreeStatusFn = unsafe extern "C" fn(status: *mut RimeStatus) -> Bool;
pub type SetOptionFn =
    unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char, value: Bool);
pub type GetOptionFn = unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char) -> Bool;
pub type SetPropertyFn =
    unsafe extern "C" fn(session_id: RimeSessionId, prop: *const c_char, value: *const c_char);
pub type GetPropertyFn = unsafe extern "C" fn(
    session_id: RimeSessionId,
    prop: *const c_char,
    value: *mut c_char,
    buffer_size: usize,
) -> Bool;
pub type GetSchemaListFn = unsafe extern "C" fn(schema_list: *mut RimeSchemaList) -> Bool;
pub type FreeSchemaListFn = unsafe extern "C" fn(schema_list: *mut RimeSchemaList);
pub type GetCurrentSchemaFn = unsafe extern "C" fn(
    session_id: RimeSessionId,
    schema_id: *mut c_char,
    buffer_size: usize,
) -> Bool;
pub type SelectSchemaFn =
    unsafe extern "C" fn(session_id: RimeSessionId, schema_id: *const c_char) -> Bool;
pub type SchemaOpenFn =
    unsafe extern "C" fn(schema_id: *const c_char, config: *mut RimeConfig) -> Bool;
pub type ConfigOpenFn =
    unsafe extern "C" fn(config_id: *const c_char, config: *mut RimeConfig) -> Bool;
pub type ConfigCloseFn = unsafe extern "C" fn(config: *mut RimeConfig) -> Bool;
pub type ConfigGetBoolFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: *mut Bool) -> Bool;
pub type ConfigGetIntFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: *mut c_int) -> Bool;
pub type ConfigGetDoubleFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: *mut f64) -> Bool;
pub type ConfigGetStringFn = unsafe extern "C" fn(
    config: *mut RimeConfig,
    key: *const c_char,
    value: *mut c_char,
    buffer_size: usize,
) -> Bool;
pub type ConfigGetCstringFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char) -> *const c_char;
pub type ConfigUpdateSignatureFn =
    unsafe extern "C" fn(config: *mut RimeConfig, signer: *const c_char) -> Bool;
pub type ConfigBeginMapFn = unsafe extern "C" fn(
    iterator: *mut RimeConfigIterator,
    config: *mut RimeConfig,
    key: *const c_char,
) -> Bool;
pub type ConfigNextFn = unsafe extern "C" fn(iterator: *mut RimeConfigIterator) -> Bool;
pub type ConfigEndFn = unsafe extern "C" fn(iterator: *mut RimeConfigIterator);
pub type SimulateKeySequenceFn =
    unsafe extern "C" fn(session_id: RimeSessionId, key_sequence: *const c_char) -> Bool;

pub type RegisterModuleFn = unsafe extern "C" fn(module: *mut RimeModule) -> Bool;
pub type FindModuleFn = unsafe extern "C" fn(module_name: *const c_char) -> *mut RimeModule;
pub type RunTaskFn = unsafe extern "C" fn(task_name: *const c_char) -> Bool;
pub type GetSharedDataDirFn = unsafe extern "C" fn() -> *const c_char;
pub type GetUserDataDirFn = unsafe extern "C" fn() -> *const c_char;
pub type GetSyncDirFn = unsafe extern "C" fn() -> *const c_char;
pub type GetUserIdFn = unsafe extern "C" fn() -> *const c_char;
pub type GetUserDataSyncDirFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);
pub type ConfigInitFn = unsafe extern "C" fn(config: *mut RimeConfig) -> Bool;
pub type ConfigLoadStringFn =
    unsafe extern "C" fn(config: *mut RimeConfig, yaml: *const c_char) -> Bool;
pub type ConfigSetBoolFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: Bool) -> Bool;
pub type ConfigSetIntFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: c_int) -> Bool;
pub type ConfigSetDoubleFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char, value: f64) -> Bool;
pub type ConfigSetStringFn = unsafe extern "C" fn(
    config: *mut RimeConfig,
    key: *const c_char,
    value: *const c_char,
) -> Bool;
pub type ConfigGetItemFn = unsafe extern "C" fn(
    config: *mut RimeConfig,
    key: *const c_char,
    value: *mut RimeConfig,
) -> Bool;
pub type ConfigSetItemFn = unsafe extern "C" fn(
    config: *mut RimeConfig,
    key: *const c_char,
    value: *mut RimeConfig,
) -> Bool;
pub type ConfigClearFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char) -> Bool;
pub type ConfigCreateListFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char) -> Bool;
pub type ConfigCreateMapFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char) -> Bool;
pub type ConfigListSizeFn =
    unsafe extern "C" fn(config: *mut RimeConfig, key: *const c_char) -> usize;
pub type ConfigBeginListFn = unsafe extern "C" fn(
    iterator: *mut RimeConfigIterator,
    config: *mut RimeConfig,
    key: *const c_char,
) -> Bool;
pub type GetInputFn = unsafe extern "C" fn(session_id: RimeSessionId) -> *const c_char;
pub type GetCaretPosFn = unsafe extern "C" fn(session_id: RimeSessionId) -> usize;
pub type SelectCandidateFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type GetVersionFn = unsafe extern "C" fn() -> *const c_char;
pub type SetCaretPosFn = unsafe extern "C" fn(session_id: RimeSessionId, caret_pos: usize);
pub type SelectCandidateOnCurrentPageFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type HighlightCandidateFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type HighlightCandidateOnCurrentPageFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type ChangePageFn = unsafe extern "C" fn(session_id: RimeSessionId, backward: Bool) -> Bool;
pub type CandidateListBeginFn =
    unsafe extern "C" fn(session_id: RimeSessionId, iterator: *mut RimeCandidateListIterator) -> Bool;
pub type CandidateListNextFn =
    unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator) -> Bool;
pub type CandidateListEndFn = unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator);
pub type UserConfigOpenFn =
    unsafe extern "C" fn(config_id: *const c_char, config: *mut RimeConfig) -> Bool;
pub type CandidateListFromIndexFn =
    unsafe extern "C" fn(session_id: RimeSessionId, iterator: *mut RimeCandidateListIterator, index: c_int) -> Bool;
pub type GetPrebuiltDataDirFn = unsafe extern "C" fn() -> *const c_char;
pub type GetStagingDirFn = unsafe extern "C" fn() -> *const c_char;
/// RIME_PROTO_BUILDER = void（librime-proto 插件用，core 不实现）
pub type CommitProtoFn = unsafe extern "C" fn(session_id: RimeSessionId, builder: *mut c_void);
pub type ContextProtoFn = unsafe extern "C" fn(session_id: RimeSessionId, builder: *mut c_void);
pub type StatusProtoFn = unsafe extern "C" fn(session_id: RimeSessionId, builder: *mut c_void);
pub type GetStateLabelFn =
    unsafe extern "C" fn(session_id: RimeSessionId, option_name: *const c_char, state: Bool) -> *const c_char;
pub type DeleteCandidateFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type DeleteCandidateOnCurrentPageFn =
    unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> Bool;
pub type GetStateLabelAbbreviatedFn =
    unsafe extern "C" fn(session_id: RimeSessionId, option_name: *const c_char, state: Bool, abbreviated: Bool) -> RimeStringSlice;
pub type SetInputFn = unsafe extern "C" fn(session_id: RimeSessionId, input: *const c_char) -> Bool;
pub type GetSharedDataDirSFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);
pub type GetUserDataDirSFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);
pub type GetPrebuiltDataDirSFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);
pub type GetStagingDirSFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);
pub type GetSyncDirSFn = unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize);

/// C: `RimeApi`。字段顺序 = rime_api.h 259–508 行，截取至 `simulate_key_sequence`。
#[repr(C)]
pub struct RimeApi {
    pub data_size: c_int,
    pub setup: Option<SetupFn>,
    pub set_notification_handler: Option<SetNotificationHandlerFn>,
    pub initialize: Option<InitializeFn>,
    pub finalize: Option<FinalizeFn>,
    pub start_maintenance: Option<StartMaintenanceFn>,
    pub is_maintenance_mode: Option<IsMaintenanceModeFn>,
    pub join_maintenance_thread: Option<JoinMaintenanceThreadFn>,
    pub deployer_initialize: Option<DeployerInitializeFn>,
    pub prebuild: Option<PrebuildFn>,
    pub deploy: Option<DeployFn>,
    pub deploy_schema: Option<DeploySchemaFn>,
    pub deploy_config_file: Option<DeployConfigFileFn>,
    pub sync_user_data: Option<SyncUserDataFn>,
    pub create_session: Option<CreateSessionFn>,
    pub find_session: Option<FindSessionFn>,
    pub destroy_session: Option<DestroySessionFn>,
    pub cleanup_stale_sessions: Option<CleanupStaleSessionsFn>,
    pub cleanup_all_sessions: Option<CleanupAllSessionsFn>,
    pub process_key: Option<ProcessKeyFn>,
    pub commit_composition: Option<CommitCompositionFn>,
    pub clear_composition: Option<ClearCompositionFn>,
    pub get_commit: Option<GetCommitFn>,
    pub free_commit: Option<FreeCommitFn>,
    pub get_context: Option<GetContextFn>,
    pub free_context: Option<FreeContextFn>,
    pub get_status: Option<GetStatusFn>,
    pub free_status: Option<FreeStatusFn>,
    pub set_option: Option<SetOptionFn>,
    pub get_option: Option<GetOptionFn>,
    pub set_property: Option<SetPropertyFn>,
    pub get_property: Option<GetPropertyFn>,
    pub get_schema_list: Option<GetSchemaListFn>,
    pub free_schema_list: Option<FreeSchemaListFn>,
    pub get_current_schema: Option<GetCurrentSchemaFn>,
    pub select_schema: Option<SelectSchemaFn>,
    pub schema_open: Option<SchemaOpenFn>,
    pub config_open: Option<ConfigOpenFn>,
    pub config_close: Option<ConfigCloseFn>,
    pub config_get_bool: Option<ConfigGetBoolFn>,
    pub config_get_int: Option<ConfigGetIntFn>,
    pub config_get_double: Option<ConfigGetDoubleFn>,
    pub config_get_string: Option<ConfigGetStringFn>,
    pub config_get_cstring: Option<ConfigGetCstringFn>,
    pub config_update_signature: Option<ConfigUpdateSignatureFn>,
    pub config_begin_map: Option<ConfigBeginMapFn>,
    pub config_next: Option<ConfigNextFn>,
    pub config_end: Option<ConfigEndFn>,
    pub simulate_key_sequence: Option<SimulateKeySequenceFn>,
    pub register_module: Option<RegisterModuleFn>,
    pub find_module: Option<FindModuleFn>,
    pub run_task: Option<RunTaskFn>,
    pub get_shared_data_dir: Option<GetSharedDataDirFn>,
    pub get_user_data_dir: Option<GetUserDataDirFn>,
    pub get_sync_dir: Option<GetSyncDirFn>,
    pub get_user_id: Option<GetUserIdFn>,
    pub get_user_data_sync_dir: Option<GetUserDataSyncDirFn>,
    pub config_init: Option<ConfigInitFn>,
    pub config_load_string: Option<ConfigLoadStringFn>,
    pub config_set_bool: Option<ConfigSetBoolFn>,
    pub config_set_int: Option<ConfigSetIntFn>,
    pub config_set_double: Option<ConfigSetDoubleFn>,
    pub config_set_string: Option<ConfigSetStringFn>,
    pub config_get_item: Option<ConfigGetItemFn>,
    pub config_set_item: Option<ConfigSetItemFn>,
    pub config_clear: Option<ConfigClearFn>,
    pub config_create_list: Option<ConfigCreateListFn>,
    pub config_create_map: Option<ConfigCreateMapFn>,
    pub config_list_size: Option<ConfigListSizeFn>,
    pub config_begin_list: Option<ConfigBeginListFn>,
    pub get_input: Option<GetInputFn>,
    pub get_caret_pos: Option<GetCaretPosFn>,
    pub select_candidate: Option<SelectCandidateFn>,
    pub get_version: Option<GetVersionFn>,
    pub set_caret_pos: Option<SetCaretPosFn>,
    pub select_candidate_on_current_page: Option<SelectCandidateOnCurrentPageFn>,
    // ---- v1.6+：select_candidate_on_current_page 之后的成员，严格按 rime_api.h 顺序 ----
    pub candidate_list_begin: Option<CandidateListBeginFn>,
    pub candidate_list_next: Option<CandidateListNextFn>,
    pub candidate_list_end: Option<CandidateListEndFn>,
    pub user_config_open: Option<UserConfigOpenFn>,
    pub candidate_list_from_index: Option<CandidateListFromIndexFn>,
    /// deprecated（返回静态串）
    pub get_prebuilt_data_dir: Option<GetPrebuiltDataDirFn>,
    /// deprecated（返回静态串）
    pub get_staging_dir: Option<GetStagingDirFn>,
    pub commit_proto: Option<CommitProtoFn>,
    pub context_proto: Option<ContextProtoFn>,
    pub status_proto: Option<StatusProtoFn>,
    pub get_state_label: Option<GetStateLabelFn>,
    pub delete_candidate: Option<DeleteCandidateFn>,
    pub delete_candidate_on_current_page: Option<DeleteCandidateOnCurrentPageFn>,
    pub get_state_label_abbreviated: Option<GetStateLabelAbbreviatedFn>,
    pub set_input: Option<SetInputFn>,
    pub get_shared_data_dir_s: Option<GetSharedDataDirSFn>,
    pub get_user_data_dir_s: Option<GetUserDataDirSFn>,
    pub get_prebuilt_data_dir_s: Option<GetPrebuiltDataDirSFn>,
    pub get_staging_dir_s: Option<GetStagingDirSFn>,
    pub get_sync_dir_s: Option<GetSyncDirSFn>,
    /// v1.6 尾部成员（data_size 版本化探测）
    pub highlight_candidate: Option<HighlightCandidateFn>,
    pub highlight_candidate_on_current_page: Option<HighlightCandidateOnCurrentPageFn>,
    pub change_page: Option<ChangePageFn>,
}

unsafe extern "C" {
    /// C: `RimeApi* rime_get_api(void);` —— librime 的唯一 API 入口
    pub fn rime_get_api() -> *mut RimeApi;
}

impl RimeTraits {
    pub fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
    /// 等价 `RIME_STRUCT_INIT(RimeTraits, var)`
    pub fn init_data_size(&mut self) {
        self.data_size =
            (std::mem::size_of::<Self>() - std::mem::size_of::<c_int>()) as c_int;
    }
}

impl RimeCommit {
    pub fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
    pub fn init_data_size(&mut self) {
        self.data_size =
            (std::mem::size_of::<Self>() - std::mem::size_of::<c_int>()) as c_int;
    }
}

impl RimeContext {
    pub fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
    pub fn init_data_size(&mut self) {
        self.data_size =
            (std::mem::size_of::<Self>() - std::mem::size_of::<c_int>()) as c_int;
    }
}

impl RimeStatus {
    pub fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
    pub fn init_data_size(&mut self) {
        self.data_size =
            (std::mem::size_of::<Self>() - std::mem::size_of::<c_int>()) as c_int;
    }
}
