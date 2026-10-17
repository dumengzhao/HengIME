//! 用户词库同步（docs/SYNC-PLAN.md）。
//!
//! 一轮同步四个动作，顺序有讲究：
//!
//! 1. **拉取** —— 取 manifest，按 sha256 比对，把其他设备的快照下载到
//!    `sync/<other_id>/`
//! 2. **导出 + 合并** —— `heng_sync_user_data()`。librime 的 sync 会把
//!    `sync/` 下**所有**设备子目录的快照合并进本机 userdb（词条级、按每条
//!    `t=` 取新），再把本机 userdb 重新导出为 `sync/<self>/` 下的快照
//! 3. **上传** —— 把本机快照逐个 PUT（整份覆盖）
//! 4. **收尾** —— 成功清退避；有活动会话时整轮推迟（librime 的 sync 会
//!    `CleanupAllSessions`，输入中执行会打断会话），只留待合并标记
//!
//! 下载必须早于导出：librime 只合并「sync 目录里已存在的文件」，先导出后
//! 下载意味着新快照要等到下一轮才合并。
//!
//! 触发（§7）：桌面三端进程内定时器（默认 30 分钟）；移动端会话启动时惰性
//! 检查；另有设置页「立即同步」与 `heng-cli sync`。未到间隔不起任何请求。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::global::SESSIONS;

// ---- 常量 ----

/// 同步配置与状态标记的落点：`<user_dir>/.heng/`。
const CONFIG_DIR: &str = ".heng";
const CONFIG_FILE: &str = "sync.yaml";
/// 待合并标记：拉取到变更但因活动会话未能合并时写下，下次引擎就绪即补跑。
const PENDING_MERGE: &str = "pending-merge";
const DEFAULT_INTERVAL_SECS: i64 = 1800;
/// 被跳过/被推迟时的重试间隔（活动会话随时可能结束，不宜等满一个周期）。
const RETRY_SOON_SECS: i64 = 60;
const MAX_BACKOFF_SECS: i64 = 6 * 3600;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// 定时器睡眠上限：保证 run_asap 能在 30 秒内被唤醒。
const MAX_SLEEP_SECS: u64 = 30;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- 平台标识与 installation_id ----

pub fn platform_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_env = "ohos") {
        // OpenHarmony：target_os 为 linux，靠 target_env 区分
        "harmony"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// 主机名（小写、仅保留字母数字与 `-_`）。Windows 只有 COMPUTERNAME，
/// Unix 惯例 HOSTNAME；都取不到时 "unknown"。
pub fn hostname() -> String {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return sanitize_host(&v);
            }
        }
    }
    "unknown".into()
}

fn sanitize_host(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.len() > 24 {
        trimmed[..24].trim_end_matches('-').to_string()
    } else if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed
    }
}

/// `<platform>-<hostname>-<6位大写十六进制>`。
/// 取代 librime 自生成的 UUID：可读，且管理页面与本地同步目录都能直接看出
/// 是哪台机器（SYNC-PLAN §3）。
pub fn generate_installation_id() -> String {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("系统随机数源不可用");
    let n = u32::from_be_bytes(bytes) & 0x00ff_ffff;
    format!("{}-{}-{:06X}", platform_tag(), hostname(), n)
}

/// 是否已是 core 生成的格式（末段 6 位十六进制、至少三段）。
/// 用于把 librime 遗留的 UUID 换成可读 id，且不反复重写。
fn is_heng_installation_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() >= 3
        && parts
            .last()
            .map(|t| t.len() == 6 && t.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or(false)
}

