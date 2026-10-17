//! heng-sync-server —— 用户词库同步服务端（docs/SYNC-PLAN.md）。
//!
//! 单二进制：axum + SQLite(bundled) + argon2。账号只能由本服务的管理页面
//! 创建，无自助注册接口。管理端凭据存 `<data_dir>/admin.json`（只有
//! Argon2id 哈希）：首次启动未初始化时管理页显示初始化卡片，由管理员粘贴
//! 本地 `heng-sync-server hash` 生成的哈希串完成初始化 —— 明文口令从头到尾
//! 不经过服务端，任何环境变量里也没有它。重置 = 删 admin.json 重启。
//!
//! 用法：
//!   heng-sync-server [--bind 0.0.0.0:8080] [--data ./heng-sync-data]
//!   heng-sync-server hash            # 交互式生成管理员口令哈希串（不回显）
//!   heng-sync-server hash --stdin    # 从管道读两行（口令 / 确认）
//! 环境变量：HENG_SYNC_BIND / HENG_SYNC_DATA / HENG_SYNC_TRUSTED_PROXIES
//! 与参数等价，参数优先。

// Windows 无控制台子系统：服务端是常驻进程，被计划任务或资源管理器拉起时
// 不弹控制台窗口。从终端启动时继承父进程控制台，日志照常输出到该终端；
// `hash` 子命令同样在终端里跑（读口令不回显依赖那个共享控制台）。
#![windows_subsystem = "windows"]

use heng_sync_server::{api, auth, db};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn parse_args() -> (String, PathBuf) {
    let mut bind = std::env::var("HENG_SYNC_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let mut data = std::env::var("HENG_SYNC_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("heng-sync-data"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" => {
                if let Some(v) = args.next() {
                    bind = v;
                }
            }
            "--data" => {
                if let Some(v) = args.next() {
                    data = PathBuf::from(v);
                }
            }
            "--help" | "-h" => {
                println!("用法: heng-sync-server [--bind 0.0.0.0:8080] [--data ./heng-sync-data]");
                println!("      heng-sync-server hash [--stdin]");
                std::process::exit(0);
            }
            other => {
                eprintln!("未知参数 {other}（--help 看用法）");
                std::process::exit(2);
            }
        }
    }
    (bind, data)
}

/// 受信反向代理出口 IP（`HENG_SYNC_TRUSTED_PROXIES`，逗号分隔）。
/// 登录退避的源 IP 判定用：直连地址是环回或受信代理时才采信转发头，
/// 否则公网可伪造的 X-Real-IP / X-Forwarded-For 一律忽略（见
/// `auth::client_throttle_key`）。
fn parse_trusted_proxies() -> Vec<std::net::IpAddr> {
    std::env::var("HENG_SYNC_TRUSTED_PROXIES")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .collect()
}

/// `heng-sync-server hash` 的用法说明。
///
/// 输出一律 ASCII：中文在 Windows 控制台（GBK 代码页）会显示成乱码。
const HASH_USAGE: &str = "\
Usage: heng-sync-server hash [--stdin]

