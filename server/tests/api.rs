//! 服务端集成测试（docs/SYNC-PLAN.md §5 全接口）。
//!
//! 每测试独立临时数据目录 + 库文件，起真实 axum 服务（ephemeral 端口），
//! 用 ureq 走完整 HTTP 协议栈（与客户端同路径）。
//!
//! 测试必须用 `flavor = "multi_thread"`：ureq 是阻塞式客户端，同步调用会占住
//! 当前线程；current-thread 运行时下被 spawn 的 axum::serve 任务永远得不到
//! 轮询，请求无人处理，测试死锁。

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use heng_sync_server::api::{self, AppState};
use heng_sync_server::{auth, db};

// ---- 测试夹具 ----

struct Server {
    base: String,
    data_dir: PathBuf,
}

async fn spawn_server() -> Server {
    // 秒级时间戳 + 进程内自增：并行测试之间不得共享同一个数据目录
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "heng-sync-it-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = db::open(&dir.join("test.db")).unwrap();
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        data_dir: Arc::new(dir.clone()),
        admin_creds: Arc::new(Mutex::new(None)),
        server_key: "test-server-key".into(),
        admin_store: auth::AdminStore::new(dir.clone()),
        trusted_proxies: vec![],
    };
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        base: format!("http://{addr}"),
        data_dir: dir,
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

/// 请求的状态码；传输层错误直接 panic（测试里不应出现）。
fn code(res: Result<ureq::Response, ureq::Error>) -> u16 {
    match res {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(c, _)) => c,
        Err(e) => panic!("传输层错误（非 HTTP 状态码）: {e}"),
    }
}

/// 直接往服务端的库灌一个客户端账号（管理接口才能建号，客户端接口不行）。
fn seed_account(srv: &Server, username: &str, password: &str) {
    let conn = db::open(&srv.data_dir.join("test.db")).unwrap();
    let hash = auth::hash_pass(password);
    db::create_account(&conn, username, &hash).unwrap();
}

fn login(srv: &Server, user: &str, pass: &str) -> Result<String, u16> {
    let res = ureq::post(&format!("{}/api/auth/login", srv.base)).send_json(
        serde_json::json!({ "username": user, "password": pass }),
    );
    match res {
        Ok(r) => {
            let body: serde_json::Value = r.into_json().unwrap();
            Ok(body["token"].as_str().unwrap().to_string())
        }
        Err(ureq::Error::Status(c, _)) => Err(c),
        Err(e) => panic!("传输层错误: {e}"),
    }
}

/// 管理员登录：成功返回 JWT，失败返回服务端的错误说明。
/// 失败时 HTTP 是 **401**（退避锁定期是 429）——前端按状态码判断成败，
/// 200 + body.error 会被当成功放行，`admin_flow` 里有断言锁住这一点。
fn admin_login(srv: &Server, user: &str, pass: &str) -> Result<String, String> {
    let res = ureq::post(&format!("{}/api/admin/login", srv.base)).send_json(
        serde_json::json!({ "user": user, "pass": pass }),
    );
    match res {
        Ok(r) => {
            let body: serde_json::Value = r.into_json().unwrap();
            match body["token"].as_str() {
                Some(t) => Ok(t.to_string()),
                None => Err(body["error"].as_str().unwrap_or("登录失败").to_string()),
            }
        }
        Err(ureq::Error::Status(c, r)) => {
            let body: serde_json::Value = r.into_json().unwrap_or_default();
            Err(body["error"].as_str().unwrap_or(&format!("HTTP {c}")).to_string())
        }
        Err(e) => panic!("传输层错误: {e}"),
    }
}

/// 初始化管理员凭据（本地生成哈希串 → init）。返回 init 的状态码。
fn admin_init(srv: &Server, user: &str, pass: &str) -> u16 {
    let hash = auth::hash_pass(pass);
    code(ureq::post(&format!("{}/api/admin/init", srv.base)).send_json(
        serde_json::json!({ "user": user, "pass_hash": hash }),
    ))
}

fn api_json(srv: &Server, token: &str, path: &str) -> serde_json::Value {
    ureq::get(&format!("{}{path}", srv.base))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap()
}

