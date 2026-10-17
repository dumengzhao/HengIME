//! HTTP 接口（docs/SYNC-PLAN.md §5）。
//!
//! 客户端接口 7 个 + 管理接口（数据面 5 个 + 认证面 4 个）+ 管理页面。
//! 客户端鉴权 `Authorization: Bearer <token>`（token 为库内随机值）；
//! 管理接口走 JWT 会话（`admin.rs`：init / login / password + 中间件）。
//!
//! 服务端**不解析、不合并快照**：PUT 只做整份覆盖（重算 sha256 后落盘），
//! GET 原样返回字节。合并由各端 librime 在 `sync_user_data()` 时完成。

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admin;
use crate::auth::{self, AdminStore};
use crate::db;

/// 单次上传体积上限：用户词库快照通常是几十 KB～几 MB，64MB 留足余量。
const MAX_BODY: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub data_dir: Arc<PathBuf>,
    /// 管理员凭据（`admin.json` 的内存态；`None` = 尚未初始化）。
    pub admin_creds: Arc<Mutex<Option<auth::AdminCreds>>>,
    /// 会话签名密钥（`server.key`）。
    pub server_key: String,
    /// admin.json / server.key 的文件存储。
    pub admin_store: AdminStore,
    /// 受信反向代理出口 IP（`HENG_SYNC_TRUSTED_PROXIES`，逗号分隔）：
    /// 登录退避的源 IP 判定用，见 `auth::client_throttle_key`。
    pub trusted_proxies: Vec<IpAddr>,
}

pub fn router(state: AppState) -> Router {
    // 管理数据面：JWT 中间件统一鉴权（未初始化 409、令牌不对 401）。
    // 挂在子 Router 上，只影响这一组路由；认证面四个端点保持公开。
    let admin_data = Router::new()
        .route("/api/admin/users", get(admin_list_users).post(admin_create_user))
        .route("/api/admin/users/{name}/passwd", post(admin_change_password))
        .route("/api/admin/users/{name}", delete(admin_delete_user))
        .route("/api/admin/devices", get(admin_list_devices))
        .route("/api/admin/devices/{installation_id}", delete(admin_unbind_device))
        .route("/api/admin/password", post(admin::change_password))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin::admin_auth,
        ));
    Router::new()
        // ---- 客户端 ----
        .route("/api/auth/login", post(login))
        .route("/api/devices", get(list_devices).post(register_device))
        .route("/api/devices/{installation_id}", delete(unbind_device))
        .route("/api/sync/manifest", get(manifest))
        .route(
            "/api/sync/file/{installation_id}/{name}",
            get(download_file).put(upload_file),
        )
        // ---- 管理：页面与认证面（公开） ----
        .route("/admin", get(admin_page))
        .route("/api/admin/init-status", get(admin::admin_init_status))
        .route("/api/admin/init", post(admin::admin_init))
        .route("/api/admin/login", post(admin::admin_login))
        // ---- 管理：数据面（JWT 守卫） ----
        .merge(admin_data)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

// ---- 错误与鉴权 ----

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 从请求头解析令牌并取账号。失败即 401。
fn authed(state: &AppState, headers: &HeaderMap) -> Result<db::Account, ApiError> {
    let token = bearer(headers).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "缺少 Bearer 令牌"))?;
    let conn = state.db.lock().unwrap();
    db::account_for_token(&conn, &token)
        .ok()
        .flatten()
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "令牌无效或已过期"))
}

// ---- 输入校验 ----

/// installation_id 由 core 生成（`<platform>-<hostname>-<6位随机>`），
/// 同时充当磁盘目录名，字符集必须收紧。
fn valid_installation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 只同步 `*.userdb.txt`：rimesync 的快照目录另含 `*.custom.yaml` 与
/// `installation.yaml`（BackupConfigFiles 的产物），两者不参与同步
/// （docs/SYNC-PLAN.md §1）。该函数同时挡掉目录穿越。
fn valid_snapshot_name(name: &str) -> bool {
    name.ends_with(".userdb.txt")
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name != ".userdb.txt"
}

fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_'
        })
}

// ---- 客户端接口 ----

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> Result<Json<Value>, ApiError> {
    if req.username.is_empty() || req.password.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "用户名或密码为空"));
    }
    let conn = state.db.lock().unwrap();
    let Some(account) = db::find_account_by_name(&conn, &req.username).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err(err(StatusCode::UNAUTHORIZED, "用户名或密码错误"));
    };
    if !auth::verify_pass(&account.password_hash, &req.password) {
        return Err(err(StatusCode::UNAUTHORIZED, "用户名或密码错误"));
    }
    let token = auth::random_token();
    db::issue_token(&conn, account.id, &token)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "token": token,
        "username": account.username,
    })))
}