/// 读 installation.yaml 的 `installation_id`；是 librime UUID 或缺格式时
/// 生成可读 id 并写回（保留文件其余键）。librime 的 InstallationUpdate 任务
/// 每次 sync 都会重读该文件并用作 `user_id`，快照即落到
/// `sync/<installation_id>/`。
pub fn ensure_installation_id(user_dir: &Path) -> String {
    let path = user_dir.join("installation.yaml");
    let mut map: serde_yaml::Mapping = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .and_then(|v| v.as_mapping().cloned())
        .unwrap_or_default();
    let existing = map
        .get(serde_yaml::Value::from("installation_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if is_heng_installation_id(&existing) {
        return existing;
    }
    let id = generate_installation_id();
    map.insert(
        serde_yaml::Value::from("installation_id"),
        serde_yaml::Value::from(id.clone()),
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_yaml::to_string(&serde_yaml::Value::Mapping(map)) {
        let _ = std::fs::write(&path, text);
    }
    id
}

// ---- 配置与状态 ----

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub username: String,
    /// 登录后签发的长期令牌。
    #[serde(default)]
    pub token: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_interval")]
    pub interval_secs: i64,
}

fn default_true() -> bool {
    true
}
fn default_interval() -> i64 {
    DEFAULT_INTERVAL_SECS
}

impl SyncConfig {
    fn is_ready(&self) -> bool {
        !self.server_url.is_empty() && !self.token.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncStatus {
    pub configured: bool,
    pub enabled: bool,
    pub server_url: String,
    pub username: String,
    pub installation_id: String,
    /// idle | running | ok | error | deferred | skipped | not_configured
    pub state: String,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_downloaded: i64,
    pub last_uploaded: i64,
    pub pending_merge: bool,
    pub next_attempt_at: i64,
    pub consecutive_failures: i64,
}

/// 同步结果（供 UI 与 CLI 区分「跳过」与「失败」）。
#[derive(Debug, Clone)]
pub enum SyncOutcome {
    Skipped,
    Deferred,
    Ok { downloaded: usize, uploaded: usize },
    Failed(String),
}

// ---- manifest（GET /api/sync/manifest 的响应） ----

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    pub name: String,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestDevice {
    pub installation_id: String,
    #[serde(default)]
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub devices: Vec<ManifestDevice>,
}

// ---- 共享状态 ----

struct TimerCtl {
    next_due: i64,
    run_asap: bool,
}

struct Shared {
    user_dir: PathBuf,
    config: Mutex<SyncConfig>,
    status: Mutex<SyncStatus>,
    ctl: Mutex<TimerCtl>,
    cv: Condvar,
    running: AtomicBool,
    timer_started: AtomicBool,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

impl Shared {
    fn new(user_dir: PathBuf) -> Self {
        // interval_secs 不能依赖 derive(Default)（i64 零值）：serde 的
        // `default = "default_interval"` 只在反序列化缺字段时生效
        let config = SyncConfig {
            interval_secs: DEFAULT_INTERVAL_SECS,
            ..SyncConfig::default()
        };
        Self {
            user_dir,
            config: Mutex::new(config),
            status: Mutex::new(SyncStatus::default()),
            ctl: Mutex::new(TimerCtl {
                next_due: now_unix(),
                run_asap: false,
            }),
            cv: Condvar::new(),
            running: AtomicBool::new(false),
            timer_started: AtomicBool::new(false),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.user_dir.join(CONFIG_DIR).join(CONFIG_FILE)
    }

    fn pending_path(&self) -> PathBuf {
        self.user_dir.join(CONFIG_DIR).join(PENDING_MERGE)
    }

    fn load_config(&self) {
        if let Ok(text) = std::fs::read_to_string(self.config_path()) {
            if let Ok(cfg) = serde_yaml::from_str::<SyncConfig>(&text) {
                *self.config.lock().unwrap() = cfg;
            }
        }
    }

    fn save_config(&self) -> std::io::Result<()> {
        let cfg = self.config.lock().unwrap().clone();
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 手写平面 YAML：字段少且稳定，不必为一次落盘引 toml/JSON 双写。
        let text = format!(
            "# HengIME 同步配置（core 自动维护，勿手改）\nserver_url: {}\nusername: {}\ntoken: {}\nenabled: {}\ninterval_secs: {}\n",
            yaml_quote(&cfg.server_url),
            yaml_quote(&cfg.username),
            yaml_quote(&cfg.token),
            cfg.enabled,
            cfg.interval_secs.max(60),
        );
        std::fs::write(path, text)
    }

    fn sync_root(&self) -> PathBuf {
        self.user_dir.join("sync")
    }

    fn status_snapshot(&self) -> SyncStatus {
        let mut st = self.status.lock().unwrap().clone();
        let cfg = self.config.lock().unwrap();
        st.configured = cfg.is_ready();
        st.enabled = cfg.enabled;
        st.server_url = cfg.server_url.clone();
        st.username = cfg.username.clone();
        st.pending_merge = self.pending_path().exists();
        st.next_attempt_at = self.ctl.lock().unwrap().next_due;
        st
    }

    fn set_state(&self, state: &str, err: Option<String>) {
        let mut st = self.status.lock().unwrap();
        st.state = state.into();
        st.last_error = err;
    }

    /// 请求定时器尽快跑一轮（设置页「立即同步」/ CLI / 活动会话结束补跑）。
    fn request_run(&self) {
        self.ctl.lock().unwrap().run_asap = true;
        self.cv.notify_all();
    }
}

/// YAML 标量加引号：token/URL 含 `:` `//` 等字符，不引号会被解析成映射。
fn yaml_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ---- 对外入口 ----

/// 引擎初始化后调用一次（幂等）。加载配置、接管 installation_id、
/// 按需拉起定时器线程；有待合并标记时立即补跑一轮。
pub fn init(user_dir: &Path) {
    let shared = SHARED.get_or_init(|| Shared::new(user_dir.to_path_buf()));
    shared.load_config();
    let id = ensure_installation_id(&shared.user_dir);
    {
        let mut st = shared.status.lock().unwrap();
        st.installation_id = id;
        st.state = "idle".into();
    }
    if shared.config.lock().unwrap().is_ready() && shared.config.lock().unwrap().enabled {
        start_timer(shared);
    }
    // 上次因活动会话留下的未合并快照：启动即补
    if shared.pending_path().exists() {
        shared.request_run();
    }
}

fn shared() -> Option<&'static Shared> {
    SHARED.get()
}

/// 当前 installation_id（未初始化时返回空串）。
pub fn installation_id() -> String {
    shared()
        .map(|s| s.status.lock().unwrap().installation_id.clone())
        .unwrap_or_default()
}

/// 当前配置（未初始化返回默认值）。
pub fn config_snapshot() -> SyncConfig {
    shared()
        .map(|s| s.config.lock().unwrap().clone())
        .unwrap_or_default()
}

/// 当前状态（未初始化返回默认值，state = not_configured）。
pub fn status_snapshot() -> SyncStatus {
    match shared() {
        Some(s) => s.status_snapshot(),
        None => SyncStatus {
            state: "not_configured".into(),
            ..Default::default()
        },
    }
}

/// 登录并保存账号（设置页/CLI 调）。成功后启用同步并拉起定时器。
pub fn configure(server_url: &str, username: &str, password: &str) -> Result<(), String> {
    let base = normalize_base(server_url)?;
    let agent = http_agent();
    let res: serde_json::Value = agent
        .post(&format!("{base}/api/auth/login"))
        .send_json(serde_json::json!({ "username": username, "password": password }))
        .map_err(|e| format!("登录失败: {e}"))?
        .into_json()
        .map_err(|e| format!("登录响应解析失败: {e}"))?;
    let token = res
        .get("token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "登录响应无 token".to_string())?
        .to_string();

    // init() 可能尚未跑过（CLI 不经 heng_create 时）：补一次
    let user_dir = match shared() {
        Some(s) => s.user_dir.clone(),
        None => return Err("引擎未初始化".into()),
    };
    let shared = SHARED.get_or_init(|| Shared::new(user_dir));
    {
        let mut cfg = shared.config.lock().unwrap();
        cfg.server_url = base;
        cfg.username = username.to_string();
        cfg.token = token;
        cfg.enabled = true;
    }
    shared.save_config().map_err(|e| format!("配置保存失败: {e}"))?;
    shared.status.lock().unwrap().state = "idle".into();
    start_timer(shared);
    shared.request_run();
    Ok(())
}

/// 启用/停用同步开关（设置页拨杆）。
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let shared = shared().ok_or("引擎未初始化")?;
    {
        let mut cfg = shared.config.lock().unwrap();
        cfg.enabled = enabled;
    }
    shared.save_config().map_err(|e| format!("配置保存失败: {e}"))?;
    if enabled && shared.config.lock().unwrap().is_ready() {
        start_timer(shared);
        shared.request_run();
    }
    Ok(())
}

/// 退出登录：清令牌并停用同步，`server_url`/`username` 保留供下次登录回显。
/// 本地已下载的他人快照与 `sync/` 目录不动（属于用户数据，不是登录态）。
pub fn logout() -> Result<(), String> {
    let shared = shared().ok_or("引擎未初始化")?;
    {
        let mut cfg = shared.config.lock().unwrap();
        cfg.token.clear();
        cfg.enabled = false;
    }
    shared.save_config().map_err(|e| format!("配置保存失败: {e}"))?;
    shared.set_state("not_configured", None);
    Ok(())
}

/// 「立即同步」：只负责唤醒定时器（同步含网络 IO，不能卡调用方线程）。
/// 返回 false = 未配置/已停用/定时器未起。
pub fn request_sync_now() -> bool {
    let Some(shared) = shared() else {
        return false;
    };
    let cfg = shared.config.lock().unwrap();
    if !cfg.is_ready() || !cfg.enabled {
        return false;
    }
    drop(cfg);
    shared.request_run();
    true
}

/// 会话计数归零（`heng_end_session` 末尾调用）：有待合并标记则补跑一轮。
/// 无标记或定时器未起时什么都不做。
pub fn notify_sessions_idle() {
    if let Some(s) = shared() {
        if s.pending_path().exists() {
            s.request_run();
        }
    }
}

// ---- 定时器 ----

fn start_timer(shared: &'static Shared) {
    if shared.timer_started.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("heng-sync".into())
        .spawn(move || timer_loop(shared))
        .ok();
}

fn timer_loop(shared: &'static Shared) {
    // 启动后先尽快跑一轮：刚配好账号的用户期望立刻同步；
    // 之后按 interval / 退避节奏。
    shared.ctl.lock().unwrap().next_due = now_unix();
    loop {
        let (sleep, asap) = {
            let ctl = shared.ctl.lock().unwrap();
            if ctl.run_asap {
                (Duration::ZERO, true)
            } else {
                let wait = (ctl.next_due - now_unix()).max(0) as u64;
                (Duration::from_secs(wait.min(MAX_SLEEP_SECS)), false)
            }
        };
        if asap || sleep.is_zero() {
            shared.ctl.lock().unwrap().run_asap = false;
            let enabled = shared.config.lock().unwrap().enabled;
            let outcome = if enabled {
                run_sync(shared, false)
            } else {
                SyncOutcome::Skipped
            };
            let now = now_unix();
            let interval = shared.config.lock().unwrap().interval_secs.max(60);
            let mut ctl = shared.ctl.lock().unwrap();
            let mut st = shared.status.lock().unwrap();
            match outcome {
                SyncOutcome::Skipped => ctl.next_due = now + RETRY_SOON_SECS,
                SyncOutcome::Deferred => ctl.next_due = now + RETRY_SOON_SECS,
                SyncOutcome::Ok { downloaded, uploaded } => {
                    ctl.next_due = now + interval;
                    st.last_sync_at = Some(now);
                    st.last_downloaded = downloaded as i64;
                    st.last_uploaded = uploaded as i64;
                    st.consecutive_failures = 0;
                }
                SyncOutcome::Failed(e) => {
                    st.consecutive_failures += 1;
                    st.last_error = Some(e);
                    let backoff = backoff_secs(interval, st.consecutive_failures);
                    ctl.next_due = now + backoff;
                }
            }
        } else {
            let ctl = shared.ctl.lock().unwrap();
            let _ = shared.cv.wait_timeout(ctl, sleep);
        }
    }
}

/// 指数退避：interval × 2^n，封顶 6 小时。
fn backoff_secs(interval: i64, failures: i64) -> i64 {
    (interval << failures.min(6)).min(MAX_BACKOFF_SECS)
}

// ---- 一轮同步 ----

/// 会话创建门闩：librime 的 sync 会 `CleanupAllSessions` 摧毁全部会话，
/// 一轮 sync 全程持锁，`heng_start_session` 同样持锁 → 会话创建最多等一轮
/// sync（秒级），不会出现「会话被摧毁但外壳仍持有旧句柄」。
pub static SESSION_GATE: Mutex<()> = Mutex::new(());

fn run_sync(shared: &Shared, _force: bool) -> SyncOutcome {
    if shared.running.swap(true, Ordering::SeqCst) {
        return SyncOutcome::Skipped; // 已有轮次在跑
    }
    struct Guard<'a>(&'a Shared);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.running.store(false, Ordering::SeqCst);
        }
    }
    let _guard = Guard(shared);

    let cfg = shared.config.lock().unwrap().clone();
    if !cfg.is_ready() {
        shared.set_state("not_configured", Some("未配置同步账号".into()));
        return SyncOutcome::Failed("未配置同步账号".into());
    }
    let id = shared.status.lock().unwrap().installation_id.clone();
    if id.is_empty() {
        return SyncOutcome::Failed("installation_id 未生成".into());
    }

    // 活动会话检查（持门闩前快速路径 + 持门闩后复核）
    if SESSIONS.count() > 0 {
        return defer_sync(shared, "输入会话进行中，推迟合并");
    }
    let _gate = SESSION_GATE.lock().unwrap();
    if SESSIONS.count() > 0 {
        return defer_sync(shared, "输入会话进行中，推迟合并");
    }

    shared.set_state("running", None);
    let base = cfg.server_url.trim_end_matches('/').to_string();
    let agent = http_agent();
    let auth = format!("Bearer {}", cfg.token);

    // 1. 注册/更新本机设备
    if let Err(e) = register_device(&agent, &base, &auth, &id) {
        shared.set_state("error", Some(e.clone()));
        return SyncOutcome::Failed(e);
    }

    // 2. 拉取其他设备快照（必须早于导出：librime 只合并已存在的文件）
    let downloaded = match pull_snapshots(&agent, &base, &auth, &id, shared) {
        Ok(n) => n,
        Err(e) => {
            shared.set_state("error", Some(e.clone()));
            return SyncOutcome::Failed(e);
        }
    };

    // 3. 导出 + 合并 + 重导本机快照
    if let Some(engine) = crate::global::engine_opt() {
        let _op = crate::global::OP_LOCK.lock().unwrap();
        match engine.sync_user_data_blocking() {
            Ok(true) => {}
            Ok(false) => {
                let e = "librime 导出快照失败".to_string();
                shared.set_state("error", Some(e.clone()));
                return SyncOutcome::Failed(e);
            }
            Err(e) => {
                let msg = format!("librime 同步调用失败: {e}");
                shared.set_state("error", Some(msg.clone()));
                return SyncOutcome::Failed(msg);
            }
        }
    } else {
        let e = "引擎未初始化".to_string();
        shared.set_state("error", Some(e.clone()));
        return SyncOutcome::Failed(e);
    }

    // 4. 上传本机快照
    let uploaded = match push_snapshots(&agent, &base, &auth, &id, shared) {
        Ok(n) => n,
        Err(e) => {
            shared.set_state("error", Some(e.clone()));
            return SyncOutcome::Failed(e);
        }
    };

    // 5. 收尾
    let _ = std::fs::remove_file(shared.pending_path());
    shared.set_state("ok", None);
    SyncOutcome::Ok { downloaded, uploaded }
}

/// 有活动会话：写下待合并标记，整轮推迟（librime sync 会 CleanupAllSessions，
/// 输入中执行会打断会话）。标记由 init()/heng_end_session 检查后补跑。
fn defer_sync(shared: &Shared, reason: &str) -> SyncOutcome {
    if let Some(parent) = shared.pending_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        shared.pending_path(),
        format!("deferred at {}: {reason}\n", now_unix()),
    );
    shared.set_state("deferred", Some(reason.into()));
    SyncOutcome::Deferred
}

