# HengIME 架构

> 本文描述系统的当前结构。功能范围与进度见 [FEATURES.md](FEATURES.md)，
> 阶段安排见 [README.md](../README.md)。
> 文中所有数字以代码为准，核对日期 2026-10-11。

---

## 1. 一句话

一份 librime 引擎 + 桌面端的 Slint 自绘候选窗，收在 Rust 的 `heng-core` 里；
六端外壳只负责把宿主按键/触摸事件转成 core 调用——桌面外壳取 core 产出的帧缓冲
上屏，移动外壳取候选快照填入宿主原生 UI 组件。

---

## 2. 进程模型

```
宿主应用（记事本/浏览器…）
    │  键盘消息
    ▼
宿主输入法框架                 每端不同，但只做事件搬运
TSF / IMK / Fcitx5 / IMS / Custom Keyboard Extension / IME Kit
    │  ①创建会话 ②转发按键 ③提交文本 ④上报光标位置
    ▼
外壳（Rust FFI 或 C++ 薄壳）
    │  heng_* C ABI（同进程直接函数调用）
    ▼
heng-core（Rust，编译成 heng_core.dll / .so / .dylib）
    ├─ engine/     librime FFI + 会话封装
    ├─ ui.rs       Slint 自绘候选窗 + 七页设置窗
    ├─ settings.rs 配置读写（写 patch 文件）
    ├─ capi.rs     C ABI（54 个 heng_* 导出，abi_version=10）
    └─ server.rs   本地 HTTP，仅诊断（127.0.0.1:9371）
```

**关键点：热路径上没有 IPC。** Windows 侧的 `WeaselServer.exe` 在自己进程内
`LoadLibrary` 加载 `heng_core.dll` 并直接调用 `heng_*` —— TSF DLL 只做事件搬运，
候选窗和 librime 都在服务进程里。HTTP 端口 9371 只给 `heng-cli` 诊断用，
不在输入链路上。

### 2.1 Windows 的特殊之处

宿主 TSF DLL（`weaselx64.dll`）**注入到每个前台应用进程**，这是 Windows 输入法的
机制要求。它的职责限于：收按键、开/关会话、提交文本、报光标矩形。
librime 与候选窗都在 `WeaselServer.exe` 内，该进程直接调用 `heng_*`，按键不经过
任何管道。（上游小狼毫用命名管道把每次按键转发到服务进程，本项目未沿用该结构。）

---

## 3. core 内部

| 文件 | 职责 |
|---|---|
| `engine/mod.rs` | librime 绑定封装、会话管理、`redeploy()` |
| `engine/rime_ffi.rs` | 手写 FFI 声明（对照 librime 1.17.0 的 `rime_api.h`） |
| `global.rs` | 全局引擎 + 会话表 + 行为统一层（传播策略/app_options/开关记忆） |
| `capi.rs` | C ABI，54 个 `heng_*` 导出，`abi_version = 10` |
| `ui.rs` | 候选窗与设置窗的 Slint 场景、平台后端、事件分发 |
| `settings.rs` | 读配置（config API v4）/ 写配置（直接改 patch 文件） |
| `server.rs` | 本地 HTTP，诊断用 |

**ABI 是手写的**，没有 IDL 代码生成。改 ABI 时必须三处同步：
`core/include/heng.h`、`core/src/capi.rs`、`patches/heng.def`（控制 Windows 导出表）。
导出数变了还要重建 `third_party/src/weasel/lib64/heng.lib`（导入库不跟随 dll 更新）。

### 3.1 UI 后端

候选窗用 Slint **软件渲染**，产出预乘 BGRA 的 `Argb` 帧缓冲，平台后端只负责搬帧：

| 平台 | 后端 | 关键做法 |
|---|---|---|
| Windows | `win_backend` | `WS_POPUP \| NOACTIVATE \| TOOLWINDOW` + `UpdateLayeredWindow(ULW_ALPHA)` |
| Linux | `x11_backend` | X11/XWayland，override-redirect 窗口 |
| macOS | **未实现** | `create_backend()` 返回 `None`，自绘候选窗不可用，其余能力不受影响 |
| Android / iOS / 鸿蒙 | 不适用 | 移动端不使用 Slint 后端：候选窗与键盘 UI 由宿主原生框架渲染 |

三个后端都只做两件事：`blit`（搬帧）和 `poll_events`（非阻塞泵事件）。
**没有后端时引擎与会话仍可正常工作**，只是没有自绘候选窗。

移动三端不接入 Slint 后端。键盘 UI 与候选栏由宿主原生框架绘制（Android View、
UIKit、ArkUI），外壳经 `heng_get_context` 取候选快照后填入；`heng_ui_sync` /
`heng_ui_hide` / `heng_settings_show` 等 UI ABI 在移动端编译时不链接平台后端，
调用静默 no-op——与 macOS 无后端时的行为一致。

