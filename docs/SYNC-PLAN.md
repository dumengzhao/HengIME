# 用户词库同步

> 「服务端 + 账号」方案，同步用户词库快照。
> 在整体架构中的位置见 [ARCHITECTURE.md](ARCHITECTURE.md) §8。

---

## 1. 同步范围

同步单元是用户词库快照 `*.userdb.txt`（每个 schema 一个文件）。

| 内容 | 是否同步 | 原因 |
|---|---|---|
| `*.userdb.txt` | 是 | 用户词频与自造词，跨端价值最高 |
| `*.custom.yaml` | 否 | 含平台特有项（候选窗位置、托盘行为、快捷键），跨端同步会互相覆盖 |
| `installation.yaml` | 否 | 含 `installation_id` 与 `sync_dir`，跨端覆盖会使设备身份错乱 |
| `dicts/`、`lua/` | 否 | 不属 librime sync 范围，由各端部署流程铺 |

rimesync 会随快照单向备份配置目录**根目录**的 YAML/TXT，因此实现上需要显式排除，
只挑 `*.userdb.txt` 上传与下载。

配置不参与同步的原因与设备差异直接相关：同一个配置项在不同端的语义不同
（`weasel.custom.yaml` 只对 Windows 有意义，`squirrel.custom.yaml` 只对 macOS 有意义），
同步后各端会拿到不属于自己的设置。

---

## 2. 服务端不做合并

快照是文本，头部为元信息，其后每行一条词条：

```
# Rime user dictionary
#@/db_name rime_ice
#@/db_type userdb
#@/rime_version 1.13.1
#@/tick 793
#@/user_id desktop-win
你好 ni3hao3 c=5 d=2.5 t=1640000000
```

- `#@/tick` 是快照级计数器，每次 sync 递增，用于**同设备**去重。
- 词条行末的 `t=` 是最后更新的 Unix 时间戳。

**合并由各端 librime 在部署时完成**：同一「词条 + 编码」按 `t=` 取新；
同一设备的同词条只取最新 `tick`。

因此服务端为每台设备保留独立槽位，**不解析、不合并快照**。若服务端按 tick
只保留一份最新快照，其他设备的独有词条会在下次覆盖时丢失。

---

## 3. 账号与设备

| 项 | 设计 |
|---|---|
| 账号来源 | 管理页面新增，无自助注册接口 |
| 客户端认证 | 用户名 + 密码（argon2id，PHC 串），登录返回长期 token，无 refresh |
| 管理端认证 | 独立体系：凭据存 `admin.json`（只有 Argon2id 哈希），会话为 JWT（HS256），详见 §5.2 |
| 设备标识 | `installation_id`，同账号内唯一 |
| `installation_id` 生成 | 由 core 接管，格式 `<platform>-<hostname>-<6 位大写十六进制>` |

口令一律 Argon2id，存储为 PHC 标准串
`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`：算法与参数记录在串里，
以后调参数，历史哈希仍按串内声明验过。客户端账号口令存
`account.password_hash`；管理端口令与账号表分离，明文不经过服务端——
首次初始化时管理员粘贴本地 `heng-sync-server hash` 生成的哈希串，落盘到
`<data_dir>/admin.json`（0600）。

`installation_id` 的 hostname 段截断至 24 字符（总长 ≤64），非法字符替换为
`-`；`InstallationUpdate` 只在该字段缺失时写入，故 core 先落一个可读 id 即被
librime 采用。已是 core 格式时原样保留（不反复重写）；librime 遗留的 UUID
或缺格式时生成新 id 写回 `installation.yaml`（保留文件其余键）。id 在同一
用户目录内跨重启保持稳定；目录重建（重装/迁移）会生成新 id，服务端视为新设备。

core 接管 `installation_id` 的原因：现实现在由 librime 自行生成 UUID（见
[ARCHITECTURE.md](ARCHITECTURE.md) §10 第 6 条），既不可读也无法保证跨设备唯一，
而服务端以它作为槽位键。改为可读格式后，管理页面的设备列表与本地同步目录
都能直接看出是哪台机器。

---

## 4. 数据模型

```sql
CREATE TABLE account (
  id            INTEGER PRIMARY KEY,
  username      TEXT    NOT NULL UNIQUE,
  password_hash TEXT    NOT NULL,
  created_at    INTEGER NOT NULL
);

CREATE TABLE device (
  id              INTEGER PRIMARY KEY,
  account_id      INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  installation_id TEXT    NOT NULL,
  display_name    TEXT    NOT NULL,
  platform        TEXT    NOT NULL,
  last_seen_at    INTEGER,
  last_sync_at    INTEGER,
  UNIQUE (account_id, installation_id)
);

CREATE TABLE snapshot (
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
```

快照本体落磁盘（`data/<account_id>/<installation_id>/<file_name>`），
表中只存元数据。`sha256` 用于客户端判断是否需要传输，`tick` 用于管理页面显示。

