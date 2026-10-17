# HengIME

> 衡 —— 跨六端的自用输入法
>
> Windows · macOS · Linux · Android · iOS · 鸿蒙

一份引擎，六套外壳，上层统一。

---

## 项目定位

自用输入法，不上架应用商店。桌面三端用免费证书发布；iOS 的键盘扩展需 Apple Developer 账号（$99/年）。目标是六个平台上**行为、设置、数据一致**，桌面三端候选窗逐像素统一，移动三端 UI 用宿主原生框架、观感对齐。

## 总体架构

所有跨端一致的能力收敛到 core（一份代码），外壳只保留与宿主系统交互所必需的最薄一层。桌面三端候选窗收进 core，由内置 Slint 自绘 UI 统一呈现；移动三端候选窗与键盘 UI 用宿主原生框架，core 只提供候选数据。

```
core（Rust，heng-core）             写一次 ×1
├─ librime 封装与 FFI
├─ 版本化 C ABI（include/heng.h，abi_version=11）
├─ 内置自绘候选窗 + 八页设置窗口（Slint）
├─ 用户词库同步（sync.rs：服务端 + 账号，见 docs/SYNC-PLAN.md）
├─ 配置中心对接（config-center/shared/heng.yaml 为配置真源）
└─ 本地 HTTP（诊断与外部工具用，127.0.0.1:9371，非输入链路）
        ↕ 同进程 C ABI：五端外壳进程内直接加载 heng_core 动态库
        （Windows 的 WeaselServer.exe 同样进程内加载 heng_core.dll，无 IPC）
六端外壳                            每端一次 ×6
（只做：按键转发 + 会话管理 + 输入位置上报；移动端另含键盘 UI）
        ↕
librime + 词库（雾凇拼音）           现成 ×0
```

## 六端外壳

| 平台 | 宿主框架 | 外壳形态 | 候选窗 | 状态 |
|---|---|---|---|---|
| Windows | TSF | Weasel 改造（服务进程 `WeaselServer.exe`） | core 自绘（Win32 分层窗口） | **可用** |
| Linux | Fcitx5 | fcitx5 addon + heng_blue 主题 | core 自绘（X11/XWayland） | **可用** |
| macOS | IMK | Squirrel 改造（`third_party/src/squirrel`） | core 自绘（NSPanel 贴帧） | **可用** |
| Android | IMS | 自研，Trime 参考 | 宿主原生 View | 未开始 |
| iOS | Custom Keyboard Extension | 自研（UIKit） | 宿主原生 UIKit | 未开始 |
| 鸿蒙 | IME Kit | 自研（librime 无鸿蒙前端） | 宿主原生 ArkUI | 未开始 |

桌面三端候选窗由 core 的 Slint 软渲染统一产出，逐像素一致；移动三端候选窗与
键盘 UI 由各端原生框架绘制，颜色/字号/间距走 `config-center/shared/heng.yaml`
的 tokens 对齐，观感一致。

## core 组件

| 模块 | 职责 |
|---|---|
| `engine/` | librime FFI 绑定与会话封装（组词、候选、上屏、开关状态） |
| `capi.rs` | 版本化 C ABI（`heng.h`，abi_version=11），各端外壳唯一的对接面 |
| `ui.rs` | 内置自绘候选窗：Slint 软渲染产出预乘 ARGB 帧，平台后端只负责搬帧与指针事件（Linux=X11，Windows=Win32 分层窗口，macOS=NSPanel 贴帧）；另含**八页设置窗口**（无边框 Slint 窗口，托盘菜单 `heng_settings_show` 唤起） |
| `settings.rs` | 设置数据层：config API v4 读 staging 生效值；写直接改 `*.custom.yaml` patch（不碰上游 YAML） |
| `sync.rs` | 用户词库同步：`installation_id` 接管、timer 线程、拉取-导出-上传编排；账号经 `heng-cli sync login` 录入 |
| `server.rs` | 本地 HTTP 服务（诊断与 heng-cli serve 用，127.0.0.1:9371，不在输入链路上） |
| `global.rs` | 全局引擎与会话表 |

## 目录结构