窗口属性（置顶、不抢焦点、点击不激活系统）已真机验证：Windows 用原生窗口样式位
（`NOACTIVATE`），连 `WM_MOUSEACTIVATE` 都不会收到；Linux 靠 override-redirect。

### 3.2 行为统一层

「五端行为一致」不靠各端自觉，靠 `global.rs` 里的三件事：

- **传播策略** `propagation.policy`：`per_session` / `per_app` / `global`，
  对齐 fcitx5-rime 的 `SharedStatePolicy` 三档。设一次，五端都听。
- **按应用初始选项** `app_options.<app_id>.<option>`：外壳上报 `app_id`，
  core 按应用套用初始开关（如 `abitest.exe` 默认全角）。
- **跨重启开关记忆**：写入 `<user_data_dir>/heng_options.yaml`。

这三项的配置文件是 `config-center/shared/heng.yaml`，由 `config-center/deploy.sh`
分发，Engine 启动时自动部署进 staging。**外壳不实现任何一项。**

---

## 4. 候选窗视觉

**配色不走 `style/color_scheme`** —— 那个键是 weasel 原生 UI 用的，
自绘窗根本不读它。四项颜色存在 patch 文件里，键名带 `heng_` 前缀
（weasel 不认识会忽略，不与上游样式语义打架）：

```
weasel.custom.yaml
  style/heng_container_back   容器背景
  style/heng_hilite_back      选中项背景
  style/heng_hilite_text      选中项文字
  style/heng_cand_text        候选词文字
```

值格式是 `#rrggbb` 或字面量 `transparent`。不用 librime 的 `0xBBGGRR`，
因为该格式没有 alpha 通道，`0x000000` 与透明无法区分。

四项而非三项：容器背景可配到深色后，未选中文字若仍为硬编码颜色会形成深底深字，
「候选词文字」因此必须可配。

**取色器是 HSV 取色器**（SV 方块 28×18 格 + 色相条 36 段），
四项均支持「不填」= 透明。`patches/rime-weasel.yaml` 里的
`preset_color_schemes`（39 套）不参与自绘窗渲染。

配色改动**不触发 redeploy**（会作废活动会话），写完 patch 直接 `set_*` + 重绘。

---

## 5. 设置窗口

core 内的 Slint 原生窗口，`heng_settings_show(page)` 唤起。七页，页号是外壳契约：

| 页 | 名称 | 内容 |
|---|---|---|
| 0 | 输入方案 | schema 列表、默认方案 |
| 1 | 外观 | 四项配色 + HSV 取色器、字号、inline preedit |
| 2 | 快捷键 | 左右键行为、翻页键、简繁/标点/全半角切换键 |
| 3 | 标点符号 | 半角标点模式拨杆 |
| 4 | 词库 | 用户词典文件列表、打开用户目录、备份用户词典 |
| 5 | 服务 | 重启/退出服务、重新部署、词典管理、三个目录按钮 |
| 6 | 关于 | 版本、ABI、目录信息 |

改 `SET_NAV` 顺序必须同步改 `page_id` 常量，否则外壳调 `heng_settings_show(5)`
会跳错页。

**配置读写不对称**，这点要知道：

- **读**：config API v4（`heng_config_*`），读 staging 编译结果，即生效值。
- **写**：不走 ABI，直接改 patch 源文件（`*.custom.yaml`），
  因为设置 UI 本身就在 core 内，没有跨进程调用需求。
- 例外：配色读 patch 源文件而非 staging —— 样式不参与 librime 部署语义，
  源 patch 即真相。

---

## 6. 目录结构（实际存在的）

```
core/                heng-core：引擎 + 自绘 UI + ABI
  include/heng.h     C ABI 头文件（外壳唯一定价面）
  src/{engine,ui,settings,capi,global,server}.rs
shells/
  linux/fcitx5/      Fcitx5 addon（唯一已建的 shells 子目录）
tools/
  heng-cli/          调试 CLI：version cand commit bench serve abitest
                             uitest pagetest bartest settings
  weasel-dev-install.ps1   Windows dev 版注册/启动/卸载
  squirrel-dev-install.sh  macOS dev 版部署
  icons/
patches/             weasel 改动固化 + 雾凇/rime yaml + heng.def
  weasel-0.17.4-heng-m1.patch
config-center/       配置真源与分发
  shared/heng.yaml   行为统一层的配置
  deploy.sh
third_party/src/     上游源码浅克隆（weasel/squirrel/fcitx5-rime/trime，gitignored）
runtime/             正式版用户数据（shared data + user dir）
runtime-dev/         dev 版用户数据
experiments/         slint-or-probe / slint-probe 实验
docs/                文档
```

