//! 进程级共享状态：全局引擎、会话注册表、操作锁。
//!
//! librime 的 api 表进程内唯一、会话操作不保证线程安全，因此：
//! - `Engine` 用 [`OnceLock`] 做单例
//! - 所有 librime 会话操作经 [`OP_LOCK`] 串行化（单次操作亚毫秒级，锁不构成瓶颈）

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::engine::{Engine, EngineConfig, EngineError, RimeSessionId};

static ENGINE: OnceLock<Engine> = OnceLock::new();

// ---- v6 行为统一层：传播策略、选项持久化、app_options ----
//
// 责任边界（docs/ROUTE-P.md §2）：前端不再各自实现「开关记住范围/按应用初始
// 选项」，全部收进 core。外壳只调 heng_set_option / heng_set_session_owner。

/// 开关传播策略（对齐 fcitx5-rime 的 SharedStatePolicy 三档 + Weasel 的 global_ascii）
pub const POLICY_PER_SESSION: i32 = 0; // 只作用于当前会话
pub const POLICY_PER_APP: i32 = 1; // 广播到同归属应用的全部会话
pub const POLICY_GLOBAL: i32 = 2; // 广播到全部会话

pub static PROPAGATION_POLICY: AtomicI32 = AtomicI32::new(POLICY_PER_SESSION);

/// 选项应用名单：跨重启记忆 + app_options 初始应用都只走这些选项
pub const APPLICABLE_OPTIONS: &[&str] =
    &["ascii_mode", "full_shape", "simplification", "ascii_punct"];

/// 持久化选项存储：新会话创建时应用初值；文件 <user_data_dir>/heng_options.yaml。
/// 手写平面 YAML（仅 bool 键值），不引新依赖。
static PERSISTED_OPTIONS: LazyLock<Mutex<BTreeMap<String, bool>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn options_file(engine: &Engine) -> PathBuf {
    engine.user_data_dir().join("heng_options.yaml")
}

/// 引擎初始化后调用：读持久化文件 + 从 heng.yaml 读传播策略初值。
/// 全部容错：缺文件/缺键都走默认值，不阻塞启动。
pub fn init_behavior_layer(engine: &Engine) {
    // 1. 选项持久化文件（形如 "  ascii_mode: true" 的平面键值）
    if let Ok(text) = std::fs::read_to_string(options_file(engine)) {
        let mut map = PERSISTED_OPTIONS.lock().unwrap();
        map.clear();
        for line in text.lines() {
            let line = line.trim();
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                if k.is_empty() || k.starts_with('#') {
                    continue;
                }
                if let Some(b) = parse_bool(v.trim()) {
                    map.insert(k.to_string(), b);
                }
            }
        }
    }
    // 2. heng.yaml 的传播策略（config 组件只读 staging，Engine::new 已部署 heng.yaml）
    if let Ok(mut config) = engine.config_open("heng") {
        let mut buf = [0u8; 32];
        if let Some(len) = engine.config_get_string(&config, "propagation/policy", &mut buf) {
            let policy = std::str::from_utf8(&buf[..len]).unwrap_or("");
            let p = match policy.trim() {
                "per_app" => POLICY_PER_APP,
                "global" => POLICY_GLOBAL,
                _ => POLICY_PER_SESSION,
            };
            PROPAGATION_POLICY.store(p, Ordering::Relaxed);
        }
        engine.config_close(&mut config);
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn save_persisted_options(engine: &Engine) {
    let map = PERSISTED_OPTIONS.lock().unwrap();
    let mut text = String::from("# HengIME 跨重启选项记忆（core 自动维护，勿手改）\noptions:\n");
    for (k, v) in map.iter() {
        text.push_str(&format!("  {k}: {v}\n"));
    }
    let path = options_file(engine);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, text);
}

/// 选项值变化（用户经 heng_set_option 触发）：名单内则更新存储并落盘
pub fn record_option_change(engine: &Engine, option: &str, value: bool) {
    if !APPLICABLE_OPTIONS.contains(&option) {
        return;
    }
    PERSISTED_OPTIONS
        .lock()
        .unwrap()
        .insert(option.to_string(), value);
    save_persisted_options(engine);
}

/// 新会话初始状态：持久化选项 → app_options（后者覆盖前者，对齐 Weasel 语义）。
/// 调用方须持有 OP_LOCK。
pub fn apply_session_initial_state(engine: &Engine, rime_id: RimeSessionId, owner: Option<&str>) {
    // 1. 跨重启记忆
    let store = PERSISTED_OPTIONS.lock().unwrap().clone();
    for (name, value) in &store {
        let _ = engine.set_option(rime_id, name, *value);
    }
    // 2. 按应用初始选项（app_options/<owner>/<option>，仅名单内选项）
    if let Some(app) = owner {
        if let Ok(mut config) = engine.config_open("heng") {
            for opt in APPLICABLE_OPTIONS {
                if let Some(v) =
                    engine.config_get_bool(&config, &format!("app_options/{app}/{opt}"))
                {
                    let _ = engine.set_option(rime_id, opt, v);
                }
            }
            engine.config_close(&mut config);
        }
    }
}

