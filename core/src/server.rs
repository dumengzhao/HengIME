//! 本机 HTTP 服务（axum）—— M0 第二交付物。
//!
//! 定位：与 C ABI 同源（都走 `global` 层），是「一份 IDL 两种绑定」中
//! IPC 绑定的最简形态（JSON over HTTP）。仅监听 127.0.0.1，供前端与工具本地联调。
//!
//! 端点（除 /api/describe 外均须携带 token，见下方「鉴权」）：
//! - GET  /api/describe                        服务元数据（免鉴权，供工具发现服务）
//! - GET  /api/status                          引擎版本 + 已装方案
//! - POST /api/sessions                        开会话  -> {"session": id}
//! - DELETE /api/sessions/{id}                 关会话
//! - POST /api/sessions/{id}/keys              单键    {keysym, mask} -> {handled}
//! - POST /api/sessions/{id}/simulate          序列    {sequence}     -> {handled}
//! - GET  /api/sessions/{id}/context           组合串+候选快照
//! - GET  /api/sessions/{id}/commit            取待上屏文本（消费语义）
//! - GET  /api/sessions/{id}/status            输入状态
//! - POST /api/sessions/{id}/select            选候选  {index}
//! - POST /api/sessions/{id}/highlight         高亮候选 {index}（不提交）
//! - POST /api/sessions/{id}/page              翻页    {backward}
//! - POST /api/sessions/{id}/owner             绑定归属应用 {app_id}
//! - POST /api/sessions/{id}/clear             清组合串
//!
//! 鉴权：服务承载全部击键与本机配置，任何本机进程不得未授权访问。
//! token 来源：环境变量 HENG_TOKEN 优先；否则启动时生成 256 位随机值并写入
//! `<HENG_RUNTIME>/token`（0600）。请求方以 `Authorization: Bearer <token>`
//! 或 `X-Heng-Token: <token>` 头携带。