Generate an Argon2id hash string for the admin password (used by the
first-time setup page at http://<server>:8080/admin).

  --stdin   Read two lines from stdin (password / confirm). For scripts.
            Default is interactive input with echo disabled.

Examples:
  heng-sync-server hash
  printf '%s\\n%s\\n' \"$PW\" \"$PW\" | heng-sync-server hash --stdin

Output: the hash string on stdout, nothing else.";

/// 读一次口令。`from_stdin=true` 时走普通 stdin（脚本/管道场景，无法隐藏回显），
/// 否则走终端隐藏回显 —— 后者要求**真的有个终端**：用管道喂会一直等键盘输入，
/// 所以脚本里必须显式加 `--stdin`。
fn read_secret(prompt: &str, from_stdin: bool) -> std::io::Result<String> {
    if from_stdin {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    } else {
        rpassword::prompt_password(prompt)
    }
}

/// `heng-sync-server hash` —— 交互式生成管理员口令的 Argon2id 哈希串。
///
/// 让人不必去找第三方工具，用服务端自己的二进制就能生成**和运行时校验完全
/// 同一套参数**的哈希串（自己算的最大好处是参数不会跟服务端对不上）。
///
/// 交互读、终端不回显，所以明文口令既不进 shell history，也不落任何文件；
/// 哈希串打到 **stdout**（可重定向），提示信息走 stderr。
fn cmd_hash() -> i32 {
    // --stdin：从标准输入读口令（脚本用）。注意此时无法隐藏回显，
    // 也就意味着口令可能出现在管道或 shell history 里 —— 只为自动化保留。
    let from_stdin = std::env::args().any(|a| a == "--stdin");
    let first = match read_secret("Password: ", from_stdin) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read password: {e} (use --stdin in scripts)");
            return 1;
        }
    };
    // 规则与页面同一套常量，但文案用英文：中文在 Windows 控制台（GBK 代码页）
    // 会显示成乱码。去掉首尾空白后再校验与哈希，粘贴带空格不至于踩坑。
    let p = first.trim();
    if p.len() < auth::MIN_PASSWORD_LEN {
        eprintln!(
            "error: password too short (minimum {} characters)",
            auth::MIN_PASSWORD_LEN
        );
        return 1;
    }
    if p == auth::DEFAULT_PASSWORD {
        eprintln!("error: password must not be the default one");
        return 1;
    }
    let second = match read_secret("Confirm: ", from_stdin) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read password: {e}");
            return 1;
        }
    };
    if first != second {
        eprintln!("error: the two entries do not match");
        return 1;
    }
    // stdout 只有这一行哈希串，方便重定向 / 直接粘贴
    println!("{}", auth::hash_pass(p));
    0
}

#[tokio::main]
async fn main() {
    // 子命令：不碰存储与网络，先处理掉（`hash` 需要在没有数据目录的机器上也能跑）
    if std::env::args().nth(1).as_deref() == Some("hash") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        if rest.iter().any(|a| a == "--help" || a == "-h") {
            println!("{HASH_USAGE}");
            return;
        }
        // 未知参数直接报错，别默默忽略 —— 否则多打一个字符就变成「等键盘输入」的假死
        if let Some(bad) = rest.iter().find(|a| *a != "--stdin") {
            eprintln!("[heng-sync-server] unknown argument: {bad}\n\n{HASH_USAGE}");
            std::process::exit(1);
        }
        std::process::exit(cmd_hash());
    }

    let (bind, data_dir) = parse_args();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("数据目录创建失败 {}: {e}", data_dir.display());
        std::process::exit(1);
    }
    let db_path = data_dir.join("heng-sync.db");
    let conn = match db::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("数据库打开失败 {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };

    // 管理端凭据与会话密钥：admin.json 不存在 = 尚未初始化（服务照常起，
    // 管理 API 全部 409，管理页显示初始化卡片）。
    let store = auth::AdminStore::new(data_dir.clone());
    let creds = store.load_admin();
    let server_key = match store.load_or_create_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("会话密钥读写失败 {}: {e}", data_dir.display());
            std::process::exit(1);
        }
    };
    let initialized = creds.is_some();

    let state = api::AppState {
        db: Arc::new(Mutex::new(conn)),
        data_dir: Arc::new(data_dir.clone()),
        admin_creds: Arc::new(Mutex::new(creds)),
        server_key,
        admin_store: store,
        trusted_proxies: parse_trusted_proxies(),
    };
    let app = api::router(state);

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {bind} 失败: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("heng-sync-server 监听 http://{bind}（数据目录 {}）", data_dir.display());
    eprintln!("管理页: http://{bind}/admin");
    if !initialized {
        eprintln!(
            "管理员凭据未初始化：打开管理页，按提示粘贴 `heng-sync-server hash` 生成的哈希串"
        );
    }
    // 带 ConnectInfo serve：登录退避要按源 IP 计数（见 auth::client_throttle_key）
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    {
        eprintln!("服务退出: {e}");
        std::process::exit(1);
    }
}
