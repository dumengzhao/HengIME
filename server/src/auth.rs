//! 口令哈希、管理员凭据与管理员会话（docs/SYNC-PLAN.md §3）。
//!
//! 口令一律 Argon2id，存储为 PHC 标准串
//! `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`：算法与参数记录在串里，
//! 以后调参数，历史哈希仍按串内声明验过。账号口令（客户端登录，存库）与
//! 管理员口令（`<data_dir>/admin.json`）共用同一套实现。
//!
//! 管理员口令的**明文不经过服务端**：初始化时管理员粘贴自己在别处生成的
//! 哈希串（`heng-sync-server hash` 或第三方工具），落盘与内存里只有哈希。
//! 与 ClipSync 服务端同一套模式。
//!
//! 管理员会话是自实现 JWT（HS256，密钥 `<data_dir>/server.key`），claims 带
//! 密码版本号 `pwd`：改密后旧令牌立即失效。服务端不存储会话。
//!
//! 客户端令牌仍是 256 位随机值 hex，存 `token` 表（db 层），长期有效。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::Error as HashError;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 生成随机盐 / 令牌的熵源（系统 CSPRNG）。
fn random_bytes(n: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

/// 客户端登录令牌：256 位随机值 hex，存 `token` 表。
pub fn random_token() -> String {
    random_bytes(32).iter().map(|b| format!("{b:02x}")).collect()
}

// ---- 口令哈希（PHC 标准串） ----

/// Argon2id 参数：OWASP 最低档（m=19MiB、t=2、p=1）。
const ARGON_M_KIB: u32 = 19 * 1024;
const ARGON_T: u32 = 2;
const ARGON_P: u32 = 1;

/// 口令哈希：Argon2id，输出 PHC 标准串。
///
/// 参数取 OWASP 当前推荐的最低档（再往上要看部署机内存）。盐由
/// `SaltString::generate` 随机生成并编码进串里，不用另记。
pub fn hash_pass(pass: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    let params = Params::new(ARGON_M_KIB, ARGON_T, ARGON_P, None).unwrap_or_default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password(pass.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

/// 校验口令：算法与参数都**从 PHC 串里取**，这样以后换算法/调参数，
/// 老哈希照样能验（硬编码实例会让历史哈希全部验不过）。
pub fn verify_pass(hash: &str, pass: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    let algo = match parsed.algorithm.as_str() {
        "argon2d" => Algorithm::Argon2d,
        "argon2i" => Algorithm::Argon2i,
        _ => Algorithm::Argon2id,
    };
    Argon2::new(algo, Version::V0x13, Params::default())
        .verify_password(pass.as_bytes(), &parsed)
        .is_ok()
}

/// 新口令的最短长度（初始化、页面改密码、`hash` 子命令共用同一套规则）。
pub const MIN_PASSWORD_LEN: usize = 8;
/// 一律拒绝的默认口令。
pub const DEFAULT_PASSWORD: &str = "heng";

/// 新口令的强度校验。
///
/// 这个口令保护的是「账号与设备管理」接口——拿到它就能看全部用户的词库
/// 快照、建号、强制解绑，所以太短或仍是默认值一律拒绝，绝不静默放行。
///
/// 错误文案是中文，给管理页（JSON）用；`hash` 子命令走终端，那边自己拼
/// 英文短句（Windows 控制台 GBK 代码页下中文会乱码），规则常量在这里共用。
pub fn validate_new_password(pass: &str) -> Result<(), String> {
    let p = pass.trim();
    if p.len() < MIN_PASSWORD_LEN {
        return Err(format!("密码至少 {MIN_PASSWORD_LEN} 位"));
    }
    if p == DEFAULT_PASSWORD {
        return Err(format!("密码不能是默认口令 {DEFAULT_PASSWORD}"));
    }
    Ok(())
}

/// 外部哈希串的长度上限。正常 PHC 串不超过约 120 字符，512 足够宽松。
const HASH_INPUT_MAX_LEN: usize = 512;
/// 接受的最大内存参数（KiB）。上限存在的意义见 `check_hash_params_bounded`。
const HASH_MEM_MAX_KIB: u64 = 256 * 1024;
/// 接受的最大迭代次数。
const HASH_TIME_MAX: u64 = 16;
/// 接受的最大并行度。
const HASH_LANES_MAX: u64 = 8;

/// 检查外部哈希串声明的 Argon2 参数是否在合理范围内。
///
/// 这不是「参数调优」，而是**防 DoS**：`verify_pass` 会按串里声明的参数
/// 分配内存、跑那么多轮 —— 而初始化端点（未初始化时）是公开的，任何人都能
/// 贴一串 `m=4194304,t=100,p=8`（4 GiB / 100 轮）进来，让服务端在分配内存
/// 时被拖死。我们自己生成时固定 19 MiB 档，这里给外部输入留 256 MiB 的
/// 宽裕上限即可。
///
/// 直接从串的 params 段（第 4 段 `m=..,t=..,p=..`）解析，不依赖库的内部
/// 类型；解析不出来就不拦（后面的 `verify_pass` 会自己失败）。
fn check_hash_params_bounded(s: &str) -> Result<(), String> {
    let seg = s.split('$').nth(3).unwrap_or("");
    for kv in seg.split(',') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let Ok(n) = v.trim().parse::<u64>() else {
            continue;
        };
        match k.trim() {
            "m" if n > HASH_MEM_MAX_KIB => {
                return Err(format!(
                    "哈希串声明的内存参数过大（m={n} KiB，上限 {HASH_MEM_MAX_KIB} KiB）"
                ))
            }
            "t" if n > HASH_TIME_MAX => {
                return Err(format!(
                    "哈希串声明的迭代次数过多（t={n}，上限 {HASH_TIME_MAX}）"
                ))
            }
            "p" if n > HASH_LANES_MAX => {
                return Err(format!(
                    "哈希串声明的并行度过大（p={n}，上限 {HASH_LANES_MAX}）"
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// 校验「外部生成的口令哈希串」能不能直接当管理员凭据用。
///
/// 初始化页面允许管理员粘贴自己在别处生成的哈希串 —— 明文口令不经过本服务，
/// 所以这里必须挡住四类坏输入：
/// ① 不是 PHC 串（典型就是把**明文口令**当哈希贴进来）；
/// ② 不是 argon2 家族（bcrypt/scrypt/md5crypt 一律不收）；
/// ③ 串过长、或参数离谱（见 `check_hash_params_bounded` —— 这是防 DoS）；
/// ④ 串本身可解析，但参数不受支持。
///
/// 判据：拿一个**探测口令**去验一次。返回 `Error::Password`（只是口令不匹配）
/// 说明串的格式、算法、参数全都受支持；其它错误一律拒绝。
/// 返回规范化后的哈希串（去掉首尾空白）。
pub fn validate_hash_input(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("哈希串不能为空".into());
    }
    if s.len() > HASH_INPUT_MAX_LEN {
        return Err(format!(
            "哈希串过长（{} 字符，上限 {HASH_INPUT_MAX_LEN}）",
            s.len()
        ));
    }
    let parsed = PasswordHash::new(s).map_err(|_| {
        "不是合法的口令哈希串：应形如 $argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>，\
         直接贴明文口令是不行的"
            .to_string()
    })?;
    let algo = match parsed.algorithm.as_str() {
        "argon2d" => Algorithm::Argon2d,
        "argon2i" => Algorithm::Argon2i,
        "argon2id" => Algorithm::Argon2id,
        other => {
            return Err(format!(
                "不支持的哈希算法 {other}（只接受 argon2id / argon2i / argon2d）"
            ))
        }
    };
    // 必须在真正跑 Argon2 **之前**拦：这一步就是攻击者想让我们分配内存的地方
    check_hash_params_bounded(s)?;
    match Argon2::new(algo, Version::V0x13, Params::default())
        .verify_password(b"__heng_init_probe__", &parsed)
    {
        // 探测口令不可能真的匹配上；真匹配说明这串有问题，拒绝
        Ok(()) => Err("该哈希串校验异常，拒绝使用".into()),
        Err(HashError::Password) => Ok(s.to_string()),
        Err(e) => Err(format!("哈希串不可用：{e}")),
    }
}

// ---- 管理员凭据（admin.json） ----

/// 管理员凭据（**只存哈希，绝不存明文**），落盘到 `<data_dir>/admin.json`。
///
/// 文件不存在 = **尚未初始化**：服务照常启动，但管理 API 全部拒绝，管理页
/// 改为显示「初始化」卡片，由管理员粘贴**自己在别处生成**的哈希串（明文
/// 口令从头到尾不经过服务端）。重置 = 删掉这个文件后重启。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminCreds {
    pub user: String,
    /// PHC 字符串：`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`
    pub pass_hash: String,
    /// 最近一次改密码的时间（Unix 秒）—— 同时作为**会话版本号**：
    /// 改密码后旧 JWT 立即失效（见 `verify_session` 的 `pwd_epoch` 参数）。
    #[serde(default)]
    pub updated_at: u64,
}

/// 管理员凭据与会话密钥的文件存储（数据目录下两个小文件）。
#[derive(Clone)]
pub struct AdminStore {
    dir: PathBuf,
}

impl AdminStore {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        AdminStore { dir }
    }

    fn admin_path(&self) -> PathBuf {
        self.dir.join("admin.json")
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("server.key")
    }

    /// 读取管理员凭据；文件不存在/损坏都返回 `None`（= 未初始化）。
    pub fn load_admin(&self) -> Option<AdminCreds> {
        let s = std::fs::read_to_string(self.admin_path()).ok()?;
        serde_json::from_str::<AdminCreds>(&s).ok()
    }

    /// 写入管理员凭据（原子写 + 0600）。
    pub fn save_admin(&self, c: &AdminCreds) -> Result<(), String> {
        let s = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
        atomic_write(&self.admin_path(), &s)
    }

    /// 改密码：校验旧口令通过后才落新哈希，并刷新 `updated_at`（使旧会话失效）。
    /// 返回新凭据；旧口令不对返回 `Err`。
    pub fn change_password(&self, old_pass: &str, new_pass: &str) -> Result<AdminCreds, String> {
        let cur = self.load_admin().ok_or("凭据未初始化".to_string())?;
        if !verify_pass(&cur.pass_hash, old_pass) {
            return Err("当前密码不正确".into());
        }
        let next = AdminCreds {
            user: cur.user,
            pass_hash: hash_pass(new_pass),
            updated_at: now_unix(),
        };
        self.save_admin(&next)?;
        Ok(next)
    }

    /// 读取会话签名密钥；缺失则随机生成并落盘。
    pub fn load_or_create_key(&self) -> Result<String, String> {
        let p = self.key_path();
        if let Ok(s) = std::fs::read_to_string(&p) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Ok(t);
            }
        }
        let key = random_token();
        atomic_write(&p, &key)?;
        Ok(key)
    }
}

/// 原子写：写临时文件 + rename，避免半截文件。
///
/// **落盘即 0600**：这个目录下的东西没有一个是该给别人看的 ——
/// `admin.json`（口令哈希）、`server.key`（会话签名密钥）。默认 0644 会让
/// 同机任意用户读到。只在 unix 下设置权限：Windows 的 `set_mode` 只映射
/// 「只读」属性位，设 0600 会把文件变成只读，反而让下一次 rename 覆盖失败。
fn atomic_write(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- 管理员会话（JWT HS256） ----

/// 会话有效期（秒）：3 天。
pub const SESSION_TTL: i64 = 3 * 24 * 3600;

/// 管理员会话 Claims（标准 JWT）。
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// 主题：管理员用户名
    sub: String,
    /// 签发时间（Unix 秒）
    iat: usize,
    /// 过期时间（Unix 秒）
    exp: usize,
    /// **密码版本号**（`AdminCreds.updated_at`）：改密码后旧令牌立即失效。
    ///
    /// JWT 是无状态的，不带上这个字段的话，改完密码旧令牌仍能用 3 天 ——
    /// 万一口令已泄露，改密码就形同虚设。
    #[serde(default)]
    pwd: u64,
}

#[derive(Serialize)]
struct JwtHeader {
    alg: &'static str,
    typ: &'static str,
}

fn b64url_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).ok()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// 常量时间比较（HMAC-SHA256 输出固定 32 字节）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 签发管理员会话 JWT（自实现 HS256，密钥为 server.key）。
/// 完全无状态：令牌自包含 exp，服务端不存储任何会话。
/// 不依赖任何 C 工具链，可纯 Rust 交叉编译到任意平台。
pub fn issue_session(key: &str, user: &str, pwd_epoch: u64) -> String {
    let now = now_unix() as usize;
    let claims = Claims {
        sub: user.to_string(),
        iat: now,
        exp: now + SESSION_TTL as usize,
        pwd: pwd_epoch,
    };
    let header_json = serde_json::to_string(&JwtHeader {
        alg: "HS256",
        typ: "JWT",
    })
    .expect("jwt header");
    let payload_json = serde_json::to_string(&claims).expect("jwt payload");
    let header_b64 = b64url_encode(header_json.as_bytes());
    let payload_b64 = b64url_encode(payload_json.as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(key.as_bytes(), signing_input.as_bytes());
    let sig_b64 = b64url_encode(&sig);
    format!("{signing_input}.{sig_b64}")
}

/// 校验管理员会话 JWT（验签名 + 验 exp + **验密码版本号**）。成功返回用户名
/// （sub），失败返回 None。
///
/// `pwd_epoch` 为当前 `AdminCreds.updated_at`；令牌里的版本对不上（说明改过
/// 密码）一律拒绝。
pub fn verify_session(key: &str, token: &str, pwd_epoch: u64) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let expected = hmac_sha256(key.as_bytes(), signing_input.as_bytes());
    let got = b64url_decode(parts[2])?;
    if !ct_eq(&expected, &got) {
        return None;
    }
    let payload = b64url_decode(parts[1])?;
    let claims: Claims = serde_json::from_slice(&payload).ok()?;
    if claims.exp as i64 <= now_unix() as i64 {
        return None;
    }
    if claims.pwd != pwd_epoch {
        return None;
    }
    Some(claims.sub)
}

// ---- 登录退避 ----

/// 登录失败的退避策略参数。
///
/// 管理端只有一个账号，没有失败计数就能无限次尝试口令——配合**每请求一次
/// Argon2 校验**，既是爆破入口也是 CPU 消耗入口。这里做按 IP 退避：连续失败
/// 到阈值即锁定，锁定时长逐次翻倍（有上限），登录成功立刻清零。
const LOGIN_MAX_FAILS: u32 = 5;
const LOGIN_BASE_LOCK: Duration = Duration::from_secs(30);
const LOGIN_MAX_LOCK: Duration = Duration::from_secs(900);

struct LoginThrottle {
    fails: u32,
    locked_until: Option<Instant>,
    lock_len: Duration,
}

/// 登录退避：**按源 IP** 各自计数。任意远程主机发 5 次错误口令只能锁死自己。
/// 表项上限 1024，超出时清理已过期的锁定项（防内存被伪造源打爆）。
static LOGIN_THROTTLES: LazyLock<Mutex<HashMap<IpAddr, LoginThrottle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 单 IP 失败表上限：超过即做一轮过期清理（攻击者伪造海量源时防内存膨胀）
const LOGIN_THROTTLE_MAX_IPS: usize = 1024;

/// 解析 IP，容忍带端口的写法（`1.2.3.4:5678`）与两侧引号。
fn parse_ip_lax(raw: &str) -> Option<IpAddr> {
    let s = raw.trim().trim_matches('"');
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some(ip);
    }
    // 仅有单个冒号时才按 host:port 拆（裸 IPv6 已在上面直接解析成功）
    let (host, _) = s.rsplit_once(':')?;
    host.trim().parse::<IpAddr>().ok()
}

/// 取「登录退避计数用的客户端 IP」。
///
/// 直连地址是**环回**或**受信代理**（`HENG_SYNC_TRUSTED_PROXIES` 环境变量，
/// 逗号分隔 IP）时，说明请求来自已知反向代理，此时所有请求的直连地址都相同
/// ——按它计数会让退避退化成单桶。此时改看代理写入的转发头：
/// - 优先 `X-Real-IP`：代理 `proxy_set_header X-Real-IP $remote_addr` 会
///   **覆盖**客户端传入值，不可伪造；
/// - 退而取 `X-Forwarded-For` 的**最后一段**：`$proxy_add_x_forwarded_for`
///   把真实来源追加在末尾，取第一段会被客户端预置的假值欺骗。
///
/// 直连地址既非环回也不在受信列表（客户端直连，或**未经声明**的代理）时
/// **一律忽略转发头**：那些头在公网可任意伪造，采信等于把退避键交给攻击者。
pub fn client_throttle_key(
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    trusted: &[IpAddr],
) -> IpAddr {
    let direct = peer.map(|p| p.ip());
    if direct.is_some_and(|ip| ip.is_loopback() || trusted.contains(&ip)) {
        if let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_ip_lax)
        {
            return ip;
        }
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.rsplit(',').next())
            .and_then(parse_ip_lax)
        {
            return ip;
        }
    }
    direct.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