**没有** `core/idl/`（ABI 手写）、`settings-web/`、`skins/`。
设置界面从 HTTP+Web 改成了 Slint 原生窗口，Web 方案已废弃。

---

## 7. 各端现状

| 端 | 宿主 | 外壳位置 | 候选窗 | 状态 |
|---|---|---|---|---|
| Windows | TSF | `third_party/src/weasel`（改动在 `patches/`） | core 自绘（Win32 分层窗口） | 可用 |
| Linux | Fcitx5 | `shells/linux/fcitx5`（310 行） | core 自绘（X11） | 可用 |
| macOS | IMK | `third_party/src/squirrel` | core 自绘（NSPanel **未实现**） | core 可编译运行，无候选窗 |
| Android | IMS | 未建 | 原生 View | 未开始 |
| iOS | Custom Keyboard Extension | 未建 | 原生 UIKit | 未开始 |
| 鸿蒙 | IME Kit | 未建 | 原生 ArkUI | 未开始 |

Windows 与 macOS 的外壳是**上游 C++ 改造**，源码在 gitignored 目录里，
改动靠 `patches/*.patch` 固化。**改完必须重生成补丁**，否则改动不入库。
Linux 外壳为自研 fcitx5 addon；Android / iOS / 鸿蒙三端外壳尚未建目录，
计划均自研（Android 参考 Trime 的 IMS 骨架但数据源换成内嵌 heng_core，
iOS 走 Custom Keyboard Extension 直链 C 静态库，鸿蒙走 NAPI）。

---

## 8. 词库与用户资料同步

librime 的 sync 机制只做一件事：把 userdb 导出成文本快照（头部带 `#@/tick`、
`#@/user_id`）写到一个目录，**不含网络代码**。跨设备需要用网盘或U盘同步该目录。