fn register_device(agent: &ureq::Agent, base: &str, auth: &str, id: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "installation_id": id,
        "display_name": hostname(),
        "platform": platform_tag(),
    });
    agent
        .post(&format!("{base}/api/devices"))
        .set("Authorization", auth)
        .send_json(&body)
        .map_err(|e| format!("设备注册失败: {e}"))?;
    Ok(())
}

fn pull_snapshots(
    agent: &ureq::Agent,
    base: &str,
    auth: &str,
    self_id: &str,
    shared: &Shared,
) -> Result<usize, String> {
    let manifest: Manifest = agent
        .get(&format!("{base}/api/sync/manifest"))
        .set("Authorization", auth)
        .call()
        .map_err(|e| format!("manifest 拉取失败: {e}"))?
        .into_json()
        .map_err(|e| format!("manifest 解析失败: {e}"))?;

    let sync_root = shared.sync_root();
    let mut count = 0usize;
    for dev in manifest.devices.iter().filter(|d| d.installation_id != self_id) {
        for f in &dev.files {
            if !is_sync_file(&f.name) {
                continue;
            }
            let dest = sync_root.join(&dev.installation_id).join(&f.name);
            // 本地已有且 sha 一致 → 不传输（§7 开销约束）
            if let Ok(local) = std::fs::read(&dest) {
                if !f.sha256.is_empty() && sha256_hex(&local) == f.sha256 {
                    continue;
                }
            }
            let mut reader = agent
                .get(&format!(
                    "{base}/api/sync/file/{}/{}",
                    dev.installation_id, f.name
                ))
                .set("Authorization", auth)
                .call()
                .map_err(|e| format!("快照 {} 下载失败: {e}", f.name))?
                .into_reader();
            let mut bytes = Vec::new();
            use std::io::Read;
            reader
                .read_to_end(&mut bytes)
                .map_err(|e| format!("快照 {} 读取失败: {e}", f.name))?;
            // 完整性校验：不一致则丢弃（下轮重试）
            if !f.sha256.is_empty() && sha256_hex(&bytes) != f.sha256 {
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("同步目录创建失败: {e}"))?;
            }
            let tmp = dest.with_file_name(format!("{}.downloading", f.name));
            std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &dest))
                .map_err(|e| format!("快照 {} 落盘失败: {e}", f.name))?;
            count += 1;
        }
    }
    Ok(count)
}