/// 该 IP 是否处于锁定期；返回剩余秒数（向上取整）。
pub fn throttle_locked_secs(key: IpAddr) -> Option<u64> {
    let t = LOGIN_THROTTLES.lock().unwrap_or_else(|e| e.into_inner());
    let th = t.get(&key)?;
    let until = th.locked_until?;
    let now = Instant::now();
    if now < until {
        Some((until - now).as_secs() + 1)
    } else {
        None
    }
}

/// 记一次登录失败；到阈值即上锁（时长翻倍，封顶 `LOGIN_MAX_LOCK`）。
pub fn record_login_fail(key: IpAddr) {
    let mut t = LOGIN_THROTTLES.lock().unwrap_or_else(|e| e.into_inner());
    if t.len() >= LOGIN_THROTTLE_MAX_IPS {
        // 防伪造源撑爆内存：清掉已过期的锁定项
        let now = Instant::now();
        t.retain(|_, th| th.locked_until.map(|u| now < u).unwrap_or(true));
    }
    let th = t.entry(key).or_insert_with(|| LoginThrottle {
        fails: 0,
        locked_until: None,
        lock_len: LOGIN_BASE_LOCK,
    });
    th.fails = th.fails.saturating_add(1);
    if th.fails >= LOGIN_MAX_FAILS {
        let now = Instant::now();
        th.locked_until = Some(now + th.lock_len);
        th.lock_len = (th.lock_len * 2).min(LOGIN_MAX_LOCK);
        th.fails = 0;
    }
}