- 未配 `sync_dir` 时，默认落点是 `user_dir\sync\<installation_id>\`，
  即本地镜像，没有传输行为。
- 快照包含 `*.custom.yaml` 与 `installation.yaml`，配置随快照一起备份，换机可整体恢复。

core 的 `heng_sync_user_data()` 导出当前无 UI 入口，是「服务端 + 账号」方案的
数据基础：上传的是这份快照，服务端存文件并合并 tick 回传。设置页的
「用户资料同步」按钮已移除，与该方案范围重叠。

传输层尚未实现，详见 [FEATURES.md](FEATURES.md) §4。

---

## 9. 构建与验证

| 环节 | 命令 |
|---|---|
| 构建 core | `cargo build -p heng-core --release` |
| 单测 | `cargo test -p heng-core --lib`（7 项） |
| ABI 全链路 | `cargo run --release -p heng-cli -- abitest` |
| 视觉验证 | `HENG_UI_DUMP=1 heng-cli settings <page>` 抓帧到 `%TEMP%/*.ppm` |
| Windows 外壳 | `third_party/src/weasel/build-x64.bat`（正式）/ `build-x64-dev.bat`（dev） |
| Windows dev 部署 | `tools/weasel-dev-install.ps1`（注册需管理员） |

## 9.1 GitHub Actions

`.github/workflows/ci.yml` 四个 job，`push` 与 `pull_request` 到 `main` 时触发。

| job | runner | 内容 |
|---|---|---|
| `core` | ubuntu / windows / macos 三平台矩阵 | 编 core、跑单测、过 ABI 全链路 |
| `shell-windows` | windows-2022 | 重放 weasel 补丁、装 boost 1.84、编 weasel 全链 |
| `shell-linux` | ubuntu-24.04 | 编 fcitx5 addon |
| `shell-macos` | macos-15 | 重放 squirrel 补丁、`xcodebuild` 编 Squirrel.app |

**librime 的获取三条路**（`core/build.rs` 按target_os 分支）：Windows 与 macOS
强制要求 `third_party/librime/dist/lib` 下有官方预编译产物，CI 从 librime release
下载对应平台的包；Linux 有本地产物就用，没有则链发行版包 `librime-dev`。

**CI 的运行时配置来自雾凇的 `full.zip`**。`runtime/` 被 gitignore，CI 上不存在，
而 `abitest` 的断言要读配置与词库。雾凇原版的文件（词典、`lua/`、符号表、
双拼 schema）不入库也不该入库——实测 `full.zip` 里 63 个文件与本机逐文件
md5 相同，且词典跟随上游更新是期望行为；入库的是本项目改过的四个文件
（见 `third_party/README.md` 的「配置来源的分工」）。

`default.yaml` 的 `schema_list` 在 CI 上裁剪到只剩 `rime_ice`：其余 8 个方案
（t9 + 7 个双拼）本机在用但未单独入库，librime 遇到缺失的 schema 会让整个
部署以 `failure` 收场。裁剪由 `tools/ci/make_ci_default.py` 完成。

**Windows job 里导入库必须重建**：`lib64/heng.lib` 是 core 的导入库，core 每新增
导出后它不会自动更新，链接期报 `LNK2001`。CI 在编完 core 之后、msbuild 之前从
dll 的 PE 导出表生成 `.def` 再 `lib /def`，这一步不能用仓库内的工具脚本
（`.workbuddy/` 是 gitignored 的）。

**`weasel.props` 由 CI 生成**：该文件被上游 `.gitignore` 忽略，不在补丁里，
入库的模板中 `BOOST_ROOT` 是绝对路径，CI 按 runner 上的实际路径重写。

产物以 artifact 形式上传（`heng-windows` / `heng-linux` / `heng-macos`），
不做发布——项目自用不上架。

**两套工具链不自动同步。** cargo 与 msbuild 互不知晓对方的存在，
`heng_core.dll` 由 cargo 产出、由 msbuild 链接进 exe，两者之间靠
`build-core.bat` 显式拷贝。该脚本由两个 `build-x64*.bat` 在 msbuild 之前调用，
依次完成 `cargo build --release`、结束占用 `heng_core.dll` 的 server 进程、
把产物复制到 `output/` 或 `output-dev/`。

跳过这一步的后果是 exe 链接当前 ABI、运行时加载旧版 core：服务能启动、
管道能建立、`heng-cli` 能连上引擎，但 TSF 输入到不了编辑器。静态检查全过，
不一致只在真机打字时暴露。

**运行时外部依赖由 `sync-deps.bat` 补齐**，由两个 `build-x64*.bat` 在
msbuild 之后调用。`WinSparkle.dll`、`rime.dll`、共享数据目录 `data/` 都不是
msbuild 产物，清空 `output/` 后必须恢复，否则服务在加载阶段退出且早于第一条日志。
它们的来源集中放在 `third_party/externals/`，缺什么补什么、已存在则跳过。
`data/` 只在缺失时整目录拷贝，不做镜像同步——引擎运行时会写入 `data/build/`，
镜像会在每次构建时抹掉它并触发重新部署。

**`weaselx64.dll` 编译前须让位。** 它是 TSF 进程内服务器，被每个激活过输入法的
宿主（explorer、记事本、各编辑器）加载，同时作为 COM InprocServer32 注册。
link.exe 写输出前先 unlink，而 `DeleteFile` 对已映射为映像的文件失败，
报 `LNK1104`。改文件名则可行——宿主继续用重命名后文件的映射，链接器发现
路径已空。`unlock-tsf.bat` 做这件事，不需要管理员权限，也不需要
`regsvr32 /u`：COM 注册存的是路径而非文件身份，链接产物写回同名即生效。

让位的文件带随机后缀（`weaselx64.dll.parked<rand>`），每次运行顺带清扫遗留：
已释放映射的删得掉，仍被占用的删不掉并留到下次——这正是需要的行为。

四个脚本按此顺序串在两个 `build-x64*.bat` 里：

```
build-core.bat   (msbuild 前)  cargo build + 停 server + 部署 heng_core.dll
unlock-tsf.bat   (msbuild 前)  让出 weaselx64.dll 路径
msbuild                      编译全部项目
sync-deps.bat   (msbuild 后)  补 WinSparkle.dll / rime.dll / data\
```

**并行开发隔离**（`HENG_DEV` 编译期宏）：dev 版与正式版共用一个进程空间会互相抢占，
锚点有五处——TSF GUID 组、IPC 管道名、显示名、数据路径、单实例互斥锁。
**漏掉互斥锁会导致第二个 server 静默退出（code=-1）**，编译产物分别落在
`output/` 与 `output-dev/`。

改 ABI 的完整流程见 `.workbuddy/tools/rebuild_heng_lib.py` 的文档字符串。
weasel 侧改动的固化与验证流程见 [patches/README.md](../patches/README.md)。

---

## 10. 已知局限

1. **UI 线程死亡不重建** —— `ensure_started()` 只在命令通道为空时 spawn，
   线程崩溃后通道仍存在，候选窗永久失效。需要手动重启。
2. **UI 线程 8ms 常驻轮询** —— 没有走事件驱动，是已知的 CPU 开销来源。
3. **X11 全量 blit** —— 没做 DirtyRegion 局部刷新。
4. **多屏异构缩放** —— 取主屏 DPI，副屏窗口尺寸会偏差。
5. **窗口不可缩放** —— 设置窗固定 640×480（乘 DPI 缩放），无最小/最大尺寸约束。
6. **`installation_id` 未由 core 管理** —— 现在靠 librime 自己在
   `installation.yaml` 里生成。服务端方案需要处理多机撞车。
7. **macOS 无候选窗** —— 见 §7。