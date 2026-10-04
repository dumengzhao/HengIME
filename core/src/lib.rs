//! heng-core —— HengIME 统一能力模块。
//!
//! 分层：
//! - `engine`：librime 安全封装（热路径）
//! - `global`：进程级共享状态（引擎单例、会话注册表、操作锁）
//! - `capi`：C ABI 导出层，对应 `include/heng.h`
//! - `server`：本机 HTTP 服务（JSON over HTTP，与 C ABI 同源）
//!
//! M0 阶段仅此四层；后续按架构方案补齐 config / dict / sync / state。

pub mod capi;
pub mod engine;
pub mod global;
pub mod server;
pub mod ui;

pub use engine::{
    Candidate, ContextSnapshot, Engine, EngineConfig, EngineError, Session, StatusSnapshot,
};