`account` 表只服务**客户端**登录（输入法六端）；管理端凭据不落库，
见 §5.2 的 `admin.json`。

不存同步历史流水，管理页面只展示各设备的最后一次同步状态。

---

## 5. 接口

### 5.1 客户端

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/auth/login` | 登录，返回 token |
| GET | `/api/devices` | 本账号设备列表 |
| POST | `/api/devices` | 注册或更新本机设备 |
| DELETE | `/api/devices/{installation_id}` | 解绑设备 |
| GET | `/api/sync/manifest` | 全部设备的文件清单（名称 + sha256 + tick） |
| GET | `/api/sync/file/{installation_id}/{name}` | 下载单个快照 |
| PUT | `/api/sync/file/{installation_id}/{name}` | 上传，整份覆盖本机槽位 |

鉴权统一为请求头 `Authorization: Bearer <token>`。
不存在 `/api/auth/register`，账号只能由管理页面创建。

上传与下载只接受文件名匹配 `*.userdb.txt` 的请求。

`POST /api/devices` 请求体：

```json
{ "installation_id": "desktop-win-D4F1A2", "display_name": "台式机", "platform": "windows" }
```

`GET /api/sync/manifest` 响应体：

```json
{
  "devices": [
    {
      "installation_id": "macbook-pro-9B3C11",
      "display_name": "MacBook Pro",
      "platform": "macos",
      "last_sync_at": 1760450400,
      "files": [
        { "name": "rime_ice.userdb.txt", "size": 98304, "sha256": "…", "tick": 812 }
      ]
    }
  ]
}
```

### 5.2 管理页面

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/admin` | 管理页面（单文件 HTML） |
| GET | `/api/admin/init-status` | 管理员凭据是否已初始化（公开） |
| POST | `/api/admin/init` | 初始化：用户名 + 口令哈希串（公开、仅一次、不签发会话） |
| POST | `/api/admin/login` | 管理员登录，返回 JWT |
| POST | `/api/admin/password` | 修改管理员密码（须验旧口令，旧会话全部失效） |
| GET / POST | `/api/admin/users` | 用户列表 / 新增用户 |
| POST | `/api/admin/users/{name}/passwd` | 改密码 |
| DELETE | `/api/admin/users/{name}` | 删除用户（级联其设备与快照） |
| GET | `/api/admin/devices` | 全部设备，按账号分组 |
| DELETE | `/api/admin/devices/{installation_id}` | 强制解绑 |

管理数据面（users / devices / password）由 JWT 中间件统一鉴权：未初始化
返回 409（`code: not_initialized`），令牌缺失或无效返回 401。认证面端点中
init-status / init / login 公开，password 在守卫组内。

口令模式与 ClipSync 服务端一致：明文口令不经过服务端，`admin.json` 只存
Argon2id 哈希（0600），会话签名密钥在 `server.key`；JWT claims 带密码版本号，
改密后旧令牌全部失效。登录按源 IP 退避：5 次失败锁 30s、逐次倍增封顶
900s，锁定期内不做口令校验。重置 = 删 `admin.json` 重启。

未初始化时 `/admin` 显示初始化卡片（用户名 + 哈希串粘贴 + 生成说明），
已初始化未登录显示登录卡片；管理端只有一个账号，与客户端账号体系
（§5.1）完全分离。

---

## 6. 客户端同步流程

一轮同步按以下顺序执行（`core/src/sync.rs` 的 `run_sync`）：

0. **会话门闩** —— 先查活动会话计数，为 0 再持 `SESSION_GATE` 并复核；
   存在活动会话则整轮推迟（见下节）。`heng_start_session` 全程持同一把锁，
   会话创建最多等待一轮同步（秒级）。
1. **注册设备** —— `POST /api/devices` 登记本机 installation_id、主机名、平台。
2. **拉取** —— 取 manifest，比对 sha256，把其他设备槽位中有变化的
   `*.userdb.txt` 下载到本地 `sync/<other_id>/`。拉取必须先于导出：
   librime 的合并只处理合并前已存在于 `sync/` 下的文件。
3. **导出与合并** —— 调 `heng_sync_user_data()` 并 join 维护线程
   （`sync_user_data_blocking`）。librime 内部依次执行
   `CleanupAllSessions` → 导出本机快照到 `sync/<installation_id>/` →
   合并 `sync/` 下全部槽位 → deploy 重建索引。合并由 librime 完成，
   core 不自行读写 userdb。
4. **上传** —— 把导出后的本机快照逐个 PUT，整份覆盖本机槽位。
5. **收尾** —— 删除待合并标记，状态置 `ok`。

网络 IO 全部走同步模块自带的 timer 线程，调用方（设置页、CLI）只发起
唤醒，不被阻塞。

### 待合并标记

合并依赖 deploy 重建索引，且 librime sync 会先销毁全部会话。拉取到变更
但存在活动会话时，整轮同步推迟，并写下标记
`<user_dir>/.heng/pending-merge`：

