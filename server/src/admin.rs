//! 管理端自身的认证接口（docs/SYNC-PLAN.md §5.2）。
//!
//! 与客户端账号体系（`/api/auth/login` + account 表）**完全分离**：管理端
//! 只有一个账号，凭据存 `<data_dir>/admin.json`（只有 Argon2id 哈希），会话
//! 是自实现 JWT（HS256，密钥 `server.key`）。与 ClipSync 服务端同一套模式：
//!
//! - 首次启动未初始化 → 管理页显示初始化卡片 → 管理员粘贴**本地生成的
//!   Argon2id PHC 哈希串**（`heng-sync-server hash`）→ `POST /api/admin/init`
//!   （公开、仅一次、不签发会话，须用真实口令登录一次验证）
//! - `POST /api/admin/login` 校验口令 → 签发 JWT（claims 带密码版本号，
//!   改密后旧令牌全部失效）
//! - `POST /api/admin/password` 改密（须验旧口令）
//! - 重置 = 删 `admin.json` 重启
//!
//! 登录退避按源 IP 计数（5 次锁 30s、倍增封顶 900s），锁定期内不做口令
//! 校验（不耗 Argon2）。

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;

use crate::api::AppState;
use crate::auth::{self, AdminCreds};

/// 对端地址（**可选**提取）：读 `ConnectInfo` 扩展，拿不到就是 `None`。
///
/// axum 0.8 里 `Option<ConnectInfo<_>>` 不能直接当提取器用（它的 rejection 是
/// 500 而不是 `Infallible`，`Option<T>` 只兜 `OptionalFromRequestParts` 实现的
/// 类型）。单测的 oneshot 请求没有对端地址，此时退避退化为全局键；正常 serve
/// （`into_make_service_with_connect_info`）下扩展里一定有值。
pub struct PeerAddr(Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for PeerAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(PeerAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// 取一份凭据快照：`None` = 尚未初始化。
fn creds_snapshot(state: &AppState) -> Option<AdminCreds> {
    state
        .admin_creds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 「尚未初始化」的统一响应：409 + `code`，前端据此切到初始化页面。
fn not_initialized() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": "管理员凭据尚未初始化", "code": "not_initialized"})),
    )
        .into_response()
}

/// 管理 API 鉴权中间件：校验 Bearer 会话 token（JWT HS256）。
///
/// 未初始化时一律 409 —— 此时根本没有凭据可校验，必须明确区分「没初始化」
/// 和「token 不对」，否则前端只会看到「未授权」然后反复弹登录框。
pub async fn admin_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(creds) = creds_snapshot(&state) else {
        return not_initialized();
    };
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let ok = token
        .map(|t| auth::verify_session(&state.server_key, t, creds.updated_at).is_some())
        .unwrap_or(false);
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub user: String,
    pub pass: String,
}

/// 登录：校验密码后签发标准 JWT 会话令牌（HS256，3 天有效，无状态）。
///
/// 状态码约定：成功 200 + `{token}`；口令或用户名不对 **401**；退避锁定期
/// **429**。失败一律带非 2xx 状态码——前端按 `res.ok` 判断成败，200 + body.error
/// 会被当成功放行（空口令直进主界面就是这么来的）。
pub async fn admin_login(
    State(state): State<AppState>,
    // 对端地址可选：单测的 oneshot 请求没有，此时退避退化为全局键
    PeerAddr(peer): PeerAddr,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let Some(creds) = creds_snapshot(&state) else {
        return not_initialized();
    };
    let throttle_key = auth::client_throttle_key(peer, &headers, &state.trusted_proxies);
    // 1) 退避检查（按源 IP）：锁定期内直接拒绝，且**不做口令校验**（不耗 Argon2）
    if let Some(left) = auth::throttle_locked_secs(throttle_key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": format!("登录尝试过于频繁，请 {left} 秒后再试")
            })),
        )
            .into_response();
    }
    if body.user != creds.user || !auth::verify_pass(&creds.pass_hash, &body.pass) {
        // 2) 失败计数与递增锁定
        auth::record_login_fail(throttle_key);
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "用户名或密码错误"})),
        )
            .into_response();
    }
    // 3) 成功：清零退避状态
    auth::record_login_success(throttle_key);
    let token = auth::issue_session(&state.server_key, &creds.user, creds.updated_at);
    Json(json!({ "token": token })).into_response()
}

#[derive(Deserialize)]
pub struct InitBody {
    /// 管理员用户名
    pub user: String,
    /// **口令哈希串**（不是口令本身）：管理员在别处用 Argon2id 生成后粘贴过来
    pub pass_hash: String,
}