```
HengIME/
├── docs/                    现行文档
│   ├── ARCHITECTURE.md        架构现状
│   ├── FEATURES.md            功能规划清单
│   ├── SETTINGS-PLAN.md       设置窗口结构与开发注意事项
│   ├── SYNC-PLAN.md           词库同步方案（服务端 + 账号）
│   └── archive/               决策记录与试验报告
│
├── core/                    统一能力模块（cdylib: heng_core.dll/.so/.dylib）
│   ├── src/                   engine · capi · ui · settings · sync · server · global
│   └── include/heng.h         C ABI 头文件（供各端外壳 include）
│
├── server/                   heng-sync-server：词库同步服务端（axum + SQLite）
│
├── shells/                  自研外壳
│   └── linux/fcitx5/          fcitx5 addon
│
├── third_party/src/         上游改造型外壳的源码（gitignored）
│   ├── weasel/                 Windows：Weasel 改造，改动固化在 patches/windows/
│   ├── squirrel/               macOS：Squirrel 改造
│   ├── fcitx5-rime/ trime/     参考实现
│
├── config-center/           配置真源（shared/heng.yaml）与分发脚本
├── patches/                 按平台分组的外壳补丁与配置
│   ├── windows/             weasel 补丁 + heng.def + 编译配置（props/env.bat/jam）
│   ├── macos/               squirrel 补丁
│   └── rime/                雾凇基础上叠加的 rime yaml
├── tools/
│   ├── heng-cli/              调试 CLI
│   ├── weasel-dev-install.ps1  Windows dev 版注册/启动/卸载
│   └── squirrel-dev-install.sh
├── runtime/ runtime-dev/   正式版 / dev 版用户数据
├── experiments/             窗口属性实验
└── tools/icons/
```

## 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | core 地基：librime 绑定 + 版本化 C ABI + heng-cli + 本地 HTTP | 已完成 |
| M0.5 | 决策门：三条试验（薄壳化 / GUI 工具包 / 前端差异），裁决走 core 前置路线 | 已完成 |
| M1 | Windows 外壳：Weasel 改造，数据源整体换为 heng-core | 已完成 |
| M5 | Windows 收尾：样式配置化、app_options、配置中心对接 | 已完成 |
| M-P1 | core 内置自绘候选窗（Linux + Windows），替代各端自带候选 UI | 已完成 |
| M7 | Linux 外壳：fcitx5 addon | 可用 |
| M2 | 设置界面：core 内置八页 Slint 设置窗（Web 方案已废弃） | 已上线，进程级动作待验证 |
| M6 | macOS 外壳：Squirrel 改造，NSPanel 贴帧 + 自绘候选窗 | 可用 |
| M3 | 词库与数据同步（账号 + 服务端方案）：core sync + heng-sync-server + CLI + 设置页 | core/服务端/测试已完成，移动端接入待各端外壳 |
| M-P3 | Android 外壳：aarch64 交叉编译 + JNI + IMS 骨架 + 原生键盘 | 未开始 |
| M-P3.5 | iOS 外壳：Custom Keyboard Extension（UIKit 键盘 + 候选栏） | 未开始，需 Apple Developer 账号 |
| M4 | 鸿蒙外壳 | 未开始 |
| M8 | AI 预测（可插拔模块） | 未开始 |

## 命名约定

| 用途 | 取值 |
|---|---|
| 包名 / Bundle ID | `com.heng.ime` |
| Rust crate 前缀 | `heng-` |
| Windows 服务进程 | `WeaselServer.exe`（上游 weasel 自带） |
| 本地 HTTP 自报名 | `heng-server`（由 `heng-cli serve` 拉起，仅诊断用） |
| 命令行工具 | `heng-cli` |
| 中文名 | 衡 |

> 注意：鸿蒙端的包名一旦确定不要修改——改包名会导致旧 Profile 直接失效，需重新申请签名。

## 文档

- [docs/FEATURES.md](docs/FEATURES.md) —— 功能规划清单：功能范围与进度
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) —— 架构现状：进程模型、core 结构、各端状态、已知局限
- [docs/SETTINGS-PLAN.md](docs/SETTINGS-PLAN.md) —— 设置窗口结构、配置读写取舍、Slint 开发注意事项
- [docs/SYNC-PLAN.md](docs/SYNC-PLAN.md) —— 词库同步：服务端 + 账号方案的接口、数据模型与实现状态
- [docs/archive/](docs/archive/) —— 决策记录与试验报告
- [third_party/README.md](third_party/README.md) —— 第三方依赖重建步骤