fn push_snapshots(
    agent: &ureq::Agent,
    base: &str,
    auth: &str,
    self_id: &str,
    shared: &Shared,
) -> Result<usize, String> {
    let dir = shared.sync_root().join(self_id);
    let mut count = 0usize;
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(0); // 首次同步前目录不存在属正常
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_sync_file(&name) {
            continue;
        }
        let bytes = std::fs::read(entry.path())
            .map_err(|e| format!("读取本机快照 {name} 失败: {e}"))?;
        if bytes.is_empty() {
            continue;
        }
        agent
            .put(&format!("{base}/api/sync/file/{self_id}/{name}"))
            .set("Authorization", auth)
            .send_bytes(&bytes)
            .map_err(|e| format!("快照 {name} 上传失败: {e}"))?;
        count += 1;
    }
    Ok(count)
}

// ---- 纯函数 ----

/// 同步单元只含 `*.userdb.txt`：rimesync 的快照目录另含配置目录根目录的
/// `*.custom.yaml` 与 `installation.yaml`（BackupConfigFiles 的产物），
/// 两者不参与同步（SYNC-PLAN §1）。
pub fn is_sync_file(name: &str) -> bool {
    name.ends_with(".userdb.txt") && name.len() > ".userdb.txt".len()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn normalize_base(url: &str) -> Result<String, String> {
    let u = url.trim();
    if !u.starts_with("http://") && !u.starts_with("https://") {
        return Err("服务器地址必须以 http:// 或 https:// 开头".into());
    }
    if u.len() < 9 {
        return Err("服务器地址过短".into());
    }
    Ok(u.trim_end_matches('/').to_string())
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(HTTP_TIMEOUT)
        .timeout_read(HTTP_TIMEOUT)
        .timeout_write(HTTP_TIMEOUT)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_id_format() {
        let id = generate_installation_id();
        assert!(id.starts_with(&format!("{}-", platform_tag())));
        assert!(id.contains(&format!("-{}-", hostname())));
        let tail = id.rsplit('-').next().unwrap();
        assert_eq!(tail.len(), 6);
        assert!(tail.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(is_heng_installation_id(&id));
    }

    #[test]
    fn uuid_is_replaced_by_readable_id() {
        // librime 自生成的 UUID 不是我们的格式 → 应被改写
        assert!(!is_heng_installation_id("73074379-e61e-40cf-a492-b955193a5191"));
        assert!(!is_heng_installation_id(""));
        assert!(!is_heng_installation_id("win-a-1A2B3")); // 末段 5 位
        assert!(is_heng_installation_id("windows-dmz-pc-1A2B3C"));
    }

    #[test]
    fn ensure_id_writes_installation_yaml_and_keeps_other_keys() {
        let dir = std::env::temp_dir().join(format!("heng-sync-inst-{}", now_unix()));
        std::fs::create_dir_all(&dir).unwrap();
        // 模拟 librime 刚部署完的 installation.yaml（UUID + 其余键）
        std::fs::write(
            dir.join("installation.yaml"),
            "distribution_code_name: heng\ninstallation_id: \"73074379-e61e-40cf-a492-b955193a5191\"\nrime_version: 1.17.0\n",
        )
        .unwrap();
        let id = ensure_installation_id(&dir);
        assert!(is_heng_installation_id(&id));
        // 再次调用不重复生成
        assert_eq!(ensure_installation_id(&dir), id);
        let text = std::fs::read_to_string(dir.join("installation.yaml")).unwrap();
        assert!(text.contains("distribution_code_name: heng"));
        assert!(text.contains("rime_version"));
        assert!(text.contains(&id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_file_filter() {
        assert!(is_sync_file("rime_ice.userdb.txt"));
        assert!(is_sync_file("luna_pinyin.userdb.txt"));
        assert!(!is_sync_file("weasel.custom.yaml"));
        assert!(!is_sync_file("installation.yaml"));
        assert!(!is_sync_file(".userdb.txt"));
        assert!(!is_sync_file("userdb.txt"));
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        // interval 1800s（30 分钟）：30m → 1h → 2h → 4h → 触顶 6h
        assert_eq!(backoff_secs(1800, 1), 3600);
        assert_eq!(backoff_secs(1800, 2), 7200);
        assert_eq!(backoff_secs(1800, 3), 14400);
        assert_eq!(backoff_secs(1800, 4), MAX_BACKOFF_SECS); // 28800 被截到 6h
        assert_eq!(backoff_secs(1800, 6), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(1800, 10), MAX_BACKOFF_SECS);
        assert_eq!(MAX_BACKOFF_SECS, 6 * 3600);
    }

    #[test]
    fn normalize_base_url() {
        assert_eq!(normalize_base("http://127.0.0.1:8080/").unwrap(), "http://127.0.0.1:8080");
        assert_eq!(normalize_base(" https://a.b ").unwrap(), "https://a.b");
        assert!(normalize_base("127.0.0.1:8080").is_err());
        assert!(normalize_base("ftp://x").is_err());
    }

    #[test]
    fn config_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("heng-sync-cfg-{}", now_unix()));
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Shared::new(dir.clone());
        {
            let mut cfg = shared.config.lock().unwrap();
            cfg.server_url = "http://127.0.0.1:8080".into();
            cfg.username = "alice".into();
            cfg.token = "deadbeef:0123".into();
            cfg.enabled = false;
            cfg.interval_secs = 600;
        }
        shared.save_config().unwrap();
        let shared2 = Shared::new(dir.clone());
        shared2.load_config();
        let cfg = shared2.config.lock().unwrap();
        assert_eq!(cfg.server_url, "http://127.0.0.1:8080");
        assert_eq!(cfg.username, "alice");
        assert_eq!(cfg.token, "deadbeef:0123");
        assert!(!cfg.enabled);
        assert_eq!(cfg.interval_secs, 600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_merge_marker_lifecycle() {
        let dir = std::env::temp_dir().join(format!("heng-sync-pend-{}", now_unix()));
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Shared::new(dir.clone());
        assert!(!shared.pending_path().exists());
        let _ = std::fs::create_dir_all(shared.pending_path().parent().unwrap());
        std::fs::write(shared.pending_path(), "deferred\n").unwrap();
        assert!(shared.pending_path().exists());
        assert!(shared.status_snapshot().pending_merge);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_interval_is_thirty_minutes() {
        // derive(Default) 会给 interval_secs 零值；Shared::new 必须显式兜底
        let dir = std::env::temp_dir().join(format!("heng-sync-itv-{}", now_unix()));
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Shared::new(dir.clone());
        assert_eq!(
            shared.config.lock().unwrap().interval_secs,
            DEFAULT_INTERVAL_SECS
        );
        // 落盘再读回：save_config 的 max(60) 不得把 1800 压成 60
        shared.save_config().unwrap();
        let shared2 = Shared::new(dir.clone());
        shared2.load_config();
        assert_eq!(
            shared2.config.lock().unwrap().interval_secs,
            DEFAULT_INTERVAL_SECS
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostname_sanitize() {
        assert_eq!(sanitize_host("DMZ-PC"), "dmz-pc");
        assert_eq!(sanitize_host("my host!"), "my-host");
        assert_eq!(sanitize_host("--"), "unknown");
        // 截断到 24 字符（platform + '-' + host + '-' + 6 位随机 ≤ 64）
        assert_eq!(
            sanitize_host("averylonghostnamethatiswaytoolong"),
            "averylonghostnamethatisw"
        );
    }
}