/// 管理员用户名长度上限。
///
/// 初始化端点是**公开**的（未初始化时谁都能调），所以每个入参都要有上限 ——
/// 否则可以在窗口期塞一个几 MB 的用户名进 `admin.json`，之后每次读凭据都难受。
const ADMIN_USER_MAX_LEN: usize = 64;
/// 新口令长度上限（正常口令远小于此；防止已登录者用超长输入拖慢 Argon2）。
const ADMIN_PASS_MAX_LEN: usize = 1024;

/// GET /api/admin/init-status —— 是否已经初始化（**公开**端点）。
///
/// 公开是因为它只回答「有没有凭据」这一个比特，而「没有凭据」这件事对任何
/// 访问者都是显然的（届时所有管理接口都返回同一个 409）。前端据此决定显示
/// 登录卡片还是初始化卡片。
pub async fn admin_init_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "initialized": creds_snapshot(&state).is_some() }))
}

/// POST /api/admin/init —— **仅在未初始化时**可用：写入用户名与口令哈希串。
///
/// 设计取舍：
/// - 只收**哈希串**、不收明文口令：明文从生成到使用都不经过服务端；
/// - 只能做一次（已初始化即 409）—— 否则谁都能抢先给自己设个口令；
///   判据是**内存态 + 磁盘各查一次**：内存态挡住正常路径，磁盘那次挡住
///   「运行期间手工放进 admin.json」这种两边不一致的情况；
/// - 写入后**不签发会话**：管理员必须用真实口令登录一次，能登进去才证明他
///   贴的这串哈希确实对应他知道的那个口令。否则贴错一串就等于把自己永久
///   锁在门外（只能去删 admin.json 重启）。
pub async fn admin_init(
    State(state): State<AppState>,
    Json(body): Json<InitBody>,
) -> Response {
    let user = body.user.trim().to_string();
    if user.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "用户名不能为空"})),
        )
            .into_response();
    }
    if user.chars().count() > ADMIN_USER_MAX_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("用户名过长（上限 {ADMIN_USER_MAX_LEN} 字符）")})),
        )
            .into_response();
    }
    // 控制字符一律拒绝：这个值会写进 admin.json、也会进日志与页面，
    // 带换行就能伪造日志行、把管理页的显示搅乱。
    if user.chars().any(|c| c.is_control()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "用户名不能包含换行等控制字符"})),
        )
            .into_response();
    }
    // 哈希校验放在取锁之前：它要跑一次 Argon2（慢），别占着锁
    let hash = match auth::validate_hash_input(&body.pass_hash) {
        Ok(h) => h,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    // 检查与写入在同一个临界区内，避免两个并发请求都通过「未初始化」检查
    let mut guard = state.admin_creds.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "管理员凭据已初始化", "code": "already_initialized"})),
        )
            .into_response();
    }
    // 二次确认：**磁盘上真的没有凭据文件**。
    //
    // 内存态是启动时读进来的，两者可能不一致 —— 比如运行期间有人（部署脚本）
    // 手工放了一份 admin.json，内存里还是 None。只看内存的话，这个请求就会把
    // 那份文件**悄悄覆盖掉**，管理员还以为自己配的口令生效了。
    // 顺手把内存态补成磁盘内容，状态就自愈了。
    if let Some(on_disk) = state.admin_store.load_admin() {
        *guard = Some(on_disk);
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "凭据文件已存在，不能覆盖（请用真实密码登录，或直接改数据目录下的 admin.json）",
                "code": "already_initialized"
            })),
        )
            .into_response();
    }
    let creds = AdminCreds {
        user: user.clone(),
        pass_hash: hash,
        updated_at: auth::now_unix(),
    };
    if let Err(e) = state.admin_store.save_admin(&creds) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("写入 admin.json 失败：{e}")})),
        )
            .into_response();
    }
    *guard = Some(creds);
    Json(json!({"ok": true, "user": user})).into_response()
}

#[derive(Deserialize)]
pub struct ChangePassBody {
    /// 当前密码（必须提供：防止拿到会话就能改密码）
    pub old_pass: String,
    pub new_pass: String,
}

