//! SQLite 数据层（docs/SYNC-PLAN.md §4）。
//!
//! 表结构三张：`account` / `device` / `snapshot`，另加 `token` 存放长期登录
//! 令牌。快照本体不落库，只存元数据；文件本体在磁盘
//! `<data_dir>/<account_id>/<installation_id>/<file_name>`。
//!
//! `account` 只服务**客户端**登录（输入法六端）；管理端凭据不走库，见
//! `admin.json`（`auth::AdminStore`）。
//!
//! `Connection` 非 `Sync`，整个服务用一把 `Mutex` 串行化写路径。同步是低频
//! 操作（默认 30 分钟一轮），锁不构成瓶颈。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{named_params, Connection, OptionalExtension};

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---- 行模型 ----

#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub id: i64,
    pub account_id: i64,
    pub installation_id: String,
    pub display_name: String,
    pub platform: String,
    pub last_seen_at: Option<i64>,
    pub last_sync_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub file_name: String,
    pub size: i64,
    pub sha256: String,
    pub tick: Option<i64>,
    pub updated_at: i64,
}

// ---- 建库 ----

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS account (
  id            INTEGER PRIMARY KEY,
  username      TEXT    NOT NULL UNIQUE,
  password_hash TEXT    NOT NULL,
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS device (
  id              INTEGER PRIMARY KEY,
  account_id      INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  installation_id TEXT    NOT NULL,
  display_name    TEXT    NOT NULL,
  platform        TEXT    NOT NULL,
  last_seen_at    INTEGER,
  last_sync_at    INTEGER,
  UNIQUE (account_id, installation_id)
);
CREATE TABLE IF NOT EXISTS snapshot (
  id              INTEGER PRIMARY KEY,
  account_id      INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  installation_id TEXT    NOT NULL,
  file_name       TEXT    NOT NULL,
  size            INTEGER NOT NULL,
  sha256          TEXT    NOT NULL,
  tick            INTEGER,
  updated_at      INTEGER NOT NULL,
  UNIQUE (account_id, installation_id, file_name)
);
CREATE TABLE IF NOT EXISTS token (
  token      TEXT    PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
);
"#;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

// ---- 账号 ----

pub fn create_account(
    conn: &Connection,
    username: &str,
    password_hash: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO account (username, password_hash, created_at)
         VALUES (:username, :hash, :now)",
        named_params! {
            ":username": username,
            ":hash": password_hash,
            ":now": now_unix(),
        },
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn find_account_by_name(conn: &Connection, username: &str) -> rusqlite::Result<Option<Account>> {
    conn.query_row(
        "SELECT id, username, password_hash, created_at FROM account WHERE username = :u",
        named_params! { ":u": username },
        |r| {
            Ok(Account {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )
    .optional()
}

pub fn find_account_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Account>> {
    conn.query_row(
        "SELECT id, username, password_hash, created_at FROM account WHERE id = :i",
        named_params! { ":i": id },
        |r| {
            Ok(Account {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )
    .optional()
}

pub fn list_accounts(conn: &Connection) -> rusqlite::Result<Vec<Account>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, password_hash, created_at FROM account ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Account {
            id: r.get(0)?,
            username: r.get(1)?,
            password_hash: r.get(2)?,
            created_at: r.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn update_password(
    conn: &Connection,
    account_id: i64,
    password_hash: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE account SET password_hash = :h WHERE id = :i",
        named_params! { ":h": password_hash, ":i": account_id },
    )?;
    Ok(())
}

/// 删除账号及其全部从属数据（token/device/snapshot 行）。文件由调用方删。
pub fn delete_account(conn: &Connection, account_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM token WHERE account_id = :i", named_params! { ":i": account_id })?;
    conn.execute("DELETE FROM snapshot WHERE account_id = :i", named_params! { ":i": account_id })?;
    conn.execute("DELETE FROM device WHERE account_id = :i", named_params! { ":i": account_id })?;
    conn.execute("DELETE FROM account WHERE id = :i", named_params! { ":i": account_id })?;
    Ok(())
}

// ---- 令牌 ----

pub fn issue_token(conn: &Connection, account_id: i64, token: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO token (token, account_id, created_at) VALUES (:t, :a, :now)",
        named_params! { ":t": token, ":a": account_id, ":now": now_unix() },
    )?;
    Ok(())
}

pub fn account_for_token(conn: &Connection, token: &str) -> rusqlite::Result<Option<Account>> {
    conn.query_row(
        "SELECT a.id, a.username, a.password_hash, a.created_at
         FROM token t JOIN account a ON a.id = t.account_id WHERE t.token = :t",
        named_params! { ":t": token },
        |r| {
            Ok(Account {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )
    .optional()
}

pub fn delete_tokens_of(conn: &Connection, account_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM token WHERE account_id = :i", named_params! { ":i": account_id })?;
    Ok(())
}

// ---- 设备 ----

/// 注册或更新本机设备（upsert），返回设备 id。同时刷新 last_seen_at。
pub fn upsert_device(
    conn: &Connection,
    account_id: i64,
    installation_id: &str,
    display_name: &str,
    platform: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO device (account_id, installation_id, display_name, platform, last_seen_at)
         VALUES (:a, :i, :d, :p, :now)
         ON CONFLICT (account_id, installation_id) DO UPDATE SET
           display_name = excluded.display_name,
           platform = excluded.platform,
           last_seen_at = excluded.last_seen_at",
        named_params! {
            ":a": account_id,
            ":i": installation_id,
            ":d": display_name,
            ":p": platform,
            ":now": now_unix(),
        },
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM device WHERE account_id = :a AND installation_id = :i",
        named_params! { ":a": account_id, ":i": installation_id },
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn list_devices(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<Device>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, installation_id, display_name, platform, last_seen_at, last_sync_at
         FROM device WHERE account_id = :a ORDER BY id",
    )?;
    let rows = stmt.query_map(named_params! { ":a": account_id }, |r| {
        Ok(Device {
            id: r.get(0)?,
            account_id: r.get(1)?,
            installation_id: r.get(2)?,
            display_name: r.get(3)?,
            platform: r.get(4)?,
            last_seen_at: r.get(5)?,
            last_sync_at: r.get(6)?,
        })
    })?;
    rows.collect()
}

pub fn list_all_devices(conn: &Connection) -> rusqlite::Result<Vec<Device>> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, installation_id, display_name, platform, last_seen_at, last_sync_at
         FROM device ORDER BY account_id, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Device {
            id: r.get(0)?,
            account_id: r.get(1)?,
            installation_id: r.get(2)?,
            display_name: r.get(3)?,
            platform: r.get(4)?,
            last_seen_at: r.get(5)?,
            last_sync_at: r.get(6)?,
        })
    })?;
    rows.collect()
}

pub fn find_device(
    conn: &Connection,
    account_id: i64,
    installation_id: &str,
) -> rusqlite::Result<Option<Device>> {
    conn.query_row(
        "SELECT id, account_id, installation_id, display_name, platform, last_seen_at, last_sync_at
         FROM device WHERE account_id = :a AND installation_id = :i",
        named_params! { ":a": account_id, ":i": installation_id },
        |r| {
            Ok(Device {
                id: r.get(0)?,
                account_id: r.get(1)?,
                installation_id: r.get(2)?,
                display_name: r.get(3)?,
                platform: r.get(4)?,
                last_seen_at: r.get(5)?,
                last_sync_at: r.get(6)?,
            })
        },
    )
    .optional()
}

pub fn find_any_device(conn: &Connection, installation_id: &str) -> rusqlite::Result<Option<Device>> {
    conn.query_row(
        "SELECT id, account_id, installation_id, display_name, platform, last_seen_at, last_sync_at
         FROM device WHERE installation_id = :i",
        named_params! { ":i": installation_id },
        |r| {
            Ok(Device {
                id: r.get(0)?,
                account_id: r.get(1)?,
                installation_id: r.get(2)?,
                display_name: r.get(3)?,
                platform: r.get(4)?,
                last_seen_at: r.get(5)?,
                last_sync_at: r.get(6)?,
            })
        },
    )
    .optional()
}

pub fn touch_device_seen(conn: &Connection, device_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE device SET last_seen_at = :now WHERE id = :i",
        named_params! { ":now": now_unix(), ":i": device_id },
    )?;
    Ok(())
}

/// 解绑设备：删行。调用方负责删磁盘文件与快照行。
pub fn delete_device(conn: &Connection, device_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM device WHERE id = :i", named_params! { ":i": device_id })?;
    Ok(())
}

// ---- 快照 ----

/// 写入/覆盖一个快照的元数据（整份覆盖语义），并刷新设备 last_sync_at。
pub fn upsert_snapshot(
    conn: &Connection,
    account_id: i64,
    installation_id: &str,
    file_name: &str,
    size: i64,
    sha256: &str,
    tick: Option<i64>,
) -> rusqlite::Result<()> {
    let now = now_unix();
    conn.execute(
        "INSERT INTO snapshot (account_id, installation_id, file_name, size, sha256, tick, updated_at)
         VALUES (:a, :i, :f, :s, :h, :t, :now)
         ON CONFLICT (account_id, installation_id, file_name) DO UPDATE SET
           size = excluded.size, sha256 = excluded.sha256, tick = excluded.tick,
           updated_at = excluded.updated_at",
        named_params! {
            ":a": account_id,
            ":i": installation_id,
            ":f": file_name,
            ":s": size,
            ":h": sha256,
            ":t": tick,
            ":now": now,
        },
    )?;
    conn.execute(
        "UPDATE device SET last_sync_at = :now, last_seen_at = :now
         WHERE account_id = :a AND installation_id = :i",
        named_params! { ":now": now, ":a": account_id, ":i": installation_id },
    )?;
    Ok(())
}

pub fn list_snapshots(
    conn: &Connection,
    account_id: i64,
    installation_id: &str,
) -> rusqlite::Result<Vec<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT file_name, size, sha256, tick, updated_at FROM snapshot
         WHERE account_id = :a AND installation_id = :i ORDER BY file_name",
    )?;
    let rows = stmt.query_map(
        named_params! { ":a": account_id, ":i": installation_id },
        |r| {
            Ok(Snapshot {
                file_name: r.get(0)?,
                size: r.get(1)?,
                sha256: r.get(2)?,
                tick: r.get(3)?,
                updated_at: r.get(4)?,
            })
        },
    )?;
    rows.collect()
}

pub fn delete_snapshots_of(
    conn: &Connection,
    account_id: i64,
    installation_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM snapshot WHERE account_id = :a AND installation_id = :i",
        named_params! { ":a": account_id, ":i": installation_id },
    )?;
    Ok(())
}

/// 某账号全部设备的快照聚合（管理页「同步记录」标签）。
pub struct DeviceStats {
    pub installation_id: String,
    pub file_count: i64,
    pub total_size: i64,
    pub last_sync_at: Option<i64>,
}

pub fn snapshot_stats(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<DeviceStats>> {
    let mut stmt = conn.prepare(
        "SELECT installation_id, COUNT(*), COALESCE(SUM(size), 0), MAX(updated_at)
         FROM snapshot WHERE account_id = :a GROUP BY installation_id ORDER BY installation_id",
    )?;
    let rows = stmt.query_map(named_params! { ":a": account_id }, |r| {
        Ok(DeviceStats {
            installation_id: r.get(0)?,
            file_count: r.get(1)?,
            total_size: r.get(2)?,
            last_sync_at: r.get(3)?,
        })
    })?;
    rows.collect()
}

// ---- 磁盘文件路径 ----

/// 快照本体落点：`<data_dir>/<account_id>/<installation_id>/<file_name>`。
/// 调用方必须已校验 installation_id 与 file_name 的字符集（防目录穿越）。
pub fn snapshot_path(data_dir: &Path, account_id: i64, installation_id: &str, file_name: &str) -> PathBuf {
    data_dir
        .join(account_id.to_string())
        .join(installation_id)
        .join(file_name)
}

pub fn device_dir(data_dir: &Path, account_id: i64, installation_id: &str) -> PathBuf {
    data_dir.join(account_id.to_string()).join(installation_id)
}

pub fn account_dir(data_dir: &Path, account_id: i64) -> PathBuf {
    data_dir.join(account_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_conn() -> Connection {
        // 秒级时间戳 + 进程内自增：并行测试之间不得共享同一个库文件
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        open(&std::env::temp_dir().join(format!(
            "heng-sync-db-{}-{}-{n}.db",
            std::process::id(),
            now_unix()
        )))
        .unwrap()
    }

    #[test]
    fn schema_and_account_crud() {
        let conn = tmp_conn();
        let id = create_account(&conn, "alice", "hash").unwrap();
        assert!(find_account_by_name(&conn, "alice").unwrap().is_some());
        assert!(find_account_by_name(&conn, "bob").unwrap().is_none());
        update_password(&conn, id, "hash2").unwrap();
        assert_eq!(
            find_account_by_id(&conn, id).unwrap().unwrap().password_hash,
            "hash2"
        );
        // 同名唯一约束
        assert!(create_account(&conn, "alice", "x").is_err());
        delete_account(&conn, id).unwrap();
        assert!(find_account_by_name(&conn, "alice").unwrap().is_none());
    }

    #[test]
    fn device_upsert_and_cascade() {
        let conn = tmp_conn();
        let acc = create_account(&conn, "alice", "h").unwrap();
        upsert_device(&conn, acc, "win-a-1A2B3C", "台式机", "windows").unwrap();
        upsert_device(&conn, acc, "win-a-1A2B3C", "台式机2", "windows").unwrap();
        let devs = list_devices(&conn, acc).unwrap();
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].display_name, "台式机2");
        // 快照 upsert（覆盖）
        upsert_snapshot(&conn, acc, "win-a-1A2B3C", "rime_ice.userdb.txt", 100, "aa", Some(5)).unwrap();
        upsert_snapshot(&conn, acc, "win-a-1A2B3C", "rime_ice.userdb.txt", 200, "bb", Some(6)).unwrap();
        let snaps = list_snapshots(&conn, acc, "win-a-1A2B3C").unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].size, 200);
        assert_eq!(snaps[0].sha256, "bb");
        // last_sync_at 已被刷新
        assert!(list_devices(&conn, acc).unwrap()[0].last_sync_at.is_some());
        // 删账号级联设备与快照
        delete_account(&conn, acc).unwrap();
        assert!(list_devices(&conn, acc).unwrap().is_empty());
        assert!(list_snapshots(&conn, acc, "win-a-1A2B3C").unwrap().is_empty());
    }

    #[test]
    fn token_roundtrip() {
        let conn = tmp_conn();
        let acc = create_account(&conn, "alice", "h").unwrap();
        issue_token(&conn, acc, "tok123").unwrap();
        let a = account_for_token(&conn, "tok123").unwrap().unwrap();
        assert_eq!(a.username, "alice");
        assert!(account_for_token(&conn, "nope").unwrap().is_none());
        delete_tokens_of(&conn, acc).unwrap();
        assert!(account_for_token(&conn, "tok123").unwrap().is_none());
    }
}