use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::extract::{Path, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::global::{engine, OP_LOCK, SESSIONS};

static TOKEN: OnceLock<String> = OnceLock::new();

/// 解析/生成本机 token。环境变量 HENG_TOKEN 优先，否则随机生成并落盘。
fn resolve_token() -> String {
    if let Ok(t) = std::env::var("HENG_TOKEN") {
        if !t.is_empty() {
            return t;
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("系统随机数源不可用");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let path = crate::global::runtime_dir().join("token");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = f.write_all(hex.as_bytes());
            }
            Err(e) => eprintln!("警告：token 写入 {} 失败：{e}", path.display()),
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(e) = std::fs::write(&path, &hex) {
            eprintln!("警告：token 写入 {} 失败：{e}", path.display());
        }
    }
    eprintln!("本机 token 已写入 {}", path.display());
    hex
}

/// token 校验中间件：Bearer 头或 X-Heng-Token 头二选一。
async fn require_token(req: Request, next: Next) -> Response {
    let expected = TOKEN.get().map(String::as_str).unwrap_or_default();
    let present = |v: Option<&axum::http::HeaderValue>| {
        v.and_then(|v| v.to_str().ok())
            .map_or(false, |s| !expected.is_empty() && s == expected)
    };
    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map_or(false, |t| t == expected)
        || present(req.headers().get("x-heng-token"));
    if authorized {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response()
    }
}

fn err_msg(e: impl std::fmt::Display) -> Value {
    json!({ "error": e.to_string() })
}

async fn describe() -> Json<Value> {
    Json(json!({
        "name": "heng-server",
        "version": env!("CARGO_PKG_VERSION"),
        "abi_version": crate::capi::HENG_ABI_VERSION,
        "engine": "librime",
        "listen": "127.0.0.1",
    }))
}

async fn status() -> Json<Value> {
    let _guard = OP_LOCK.lock().unwrap();
    match engine() {
        Ok(e) => Json(json!({
            "version": e.version(),
            "schemas": e
                .schema_list()
                .into_iter()
                .map(|(id, name)| json!({ "id": id, "name": name }))
                .collect::<Vec<_>>(),
        })),
        Err(e) => Json(err_msg(e)),
    }
}

async fn create_session() -> Json<Value> {
    let _guard = OP_LOCK.lock().unwrap();
    match engine() {
        Ok(e) => match e.create_session() {
            Ok(session) => {
                let rime_id = session.into_raw();
                let id = SESSIONS.insert(rime_id, None);
                Json(json!({ "session": id }))
            }
            Err(e) => Json(err_msg(e)),
        },
        Err(e) => Json(err_msg(e)),
    }
}

async fn end_session(Path(id): Path<u64>) -> Json<Value> {
    match engine() {
        Ok(e) => {
            let removed = SESSIONS.remove(e, id);
            Json(json!({ "removed": removed }))
        }
        Err(e) => Json(err_msg(e)),
    }
}

/// 从路径参数解析会话并映射到 librime 裸 ID。
fn resolve(id: u64) -> Result<crate::engine::RimeSessionId, Value> {
    SESSIONS
        .rime_id(id)
        .ok_or_else(|| Json(err_msg(format!("会话 {id} 不存在"))).0)
}

#[derive(serde::Deserialize)]
struct KeyReq {
    keysym: i32,
    #[serde(default)]
    mask: i32,
}

async fn process_key(Path(id): Path<u64>, Json(req): Json<KeyReq>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.process_key(rime_id, req.keysym, req.mask) {
        Ok(handled) => Json(json!({ "handled": handled })),
        Err(e) => Json(err_msg(e)),
    }
}

#[derive(serde::Deserialize)]
struct SimulateReq {
    sequence: String,
}

async fn simulate(Path(id): Path<u64>, Json(req): Json<SimulateReq>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.simulate_key_sequence(rime_id, &req.sequence) {
        Ok(handled) => Json(json!({ "handled": handled })),
        Err(e) => Json(err_msg(e)),
    }
}

async fn get_context(Path(id): Path<u64>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.get_context(rime_id) {
        Ok(snapshot) => Json(serde_json::to_value(snapshot).unwrap_or_default()),
        Err(e) => Json(err_msg(e)),
    }
}

async fn get_commit(Path(id): Path<u64>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.get_commit(rime_id) {
        Ok(text) => Json(json!({ "text": text })),
        Err(e) => Json(err_msg(e)),
    }
}

async fn get_status(Path(id): Path<u64>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.get_status(rime_id) {
        Ok(status) => Json(serde_json::to_value(status).unwrap_or_default()),
        Err(e) => Json(err_msg(e)),
    }
}

#[derive(serde::Deserialize)]
struct SelectReq {
    index: usize,
}

async fn select_candidate(Path(id): Path<u64>, Json(req): Json<SelectReq>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.select_candidate_on_current_page(rime_id, req.index) {
        Ok(selected) => Json(json!({ "selected": selected })),
        Err(e) => Json(err_msg(e)),
    }
}

#[derive(serde::Deserialize)]
struct HighlightReq {
    index: usize,
}

async fn highlight_candidate(Path(id): Path<u64>, Json(req): Json<HighlightReq>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.highlight_candidate_on_current_page(rime_id, req.index) {
        Ok(highlighted) => Json(json!({ "highlighted": highlighted })),
        Err(e) => Json(err_msg(e)),
    }
}

#[derive(serde::Deserialize)]
struct PageReq {
    #[serde(default)]
    backward: bool,
}

async fn change_page(Path(id): Path<u64>, Json(req): Json<PageReq>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.change_page(rime_id, req.backward) {
        Ok(moved) => Json(json!({ "moved": moved })),
        Err(e) => Json(err_msg(e)),
    }
}

#[derive(serde::Deserialize)]
struct OwnerReq {
    app_id: String,
}

/// 绑定/改绑会话归属应用（M0.5-Q3：SetSessionOwner 语义）。
async fn set_owner(Path(id): Path<u64>, Json(req): Json<OwnerReq>) -> Json<Value> {
    let bound = SESSIONS.set_owner(id, req.app_id);
    Json(json!({ "bound": bound }))
}

async fn clear(Path(id): Path<u64>) -> Json<Value> {
    let rime_id = match resolve(id) {
        Ok(v) => v,
        Err(e) => return Json(e),
    };
    let e = match engine() {
        Ok(e) => e,
        Err(e) => return Json(err_msg(e)),
    };
    let _guard = OP_LOCK.lock().unwrap();
    match e.clear_composition(rime_id) {
        Ok(()) => Json(json!({ "cleared": true })),
        Err(e) => Json(err_msg(e)),
    }
}

/// 启动 HTTP 服务。引擎在进入事件循环前完成部署，避免首个请求卡在部署上。
pub async fn serve(port: u16) -> std::io::Result<()> {
    if let Err(e) = engine() {
        eprintln!("引擎初始化失败: {e}");
        std::process::exit(1);
    }
    TOKEN.get_or_init(resolve_token);

    // describe 免鉴权（工具据此发现服务与 ABI 版本），其余端点全部要求 token
    let public = Router::new().route("/api/describe", get(describe));
    let protected = Router::new()
        .route("/api/status", get(status))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}", delete(end_session))
        .route("/api/sessions/{id}/keys", post(process_key))
        .route("/api/sessions/{id}/simulate", post(simulate))
        .route("/api/sessions/{id}/context", get(get_context))
        .route("/api/sessions/{id}/commit", get(get_commit))
        .route("/api/sessions/{id}/status", get(get_status))
        .route("/api/sessions/{id}/select", post(select_candidate))
        .route("/api/sessions/{id}/highlight", post(highlight_candidate))
        .route("/api/sessions/{id}/page", post(change_page))
        .route("/api/sessions/{id}/owner", post(set_owner))
        .route("/api/sessions/{id}/clear", post(clear))
        .layer(axum::middleware::from_fn(require_token));
    let app = public.merge(protected);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("heng-server 监听 http://{addr}（Ctrl+C 退出）");
    axum::serve(listener, app).await
}
