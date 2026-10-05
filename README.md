# HengIME

> 衡 —— 跨五端的自用输入法
>
> Windows · macOS · Linux · Android · 鸿蒙

一份引擎，五套外壳，上层统一。

---

## 项目定位

自用输入法，不上架应用商店，仅使用免费证书。目标是在五个平台上提供**行为、视觉、设置、功能完全统一**的输入体验。

## 总体架构

所有跨端一致的能力收敛到 core（一份代码），外壳只保留与宿主系统交互所必需的最薄一层；候选窗也收进 core，由内置自绘 UI 统一呈现。

```
设置界面（Web，计划中）              写一次 ×1
        ↕ 本地 HTTP
core（Rust，heng-core）             写一次 ×1
├─ librime 封装与 FFI
├─ 版本化 C ABI（include/heng.h）
├─ 内置自绘候选窗（Slint 软渲染）
├─ 本地 HTTP 服务（127.0.0.1:9371）
└─ 配置中心对接（config-center/shared/heng.yaml 为配置真源）
        ↕ 同进程 C ABI（macOS / Linux / Android / 鸿蒙）
        ↕ 命名管道 IPC（Windows：TSF 注入宿主进程，引擎与候选窗都在服务进程）
五端外壳                            每端一次 ×5
（只做：按键转发 + 会话管理 + 输入位置上报）
        ↕
librime + 词库（雾凇拼音）           现成 ×0
```

## 五端外壳

| 平台 | 宿主框架 | 外壳形态 | 候选窗 | 状态 |
|---|---|---|---|---|
| Windows | TSF | Weasel 改造（服务进程 `WeaselServer.exe`） | core 内置自绘（Win32 分层窗口） | **可用** |
| Linux | Fcitx5 | fcitx5 addon + heng_blue 主题 | core 内置自绘（X11/XWayland） | **可用** |
| macOS | IMK | Squirrel 改造 | core 内置自绘（NSPanel，待实现） | 外壳接入中 |
| Android | IMS | Trime 参考 | core 内置自绘（计划） | 未开始 |
| 鸿蒙 | IME Kit | 自研（librime 无鸿蒙前端） | core 内置自绘（计划） | 未开始 |

## core 组件

| 模块 | 职责 |
|---|---|
| `engine/` | librime FFI 绑定与会话封装（组词、候选、上屏、开关状态） |
| `capi.rs` | 版本化 C ABI（`heng.h`，abi_version=7），各端外壳唯一的对接面 |
| `ui.rs` | 内置自绘候选窗：Slint 软渲染产出预乘 ARGB 帧，平台后端只负责搬帧与指针事件（Linux=X11，Windows=Win32 分层窗口，macOS=NSPanel 待实现） |
| `server.rs` | 本地 HTTP 服务（设置界面与诊断用，127.0.0.1:9371） |
| `global.rs` | 全局引擎与会话表 |

## 目录结构

```
HengIME/
├── docs/                    架构方案与决策记录
│   ├── ARCHITECTURE.md        架构方案 v2（竞品对照、路线决策）
│   └── ROUTE-P.md             路线 P 裁决与规划
│
├── core/                    统一能力模块（cdylib: heng_core.dll/.so/.dylib）
│   ├── src/                   engine · capi · ui · server · global
│   └── include/heng.h         C ABI 头文件（供各端外壳 include）
│
├── shells/                  五端外壳
│   ├── linux/fcitx5/          fcitx5 集成 + heng_blue 主题
│   ├── macos/                 Squirrel 改造（进行中）
│   ├── windows/               Weasel 改造（改动固化为 patches/）
│   ├── android/               计划中
│   └── harmony/               计划中
│
├── config-center/           配置中心（shared/heng.yaml 为配置真源）
├── patches/                 weasel 源码改动的固化补丁与配置
├── tools/heng-cli/          命令行校验器（version/cand/commit/serve/abitest/uitest）
├── third_party/             预编译依赖与参考源码（gitignored，见 third_party/README.md）
├── settings-web/            计划中：设置界面（Web，五端共用）
└── skins/                   计划中：皮肤
```

## 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | core 地基：librime 绑定 + 版本化 C ABI + heng-cli + 本地 HTTP | 已完成 |
| M0.5 | 决策门：三条试验（薄壳化 / GUI 工具包 / 前端差异），裁决走路线 P（core 前置） | 已完成 |
| M1 | Windows 外壳：Weasel 改造，数据源整体换为 heng-core | 已完成 |
| M5 | Windows 收尾：样式配置化、app_options、配置中心对接 | 已完成 |
| M-P1 | core 内置自绘候选窗（Linux + Windows），替代各端自带候选 UI | 已完成 |
| M7 | Linux 外壳：fcitx5 addon + heng_blue 主题 | 主体完成 |
| M6 | macOS 外壳：Squirrel 改造（ABI v7 已就绪） | 进行中 |
| M2 | 设置界面与配置中心完善 | 未开始 |
| M3 | 词库与数据同步 | 未开始 |
| M4 | 鸿蒙外壳 | 未开始 |
| M8 | AI 预测（可插拔模块） | 未开始 |

## 命名约定

| 用途 | 取值 |
|---|---|
| 包名 / Bundle ID | `com.heng.ime` |
| Rust crate 前缀 | `heng-` |
| core 服务进程 | `heng-server` |
| 命令行工具 | `heng-cli` |
| 中文名 | 衡 |

> 注意：鸿蒙端的包名一旦确定不要修改——改包名会导致旧 Profile 直接失效，需重新申请签名。

## 设计文档

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) —— 架构方案 v2：竞品架构对照、可借鉴清单、两条路线的决策依据
- [docs/ROUTE-P.md](docs/ROUTE-P.md) —— 路线 P（core 前置 + 内置候选窗）的裁决与规划
- [docs/M0.5-*.md](docs/) —— 决策门三问的试验数据
- [third_party/README.md](third_party/README.md) —— 第三方依赖重建步骤