#[derive(Deserialize)]
struct DeviceReq {
    installation_id: String,
    display_name: String,
    platform: String,
}

async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let acc = authed(&state, &headers)?;
    let conn = state.db.lock().unwrap();
    let devices = db::list_devices(&conn, acc.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({
        "devices": devices.iter().map(|d| json!({
            "installation_id": d.installation_id,
            "display_name": d.display_name,
            "platform": d.platform,
            "last_seen_at": d.last_seen_at,
            "last_sync_at": d.last_sync_at,
        })).collect::<Vec<_>>(),
    })))
}

async fn register_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DeviceReq>,
) -> Result<Json<Value>, ApiError> {
    let acc = authed(&state, &headers)?;
    if !valid_installation_id(&req.installation_id) {
        return Err(err(StatusCode::BAD_REQUEST, "installation_id 含非法字符或超长"));
    }
    let display = if req.display_name.is_empty() {
        req.installation_id.clone()
    } else {
        req.display_name.chars().take(64).collect()
    };
    let platform = req.platform.chars().take(16).collect::<String>();
    let conn = state.db.lock().unwrap();
    db::upsert_device(&conn, acc.id, &req.installation_id, &display, &platform)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn unbind_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(installation_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let acc = authed(&state, &headers)?;
    if !valid_installation_id(&installation_id) {
        return Err(err(StatusCode::BAD_REQUEST, "installation_id 非法"));
    }
    let conn = state.db.lock().unwrap();
    let Some(device) = db::find_device(&conn, acc.id, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err(err(StatusCode::NOT_FOUND, "设备不存在"));
    };
    db::delete_snapshots_of(&conn, acc.id, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    db::delete_device(&conn, device.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let dir = db::device_dir(&state.data_dir, acc.id, &installation_id);
    let _ = std::fs::remove_dir_all(dir);
    Ok(Json(json!({ "ok": true })))
}

/// 全部设备的文件清单。客户端自行剔除本机槽位后按 sha256 决定下载。
async fn manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let acc = authed(&state, &headers)?;
    let conn = state.db.lock().unwrap();
    let devices = db::list_devices(&conn, acc.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut out = Vec::with_capacity(devices.len());
    for d in &devices {
        let snaps = db::list_snapshots(&conn, acc.id, &d.installation_id)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        out.push(json!({
            "installation_id": d.installation_id,
            "display_name": d.display_name,
            "platform": d.platform,
            "last_sync_at": d.last_sync_at,
            "files": snaps.iter().map(|s| json!({
                "name": s.file_name,
                "size": s.size,
                "sha256": s.sha256,
                "tick": s.tick,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(Json(json!({ "devices": out })))
}

async fn download_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((installation_id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let acc = authed(&state, &headers)?;
    if !valid_installation_id(&installation_id) || !valid_snapshot_name(&name) {
        return Err(err(StatusCode::BAD_REQUEST, "路径参数非法"));
    }
    // 只允许下载本账号设备的快照
    let conn = state.db.lock().unwrap();
    let exists = db::find_device(&conn, acc.id, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .is_some();
    if !exists {
        return Err(err(StatusCode::NOT_FOUND, "设备不存在"));
    }
    let path = db::snapshot_path(&state.data_dir, acc.id, &installation_id, &name);
    let bytes = std::fs::read(&path).map_err(|_| err(StatusCode::NOT_FOUND, "快照不存在"))?;
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

/// 上传 = 整份覆盖本机槽位。sha256 由服务端重算（不信客户端自述值），
/// tick 从快照头部 `#@/tick` 解析，仅用于管理页面显示。
async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((installation_id, name)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let acc = authed(&state, &headers)?;
    if !valid_installation_id(&installation_id) || !valid_snapshot_name(&name) {
        return Err(err(StatusCode::BAD_REQUEST, "路径参数非法"));
    }
    if body.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "快照内容为空"));
    }
    let conn = state.db.lock().unwrap();
    // 只允许写本账号已注册设备的槽位
    if db::find_device(&conn, acc.id, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .is_none()
    {
        return Err(err(StatusCode::NOT_FOUND, "设备未注册，请先 POST /api/devices"));
    }
    let sha256 = auth_sha256(&body);
    let tick = parse_tick(&body);
    let size = body.len() as i64;

    let path = db::snapshot_path(&state.data_dir, acc.id, &installation_id, &name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("创建目录失败: {e}")))?;
    }
    // 先写临时文件再 rename：避免半份文件被并发下载看到
    let tmp = path.with_file_name(format!("{name}.uploading"));
    std::fs::write(&tmp, &body)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("写入快照失败: {e}")))?;

    db::upsert_snapshot(&conn, acc.id, &installation_id, &name, size, &sha256, tick)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "sha256": sha256, "size": size, "tick": tick })))
}

fn auth_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 从快照头部取 `#@/tick <n>`。快照级计数器，仅同设备去重用，
/// **不是**跨设备合并依据（合并按每条词条的 t=）。
fn parse_tick(bytes: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines().take(16) {
        if let Some(rest) = line.strip_prefix("#@/tick") {
            if let Ok(n) = rest.trim().parse::<i64>() {
                return Some(n);
            }
        }
    }
    None
}

// ---- 管理接口（数据面，JWT 鉴权由 admin::admin_auth 中间件统一完成） ----

async fn admin_page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("admin.html"),
    )
        .into_response()
}

async fn admin_list_users(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let conn = state.db.lock().unwrap();
    let accounts = db::list_accounts(&conn)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut out = Vec::with_capacity(accounts.len());
    for a in &accounts {
        let device_count = db::list_devices(&conn, a.id)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .len();
        out.push(json!({
            "id": a.id,
            "username": a.username,
            "created_at": a.created_at,
            "device_count": device_count,
        }));
    }
    Ok(Json(json!({ "users": out })))
}

#[derive(Deserialize)]
struct CreateUserReq {
    username: String,
    password: String,
}

async fn admin_create_user(
    State(state): State<AppState>,
    Json(req): Json<CreateUserReq>,
) -> Result<Json<Value>, ApiError> {
    if !valid_username(&req.username) {
        return Err(err(StatusCode::BAD_REQUEST, "用户名含非法字符或超长（≤32）"));
    }
    if req.password.chars().count() < 4 {
        return Err(err(StatusCode::BAD_REQUEST, "密码至少 4 位"));
    }
    let hash = auth::hash_pass(&req.password);
    let conn = state.db.lock().unwrap();
    if db::find_account_by_name(&conn, &req.username)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .is_some()
    {
        return Err(err(StatusCode::CONFLICT, "用户名已存在"));
    }
    let id = db::create_account(&conn, &req.username, &hash)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct PasswdReq {
    password: String,
}

async fn admin_change_password(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<PasswdReq>,
) -> Result<Json<Value>, ApiError> {
    if req.password.chars().count() < 4 {
        return Err(err(StatusCode::BAD_REQUEST, "密码至少 4 位"));
    }
    let hash = auth::hash_pass(&req.password);
    let conn = state.db.lock().unwrap();
    let Some(acc) = db::find_account_by_name(&conn, &name)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err(err(StatusCode::NOT_FOUND, "用户不存在"));
    };
    db::update_password(&conn, acc.id, &hash)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // 改密即失效该用户全部既有令牌
    db::delete_tokens_of(&conn, acc.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn admin_delete_user(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let conn = state.db.lock().unwrap();
    let Some(acc) = db::find_account_by_name(&conn, &name)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err(err(StatusCode::NOT_FOUND, "用户不存在"));
    };
    // 先收磁盘文件，再删行
    let _ = std::fs::remove_dir_all(db::account_dir(&state.data_dir, acc.id));
    db::delete_account(&conn, acc.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

async fn admin_list_devices(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let conn = state.db.lock().unwrap();
    let accounts = db::list_accounts(&conn)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let name_of: HashMap<i64, String> =
        accounts.iter().map(|a| (a.id, a.username.clone())).collect();
    let devices = db::list_all_devices(&conn)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut out = Vec::with_capacity(devices.len());
    for d in &devices {
        let stats = db::snapshot_stats(&conn, d.account_id)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mine = stats.iter().find(|s| s.installation_id == d.installation_id);
        out.push(json!({
            "account": name_of.get(&d.account_id).cloned().unwrap_or_default(),
            "installation_id": d.installation_id,
            "display_name": d.display_name,
            "platform": d.platform,
            "last_seen_at": d.last_seen_at,
            "last_sync_at": d.last_sync_at,
            "file_count": mine.map(|s| s.file_count).unwrap_or(0),
            "total_size": mine.map(|s| s.total_size).unwrap_or(0),
        }));
    }
    Ok(Json(json!({ "devices": out })))
}

async fn admin_unbind_device(
    State(state): State<AppState>,
    Path(installation_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !valid_installation_id(&installation_id) {
        return Err(err(StatusCode::BAD_REQUEST, "installation_id 非法"));
    }
    let conn = state.db.lock().unwrap();
    let Some(device) = db::find_any_device(&conn, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err(err(StatusCode::NOT_FOUND, "设备不存在"));
    };
    db::delete_snapshots_of(&conn, device.account_id, &installation_id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    db::delete_device(&conn, device.id)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let dir = db::device_dir(&state.data_dir, device.account_id, &installation_id);
    let _ = std::fs::remove_dir_all(dir);
    Ok(Json(json!({ "ok": true })))
}