/// 登录成功：清零该 IP 的退避状态。
pub fn record_login_success(key: IpAddr) {
    LOGIN_THROTTLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_hashed_with_argon2id_and_salted() {
        let h = hash_pass("correct horse battery staple");
        assert!(
            h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "必须是 Argon2id 且参数固定，实际: {}",
            &h[..h.find('$').unwrap_or(0).max(9)]
        );
        assert!(verify_pass(&h, "correct horse battery staple"));
        assert!(!verify_pass(&h, "wrong password"));
        // 同一个口令两次哈希必须不同（随机盐）
        assert_ne!(h, hash_pass("correct horse battery staple"));
        // 哈希串里绝不能出现口令原文
        assert!(!h.contains("correct horse"));
    }

    #[test]
    fn verify_reads_algorithm_from_phc_string() {
        // 校验必须跟着哈希串里记录的算法走：这样以后换算法/调参数，老哈希照样能验。
        let legacy = Argon2::new(Algorithm::Argon2d, Version::V0x13, Params::default())
            .hash_password(b"legacy-pw", &SaltString::generate(&mut OsRng))
            .unwrap()
            .to_string();
        assert_eq!(legacy.split('$').nth(1), Some("argon2d"));
        assert!(
            verify_pass(&legacy, "legacy-pw"),
            "非 Argon2id 的历史哈希也要能验过"
        );
        assert!(!verify_pass(&legacy, "nope"));
    }

    #[test]
    fn garbage_hash_never_verifies() {
        assert!(!verify_pass("", "x"));
        assert!(!verify_pass("not-a-hash", "x"));
        assert!(!verify_pass("$argon2id$v=19$m=1$", "x"));
        // 旧版自定义格式（hex salt/hash）不是 PHC，一律拒
        assert!(!verify_pass("argon2id$0011223344556677$aabb", "x"));
    }

    #[test]
    fn validate_hash_input_accepts_only_real_hashes() {
        // 正常串：接受，并返回去掉空白的原文
        let h = hash_pass("some-real-password");
        assert_eq!(validate_hash_input(&h), Ok(h.clone()));
        assert_eq!(
            validate_hash_input(&format!("  {h}\n")),
            Ok(h.clone()),
            "首尾空白要容忍（页面粘贴经常带上换行）"
        );
        // 明文口令：必须挡住（否则「只存哈希」就名存实亡）
        let e = validate_hash_input("my-plain-password").unwrap_err();
        assert!(e.contains("明文"), "错误信息要能提示用户：{e}");
        // 空串 / 乱串
        assert!(validate_hash_input("").is_err());
        assert!(validate_hash_input("   ").is_err());
        assert!(validate_hash_input("$argon2id$v=19$m=1$").is_err());
        assert!(validate_hash_input("not-a-hash").is_err());
    }

    /// 公开的初始化端点不能变成 DoS 入口：外部串声明的 Argon2 参数必须有上限。
    #[test]
    fn oversized_hash_params_are_rejected_before_running_argon2() {
        // m=4 GiB + 100 轮：真跑起来会把服务端拖死 —— 必须在跑之前就拒绝
        let huge = "$argon2id$v=19$m=4194304,t=100,p=8$c2FsdHNhbHRzYWx0c2FsdA$\
                    aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNo";
        let e = validate_hash_input(huge).unwrap_err();
        assert!(
            e.contains("过大") || e.contains("过多"),
            "应报参数超限: {e}"
        );

        // 单看每一维也要拦（只超 p 的那种）
        assert!(check_hash_params_bounded("$argon2id$v=19$m=19456,t=2,p=64$x$y").is_err());
        assert!(check_hash_params_bounded("$argon2id$v=19$m=1048576,t=2,p=1$x$y").is_err());
        assert!(check_hash_params_bounded("$argon2id$v=19$m=19456,t=99,p=1$x$y").is_err());

        // 正常档位（含比默认更安全的档）必须放行
        assert!(check_hash_params_bounded("$argon2id$v=19$m=19456,t=2,p=1$x$y").is_ok());
        assert!(check_hash_params_bounded("$argon2id$v=19$m=65536,t=3,p=1$x$y").is_ok());
        // 解析不出参数段时不拦（交给 verify_password 自己失败）
        assert!(check_hash_params_bounded("garbage").is_ok());

        // 超长串直接拒绝，不进解析
        let long = format!("$argon2id$v=19$m=19456,t=2,p=1$xA$yB{}", "z".repeat(600));
        assert!(validate_hash_input(&long).unwrap_err().contains("过长"));
    }

    #[test]
    fn new_password_rules() {
        assert!(validate_new_password("1234567").is_err()); // 7 位
        assert!(validate_new_password("12345678").is_ok());
        assert!(validate_new_password("heng").is_err());
        assert!(validate_new_password("heng1234").is_ok());
    }

    #[test]
    fn admin_store_roundtrip_and_change_password() {
        let dir = std::env::temp_dir().join(format!(
            "heng-admin-store-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = AdminStore::new(dir.clone());
        // 未初始化：读不到
        assert!(store.load_admin().is_none());
        // 写 + 读
        let creds = AdminCreds {
            user: "admin".into(),
            pass_hash: hash_pass("first-password"),
            updated_at: 100,
        };
        store.save_admin(&creds).unwrap();
        let loaded = store.load_admin().unwrap();
        assert_eq!(loaded.user, "admin");
        assert_eq!(loaded.updated_at, 100);
        // 密钥：首次随机生成，再次读回同一个
        let k1 = store.load_or_create_key().unwrap();
        assert_eq!(k1.len(), 64);
        assert_eq!(store.load_or_create_key().unwrap(), k1);
        // 改密：旧口令不对拒绝；对了才落盘
        assert!(store.change_password("wrong", "second-password").is_err());
        let next = store.change_password("first-password", "second-password").unwrap();
        assert!(next.updated_at > 100, "版本号必须刷新，旧会话才会失效");
        assert!(verify_pass(&next.pass_hash, "second-password"));
        assert!(store.load_admin().unwrap().updated_at > 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_roundtrip_and_invalidation() {
        let key = "test-server-key";
        let tok = issue_session(key, "admin", 1000);
        assert_eq!(verify_session(key, &tok, 1000).as_deref(), Some("admin"));
        // 改密后版本号对不上 → 旧令牌失效
        assert!(verify_session(key, &tok, 1001).is_none());
        // 签名错/格式错/密钥错一律拒
        assert!(verify_session("other-key", &tok, 1000).is_none());
        assert!(verify_session(key, "a.b", 1000).is_none());
        assert!(verify_session(key, "a.b.c", 1000).is_none());
    }

    #[test]
    fn throttle_key_resolution() {
        // 直连（非环回）时忽略转发头：公网可伪造，采信等于把退避键交给攻击者
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", "198.51.100.7".parse().unwrap());
        let key = client_throttle_key(
            Some("203.0.113.9:5001".parse().unwrap()),
            &h,
            &[],
        );
        assert_eq!(key.to_string(), "203.0.113.9");
        // 环回 + X-Real-IP：取转发头
        let key = client_throttle_key(
            Some("127.0.0.1:5002".parse().unwrap()),
            &h,
            &[],
        );
        assert_eq!(key.to_string(), "198.51.100.7");
        // 环回 + XFF：取最后一段（首段可被客户端预置伪造）
        let mut h2 = HeaderMap::new();
        h2.insert("x-forwarded-for", "1.2.3.4, 203.0.113.9".parse().unwrap());
        let key = client_throttle_key(
            Some("[::1]:5003".parse().unwrap()),
            &h2,
            &[],
        );
        assert_eq!(key.to_string(), "203.0.113.9");
        // 环回但没有任何转发头 → 环回本身
        let key = client_throttle_key(
            Some("127.0.0.1:5004".parse().unwrap()),
            &HeaderMap::new(),
            &[],
        );
        assert!(key.is_loopback());
        // 无对端地址（单测 oneshot）→ 全局键
        let key = client_throttle_key(None, &HeaderMap::new(), &[]);
        assert_eq!(key.to_string(), "0.0.0.0");
        // 受信代理：采信转发头
        let trusted: Vec<IpAddr> = vec!["198.51.100.1".parse().unwrap()];
        let key = client_throttle_key(
            Some("198.51.100.1:5005".parse().unwrap()),
            &h,
            &trusted,
        );
        assert_eq!(key.to_string(), "198.51.100.7");
        // 未声明的非环回来源：带转发头也忽略
        let key = client_throttle_key(
            Some("203.0.113.9:5006".parse().unwrap()),
            &h,
            &trusted,
        );
        assert_eq!(key.to_string(), "203.0.113.9");
    }

    #[test]
    fn throttle_locks_after_max_fails_and_clears_on_success() {
        let key: IpAddr = "10.99.99.99".parse().unwrap();
        // 干净起步（并行测试共享静态表，先清一次）
        record_login_success(key);
        for _ in 0..super::LOGIN_MAX_FAILS {
            assert!(throttle_locked_secs(key).is_none(), "未到阈值不上锁");
            record_login_fail(key);
        }
        let left = throttle_locked_secs(key).expect("到阈值必须上锁");
        assert!(left > 0 && left <= 30, "首锁 30s，实际 {left}");
        // 成功后清零
        record_login_success(key);
        assert!(throttle_locked_secs(key).is_none());
    }
}