/// 造一份最小合法快照（头部 + 一条词条）。
fn fake_snapshot(user_id: &str, tick: i64) -> Vec<u8> {
    format!(
        "# Rime user dictionary\n#@/db_name rime_ice\n#@/db_type userdb\n#@/rime_version 1.17.0\n#@/tick {tick}\n#@/user_id {user_id}\n你好 ni3hao3 c=1 d=1.0 t=1640000000\n"
    )
    .into_bytes()
}

// ---- 账号与鉴权 ----

#[tokio::test(flavor = "multi_thread")]
async fn login_rejects_bad_credentials_and_missing_token() {
    let srv = spawn_server().await;
    // 无账号时任何登录都失败（401，且不泄漏"用户不存在"）
    assert_eq!(login(&srv, "nobody", "whatever").unwrap_err(), 401);
    seed_account(&srv, "alice", "pass-alice");
    assert_eq!(login(&srv, "alice", "wrong").unwrap_err(), 401);
    assert!(login(&srv, "alice", "pass-alice").is_ok());
    // 无令牌
    assert_eq!(
        code(ureq::get(&format!("{}/api/devices", srv.base)).call()),
        401
    );
    // 错令牌
    assert_eq!(
        code(ureq::get(&format!("{}/api/devices", srv.base))
            .set("Authorization", "Bearer deadbeef")
            .call()),
        401
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn full_client_flow() {
    let srv = spawn_server().await;
    seed_account(&srv, "alice", "pass-alice");
    seed_account(&srv, "bob", "pass-bob");
    let ta = login(&srv, "alice", "pass-alice").unwrap();
    let tb = login(&srv, "bob", "pass-bob").unwrap();

    // 设备注册（upsert：重复注册不新增，display_name 更新）
    for _ in 0..2 {
        ureq::post(&format!("{}/api/devices", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({
                "installation_id": "windows-dmz-1A2B3C",
                "display_name": "台式机",
                "platform": "windows",
            }))
            .unwrap();
    }
    let devs = api_json(&srv, &ta, "/api/devices");
    assert_eq!(devs["devices"].as_array().unwrap().len(), 1);
    assert_eq!(devs["devices"][0]["installation_id"], "windows-dmz-1A2B3C");

    // 上传快照
    let bytes = fake_snapshot("windows-dmz-1A2B3C", 42);
    let up = ureq::put(&format!(
        "{}/api/sync/file/windows-dmz-1A2B3C/rime_ice.userdb.txt",
        srv.base
    ))
    .set("Authorization", &format!("Bearer {ta}"))
    .send_bytes(&bytes)
    .unwrap();
    let up_body: serde_json::Value = up.into_json().unwrap();
    assert_eq!(up_body["size"], bytes.len() as u64);
    assert_eq!(up_body["tick"], 42);
    assert_eq!(up_body["sha256"].as_str().unwrap().len(), 64);

    // 覆盖上传：tick 更新而非新增行
    let bytes2 = fake_snapshot("windows-dmz-1A2B3C", 43);
    ureq::put(&format!(
        "{}/api/sync/file/windows-dmz-1A2B3C/rime_ice.userdb.txt",
        srv.base
    ))
    .set("Authorization", &format!("Bearer {ta}"))
    .send_bytes(&bytes2)
    .unwrap();

    // manifest：本账号全部设备与文件元数据
    let mf = api_json(&srv, &ta, "/api/sync/manifest");
    let devices = mf["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    let files = devices[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["tick"], 43);

    // 下载：字节一致
    let mut got = Vec::new();
    ureq::get(&format!(
        "{}/api/sync/file/windows-dmz-1A2B3C/rime_ice.userdb.txt",
        srv.base
    ))
    .set("Authorization", &format!("Bearer {ta}"))
    .call()
    .unwrap()
    .into_reader()
    .read_to_end(&mut got)
    .unwrap();
    assert_eq!(got, bytes2);

    // 下载不存在的快照 → 404
    assert_eq!(
        code(ureq::get(&format!(
            "{}/api/sync/file/windows-dmz-1A2B3C/nope.userdb.txt",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {ta}"))
        .call()),
        404
    );

    // 非 userdb 文件名 → 400（*.custom.yaml 等不同步，SYNC-PLAN §1）
    assert_eq!(
        code(ureq::put(&format!(
            "{}/api/sync/file/windows-dmz-1A2B3C/weasel.custom.yaml",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_bytes(b"foo: bar")),
        400
    );
    // 空文件名段 → 400
    assert_eq!(
        code(ureq::put(&format!(
            "{}/api/sync/file/windows-dmz-1A2B3C/.userdb.txt",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_bytes(b"x")),
        400
    );
    // 空 body → 400
    assert_eq!(
        code(ureq::put(&format!(
            "{}/api/sync/file/windows-dmz-1A2B3C/x.userdb.txt",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_bytes(b"")),
        400
    );

    // 跨账号隔离：bob 看不到 alice 的设备与快照
    assert_eq!(
        code(ureq::get(&format!(
            "{}/api/sync/file/windows-dmz-1A2B3C/rime_ice.userdb.txt",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {tb}"))
        .call()),
        404
    );
    let bob_devs = api_json(&srv, &tb, "/api/devices");
    assert_eq!(bob_devs["devices"].as_array().unwrap().len(), 0);

    // 未注册设备上传 → 404
    assert_eq!(
        code(ureq::put(&format!(
            "{}/api/sync/file/macbook-x-998877/rime_ice.userdb.txt",
            srv.base
        ))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_bytes(b"x")),
        404
    );

    // 解绑：行与磁盘文件一并消失
    ureq::delete(&format!(
        "{}/api/devices/windows-dmz-1A2B3C",
        srv.base
    ))
    .set("Authorization", &format!("Bearer {ta}"))
    .call()
    .unwrap();
    let devs = api_json(&srv, &ta, "/api/devices");
    assert_eq!(devs["devices"].as_array().unwrap().len(), 0);
    assert!(!srv
        .data_dir
        .join("1/windows-dmz-1A2B3C/rime_ice.userdb.txt")
        .exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_flow() {
    let srv = spawn_server().await;

    // 未初始化：数据面 409（带 not_initialized），init-status 公开可查
    assert_eq!(
        code(ureq::get(&format!("{}/api/admin/users", srv.base)).call()),
        409
    );
    let st: serde_json::Value = ureq::get(&format!("{}/api/admin/init-status", srv.base))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(st["initialized"], false);

    // 客户端账号照常建、照常登录——与管理端体系完全分离
    seed_account(&srv, "carol", "user-pass");
    let tu = login(&srv, "carol", "user-pass").unwrap();

    // 初始化管理员：明文口令当哈希贴进来 → 400
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/init", srv.base)).send_json(
            serde_json::json!({ "user": "admin", "pass_hash": "my-plain-password" })
        )),
        400
    );
    // 正常 init（本地生成哈希串），只能做一次
    assert_eq!(admin_init(&srv, "admin", "admin-root-pass"), 200);
    assert_eq!(admin_init(&srv, "admin", "another-pass"), 409);
    // 初始化后：客户端令牌访问管理数据面 → 401（管理端不认这种令牌）
    assert_eq!(
        code(ureq::get(&format!("{}/api/admin/users", srv.base))
            .set("Authorization", &format!("Bearer {tu}"))
            .call()),
        401
    );
    let st: serde_json::Value = ureq::get(&format!("{}/api/admin/init-status", srv.base))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(st["initialized"], true);

    // 管理员登录：错口令必须 401（不能 200 + body.error，否则前端放行）
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/login", srv.base)).send_json(
            serde_json::json!({ "user": "admin", "pass": "wrong-pass" })
        )),
        401
    );
    assert!(admin_login(&srv, "admin", "wrong-pass").is_err());
    // 空 user/空口令同样 401
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/login", srv.base)).send_json(
            serde_json::json!({ "user": "", "pass": "" })
        )),
        401
    );
    assert!(admin_login(&srv, "admin", "admin-root-pass").is_ok());
    let ta = admin_login(&srv, "admin", "admin-root-pass").unwrap();
    assert!(!ta.is_empty());

    // 用户列表：只有客户端账号（管理员不在库里）
    let users = api_json(&srv, &ta, "/api/admin/users");
    assert_eq!(users["users"].as_array().unwrap().len(), 1);
    assert_eq!(users["users"][0]["username"], "carol");

    // 新增用户 + 登录 + 同名冲突 + 过短密码
    ureq::post(&format!("{}/api/admin/users", srv.base))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({ "username": "dave", "password": "pw-dave" }))
        .unwrap();
    assert!(login(&srv, "dave", "pw-dave").is_ok());
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/users", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({ "username": "dave", "password": "pw-dave" }))),
        409
    );
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/users", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({ "username": "eve", "password": "ab" }))),
        400
    );

    // 改客户端用户密码：旧令牌失效、新密码可登录
    let old_dave = login(&srv, "dave", "pw-dave").unwrap();
    ureq::post(&format!("{}/api/admin/users/dave/passwd", srv.base))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({ "password": "pw-dave2" }))
        .unwrap();
    assert_eq!(
        code(ureq::get(&format!("{}/api/devices", srv.base))
            .set("Authorization", &format!("Bearer {old_dave}"))
            .call()),
        401
    );
    assert!(login(&srv, "dave", "pw-dave2").is_ok());

    // 设备列表（按账号）+ 强制解绑
    {
        let conn = db::open(&srv.data_dir.join("test.db")).unwrap();
        let carol = db::find_account_by_name(&conn, "carol").unwrap().unwrap();
        db::upsert_device(&conn, carol.id, "linux-hp-ABCDEF", "HP 主机", "linux").unwrap();
    }
    let devs = api_json(&srv, &ta, "/api/admin/devices");
    let arr = devs["devices"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["account"], "carol");
    ureq::delete(&format!(
        "{}/api/admin/devices/linux-hp-ABCDEF",
        srv.base
    ))
    .set("Authorization", &format!("Bearer {ta}"))
    .call()
    .unwrap();
    let devs = api_json(&srv, &ta, "/api/admin/devices");
    assert_eq!(devs["devices"].as_array().unwrap().len(), 0);

    // 改管理员自己的密码：验旧口令、新口令强度、旧 JWT 立即失效
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/password", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({ "old_pass": "wrong", "new_pass": "new-root-pass" }))),
        400
    );
    assert_eq!(
        code(ureq::post(&format!("{}/api/admin/password", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .send_json(serde_json::json!({ "old_pass": "admin-root-pass", "new_pass": "short" }))),
        400
    );
    ureq::post(&format!("{}/api/admin/password", srv.base))
        .set("Authorization", &format!("Bearer {ta}"))
        .send_json(serde_json::json!({ "old_pass": "admin-root-pass", "new_pass": "new-root-pass" }))
        .unwrap();
    // 旧 JWT 失效；新口令可登录
    assert_eq!(
        code(ureq::get(&format!("{}/api/admin/users", srv.base))
            .set("Authorization", &format!("Bearer {ta}"))
            .call()),
        401
    );
    let ta2 = admin_login(&srv, "admin", "new-root-pass").unwrap();
    assert!(!ta2.is_empty());

    // 删除用户（级联：令牌、设备、快照、文件）
    ureq::delete(&format!("{}/api/admin/users/dave", srv.base))
        .set("Authorization", &format!("Bearer {ta2}"))
        .call()
        .unwrap();
    assert_eq!(login(&srv, "dave", "pw-dave2").unwrap_err(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_page_served_without_auth() {
    let srv = spawn_server().await;
    let res = ureq::get(&format!("{}/admin", srv.base)).call().unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.header("content-type").unwrap().starts_with("text/html"));
    let html = res.into_string().unwrap();
    // 三态页面：初始化卡片 + 登录卡片 + 数据面
    assert!(html.contains("/api/admin/init-status"));
    assert!(html.contains("/api/admin/init"));
    assert!(html.contains("/api/admin/login"));
    assert!(html.contains("管理员登录"));
}
