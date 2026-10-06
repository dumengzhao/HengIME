//! librime 的安全封装。
//!
//! 设计要点（对应 docs/ARCHITECTURE.md 第 10.2 节）：
//! - librime 的 api 表进程内全局唯一（`rime_get_api` 幂等），`Engine` 直接持有引用
//! - 字符串内存归 librime：读出后立即转 `String`，再用 `free_commit` / `free_context` 释放
//! - 三段式异步初始化：`setup` → `initialize` → `start_maintenance` + `join_maintenance_thread`
//! - **所有按键/取态操作以裸 `RimeSessionId` 为参数放在 `Engine` 上**：
//!   C ABI 与 HTTP 层手里只有裸会话 ID（配合 `global::SESSIONS` 注册表），
//!   `Session` 只是给纯 Rust 侧用的便利薄壳。

mod rime_ffi;

pub(crate) use rime_ffi::{RimeConfig, RimeConfigIterator};

use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::path::PathBuf;

pub use rime_ffi::RimeSessionId;

#[derive(Debug)]
pub enum EngineError {
    Io(std::io::Error),
    ApiMissing(&'static str),
    ApiCall(&'static str),
    NoSession,
    SessionNotFound(RimeSessionId),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Io(e) => write!(f, "IO 错误: {e}"),
            EngineError::ApiMissing(name) => {
                write!(f, "librime API 缺少 {name}（版本不兼容？）")
            }
            EngineError::ApiCall(name) => write!(f, "librime 调用 {name} 失败"),
            EngineError::NoSession => write!(f, "创建会话失败"),
            EngineError::SessionNotFound(id) => write!(f, "会话不存在: {id}"),
        }
    }
}

