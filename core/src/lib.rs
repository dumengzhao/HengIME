//! heng-core —— HengIME 统一能力模块。
//!
//! 分层：
//! - `engine`：librime 安全封装（热路径）
//! - `global`：进程级共享状态（引擎单例、会话注册表、操作锁）
//! - `sync`：用户词库同步（六端常驻，无 feature 门控）
//! - `capi`：C ABI 导出层，对应 `include/heng.h`
//! - `ui`：Slint 自绘候选窗与设置窗（桌面三端）
//! - `server`：本机 HTTP 服务（JSON over HTTP，与 C ABI 同源）
//!
//! `ui` 与 `server` 由 feature 门控。移动三端改用宿主原生框架绘制键盘与
//! 候选窗，不需要 Slint 软件渲染器；也不需要本机 HTTP 诊断服务。
//! 关掉这两项可避免把 Slint 场景、`ui.rs` 的平台后端与 axum/tokio
//! 一起带进 aarch64 产物：
//!
//! ```sh
//! cargo build --release --no-default-features -p heng-core
//! ```
//!
//! feature 关闭时 `ui` 模块保留同名 stub 函数（C ABI 符号不变，返回失败），
//! 各端外壳无需区分是否启用。
//!
//! `sync` 不设 feature：六端都要同步，且其依赖（ureq/sha2/getrandom）
//! 不涉及平台框架，进移动端产物无负担。

pub mod capi;
pub mod engine;
pub mod global;
pub mod settings;
pub mod sync;

#[cfg(feature = "ui")]
pub mod ui;
#[cfg(not(feature = "ui"))]
#[path = "ui_stub.rs"]
pub mod ui;

#[cfg(feature = "http")]
pub mod server;

pub use engine::{
    Candidate, ContextSnapshot, Engine, EngineConfig, EngineError, Session, StatusSnapshot,
};
