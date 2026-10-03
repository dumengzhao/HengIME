# HengIME

> 衡 —— 跨五端的自用输入法
>
> Windows · macOS · Linux · Android · 鸿蒙

一份引擎，五套外壳，上层统一。

---

## 项目定位

自用输入法，不上架应用商店，仅使用免费证书。目标是在五个平台上提供**行为、视觉、设置、功能完全统一**的输入体验。

## 六条核心判断

1. **不自己写输入法框架。** 调研的 12 个跨端输入法实现（搜狗 / 微信 / 百度 / 讯飞 / RIME 全家桶 / Mozc / Keyman / Gboard），无一例外全部复用系统框架（TSF / IMK / Fcitx5 / IMS / IME Kit）。
2. **不自研引擎。** librime + YAML 配置体系免费提供全部上层逻辑，且跨平台天然通用。只有资源远超本项目的商业公司才自研引擎。
3. **候选窗该放在哪个进程，由宿主框架决定。** Windows 的 TSF 是注入宿主进程的 DLL，候选窗**必须外置**到服务进程；macOS / Android / 鸿蒙的输入法是独立进程，候选窗可同进程。
4. **「热路径必须同进程」不成立。** 小狼毫（Weasel）每一次按键都走命名管道 IPC，候选窗与引擎都在独立进程 `WeaselServer.exe` 里。真正的约束是「无阻塞 + 协议精简」。
5. **「配置级统一」有天花板。** RIME 三个前端对「开关记住范围」的处理各不相同，这类差异写在前端代码里。要突破只能自研外壳——见 `docs/ARCHITECTURE.md` 第 9 章的两条路线。
6. **鸿蒙是唯一必须自研外壳的一端。** librime 无任何 HarmonyOS 前端。参考微信输入法节奏：基础版到能力齐全约 21 个月（大团队 + 成熟引擎的前提下）。

## 架构速览

```
设置界面（Web）                   写一次 ×1
        ↕ 本地 HTTP
core 服务进程（Rust，常驻）        写一次 ×1
librime · 词库词频 · 数据同步
AI 预测 · 设置存储 · 本地 IPC
        ↕ 同进程 C ABI（macOS / Linux / Android / 鸿蒙）
        ↕ 命名管道 IPC（Windows，候选窗也在服务进程里）
五端外壳                          每端一次 ×5
（只做：按键转发 + 候选窗渲染）
        ↕
librime + 词库                    现成 ×0
```

完整设计见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)（含竞品架构对照、可借鉴清单 30 条、C ABI 草案、里程碑、风险清单）。

## 目录结构

```
HengIME/
├── docs/
│   ├── ARCHITECTURE.md     架构方案 v2（含竞品对照、可借鉴清单、风险清单）
│   └── M0.5-*.md           决策门三问试验报告（薄壳化 / Slint / 前端差异）
│
├── core/                   Rust 统一能力模块（cdylib: heng_core.dll）
│   ├── src/
│   │   ├── engine/           librime FFI 绑定与封装（M0 已就绪）
│   │   ├── capi.rs           版本化 C ABI（abi_version=3，23 个导出）
│   │   └── server/           本地 HTTP 服务（127.0.0.1:9371）
│   └── include/heng.h        C ABI 头文件（供各端外壳 include）
│
├── tools/
│   └── heng-cli/           命令行校验器（version/cand/commit/bench/serve/abitest）
│
├── third_party/            预编译依赖与参考源码（gitignored，见 third_party/README.md）
│   └── src/weasel/           小狼毫源码（M1 起：RimeWithWeasel 数据源已换 heng-core）
│
├── settings-web/           计划中：设置界面（Web，五端共用）
│
├── skins/                  计划中：皮肤
│
├── shells/                 计划中：五端外壳（Windows 走 Weasel 改造，见里程碑 M1）
│   ├── android/              Kotlin + JNI
│   ├── harmony/              ArkTS + NAPI
│   ├── windows/              Weasel 改造（TSF + 服务进程，命名管道 IPC）
│   ├── macos/                ObjC/Swift，基于 Squirrel 改造
│   └── linux/                Fcitx5 addon（+ 可选独立 UI 进程）
│
└── config-center/          计划中：配置中心
```

## 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | 地基：Rust 骨架 + librime C API 绑定 + 版本化 C ABI + heng-cli + 本地 HTTP | **完成（2026-09）**：abitest 28 项全过 · heng_core.dll 18 个导出经外部进程验证 · HTTP 全链路跑通 |
| M0.5 | **决策门**：薄壳化试验 · 跨端 GUI 工具包验证 · 前端行为差异清单 | **完成并已裁决（2026-10-03）**：三问数据齐备（`docs/M0.5-*.md`），证据倒向路线 P；Windows 外壳裁决选 **A（Weasel 改造）** |
| M1 | **Windows 外壳（Weasel 改造，路线 A 首站）**：Weasel 编译链 + core 数据源整体换血 + 真机打字验证 | **完成（2026-10-03）**：boost/MSVC 编译链闭环 · `RimeWithWeaselHandler` 全部 rime_api 调用替换为 heng_* C ABI · WeaselServer.exe 导入表验证换血成功 · 真机注册 TSF 后打字/候选窗/上屏全链路可用 · 修复上游高亮块溢出压序号问题（横竖排布局间距补偿） |
| M2 | 设置界面与配置中心 | 未开始 |
| M3 | 词库与数据同步 | 未开始 |
| M4 | 鸿蒙外壳（交叉编译 librime + NAPI + InputMethodExtensionAbility） | 未开始 |
| M5 | Windows 外壳收尾（样式配置化、app_options、通知消息、诊断日志清理） | 未开始 |
| M6 | macOS 外壳（基于 Squirrel 改造） | 未开始 |
| M7 | Linux（Fcitx5 addon + ClassicUI SVG 主题） | 未开始 |
| M8 | AI 预测（Keyman lexical model 式可插拔模块） | 未开始 |

**M0.5 是决策门，不是可选项。** 路线选择（第 9 章的两条路线）推迟到拿到试验数据之后，而不是在方案阶段预设。

## 命名约定

| 用途 | 取值 |
|---|---|
| 包名 / Bundle ID | `com.heng.ime` |
| Rust crate 前缀 | `heng-` |
| core 服务进程 | `heng-server` |
| 命令行工具 | `heng-cli` |
| 中文名 | 衡 |

> 注意：鸿蒙端的包名一旦确定不要修改——改包名会导致旧 Profile 直接失效，需重新申请签名。

## 需要提前接受的三件事

1. **逐像素一致性不可达。** 五端字体渲染引擎不同。目标定为「设计一致、比例一致、间距一致」。
2. **Linux 的 Wayland 会话下候选窗受限。** 需使用 X11 / XWayland 会话——搜狗、微信、百度、讯飞也都是这么建议用户的。
3. **「完全统一」分两个层次。** 配置级统一成本低但够不到「由前端代码决定的行为差异」；要全量统一必须自研四端外壳，成本高一个数量级。两者的取舍见 `docs/ARCHITECTURE.md` 第 9 章。
