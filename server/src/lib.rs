//! heng-sync-server 的库形态：集成测试与将来的嵌入方（如 core 的诊断接口）
//! 复用同一套路由与数据层。二进制只是薄壳（参数解析 + 启动加载 + serve）。
//!
//! 管理端凭据不走库：`<data_dir>/admin.json` 只存 Argon2id 哈希（见
//! `auth::AdminStore`），与客户端账号体系（`db::account`）完全分离。

pub mod admin;
pub mod api;
pub mod auth;
pub mod db;