/// 开关传播：按策略把一次 set_option 广播到其它会话。调用方须持有 OP_LOCK，
/// 且源会话已完成本次 set_option。
pub fn propagate_option(engine: &Engine, source_rime_id: RimeSessionId, option: &str, value: bool) {
    let policy = PROPAGATION_POLICY.load(Ordering::Relaxed);
    if policy == POLICY_PER_SESSION {
        return;
    }
    let source_owner = SESSIONS.owner_of(source_rime_id);
    for (rime_id, owner) in SESSIONS.entries() {
        if rime_id == source_rime_id {
            continue;
        }
        let hit = match policy {
            POLICY_GLOBAL => true,
            POLICY_PER_APP => source_owner.is_some() && source_owner == owner,
            _ => false,
        };
        if hit {
            let _ = engine.set_option(rime_id, option, value);
        }
    }
}
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// 运行时根目录：`HENG_RUNTIME` 环境变量优先，默认 `./runtime`。
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("HENG_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runtime"))
}

pub fn default_config() -> EngineConfig {
    let base = runtime_dir();
    EngineConfig {
        shared_data_dir: Some(base.join("rime-shared")),
        user_data_dir: base.join("rime-user"),
        min_log_level: 2, // 0=INFO 1=WARNING 2=ERROR 3=FATAL
    }
}

/// 取进程级引擎，首次调用时初始化并触发部署。
pub fn engine() -> Result<&'static Engine, EngineError> {
    engine_with(default_config())
}

pub fn engine_with(config: EngineConfig) -> Result<&'static Engine, EngineError> {
    if let Some(e) = ENGINE.get() {
        return Ok(e);
    }
    // 防并发双初始化：librime 全局状态只能 setup/initialize 一次
    let _guard = INIT_LOCK.lock().unwrap();
    if let Some(e) = ENGINE.get() {
        return Ok(e);
    }
    let engine = Engine::new(config)?;
    init_behavior_layer(&engine);
    Ok(ENGINE.get_or_init(|| engine))
}

/// 序列化所有 librime 会话操作。
pub static OP_LOCK: Mutex<()> = Mutex::new(());

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
static SESSION_MAP: LazyLock<Mutex<HashMap<u64, RimeSessionId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// 会话归属应用（M0.5-Q3：SetSessionOwner 语义，app_options 应用与开关传播的基础）
static OWNER_MAP: LazyLock<Mutex<HashMap<u64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct SessionRegistry;

impl SessionRegistry {
    /// 登记一个已创建的 librime 会话及其归属应用，返回对外句柄。
    pub fn insert(&self, rime_id: RimeSessionId, owner: Option<String>) -> u64 {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        SESSION_MAP.lock().unwrap().insert(id, rime_id);
        if let Some(app) = owner {
            OWNER_MAP.lock().unwrap().insert(id, app);
        }
        id
    }

    pub fn rime_id(&self, id: u64) -> Option<RimeSessionId> {
        SESSION_MAP.lock().unwrap().get(&id).copied()
    }

    /// 归属应用名（未绑定过则 None）。
    pub fn owner(&self, id: u64) -> Option<String> {
        OWNER_MAP.lock().unwrap().get(&id).cloned()
    }

    /// 按 librime 会话 ID 反查归属应用。
    pub fn owner_of(&self, rime_id: RimeSessionId) -> Option<String> {
        let map = SESSION_MAP.lock().unwrap();
        map.iter()
            .find(|(_, &rid)| rid == rime_id)
            .and_then(|(&hid, _)| OWNER_MAP.lock().unwrap().get(&hid).cloned())
    }

    /// 按 librime 会话 ID 反查外壳句柄。
    pub fn handle_of(&self, rime_id: RimeSessionId) -> Option<u64> {
        SESSION_MAP
            .lock()
            .unwrap()
            .iter()
            .find(|(_, &rid)| rid == rime_id)
            .map(|(&hid, _)| hid)
    }

    /// 全部会话的 (librime ID, 归属应用) 快照（传播遍历用）。
    pub fn entries(&self) -> Vec<(RimeSessionId, Option<String>)> {
        let map = SESSION_MAP.lock().unwrap();
        let owners = OWNER_MAP.lock().unwrap();
        map.iter()
            .map(|(&hid, &rid)| (rid, owners.get(&hid).cloned()))
            .collect()
    }

    /// 绑定/改绑归属应用（焦点切换时由前端调用）。
    pub fn set_owner(&self, id: u64, owner: String) -> bool {
        if !SESSION_MAP.lock().unwrap().contains_key(&id) {
            return false;
        }
        OWNER_MAP.lock().unwrap().insert(id, owner);
        true
    }

    /// 注销并销毁 librime 会话。
    pub fn remove(&self, engine: &Engine, id: u64) -> bool {
        let rime_id = SESSION_MAP.lock().unwrap().remove(&id);
        OWNER_MAP.lock().unwrap().remove(&id);
        match rime_id {
            Some(rid) => {
                engine.destroy_session(rid);
                true
            }
            None => false,
        }
    }

    /// 当前登记的全部对外句柄（顺序任意，供全量销毁遍历）。
    pub fn ids(&self) -> Vec<u64> {
        SESSION_MAP.lock().unwrap().keys().copied().collect()
    }

    pub fn count(&self) -> usize {
        SESSION_MAP.lock().unwrap().len()
    }
}

pub static SESSIONS: SessionRegistry = SessionRegistry;