/// POST /api/admin/password —— 修改管理员密码。
///
/// 要点：① 必须验旧口令；② 长度与强度校验沿用初始化时的规则（≥8 位、不许是
/// `heng`）；③ 只把 **Argon2id 哈希**写进 admin.json（0600），明文不落盘；
/// ④ 更新密码版本号 → **所有已签发的会话令牌立即失效**，前端改完必须重新登录。
pub async fn change_password(
    State(state): State<AppState>,
    Json(body): Json<ChangePassBody>,
) -> Response {
    let new = body.new_pass.trim();
    if new.len() > ADMIN_PASS_MAX_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("新密码过长（上限 {ADMIN_PASS_MAX_LEN} 字符）")})),
        )
            .into_response();
    }
    if let Err(e) = auth::validate_new_password(new) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    let updated = match state.admin_store.change_password(&body.old_pass, new) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e})),
            )
                .into_response()
        }
    };
    // 内存态同步：改完密码，新哈希与版本号都要立刻生效（否则新密码登不进来、
    // 旧令牌也还在用）
    {
        let mut cur = state.admin_creds.lock().unwrap_or_else(|e| e.into_inner());
        *cur = Some(updated);
    }
    Json(json!({"ok": true})).into_response()
}

#[cfg(test)]
mod init_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn tmp_state(creds: Option<AdminCreds>) -> (AppState, std::path::PathBuf) {
        let uniq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "heng-admin-init-ut-{}-{uniq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open(&dir.join("test.db")).unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            data_dir: Arc::new(dir.clone()),
            admin_creds: Arc::new(Mutex::new(creds)),
            server_key: "test-key".into(),
            admin_store: crate::auth::AdminStore::new(dir.clone()),
            trusted_proxies: vec![],
        };
        (state, dir)
    }

    fn on_disk_hash(state: &AppState) -> Option<String> {
        state.admin_store.load_admin().map(|c| c.pass_hash)
    }

    /// 场景一：内存已是「已初始化」→ 409，磁盘原封不动。
    #[tokio::test]
    async fn refuses_when_memory_says_initialized() {
        let original = auth::hash_pass("original-pw");
        let (state, _dir) = tmp_state(Some(AdminCreds {
            user: "admin".into(),
            pass_hash: original.clone(),
            updated_at: 1,
        }));
        state
            .admin_store
            .save_admin(&AdminCreds {
                user: "admin".into(),
                pass_hash: original.clone(),
                updated_at: 1,
            })
            .unwrap();

        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "hacker".into(),
                pass_hash: auth::hash_pass("evil-pw"),
            }),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            on_disk_hash(&state),
            Some(original),
            "磁盘上的凭据必须纹丝不动"
        );
    }

    /// 场景二：内存说没初始化，但磁盘上已经有凭据文件（运行期间被手工放进去的）
    /// → 同样 409，并且**不能覆盖**那份文件（否则管理员配的口令会被页面悄悄改写）。
    #[tokio::test]
    async fn refuses_when_credentials_file_exists_on_disk() {
        let original = auth::hash_pass("handwritten-pw");
        let (state, _dir) = tmp_state(None); // 内存：未初始化
        state
            .admin_store
            .save_admin(&AdminCreds {
                user: "admin".into(),
                pass_hash: original.clone(),
                updated_at: 1,
            })
            .unwrap();

        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "hacker".into(),
                pass_hash: auth::hash_pass("evil-pw"),
            }),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            on_disk_hash(&state),
            Some(original),
            "不能覆盖磁盘上已有的凭据"
        );
        // 顺便自愈：内存态补成磁盘内容，之后不用重启也能正常登录
        assert!(creds_snapshot(&state).is_some(), "内存态应被补成磁盘内容");
    }

    /// 场景三：真的没初始化 → 第一次成功，第二次必定被拒且内容不变。
    #[tokio::test]
    async fn succeeds_once_then_locks_out() {
        let (state, _dir) = tmp_state(None);
        let mine = auth::hash_pass("my-real-pw");

        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "admin".into(),
                pass_hash: mine.clone(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(on_disk_hash(&state), Some(mine.clone()));

        let resp2 = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "hacker".into(),
                pass_hash: auth::hash_pass("evil-pw"),
            }),
        )
        .await;
        assert_eq!(resp2.status(), StatusCode::CONFLICT);
        assert_eq!(
            on_disk_hash(&state),
            Some(mine),
            "第二次不得改写已写入的凭据"
        );
    }

    /// 明文口令、控制字符用户名、空用户名一律拒绝（初始化端点是公开的）。
    #[tokio::test]
    async fn rejects_garbage_init_input() {
        let (state, _dir) = tmp_state(None);
        // 明文口令当哈希贴进来
        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "admin".into(),
                pass_hash: "my-plain-password".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 控制字符用户名
        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "adm\nin".into(),
                pass_hash: auth::hash_pass("whatever1"),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 空用户名
        let resp = admin_init(
            State(state.clone()),
            Json(InitBody {
                user: "  ".into(),
                pass_hash: auth::hash_pass("whatever1"),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 上面全部失败 → 仍然未初始化
        assert!(creds_snapshot(&state).is_none());
        assert!(on_disk_hash(&state).is_none());
    }
}