- 无活动会话时正常执行，完成后删除标记
- 有活动会话时写标记退出；标记由引擎启动（`sync::init`）与会话计数归零
  （`heng_end_session` 末尾调 `notify_sessions_idle`）两条路径检查后补跑

manifest 的 sha256 与本地一致时不写标记、不传输。

---

## 7. 触发机制

| 路径 | 机制 |
|---|---|
| 定时器 | 进程内低频线程，默认间隔 30 分钟；账号配置完成（登录或开开关）时拉起，未配置/已停用不起线程 |
| 立即同步 | 设置页「立即同步」与 `heng-cli sync now`：唤醒定时器马上跑一轮，不等间隔 |
| 补跑 | 待合并标记存在时，引擎启动与会话计数归零两条路径各检查一次 |
| 重试 | 被跳过（上一轮未结束）与被推迟（活动会话）60 秒后再试；失败按指数退避，封顶 6 小时 |

定时器与同步执行都在同步模块自己的线程上：网络 IO 与 librime deploy 不占
调用方线程，设置页与 CLI 只做唤醒。六端同一套机制，无平台分支。

三条开销约束：

- 未到间隔时不发起任何请求，只比较时间戳
- 传输前先比对 manifest 的 sha256，无变化不上传也不下载
- 失败静默处理，按指数退避重试，不弹窗、不阻塞输入

---

## 8. 管理页面

单文件 HTML，内嵌进服务端二进制（`include_str!`），由 axum 直接返回，
不引入构建链与外部依赖。

页面分三个标签：

| 标签 | 内容 |
|---|---|
| 设备 | 已绑定设备数、快照总大小、最近同步时间；设备列表含平台、最后同步、大小、解绑 |
| 用户 | 用户列表、新增用户、改密码、删除 |
| 同步记录 | 各设备的最后一次同步状态，不存历史流水 |

另有「改密码」标签：修改管理员自己的口令（验旧口令，改完会话全失效）。

初始化流程：服务端启动时 `admin.json` 不存在 → 管理页显示初始化卡片 →
管理员在服务器上跑 `heng-sync-server hash`（交互读口令、不回显，stdout 只出
哈希串）得到 Argon2id PHC 串 → 粘贴用户名与哈希串完成 init → 用真实口令
登录一次验证。忘记口令 = 删 `admin.json` 重启后重新初始化。

---

## 9. 部署

Rust + axum + SQLite，单二进制，与 core、ClipSync 服务端技术栈一致。
部署在自有服务器，建议启用 HTTPS。

二进制为 Windows 无控制台子系统（`#![windows_subsystem]`）：被计划任务或
资源管理器拉起时不弹控制台窗口。从终端启动时继承父进程控制台，日志与
`heng-sync-server hash` 的交互都走那个终端；窗口化拉起时没有 stdout，
所以哈希串要在终端里先生成好。

---

## 10. 实现状态

| 项 | 状态 | 落点 |
|---|---|---|
| core：同步模块 | 完成 | `core/src/sync.rs`：timer 线程、会话门闩、指数退避；拉取先行、librime 导出合并、上传收尾（§6） |
| core：`installation_id` 接管 | 完成 | `ensure_installation_id`（§3），引擎初始化时执行 |
| core：统一同步目录 | 完成 | `sync_root()` = `<user_dir>/sync`，六端同一相对路径，librime sync 目录即此 |
| ABI v11 | 完成 | `heng_sync_configure` / `heng_sync_set_enabled` / `heng_sync_now` / `heng_sync_config` / `heng_sync_status` / `heng_installation_id`；`heng.h` 与 `patches/windows/heng.def` 同步 |
| CLI | 完成 | `heng-cli sync status\|login\|now\|on\|off`；密码经第三参数或 stdin |
| 设置页「同步」 | 完成 | 设置窗第 6 页（页号 0-7）：状态卡、开关、立即同步、退出登录；账号录入走 CLI（设置窗无键盘输入通路），状态由主循环 500ms 轮询刷新 |
| 服务端 | 完成 | `server/`：axum + rusqlite(bundled) + argon2id(PHC) + 自实现 JWT(HS256)，lib/bin 双目标；管理端口令模式同 ClipSync（admin.json + init + 退避 + 改密废旧） |
| 测试 | 完成 | core sync 11 项 + ui 7 项单测；服务端 17 项单测 + 4 项集成测试；冒烟覆盖 id 接管、登录落盘、上传、第二设备收发、词条级跨设备合并 |

`heng_sync_user_data` 已存在（见 [ARCHITECTURE.md](ARCHITECTURE.md) §8），
作为本方案的导出环节保留；新增导出统一走 `sync_user_data_blocking`
（sync + join 维护线程），返回时快照已落盘。

`installation_id` 接管、统一同步目录、同步编排三项落在 core，六端外壳不
各自实现；桌面三端经 `heng.h` 的 ABI v11 调用，移动三端经同一 C ABI 接入。