impl std::error::Error for EngineError {}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct EngineConfig {
    pub shared_data_dir: Option<PathBuf>,
    pub user_data_dir: PathBuf,
    pub min_log_level: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Candidate {
    pub text: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContextSnapshot {
    pub preedit: String,
    pub cursor_pos: i32,
    pub sel_start: i32,
    pub sel_end: i32,
    pub page_size: i32,
    pub page_no: i32,
    pub is_last_page: bool,
    pub highlighted: i32,
    pub select_keys: String,
    pub candidates: Vec<Candidate>,
    /// 选键标签（select_labels，librime v0.9.2+；缺省时调用方回退 select_keys/序号）
    pub select_labels: Vec<String>,
    pub commit_text_preview: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StatusSnapshot {
    pub schema_id: String,
    pub schema_name: String,
    pub is_disabled: bool,
    pub is_composing: bool,
    pub is_ascii_mode: bool,
    pub is_full_shape: bool,
    pub is_simplified: bool,
    pub is_traditional: bool,
    pub is_ascii_punct: bool,
}

unsafe fn cstr_to_string(p: *const std::os::raw::c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}

unsafe extern "C" fn on_notify(
    _context: *mut c_void,
    _session: RimeSessionId,
    message_type: *const std::os::raw::c_char,
    message_value: *const std::os::raw::c_char,
) {
    let t = cstr_to_string(message_type).unwrap_or_default();
    let v = cstr_to_string(message_value).unwrap_or_default();
    eprintln!("[rime] {t}: {v}");
}

/// librime 引擎。进程内全局单例语义：创建多个 `Engine` 会重复初始化，M0 阶段约定每进程一个。
///
/// 线程安全：`api` 指向 librime 的进程级函数指针表（`rime_get_api` 幂等），跨线程共享安全；
/// 但 librime 的会话操作本身不保证线程安全，调用方须串行化（见 `global::OP_LOCK`）。
pub struct Engine {
    api: *const rime_ffi::RimeApi,
    user_data_dir: std::path::PathBuf,
}

unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Engine, EngineError> {
        std::fs::create_dir_all(&config.user_data_dir).map_err(EngineError::Io)?;
        if let Some(dir) = &config.shared_data_dir {
            std::fs::create_dir_all(dir).map_err(EngineError::Io)?;
        }

        let api = unsafe { rime_ffi::rime_get_api() };
        if api.is_null() {
            return Err(EngineError::ApiMissing("rime_get_api"));
        }
        let api = unsafe { &*api };

        // 函数可用性探测：data_size 版本化机制（借自 librime / Keyman Core 的手法）
        fn need<'a, T>(opt: &'a Option<T>, name: &'static str) -> Result<&'a T, EngineError> {
            opt.as_ref().ok_or(EngineError::ApiMissing(name))
        }
        let setup = need(&api.setup, "setup")?;
        let set_notification_handler = need(&api.set_notification_handler, "set_notification_handler")?;
        let initialize = need(&api.initialize, "initialize")?;
        let start_maintenance = need(&api.start_maintenance, "start_maintenance")?;
        let join_maintenance_thread = need(&api.join_maintenance_thread, "join_maintenance_thread")?;
        let _create_session = need(&api.create_session, "create_session")?;

        // CString 必须活过 setup 与 initialize 两次调用
        let shared = config
            .shared_data_dir
            .as_ref()
            .map(|p| CString::new(p.to_string_lossy().as_bytes()))
            .transpose()
            .map_err(|_| EngineError::ApiCall("shared_data_dir 含非法字符"))?;
        let user = CString::new(config.user_data_dir.to_string_lossy().as_bytes())
            .map_err(|_| EngineError::ApiCall("user_data_dir 含非法字符"))?;
        let dist_name = CString::new("HengIME").unwrap();
        let dist_code = CString::new("heng").unwrap();
        let dist_ver = CString::new(env!("CARGO_PKG_VERSION")).unwrap();
        let app_name = CString::new("rime.heng").unwrap();

        let mut traits = rime_ffi::RimeTraits::zeroed();
        traits.init_data_size();
        traits.shared_data_dir = shared.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        traits.user_data_dir = user.as_ptr();
        traits.distribution_name = dist_name.as_ptr();
        traits.distribution_code_name = dist_code.as_ptr();
        traits.distribution_version = dist_ver.as_ptr();
        traits.app_name = app_name.as_ptr();
        traits.min_log_level = config.min_log_level;

        unsafe {
            setup(&mut traits);
            set_notification_handler(Some(on_notify), std::ptr::null_mut());
            initialize(&mut traits);
            // full_check = TRUE：启动时检查配置/方案 mtime 变化并自动重新部署
            start_maintenance(rime_ffi::TRUE);
            join_maintenance_thread();
        }

        // 对齐官方 WeaselDeployer（Configurator.cpp: deploy_config_file("weasel.yaml")）
        // 与官方 Squirrel（SquirrelApplicationDelegate.startRime: deploy_config_file("squirrel.yaml")）：
        // librime 的 "config" 组件只读 staging 目录，标准部署只处理 default.yaml 与
        // 各方案，外壳样式配置 weasel.yaml / squirrel.yaml 必须显式编译进 staging 才能
        // 被 config_open 读到。shared 目录无对应文件时失败返回 False，无害。
        if let Some(deploy_config_file) = api.deploy_config_file.as_ref() {
            for name in ["weasel.yaml", "squirrel.yaml", "heng.yaml"] {
                let name = CString::new(name).unwrap();
                let version_key = CString::new("config_version").unwrap();
                unsafe { deploy_config_file(name.as_ptr(), version_key.as_ptr()) };
            }
        }

        Ok(Engine {
            api: api as *const rime_ffi::RimeApi,
            user_data_dir: config.user_data_dir,
        })
    }

    fn api(&self) -> &rime_ffi::RimeApi {
        unsafe { &*self.api }
    }

    /// 用户数据目录（选项持久化文件 heng_options.yaml 的落点）
    pub fn user_data_dir(&self) -> &std::path::Path {
        &self.user_data_dir
    }

    /// 重新部署（full_check）：设置中心写入 custom.yaml patch 后调用。
    /// 阻塞至部署完成（毫秒级），期间 session 全部重建。
    pub fn redeploy(&self) {
        if let (Some(sm), Some(join)) = (
            self.api().start_maintenance.as_ref(),
            self.api().join_maintenance_thread.as_ref(),
        ) {
            unsafe {
                sm(rime_ffi::TRUE);
                join();
            }
        }
    }

    pub fn create_session(&self) -> Result<Session<'_>, EngineError> {
        let create = self
            .api()
            .create_session
            .ok_or(EngineError::ApiMissing("create_session"))?;
        let id = unsafe { create() };
        if id == 0 {
            return Err(EngineError::NoSession);
        }
        Ok(Session {
            engine: self,
            id,
            closed: false,
        })
    }

    /// 直接销毁一个 librime 会话（供会话注册表使用，不经过 [`Session`]）。
    pub(crate) fn destroy_session(&self, id: RimeSessionId) {
        if let Some(destroy) = self.api().destroy_session.as_ref() {
            unsafe { destroy(id) };
        }
    }

    pub fn version(&self) -> Option<String> {
        let api = self.api();
        let get_version = api.get_version.as_ref()?;
        unsafe { cstr_to_string(get_version()) }
    }

    /// 进程退出前调用。调用后本进程内不得再使用任何 librime 接口。
    pub fn finalize(&self) {
        if let Some(finalize) = self.api().finalize.as_ref() {
            unsafe { finalize() };
        }
    }

    pub fn schema_list(&self) -> Vec<(String, String)> {
        let api = self.api();
        let (Some(get_list), Some(free_list)) =
            (api.get_schema_list.as_ref(), api.free_schema_list.as_ref())
        else {
            return Vec::new();
        };
        unsafe {
            let mut list = rime_ffi::RimeSchemaList {
                size: 0,
                list: std::ptr::null_mut(),
            };
            if get_list(&mut list) == rime_ffi::FALSE {
                return Vec::new();
            }
            let mut out = Vec::with_capacity(list.size);
            for i in 0..list.size {
                let item = list.list.add(i);
                let id = cstr_to_string((*item).schema_id).unwrap_or_default();
                let name = cstr_to_string((*item).name).unwrap_or_default();
                out.push((id, name));
            }
            free_list(&mut list);
            out
        }
    }

    // ---------- 以下为裸会话 ID 操作（C ABI / HTTP 层使用） ----------

    pub fn process_key(
        &self,
        id: RimeSessionId,
        keycode: std::os::raw::c_int,
        modifier: std::os::raw::c_int,
    ) -> Result<bool, EngineError> {
        let process = self
            .api()
            .process_key
            .ok_or(EngineError::ApiMissing("process_key"))?;
        Ok(unsafe { process(id, keycode, modifier) == rime_ffi::TRUE })
    }

    pub fn simulate_key_sequence(
        &self,
        id: RimeSessionId,
        key_sequence: &str,
    ) -> Result<bool, EngineError> {
        let simulate = self
            .api()
            .simulate_key_sequence
            .ok_or(EngineError::ApiMissing("simulate_key_sequence"))?;
        let seq = CString::new(key_sequence)
            .map_err(|_| EngineError::ApiCall("按键序列含非法字符"))?;
        Ok(unsafe { simulate(id, seq.as_ptr()) == rime_ffi::TRUE })
    }

    pub fn get_context(&self, id: RimeSessionId) -> Result<ContextSnapshot, EngineError> {
        let api = self.api();
        let get_context = api
            .get_context
            .ok_or(EngineError::ApiMissing("get_context"))?;
        let free_context = api
            .free_context
            .ok_or(EngineError::ApiMissing("free_context"))?;
        unsafe {
            let mut ctx = rime_ffi::RimeContext::zeroed();
            ctx.init_data_size();
            if get_context(id, &mut ctx) == rime_ffi::FALSE {
                free_context(&mut ctx);
                return Err(EngineError::SessionNotFound(id));
            }
            let snapshot = Self::snapshot_context(&ctx);
            free_context(&mut ctx);
            Ok(snapshot)
        }
    }

    /// 取回并消费已产生的上屏文本。空字符串表示本次调用无上屏内容。
    pub fn get_commit(&self, id: RimeSessionId) -> Result<String, EngineError> {
        let api = self.api();
        let get_commit = api
            .get_commit
            .ok_or(EngineError::ApiMissing("get_commit"))?;
        let free_commit = api
            .free_commit
            .ok_or(EngineError::ApiMissing("free_commit"))?;
        unsafe {
            let mut commit = rime_ffi::RimeCommit::zeroed();
            commit.init_data_size();
            if get_commit(id, &mut commit) == rime_ffi::FALSE {
                free_commit(&mut commit);
                return Ok(String::new());
            }
            let text = cstr_to_string(commit.text).unwrap_or_default();
            free_commit(&mut commit);
            Ok(text)
        }
    }

    pub fn get_status(&self, id: RimeSessionId) -> Result<StatusSnapshot, EngineError> {
        let api = self.api();
        let get_status = api
            .get_status
            .ok_or(EngineError::ApiMissing("get_status"))?;
        let free_status = api
            .free_status
            .ok_or(EngineError::ApiMissing("free_status"))?;
        unsafe {
            let mut st = rime_ffi::RimeStatus::zeroed();
            st.init_data_size();
            if get_status(id, &mut st) == rime_ffi::FALSE {
                free_status(&mut st);
                return Err(EngineError::SessionNotFound(id));
            }
            let snapshot = StatusSnapshot {
                schema_id: cstr_to_string(st.schema_id).unwrap_or_default(),
                schema_name: cstr_to_string(st.schema_name).unwrap_or_default(),
                is_disabled: st.is_disabled != rime_ffi::FALSE,
                is_composing: st.is_composing != rime_ffi::FALSE,
                is_ascii_mode: st.is_ascii_mode != rime_ffi::FALSE,
                is_full_shape: st.is_full_shape != rime_ffi::FALSE,
                is_simplified: st.is_simplified != rime_ffi::FALSE,
                is_traditional: st.is_traditional != rime_ffi::FALSE,
                is_ascii_punct: st.is_ascii_punct != rime_ffi::FALSE,
            };
            free_status(&mut st);
            Ok(snapshot)
        }
    }

    pub fn clear_composition(&self, id: RimeSessionId) -> Result<(), EngineError> {
        let clear = self
            .api()
            .clear_composition
            .ok_or(EngineError::ApiMissing("clear_composition"))?;
        unsafe { clear(id) };
        Ok(())
    }

    /// 在当前页直接选中第 `index` 个候选。返回 Ok(false) 表示未选中（越界/无候选）。
    pub fn select_candidate_on_current_page(
        &self,
        id: RimeSessionId,
        index: usize,
    ) -> Result<bool, EngineError> {
        let select = self
            .api()
            .select_candidate_on_current_page
            .ok_or(EngineError::ApiMissing("select_candidate_on_current_page"))?;
        Ok(unsafe { select(id, index) == rime_ffi::TRUE })
    }

    /// 遍历全部候选页收集所有候选词，完成后回到原页。
    /// 自绘候选窗「展开更多」用。返回 (候选文本列表, 每页条数)。
    pub fn get_all_candidates(
        &self,
        id: RimeSessionId,
    ) -> Result<(Vec<String>, i32), EngineError> {
        const XK_PAGE_DOWN: i32 = 0xff56;
        const XK_PAGE_UP: i32 = 0xff55;
        let first = self.get_context(id)?;
        let page_size = first.page_size.max(first.candidates.len() as i32).max(1);
        let mut all: Vec<String> = first
            .candidates
            .iter()
            .filter(|c| !c.text.is_empty())
            .map(|c| c.text.clone())
            .collect();
        // 翻页循环方案（如雾凇 Page_Down 绕回首页）需要记录已访问页号防绕圈；
        // 注意 visited 不能在翻页前检查（当前页必在集合里 → 循环必立即退出，
        // 只会收集到第一页——这正是"展开后词少"的根因），只在翻页后检查新页。
        // 总量封顶防异常方案
        let mut visited: std::collections::HashSet<i32> =
            std::collections::HashSet::from([first.page_no]);
        let mut walked = 0i32;
        loop {
            let snap = self.get_context(id)?;
            if snap.candidates.is_empty() || snap.is_last_page {
                break;
            }
            if !self.process_key(id, XK_PAGE_DOWN, 0)? {
                break;
            }
            walked += 1;
            let after = self.get_context(id)?;
            if after.candidates.is_empty() || visited.contains(&after.page_no) {
                break; // 没翻动（到末页）或绕回已访问页：循环方案，停止
            }
            visited.insert(after.page_no);
            all.extend(
                after
                    .candidates
                    .iter()
                    .filter(|c| !c.text.is_empty())
                    .map(|c| c.text.clone()),
            );
            if all.len() > 60 {
                break;
            }
        }
        for _ in 0..walked {
            let _ = self.process_key(id, XK_PAGE_UP, 0)?;
        }
        Ok((all, page_size))
    }

    /// 单行横条候选（微信同款尽量充满一行）：**锚定菜单第一页**收集，凑满
    /// want 个，取完翻回原页（翻页保序已用 pagetest 验证：PageDown/Up 不改
    /// 页内高亮位）。锚定保证 ←→ 跨页移动高亮时列表不滚动（原第 6 个词不会
    /// 变成第 1 个）。返回 (候选列表, 高亮全局序号, 列表首项全局序号)。
    pub fn get_bar_candidates(
        &self,
        id: RimeSessionId,
        want: usize,
    ) -> Result<(Vec<String>, i32, i32), EngineError> {
        const XK_PAGE_DOWN: i32 = 0xff56;
        const XK_PAGE_UP: i32 = 0xff55;
        let first = self.get_context(id)?;
        let page_size = first.page_size.max(first.candidates.len() as i32).max(1);
        let orig_page = first.page_no;
        let hl_page = first.highlighted.max(0);
        let hl_global = orig_page * page_size + hl_page;
        // 上翻到菜单第一页
        for _ in 0..orig_page {
            if !self.process_key(id, XK_PAGE_UP, 0)? {
                break;
            }
        }
        let anchor = self.get_context(id)?;
        let base = anchor.page_no * page_size;
        let mut items: Vec<String> = anchor
            .candidates
            .iter()
            .filter(|c| !c.text.is_empty())
            .map(|c| c.text.clone())
            .collect();
        // 向后收集到 want 与「覆盖高亮所在页再多一页」的较大者
        let target = want.max(((orig_page + 2) * page_size) as usize);
        let mut fwd = 0i32;
        while items.len() < target && fwd <= 8 {
            let snap = self.get_context(id)?;
            if snap.is_last_page {
                break;
            }
            if !self.process_key(id, XK_PAGE_DOWN, 0)? {
                break;
            }
            fwd += 1;
            let after = self.get_context(id)?;
            if after.candidates.is_empty() {
                break;
            }
            items.extend(
                after
                    .candidates
                    .iter()
                    .filter(|c| !c.text.is_empty())
                    .map(|c| c.text.clone()),
            );
        }
        // 翻回原页（当前在 anchor.page_no + fwd）
        let cur_page = anchor.page_no + fwd;
        if cur_page >= orig_page {
            for _ in 0..(cur_page - orig_page) {
                let _ = self.process_key(id, XK_PAGE_UP, 0)?;
            }
        } else {
            for _ in 0..(orig_page - cur_page) {
                let _ = self.process_key(id, XK_PAGE_DOWN, 0)?;
            }
        }
        Ok((items, hl_global, base))
    }

    /// 按全局序号选候选（跨页）：翻到目标页后在页内选中。
    pub fn select_candidate_global(
        &self,
        id: RimeSessionId,
        index: usize,
    ) -> Result<bool, EngineError> {
        const XK_PAGE_DOWN: i32 = 0xff56;
        const XK_PAGE_UP: i32 = 0xff55;
        let snap = self.get_context(id)?;
        let page_size = snap.page_size.max(snap.candidates.len() as i32).max(1);
        let target = (index as i32) / page_size;
        let mut walked = 0i32;
        loop {
            let snap = self.get_context(id)?;
            if snap.page_no == target {
                break;
            }
            let keysym = if target > snap.page_no { XK_PAGE_DOWN } else { XK_PAGE_UP };
            if !self.process_key(id, keysym, 0)? {
                break;
            }
            walked += 1;
            if walked > 100 {
                break;
            }
        }
        self.select_candidate_on_current_page(id, ((index as i32) % page_size) as usize)
    }

    /// 高亮当前页第 `index` 个候选（不提交；候选窗移动光标用）。
    pub fn highlight_candidate_on_current_page(
        &self,
        id: RimeSessionId,
        index: usize,
    ) -> Result<bool, EngineError> {
        let highlight = self
            .api()
            .highlight_candidate_on_current_page
            .ok_or(EngineError::ApiMissing("highlight_candidate_on_current_page"))?;
        Ok(unsafe { highlight(id, index) == rime_ffi::TRUE })
    }

    /// 高亮全局候选序号（跨页，librime 内部换页）。
    pub fn highlight_candidate(
        &self,
        id: RimeSessionId,
        index: usize,
    ) -> Result<bool, EngineError> {
        let highlight = self
            .api()
            .highlight_candidate
            .ok_or(EngineError::ApiMissing("highlight_candidate"))?;
        Ok(unsafe { highlight(id, index) == rime_ffi::TRUE })
    }

    /// 翻页（backward = true 向前）。
    pub fn change_page(&self, id: RimeSessionId, backward: bool) -> Result<bool, EngineError> {
        let change = self
            .api()
            .change_page
            .ok_or(EngineError::ApiMissing("change_page"))?;
        Ok(unsafe { change(id, backward.into()) == rime_ffi::TRUE })
    }

    /// 设置会话选项（如 ascii_mode / inline_preedit / vim_mode）。
    pub fn set_option(&self, id: RimeSessionId, option: &str, value: bool) -> Result<(), EngineError> {
        let set = self
            .api()
            .set_option
            .ok_or(EngineError::ApiMissing("set_option"))?;
        let name =
            CString::new(option).map_err(|_| EngineError::ApiCall("选项名含非法字符"))?;
        unsafe { set(id, name.as_ptr(), value.into()) };
        Ok(())
    }

    /// 读取会话选项。返回 Ok(None) 表示 librime API 不可用或调用失败。
    pub fn get_option(&self, id: RimeSessionId, option: &str) -> Result<bool, EngineError> {
        let get = self
            .api()
            .get_option
            .ok_or(EngineError::ApiMissing("get_option"))?;
        let name =
            CString::new(option).map_err(|_| EngineError::ApiCall("选项名含非法字符"))?;
        Ok(unsafe { get(id, name.as_ptr()) != rime_ffi::FALSE })
    }

    // ---------- 配置读取（librime RimeConfig 封装，供外壳加载样式/app_options） ----------
    //
    // 句柄语义：`RimeConfig.ptr` 直接透传给调用方（heng_config_t），
    // open/close/get 之间的句柄必须配对使用；librime 会校验内部有效性。
    // 所有调用与 librime 部署线程共享全局状态，须在 OP_LOCK 下执行（capi 层负责）。

    /// 打开一个已部署/原始配置（如 "weasel"、"default"）。失败返回全零 RimeConfig。
    pub fn config_open(&self, config_id: &str) -> Result<rime_ffi::RimeConfig, EngineError> {
        let open = self
            .api()
            .config_open
            .ok_or(EngineError::ApiMissing("config_open"))?;
        let id =
            CString::new(config_id).map_err(|_| EngineError::ApiCall("config_id 含非法字符"))?;
        let mut config = rime_ffi::RimeConfig {
            ptr: std::ptr::null_mut(),
        };
        let ok = unsafe { open(id.as_ptr(), &mut config) == rime_ffi::TRUE };
        if ok {
            Ok(config)
        } else {
            Err(EngineError::ApiCall("config_open"))
        }
    }

    /// 关闭配置并释放 librime 侧资源。
    pub fn config_close(&self, config: &mut rime_ffi::RimeConfig) {
        if let Some(close) = self.api().config_close.as_ref() {
            unsafe { close(config) };
        }
        config.ptr = std::ptr::null_mut();
    }

    /// 读字符串。返回 Some(len)：buf 足够时已写入并含 NUL；buf 不足时仅返回所需长度（len+1）。
    pub fn config_get_string(
        &self,
        config: &rime_ffi::RimeConfig,
        key: &str,
        buf: &mut [u8],
    ) -> Option<usize> {
        let get_cstr = self.api().config_get_cstring.as_ref()?;
        let k = CString::new(key).ok()?;
        unsafe {
            let p = get_cstr(config as *const _ as *mut _, k.as_ptr());
            if p.is_null() {
                return None;
            }
            let len = CStr::from_ptr(p).count_bytes();
            if buf.len() > len {
                std::ptr::copy_nonoverlapping(p as *const u8, buf.as_mut_ptr(), len + 1);
            }
            Some(len)
        }
    }

    /// 读整数。None = 键不存在。
    pub fn config_get_int(&self, config: &rime_ffi::RimeConfig, key: &str) -> Option<i32> {
        let get = self.api().config_get_int.as_ref()?;
        let k = CString::new(key).ok()?;
        let mut value: std::os::raw::c_int = 0;
        let ok = unsafe { get(config as *const _ as *mut _, k.as_ptr(), &mut value) };
        (ok == rime_ffi::TRUE).then_some(value)
    }

    /// 读布尔。None = 键不存在。
    pub fn config_get_bool(&self, config: &rime_ffi::RimeConfig, key: &str) -> Option<bool> {
        let get = self.api().config_get_bool.as_ref()?;
        let k = CString::new(key).ok()?;
        let mut value: rime_ffi::Bool = 0;
        let ok = unsafe { get(config as *const _ as *mut _, k.as_ptr(), &mut value) };
        (ok == rime_ffi::TRUE).then_some(value != rime_ffi::FALSE)
    }

    /// 读浮点。None = 键不存在。（v7：Squirrel 外壳读 chord_duration / 字号等）
    pub fn config_get_double(&self, config: &rime_ffi::RimeConfig, key: &str) -> Option<f64> {
        let get = self.api().config_get_double.as_ref()?;
        let k = CString::new(key).ok()?;
        let mut value: f64 = 0.0;
        let ok = unsafe { get(config as *const _ as *mut _, k.as_ptr(), &mut value) };
        (ok == rime_ffi::TRUE).then_some(value)
    }

    /// 打开方案的已部署配置（RimeSchemaOpen，供外壳读方案级样式覆盖）。（v7）
    pub fn schema_open(&self, schema_id: &str) -> Result<rime_ffi::RimeConfig, EngineError> {
        let open = self
            .api()
            .schema_open
            .ok_or(EngineError::ApiMissing("schema_open"))?;
        let id =
            CString::new(schema_id).map_err(|_| EngineError::ApiCall("schema_id 含非法字符"))?;
        let mut config = rime_ffi::RimeConfig {
            ptr: std::ptr::null_mut(),
        };
        let ok = unsafe { open(id.as_ptr(), &mut config) == rime_ffi::TRUE };
        if ok {
            Ok(config)
        } else {
            Err(EngineError::ApiCall("schema_open"))
        }
    }

    /// 开始遍历 map 的直属子键。`iter` 指向调用方分配的 HengConfigIterator（布局同 librime）。
    ///
    /// # Safety
    /// `iter` 必须指向足够容纳 `RimeConfigIterator` 的可写内存。
    pub unsafe fn config_begin_map(
        &self,
        config: &rime_ffi::RimeConfig,
        key: &str,
        iter: *mut rime_ffi::RimeConfigIterator,
    ) -> bool {
        let Some(begin) = self.api().config_begin_map.as_ref() else {
            return false;
        };
        let Ok(k) = CString::new(key) else {
            return false;
        };
        std::ptr::write_bytes(iter, 0, 1);
        begin(iter, config as *const _ as *mut _, k.as_ptr()) == rime_ffi::TRUE
    }

    /// # Safety
    /// `iter` 必须是 begin_map/next 链上的合法指针。
    pub unsafe fn config_next(&self, iter: *mut rime_ffi::RimeConfigIterator) -> bool {
        let Some(next) = self.api().config_next.as_ref() else {
            return false;
        };
        next(iter) == rime_ffi::TRUE
    }

    /// # Safety
    /// `iter` 必须是 begin_map/next 链上的合法指针。
    pub unsafe fn config_end(&self, iter: *mut rime_ffi::RimeConfigIterator) {
        if let Some(end) = self.api().config_end.as_ref() {
            end(iter);
        }
    }

    /// 提交当前组合串（上屏原始输入或已转换文本）。返回是否产生了提交。
    pub fn commit_composition(&self, id: RimeSessionId) -> Result<bool, EngineError> {
        let commit = self
            .api()
            .commit_composition
            .ok_or(EngineError::ApiMissing("commit_composition"))?;
        Ok(unsafe { commit(id) == rime_ffi::TRUE })
    }

    // ---------- 会话辅助（v7：Squirrel 外壳所需） ----------

    /// 当前组合串的原始输入（如拼音串）。Err = 无组合。
    pub fn get_input(&self, id: RimeSessionId) -> Result<String, EngineError> {
        let get = self
            .api()
            .get_input
            .ok_or(EngineError::ApiMissing("get_input"))?;
        let ptr = unsafe { get(id) };
        if ptr.is_null() {
            return Err(EngineError::ApiCall("get_input"));
        }
        Ok(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
    }

    /// 组合串内光标位置（UTF-8 字节偏移）。
    pub fn get_caret_pos(&self, id: RimeSessionId) -> Result<usize, EngineError> {
        let get = self
            .api()
            .get_caret_pos
            .ok_or(EngineError::ApiMissing("get_caret_pos"))?;
        Ok(unsafe { get(id) })
    }

    /// 设置组合串内光标位置（UTF-8 字节偏移）。
    pub fn set_caret_pos(&self, id: RimeSessionId, pos: usize) -> Result<(), EngineError> {
        let set = self
            .api()
            .set_caret_pos
            .ok_or(EngineError::ApiMissing("set_caret_pos"))?;
        unsafe { set(id, pos) };
        Ok(())
    }

    /// 用户数据同步（Rime sync 机制）。返回是否成功。
    pub fn sync_user_data(&self) -> Result<bool, EngineError> {
        let sync = self
            .api()
            .sync_user_data
            .ok_or(EngineError::ApiMissing("sync_user_data"))?;
        Ok(unsafe { sync() == rime_ffi::TRUE })
    }

    /// 选项状态标签（abbreviated=true 取短标签，用于菜单栏图标）。None = 无标签。
    pub fn get_state_label_abbreviated(
        &self,
        id: RimeSessionId,
        option: &str,
        state: bool,
        abbreviated: bool,
    ) -> Option<String> {
        let get = self.api().get_state_label_abbreviated.as_ref()?;
        let k = CString::new(option).ok()?;
        let slice = unsafe {
            get(
                id,
                k.as_ptr(),
                state as rime_ffi::Bool,
                abbreviated as rime_ffi::Bool,
            )
        };
        if slice.str.is_null() || slice.length == 0 {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(slice.str as *const u8, slice.length) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// 从零化的 `RimeContext` 提取快照（get_context 成功后调用）。
    ///
    /// # Safety
    /// `ctx` 必须是刚被 `get_context` 填充过的合法引用。
    unsafe fn snapshot_context(ctx: &rime_ffi::RimeContext) -> ContextSnapshot {
        let composition = &ctx.composition;
        let menu = &ctx.menu;

        let mut candidates = Vec::new();
        if !menu.candidates.is_null() {
            for i in 0..menu.num_candidates as usize {
                let c = menu.candidates.add(i);
                candidates.push(Candidate {
                    text: cstr_to_string((*c).text).unwrap_or_default(),
                    comment: cstr_to_string((*c).comment),
                });
            }
        }

        // 等价 RIME_STRUCT_HAS_MEMBER(var, select_labels) && select_labels：
        // data_size 覆盖到该成员偏移且指针非空时才可读（librime v0.9.2+）
        let labels_end =
            std::mem::size_of::<std::os::raw::c_int>() + ctx.data_size.max(0) as usize;
        let has_labels = labels_end > std::mem::offset_of!(rime_ffi::RimeContext, select_labels)
            && !ctx.select_labels.is_null();
        let mut select_labels = Vec::new();
        if has_labels && menu.num_candidates > 0 {
            for i in 0..menu.num_candidates as usize {
                let label = ctx.select_labels.add(i);
                select_labels.push(cstr_to_string(*label).unwrap_or_default());
            }
        }

        ContextSnapshot {
            preedit: cstr_to_string(composition.preedit).unwrap_or_default(),
            cursor_pos: composition.cursor_pos,
            sel_start: composition.sel_start,
            sel_end: composition.sel_end,
            page_size: menu.page_size,
            page_no: menu.page_no,
            is_last_page: menu.is_last_page != rime_ffi::FALSE,
            highlighted: menu.highlighted_candidate_index,
            select_keys: cstr_to_string(menu.select_keys).unwrap_or_default(),
            candidates,
            select_labels,
            commit_text_preview: cstr_to_string(ctx.commit_text_preview),
        }
    }
}

/// 输入会话。librime 的会话持有组合串与候选状态。
///
/// 纯 Rust 侧的便利薄壳：所有实际操作委托给 [`Engine`] 的裸 ID 方法。
pub struct Session<'e> {
    engine: &'e Engine,
    id: RimeSessionId,
    closed: bool,
}

impl Session<'_> {
    /// 本会话在 librime 内部的裸会话 ID。
    pub fn rime_id(&self) -> RimeSessionId {
        self.id
    }

    /// 消费会话但**不销毁** librime 会话，交出裸 ID（供会话注册表接管）。
    pub fn into_raw(mut self) -> RimeSessionId {
        self.closed = true; // Drop 不再销毁
        self.id
    }

    /// 模拟一段按键序列，如 `nihao`、`nihao{space}`、`abc{BackSpace}d`。
    /// 返回 False 表示序列中有按键未被处理。
    pub fn simulate(&self, key_sequence: &str) -> bool {
        self.engine
            .simulate_key_sequence(self.id, key_sequence)
            .unwrap_or(false)
    }

    pub fn context(&self) -> Option<ContextSnapshot> {
        self.engine.get_context(self.id).ok()
    }

    pub fn commit_text(&self) -> Option<String> {
        match self.engine.get_commit(self.id) {
            Ok(text) if !text.is_empty() => Some(text),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<StatusSnapshot> {
        self.engine.get_status(self.id).ok()
    }

    pub fn clear(&self) {
        let _ = self.engine.clear_composition(self.id);
    }

    pub fn select_on_current_page(&self, index: usize) -> bool {
        self.engine
            .select_candidate_on_current_page(self.id, index)
            .unwrap_or(false)
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        if !self.closed {
            self.engine.destroy_session(self.id);
            self.closed = true;
        }
    }
}
