//! 进程级共享状态：全局引擎、会话注册表、操作锁。
//!
//! librime 的 api 表进程内唯一、会话操作不保证线程安全，因此：
//! - `Engine` 用 [`OnceLock`] 做单例
//! - 所有 librime 会话操作经 [`OP_LOCK`] 串行化（单次操作亚毫秒级，锁不构成瓶颈）

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::engine::{Engine, EngineConfig, EngineError, RimeSessionId};

static ENGINE: OnceLock<Engine> = OnceLock::new();
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
