# HengIME 架构方案

> 项目代号：HengIME（中文名「衡」）
> 目标平台：Windows / macOS / Linux / Android / 鸿蒙
> 约束前提：自用，不上架应用商店，仅需免费证书
> 状态：方案设计，未开始实施
> 版本：v2 —— 已并入跨端输入法竞品架构调研（第 3 章）

---

## 修订说明（v1 → v2）

本次改造调研了 12 个跨端输入法实现，推翻了 v1 的三处论证，新增两章：

| 项 | v1 的说法 | v2 修正 |
|---|---|---|
| 推理链 | 「鸿蒙必须自研 ⇒ 五端外壳全薄壳化」 | 跳步。鸿蒙必须自研推不出四端也要自研。改为**两条路线 + 一个决策门**（第 9 章） |
| 进程模型 | 「热路径必须同进程 FFI 保延迟」 | 反例成立：小狼毫每一次按键都走命名管道 IPC，候选窗根本不在进程内。改为**候选窗归属由宿主框架决定**（第 5 章） |
| 词库同步 | 只在配置中心提了一句 `installation_id` | 补全为独立章节：Rime sync 的真实机制、文本导出合并、五端各自的落法（第 8.4 节） |
| 竞品对照 | 只有搜狗与微信两家 | 扩到 12 家，含 Mozc / Keyman / Weasel / Squirrel / fcitx5 / 百度 / 讯飞（第 3 章） |
| 可借鉴清单 | 无 | 新增（第 4 章，30 条） |
| 引擎定位 | 「librime …… 或未来的自研引擎」 | 焊死：**不自研引擎**（第 3.5 节） |

---

## 1. 需求与约束

### 1.1 目标平台

五个平台，两类形态：

- **桌面端**：Windows、macOS、Linux
- **移动端**：Android、鸿蒙

鸿蒙同时存在移动（手机 / 平板）与 PC 形态，且 IME Kit 还覆盖 TV 与穿戴设备——同一输入法应用可部署到多种设备形态。

### 1.2 硬约束

| 约束 | 说明 | 带来的影响 |
|---|---|---|
| 自用，不上架 | 不走应用商店审核 | 免去隐私合规申报、商店资质、账号体系等一大块工作 |
| 仅需免费证书 | 不接受付费代码签名 / 开发者年费 | 见第 6.2 节，五端中有三端根本不需要证书 |
| 上层必须统一 | 不接受「各端各装一个不同软件」 | 本方案的核心命题 |
| 引擎不自研 | 直接采用 librime | v2 明确。理由见第 3.5 节：12 家中只有资源远超本项目的商业公司才自研引擎 |

### 1.3 要统一的五层

1. 行为与配置：输入方案、快捷键、标点、词库
2. 视觉：候选窗长相
3. 设置界面
4. 功能与数据：AI 预测、跨端剪贴板、词库同步
5. 以上全部

---

## 2. 结论摘要

八条核心判断：

1. **不该从零写输入法。** librime 与 YAML 配置体系免费提供上层逻辑，跨平台天然通用。
2. **不需要自己写输入法框架。** 调研的 12 个实现，无一例外全部接入宿主系统框架（TSF / IMK / Fcitx5 / IMS / IME Kit）。
3. **核心引擎只有一份，逐端编译。** Mozc 同一份 C++ 编译到四端；Keyman Core 一份 C 代码配五个平台层；搜狗官方明说 Linux 引擎移植自 Windows 版。
4. **只有 Android 与鸿蒙需要证书，且都是免费的。**
5. **鸿蒙是唯一必须自研外壳的一端。** librime 无任何 HarmonyOS 前端。
6. **「热路径必须同进程」是错的。** 小狼毫（Weasel）的每一次按键都走命名管道 IPC，候选窗运行在独立服务进程里。真正的约束是「无阻塞 + 协议精简」，不是同进程。
7. **候选窗该放在哪个进程，由宿主框架决定，不能自由选。** Windows TSF 是注入宿主进程的 DLL，候选窗必须外置；macOS / Android / 鸿蒙的输入法是独立进程或独立应用，候选窗可同进程。详见第 5 章。
8. **「配置级统一」有天花板。** RIME 三个前端对「开关记忆范围」的处理各不相同（小企鹅全局 / 鼠须管按会话 / 小狼毫仅中英全局），这类差异写在**前端代码**里，改配置解决不了。这决定了统一的深度上限（第 9.3 节）。

---

## 3. 竞品架构对照

### 3.1 国产商业输入法

| | 搜狗 | 微信 | 百度 | 讯飞 |
|---|---|---|---|---|
| Windows | TSF + 自研引擎 | TSF + 自研引擎 | TSF + 自研引擎 | TSF + 自研引擎 |
| macOS | IMK + 自研引擎 | IMK + 自研引擎 | IMK + 自研引擎 | IMK + 自研引擎 |
| Linux | **Fcitx 插件** + 自研 Qimpanel 面板 + 移植自 Windows 的引擎 | **基于 Fcitx5**：守护进程 + IM Module + 独立 UI Frontend 进程 | **Fcitx 框架** + 自研 BaiduIME；核心服务 / UI 渲染 / 云连接**三进程隔离**，候选面板为 `baidu-qimpanel` | Fcitx 插件形态 |
| Android | IMS + 自研引擎 | IMS + 自研引擎 | IMS + 自研引擎 | IMS + 自研引擎 |
| 鸿蒙 | 2026-07 才宣布全面适配 | 2024-10 基础版（仅打字）→ 2026-07 补齐 | 未全面适配 | 未全面适配 |
| Linux 架构支持 | x86_64 / arm64 / mips64el / loongarch64 | — | x86_64 / arm64 | x86_64 / arm64 / mips64el / loongarch64 |

**四家共同点**：桌面端全用系统框架、Linux 端全是 Fcitx 插件、候选窗全是**自己的独立面板进程**、引擎全自研。

### 3.2 RIME 生态（本项目直接依赖的现成前端）

| | Weasel（小狼毫） | Squirrel（鼠须管） | Trime（同文） | fcitx5-rime |
|---|---|---|---|---|
| 平台 | Windows | macOS | Android | Linux |
| 宿主框架 | TSF（`WeaselTSF.dll`）+ 旧 IMM32（`WeaselIME.ime`） | InputMethodKit | InputMethodService | Fcitx5 addon |
| 进程结构 | **双前端 DLL + 独立 `WeaselServer.exe`** | 单一 `.app` 进程 | 单一 APK 进程 | fcitx5 daemon 内的插件 |
| 引擎位置 | 在 `WeaselServer.exe` 里 | **同进程**（Swift 经 C API 直调 librime） | 同进程（JNI） | 同进程 |
| 候选窗位置 | **在 `WeaselServer.exe` 里**（`WeaselPanel`） | 同进程 `SquirrelPanel`（NSPanel） | 同进程 | fcitx5 addon |
| IPC | **命名管道**（`PipeChannel`） | 无（同进程 C API） | 无（JNI） | 无（同进程） |
| 语言 | C++ | Swift + ObjC | Kotlin + JNI | C++ |

#### 3.2.1 Weasel 的进程模型（本次调研最重要的发现）

小狼毫的 IPC 协议只有六条消息：`START_SESSION` / `PROCESS_KEY_EVENT` / `UPDATE_INPUT_POS` / `FOCUS_IN` / `FOCUS_OUT` / `END_SESSION`。

关键事实：

- **TSF DLL 极薄**，只做按键转发、组合串管理、语言栏。选字框（候选窗）**刻意不放在 DLL 里**，尽管 TSF 本身提供了候选词接口。
- 候选窗与 librime 一起运行在 `WeaselServer.exe` 这个独立进程，通过**命名管道**与 DLL 通信。
- 传输层**刻意避开共享内存与窗口消息**——这两者在 UWP / Windows Runtime 沙箱里被禁止，会导致输入法在 Metro 应用里静默崩溃。命名管道是可用且稳定的选择。
- 维护 per-application 的 session 映射（每个应用一个输入会话）。
- 存在 CUAS（Cicero-Unaware Application Support）兼容处理，用于取不到正确文本位置的应用。

**为什么这对本项目决定性**：它证明了两件事——① 候选窗放在独立进程是 Windows 上的**正确形态**，不是可选项；② **IPC 承载热路径完全可行**，v1 的「热路径必须同进程」论断不成立。

#### 3.2.2 Squirrel 为什么可以同进程

鼠须管把 librime 直接链进 `.app` 进程，候选窗是同进程的 `SquirrelPanel`。这是因为 **macOS 的 IMK 输入法本身就是独立应用进程**，不注入宿主应用。没有 Weasel 那种「DLL 住在别人进程里」的问题。

其他细节：`SquirrelInputController` 每个输入上下文一个实例、每个实例一个 Rime session；支持 `--build` / `--sync` / `--reload` / `--quit` / `--register-input-source` 等命令行运维子命令；librime 以 git submodule 固定版本。

### 3.3 其他跨端引擎（非中文）

| | Mozc（Google 日语） | Keyman（SIL） | Gboard |
|---|---|---|---|
| 平台 | Windows / macOS / Linux / Android | Windows / macOS / Linux / Android / iOS / Web | Android / iOS / TV / Wear OS |
| 核心 | 一份 C++，五层分层（Client / Session / Core Engine / Data / Base） | 一份 C（Keyman Core），平台无关 | 同一引擎，平台薄壳 |
| 通信 | **一份 protobuf 定义（`commands.proto`），三端三种方式**：Android JNI 直调（同进程）、macOS IPC、Windows/Linux 客户端-服务端 | **C99 类型 + C 调用约定**（为最大 FFI 兼容性） | 平台特定抽象层 |
| 关键设计 | Android 端引擎内嵌；macOS 曾用 **Qt 渲染候选窗**（多进程、独立 renderer） | **引擎无状态**：客户端（平台层）负责加载键盘、持有并传入状态；键盘元数据用 JSON 自省 | — |
| 扩展模型 | — | **键盘（keyboard）与语言模型（lexical model）解耦**，独立打包 | — |
| 许可 | 开源 | 开源 | 专有 |

**Mozc 的启发**：它没有用 C ABI，而是**用一份 IDL（protobuf）定义接口，各端选最合适的通信方式**。这比 v1 定的「C ABI + IPC 两套接口各自维护」更省。

**Keyman 的启发**：把引擎做成**无状态**的——平台层负责加载、持有、传入状态。这样 FFI 边界上不需要维护跨调用的复杂状态，C 侧实现也简单。另外「语言模型独立于键盘」正好是 AI 预测模块该有的形态。

### 3.4 Linux 输入法框架

| | Fcitx5 | IBus | Maliit |
|---|---|---|---|
| 架构 | 核心 + addon；UI 走 `UserInterfaceManager` 多策略并存 | 引擎 + UI 分离 | 插件式 client-server，D-Bus |
| UI 策略 | ClassicUI（Cairo/Pango 自渲染）、Kimpanel（D-Bus 交桌面）、VirtualKeyboard | 自带面板 | 虚拟键盘插件 |
| 主题能力 | **ClassicUI 原生支持 9-patch 图片与 SVG** | 有限 | — |
| 状态 | 活跃，主流 | 活跃，GNOME 默认 | **已停滞**（最后版本 2022-07） |

**Maliit 的教训**：造一个跨端虚拟键盘框架这条路已经被走过且失败了（被 QtVirtualKeyboard 取代）。不要再造框架。

**fcitx5 ClassicUI 的关键事实**：既然它能解析 SVG 主题，那么「Linux 端视觉对齐」有一个远比自研 UI 进程便宜的手段——**直接产出 fcitx5 主题**。自研 UI 进程应作为「ClassicUI 表达力不够时」的升级路径，而不是起点。

### 3.5 横向归纳

#### 五条共识（12 家无例外）

1. **引擎与 UI 分离，UI 逐端实现。**
2. **不自己写输入法框架**，一律接入宿主系统框架。
3. **引擎只有一份**，逐端编译或移植。
4. **候选窗自绘**，且商业实现普遍外置到独立进程（Qimpanel / UI Frontend / baidu-qimpanel / WeaselServer）。
5. **跨端数据同步只有两种形态**：账号云同步（商业）或共享目录 + 文本合并（Rime）。

#### 三条分歧（必须做选择）

| 分歧 | 选项 A | 选项 B | 本项目选择 |
|---|---|---|---|
| 接口协议 | C ABI（librime / Keyman） | IDL + IPC（Mozc protobuf） | **一份 IDL，两种绑定**（第 5.3 节） |
| 候选窗进程 | 同进程（Squirrel / Trime / IMS / 鸿蒙） | 外置进程（Weasel / 搜狗 / 微信 / 百度） | **由宿主框架决定**（第 5 章） |
| 输入定义格式 | RIME YAML | 自有格式 | **RIME YAML**（不自研引擎的直接推论） |

#### 一条硬边界

**只有资源远超本项目的商业公司才自研引擎。** 搜狗、微信、百度、讯飞、Gboard、Mozc、Keyman——自研引擎的都有专职 NLP 团队与长期词库积累。自用项目的正确姿势是用 librime。

这也意味着：**本项目永远不该走「自研引擎」这条路。** v1 在模块职责里留的「librime 或未来的自研引擎」口子，v2 焊死。

---

## 4. 可借鉴清单

逐条给出「谁做的 → 具体做法 → 本项目怎么用」。

| # | 来源 | 做法 | 本项目怎么用 |
|---|---|---|---|
| 1 | librime | C API 结构体用 `data_size` 字段做版本化，配 `RIME_STRUCT_INIT()` 与 `RIME_API_AVAILABLE()` 宏做能力探测 | core 的 C ABI 照抄这套版本化手法（第 10.2 节） |
| 2 | librime | 候选结构自带 `comment` 字段（拼音、词频等注释） | v1 草案里的 `comments` 数组本就存在，**不必自造**，直接映射 |
| 3 | librime | 自带 `tools/rime_api_console.cc` 命令行参考实现 | M0 的校验 CLI **直接对照它写**，不必从零探索 |
| 4 | librime | 提供 `RimeSimulateKeySequence` 按键序列模拟 | M0 自动化测试的入口 |
| 5 | librime | 提供 `setup / initialize / start_maintenance / join_maintenance_thread` 三段式异步初始化 | core 的启动流程照抄，避免部署阻塞界面 |
| 6 | Keyman Core | **引擎无状态**：平台层负责加载、持有并传入状态 | core 不在 FFI 边界维护跨调用状态，热路径接口全部显式传参 |
| 7 | Keyman Core | C99 类型 + C 调用约定，为最大 FFI 兼容性 | 照抄。避免在 ABI 里出现 C++ 类型 |
| 8 | Keyman | **键盘与语言模型（lexical model）解耦**、独立打包 | AI 预测做成 core 的可插拔子模块，**不参与热路径**（第 11 章） |
| 9 | Keyman Core | 键盘元数据通过 JSON 自省接口暴露 | core 暴露一个 `heng_describe()`，设置界面据此动态生成控件，省掉前后端手工同步字段 |
| 10 | Mozc | 一份 `commands.proto` 定义，各端选最合适的通信方式 | core 用一份 IDL 定义接口，C ABI 与本地 HTTP 从**同一份定义**生成（第 5.3 节） |
| 11 | Mozc | macOS 端用 **Qt 渲染候选窗** | 证明「跨端 GUI 工具包渲染候选窗」有先例可循（第 5.4 节） |
| 12 | Mozc | Android 端引擎内嵌（JNI 直调），不做 IPC | Android 外壳走同进程直调，不引入额外进程 |
| 13 | Weasel | TSF DLL 极薄，候选窗 + 引擎都在独立服务进程 | Windows 外壳的形态定义（第 7.1 节） |
| 14 | Weasel | IPC 传输层选**命名管道**，避开共享内存与窗口消息（UWP 沙箱限制） | core 的本地 IPC 用命名管道 / 本地 socket，**不用共享内存** |
| 15 | Weasel | 六条消息的极简 IPC 协议 | core 的热路径 IPC 协议照着精简，不做通用 RPC |
| 16 | Weasel | per-application session 映射 | core 的 session 按「应用 + 输入上下文」二维管理 |
| 17 | Weasel | CUAS 兼容处理（取不到文本位置时的兜底） | 记入已知坑清单，Windows 外壳实现时处理 |
| 18 | Squirrel | librime 以 git submodule 固定版本 | core 同样 pin 住 librime 版本，避免上游漂移 |
| 19 | Squirrel | 每个输入控制器一个 Rime session | core 的 session 生命周期与输入上下文绑定 |
| 20 | Squirrel | `--build` / `--sync` / `--reload` / `--register-input-source` 运维子命令 | core 提供同样的命令行运维入口 |
| 21 | fcitx5 | UI 走策略模式，多实现并存、统一调度 | 候选窗渲染做成可替换后端，宏环境先跑通一个 |
| 22 | fcitx5 | 候选列表按能力分接口：`Pageable` / `Bulk` / `CursorMovable` / `Actionable` | 照此分层设计 core 的候选数据结构，别塞进一个大 struct |
| 23 | fcitx5 | ClassicUI 原生支持 9-patch 与 **SVG** 主题 | Linux 端先出 SVG 主题，**不自研 UI 进程**（第 7.2 节） |
| 24 | fcitx5 | 自带按键模拟测试框架（`fcitx-utils/testing.h`） | M0 测试策略参照 |
| 25 | 搜狗 | 皮肤用 **SVG 矢量**定义，各端解析同一份 | `skins/*.svg` 的既有决策——本次获得**第二个独立验证** |
| 26 | 搜狗 / 微信 / 百度 | 候选窗是自带面板的独立进程 | 独立进程是商业实现的共识形态，非特例 |
| 27 | 百度 | 核心服务 / UI 渲染 / 云连接**三进程隔离** | core 的进程切分参照：云同步独立于输入路径，崩了不影响打字 |
| 28 | 鸿蒙 | 一个 `InputMethodExtensionAbility` 承载多个 subtype | 鸿蒙子类型（拼音 / 英文 / 五笔）共用同一扩展 |
| 29 | 鸿蒙 | `editorAttributeChanged` 事件感知前台应用期望（沉浸模式） | 鸿蒙外壳需订阅此事件做候选窗样式适配 |
| 30 | 全部 | 无一家为跨端重写五份候选窗——多为改造既有前端 | 支撑第 9 章的路线讨论 |

---

## 5. 进程模型

### 5.1 候选窗归属由宿主框架决定

v1 把进程模型当成一个可自由设计的选项（「热路径 FFI 同进程 + 冷路径 IPC」）。调研后确认：**这是宿主的既定事实，不是设计自由度**。

| 平台 | 宿主框架的进程形态 | 外壳代码住在哪 | 候选窗能不能同进程 |
|---|---|---|---|
| Windows | TSF：**DLL 注入宿主应用进程** | 宿主应用进程内 | **不能**。必须外置 |
| macOS | IMK：输入法是**独立 .app 进程** | 独立进程 | 能 |
| Linux | Fcitx5 addon：跑在 **fcitx5 daemon** 里 | daemon 进程内 | 能（但外观受 ClassicUI 约束，见 7.2） |
| Android | IMS：输入法是**独立应用进程** | 独立进程 | 能 |
| 鸿蒙 | IME Kit：**独立 Extension 进程** | 独立进程 | 能 |

**Windows 为什么必须外置**，两条独立理由：

1. `WS_EX_NOACTIVATE`（使窗口点击时不被激活）**在同一个线程创建的窗口之间不生效**。而 TSF DLL 与宿主应用共享线程，候选窗若建在 DLL 里，焦点行为会不对。
2. TSF DLL 住在宿主进程里，受宿主的一切限制（UWP / Windows Runtime 沙箱禁止共享内存与窗口消息，历史上曾导致小狼毫在 Metro 应用里静默崩溃）。

### 5.2 结论：core 常驻独立进程，候选窗按端归属

```
┌──────────────────────────────────────────────────────────┐
│ core 服务进程（heng-server）                    常驻 ×1   │
│ librime · 词库与词频 · 数据同步 · AI 预测 · 设置存储       │
│ 本地 HTTP（设置界面）· 本地 IPC（热路径）                  │
└──────────────────────────────────────────────────────────┘
        ↕ 命名管道 / 本地 socket          ↕ 本地 HTTP
┌────────────────────────────┐   ┌────────────────────────┐
│ 外壳进程（按宿主框架落位）   │   │ 设置界面（Web）         │
│ 按键转发 + 候选窗渲染        │   │ 浏览器 / WebView        │
└────────────────────────────┘   └────────────────────────┘
```

各端落位：

| 平台 | 外壳进程落位 | 候选窗渲染位置 | 外壳 → core |
|---|---|---|---|
| Windows | `WeaselTSF.dll` 在宿主进程内（薄转发） | **core 服务进程** | 命名管道 |
| macOS | 输入法 `.app` 进程 | 该进程内 | 同进程 C ABI |
| Linux | fcitx5 daemon 内的 addon | 先走 ClassicUI + SVG 主题；不满足再起独立 UI 进程 | 同进程 C ABI |
| Android | 输入法应用进程 | 该进程内 | 同进程 JNI |
| 鸿蒙 | Extension 进程 | 该进程内 | 同进程 NAPI |

**这个模型的价值**：Windows 上「候选窗在服务端」是照抄小狼毫的成熟形态；其余四端候选窗同进程，省掉 IPC 往返。也就是说，**跨端一致的不是进程模型，而是 core 的接口**——进程模型由宿主决定，接口由 IDL 统一。

### 5.3 一份 IDL，两种绑定

Mozc 的做法（一份 protobuf，三端三种通信）比 v1 的「C ABI 与 IPC 两套接口各自维护」更省。本项目采用：

```
heng.idl（唯一定义源）
   ├── 生成 C ABI 头（heng.h）     → 同进程 FFI：macOS / Linux / Android / 鸿蒙
   └── 生成 IPC 编解码 + JSON      → 本地 IPC：Windows；本地 HTTP：设置界面
```

收益：接口加一个字段只改一处；设置界面的 JSON 与 FFI 的结构体天然对齐，不会漂移。

### 5.4 候选窗渲染：不自研，先复用跨端工具包

调研结论：**没有任何一家为跨端重写五份候选窗渲染代码**。Mozc 在 macOS 端用 Qt，Keyman 各端用平台原生控件但共享逻辑与定义。

因此候选窗渲染按「成本递增」分三级，按需升级：

| 级别 | 做法 | 覆盖 | 何时用 |
|---|---|---|---|
| L1 | 各端既有前端自带渲染 + 共享皮肤参数 | 全部 | **起点**。Windows 用小狼毫、Linux 用 fcitx5 ClassicUI |
| L2 | 跨端 GUI 工具包渲染候选窗（Mozc 先例） | 桌面 + Android | L1 外观不达标时 |
| L3 | 各端原生自绘 | 全部 | 只有视觉不可妥协时才做，**且应作为最后手段** |

**v1 的「候选窗 ×5」被降级为 L3 的假设。** 若 L2 可行，桌面三端 + Android 是一份代码，鸿蒙仍需 ArkTS 原生——即 ×2 而非 ×5。

关于跨端工具包的候选（供 M0.5 验证时定夺）：Slint 支持 Windows / macOS / Linux / Android / iOS，体积小、声明式、生产可用；egui 更轻但外观偏工具风；iced 不适用。**但必须警惕一个未验证的风险**：候选窗需要「不抢焦点 + 置顶 + 不激活 + 跟随光标」，跨端工具包未必都开放这些窗口属性（Windows 侧要 `WS_EX_NOACTIVATE` + `SW_SHOWNOACTIVATE`，并在 `WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE`）。这是 M0.5 要验证的第三件事。

关于 WebView（Tauri / Electron 类）：**不采用**。输入法进程常驻，WebView 的内存开销不可接受；且候选窗需要亚帧级响应，WebView 启动与合成延迟不可控。

### 5.5 候选窗不能用 WebView，但设置界面可以

两者要求相反，分开处理：设置界面用 Web（延迟不敏感、交互复杂、写一次五端复用），候选窗用原生或原生绑定。

---

## 6. 平台接入层

### 6.1 五端宿主框架对照

| 平台 | 宿主框架 | 实现语言 | 交付形态 | 难度 |
|---|---|---|---|---|
| Windows | TSF 文本服务框架（旧 IMM32 已淘汰） | C++ / COM | DLL + 注册表注册 | 高 |
| macOS | InputMethodKit | Objective-C / Swift | `.app` 装入 `Library/Input Methods` | 中 |
| Linux | Fcitx5（走 D-Bus） | C / C++ | addon + 可选独立 UI 进程 | 中 |
| Android | InputMethodService | Kotlin + JNI | APK | 低 |
| 鸿蒙 | IME Kit，`InputMethodExtensionAbility` | ArkTS + NAPI C++ | HAP | 中高 |
| iOS（不在本期范围） | Custom Keyboard Extension | Swift | App Extension | 中 |

### 6.2 签名与证书要求

| 平台 | 是否必须签名 | 免费方案 | 续期与坑 |
|---|---|---|---|
| Windows | **不需要** | 零证书。TSF 是用户态 COM 组件，写注册表即可加载 | Win11 智能应用控制会拦未签名安装包，关掉即可 |
| macOS | **不需要** | 本机编译后放入 `Library/Input Methods` 即可加载 | Bundle ID 必须含 `inputmethod` 关键字，否则系统不识别 |
| Linux | **不需要** | IBus / Fcitx5 是普通用户级服务 | 无 |
| Android | **必须** | `keytool` 自签名，debug keystore 即可 | 签名一旦更换，必须卸载旧版才能装新版 |
| 鸿蒙 | **必须** | DevEco Studio 自动签名 | **唯一会过期的**：实名后 1 年，未实名仅 14 天。详见 8.2 节 |

**结论：所谓「免费证书」只涉及 Android 与鸿蒙两端，且都有官方免费途径。**

### 6.3 现成可复用的前端

| 平台 | 前端 | 来源 | 获取方式 |
|---|---|---|---|
| Windows | 小狼毫 Weasel | rime/weasel，官方维护 | 安装包，覆盖 Win 8.1 / 10 / 11 |
| macOS | 鼠鬚管 Squirrel | rime/squirrel，官方维护 | `brew install squirrel-app` |
| Linux | fcitx5-rime | fcitx/fcitx5-rime，社区维护 | 发行版包管理器 |
| Android | 同文 Trime | osfans/trime，社区维护 | GitHub Releases |
| 鸿蒙 | 无 | — | 必须自研 |

补充：

- librime 官方前端仅三个（Weasel / Squirrel / ibus-rime），其余均为社区维护。**Android 端没有官方前端**，选型时留意维护活跃度。
- librime 前端列表中**完全没有 HarmonyOS / OpenHarmony 实现**——鸿蒙必须自研的权威依据。
- fcitx5-android 需额外装 `plugin.rime`，不如 Trime 直接。
- 这些前端按 GPL 发布且无商业签名，Windows 安装时杀软弹拦截属正常现象。

---

## 7. Windows 与 Linux 专项

### 7.1 Windows 专项

**形态定义**（照抄小狼毫的成熟结构）：

```
宿主应用进程                          独立服务进程
┌───────────────────────┐           ┌──────────────────────────────┐
│ WeaselTSF.dll（极薄） │  命名管道 │ heng-server                  │
│ 按键转发 · 组合串     │ ◀───────▶ │ librime · 词库 · 同步 · 设置 │
│ 语言栏 · 焦点事件     │           │ 候选窗渲染（WS_EX_NOACTIVATE）│
└───────────────────────┘           └──────────────────────────────┘
```

热路径 IPC 协议（照抄小狼毫的精简度）：

| 消息 | 作用 |
|---|---|
| `START_SESSION` / `END_SESSION` | 会话生命周期 |
| `PROCESS_KEY` | 按键 → 候选 |
| `UPDATE_INPUT_POS` | 更新光标位置（候选窗跟随） |
| `FOCUS_IN` / `FOCUS_OUT` | 焦点变化 |

已知坑（需在实现时处理）：

- **CUAS**：Cicero 不兼容的应用取不到正确的文本位置，需兜底逻辑（小狼毫有专门 workaround）。
- **UWP / Windows Runtime 沙箱**：禁用共享内存与窗口消息。传输层只能用命名管道或本地 socket。
- **候选窗创建**：去掉 `WS_VISIBLE`、加 `WS_DISABLED` 创建，再 `ShowWindow(SW_SHOWNOACTIVATE)`；响应 `WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE`；加 `WS_EX_NOACTIVATE` 与 `WS_EX_TOOLWINDOW`（不进任务栏与 Alt-Tab）。
- 小狼毫自带皮肤系统能力弱（基本只能改配色），所以视觉对齐必须在 core 服务进程里自绘，不能依赖 Weasel 的皮肤。

### 7.2 Linux 专项

#### Fcitx5 的 UI 多策略

| 策略 | 实现方式 | 视觉可控度 |
|---|---|---|
| ClassicUI | Fcitx5 自带面板，Cairo/Pango 绘制，**支持 9-patch 与 SVG 主题** | 中，受 fcitx5 渲染能力约束 |
| Kimpanel | 通过 D-Bus 交给桌面环境渲染 | 最低 |
| 独立 UI 进程 | 输入面板置空，自有进程经 D-Bus 收数据后自绘 | **完全可控** |

#### 修正：先走主题，再考虑自研 UI 进程

v1 直接跳到「独立 UI 进程 + 自有 D-Bus 名」。调研后发现漏了一层——**ClassicUI 能解析 SVG 主题**，与项目既定的「皮肤用 SVG 矢量」决策天然契合。

| 阶段 | 做法 | 成本 |
|---|---|---|
| 先用 | 产出一份 fcitx5 ClassicUI SVG 主题，与 `skins/tokens.yaml` 同源生成 | 低 |
| 不够再上 | 独立 UI 进程 + 自有 D-Bus 名 | 中高 |

官方态度（Fcitx 开发者 Q&A）：

- 第三方引擎想自绘候选窗是常见诉求，但**不要实现新的 Kimpanel**——同时只允许一个 kimpanel 服务，会与桌面既有服务冲突，且没有「换回去」的机制。
- 也不建议实现新的 UI addon（必须对所有输入法通用）。
- **正规做法**：不给 `InputContext::inputPanel()` 设任何值，保持为空；自有 UI 进程通过 D-Bus 接收数据后自绘。**可复用 Kimpanel 的接口设计，但必须用自己的 D-Bus 名。**

搜狗的 Qimpanel、微信的 UI Frontend、百度的 `baidu-qimpanel` 走的都是这条路。

#### Wayland 限制与规避

- **X11 会话**：窗口可自由定位，无特殊限制。
- **Wayland 会话**：输入法 popup surface 需要特定协议，可能只有输入法服务本身才有权限显示；`zwp_input_method_v2` 在 GNOME 下不被支持。
- **规避**：使用 X11 / XWayland 会话。搜狗、微信、百度、讯飞也都建议用户切 X11。

---

## 8. 鸿蒙与数据同步专项

### 8.1 鸿蒙技术路径

- 继承 `InputMethodExtensionAbility` 实现 `onCreate` / `onDestroy`，在 `module.json5` 里以 `type: "inputMethod"` 声明，系统自动加入输入法列表。
- 核心组件：`InputMethodExtensionAbility`（入口 + 生命周期）、`inputMethodEngine`（面板创建与事件监听）、`Panel`（面板窗口）、`InputClient`（与前台应用通信）、`KeyboardController`（键盘逻辑）。
- **一个扩展承载多个 subtype**：中文 / 英文 / 五笔作为 subtype 注册，共享同一个 `InputMethodExtensionAbility`。
- 面板支持固定态、悬浮态、状态栏；同一应用可部署手机、平板、PC，另覆盖 TV 与穿戴。
- 订阅 `editorAttributeChanged` 事件感知前台应用的样式期望（沉浸模式）。
- **引擎不需要重写**：librime 交叉编译为鸿蒙 `.so`，ArkTS 侧经 NAPI 调用。Rust 生态已有 OpenHarmony FFI 绑定。
- 切换输入法的系统 API 需申请系统权限。

### 8.2 鸿蒙调试证书的三个坑

1. **会过期**：实名后 1 年，未实名仅 14 天。到期需重新签名并重装。
2. **换电脑会断**：自动签名本地生成，换机器或删除证书后必须重新生成，而**旧证书安装的应用无法覆盖安装，必须先卸载**。需妥善备份 `.p12` 与口令。
3. **包名一次定死**：改包名会导致旧 Profile 直接失效。

其他约束：每账号最多 3 个调试证书；调试设备需登记 UDID；调试证书不能上架（本项目不上架，无影响）。

### 8.3 时间预期

微信输入法鸿蒙版：2024-10 上线基础版（仅基础打字）→ 2026-07 的 1.0.0 补齐悬浮键盘、问 AI、排版成图等能力。

「基础功能到能力齐全约 21 个月」——但要注意这是**大团队 + 成熟引擎**下「能力齐全」的耗时，不是「最小可用」的耗时。自研鸿蒙端的真实门槛在「NAPI 桥接 + 输入法生命周期 + 真机调试」，不在时间尺度上。先做到能用，功能逐步补齐。

### 8.4 词库与数据同步

#### Rime 内置 sync 的真实机制

v1 只在配置中心提了一句 `installation_id`，这里补全。

**目录结构**：所有设备把 `sync_dir` 指向同一个共享位置（网盘 / iCloud / 本地目录）。每个设备在其中生成**以自己 `installation_id` 命名的子目录**：

```
sync_dir/
├── office_win/        ← PC-1 的 installation_id
│   └── luna_pinyin.userdb.txt
└── home_mac/          ← PC-2 的 installation_id
    └── luna_pinyin.userdb.txt
```

**合并规则**（关键，且反直觉）：

- 用户词典平时是 **LevelDB** 存储（`*.userdb/` 下有 `.ldb` / `LOG` / `MANIFEST`），**二进制不可直接合并**。
- 同步时 librime 把它**导出为文本** `*.userdb.txt`，放进各自子目录。
- **合并发生在「下一次同步」**：PC-1 同步 → 网盘传播 → PC-2 再同步，才拿到 PC-1 的内容。**不是实时的**。
- 两个文件必须位于**不同的 `installation_id` 子目录**才会合并；同一目录内不合并。
- 跨方案迁移要用「改名 + 改文件内 `#@/db_name`」的手法，让 Rime 以为是另一台设备的词典。

**同步范围有限**：只同步用户词典与部分个性化配置；**方案词库（`dicts/` 目录）、Lua 脚本等不会被同步**，只有配置目录根层的 YAML / TXT 会被单向备份。

#### 本项目的五端落法

| 端 | 共享目录可行性 | 落法 |
|---|---|---|
| Windows / macOS / Linux | 可行（网盘目录） | 直接用 Rime sync 机制，`installation_id` 各机唯一 |
| Android | 沙箱内，难挂共享目录 | 应用内「导入 / 导出用户词典」，或经 core 的中继同步 |
| 鸿蒙 | 同上 | 同上 |

**core 的增量价值**：把「各设备导出 → 等网盘 → 再同步」这个手动、异步、依赖共享文件系统的流程，换成**经自有中继的主动同步 + 冲突合并**（可复用 ClipSync 的中继通道）。这是本项目可能做得比大厂更干净的地方——搜狗 Linux 版的词库同步是「需手动导入导出」，微信靠账号体系。

#### `installation_id` 的正确处理

**不能纳入统一配置下发。** v1 说「由部署脚本注入」是错的——脚本在每台机器跑的是同一份内容，一样会撞车。

正确做法：把 `installation_id` 从配置中心摘出来，交给 **core 的 per-machine state**（core 本就承担设置存储），首次启动生成一次并持久化。

---

## 9. 两条路线与一个决策门

### 9.1 v1 断在哪

v1 的推理链是：鸿蒙必须自研外壳 → 用户必然拥有自己的前端 → 统一层下沉到引擎之上 → **四端外壳薄壳化**。

前三步成立。**第四步是额外加的。** 「鸿蒙需要自研前端」推不出「Weasel / Squirrel / Trime / fcitx5-rime 都不要了」。

### 9.2 两条路线

| | 路线 F（外壳全薄化） | 路线 P（core 前置） |
|---|---|---|
| 四端外壳 | 全部替换为薄壳，接 core | 保留现成前端，core 注入配置 + 皮肤对齐 |
| core 职责 | 引擎能力层（librime 之上） | 统一配置 + 设置界面 + 词库同步 |
| 候选窗 | 五端可控 | 四端受各自前端能力约束 |
| 鸿蒙 | 自研（两路线都要） | 自研（两路线都要） |
| 视觉统一 | 可控到「设计 / 比例 / 间距一致」 | 到「同调性」为止 |
| 行为统一 | 全量 | **仅配置级** |
| 成本 | 高 | 低 |

### 9.3 决定性的新证据

**路线 P 有天花板，而天花板的高度是这次调研才看清的。**

RIME 三个前端对「开关记忆范围」的处理各不相同：

- 小企鹅（Linux）：**全局**记住
- 鼠须管（macOS）：仅当前会话记住，每个 App、每个输入框独立管理
- 小狼毫（Windows）：仅中英状态全局，其他开关仍单独管理

这类差异**写在前端代码里**，`default.custom.yaml` 改不动。也就是说：

> 路线 P 的能力上限是「**配置级统一**」。凡是「由前端代码决定的行为差异」，配置层无解，只能改前端代码。

而「上层必须统一」这个诉求，其中「行为一致」的相当一部分正落在这一层。这是 v1 没有说清的：**路线 P 不是路线 F 的廉价子集，它是另一个终点。**

### 9.4 建议：把选择推迟到 M0.5 决策门

不建议在方案阶段就定路线，因为定路线需要的事实还不全。M0.5 应回答三个问题：

1. **薄壳化试验**：拿一个真实前端（建议 Trime 或 Weasel），试着把它的候选窗替换成 core 提供的数据。判断「改造既有前端使其变薄」与「基于 librime 重写」哪个更省事。**这是全项目最大的未验证假设。**
2. **跨端 GUI 工具包做候选窗**：验证 L2 级别（第 5.4 节）是否可行，特别是「不抢焦点 + 置顶 + 不激活」的窗口属性是否都能拿到。
3. **行为差异的实际影响面**：把三个 RIME 前端的行为差异逐项列出来，量出路线 P 的天花板到底低到什么程度。

三个问题有答案后，路线自然浮出。

---

## 10. 工程结构与对外接口

### 10.1 目录结构

```
HengIME/
├── core/                       # Rust 统一能力模块（跨平台，写一次）
│   ├── src/
│   │   ├── lib.rs              # 导出 C ABI
│   │   ├── engine/             # librime 绑定与封装
│   │   ├── config/             # YAML 配置读写、校验、部署
│   │   ├── dict/               # 用户词库、词频统计
│   │   ├── sync/               # 跨端数据同步（含冲突合并）
│   │   ├── clipboard/          # 跨端剪贴板（复用 ClipSync 中继）
│   │   ├── predict/            # AI 预测（可插拔，不参与热路径）
│   │   ├── server/             # 本地 HTTP 服务 + 本地 IPC
│   │   └── state/              # per-machine 状态（installation_id 等）
│   ├── idl/heng.idl            # 唯一定义源，生成 C ABI 与 IPC 编解码
│   ├── include/heng.h          # 生成的 C ABI 头文件
│   └── Cargo.toml
│
├── settings-web/               # 设置界面（Web，写一次）
│
├── skins/                      # 皮肤
│   ├── tokens.yaml             # 配色、圆角、间距、字体、候选数
│   ├── *.svg                   # 各端共用
│   └── fcitx5/                 # 由 tokens 生成的 fcitx5 ClassicUI 主题
│
├── shells/                     # 五端外壳
│   ├── android/                # Kotlin + JNI —— 参考实现，先做
│   ├── harmony/                # ArkTS + NAPI
│   ├── windows/                # C++ TSF（薄）+ heng-server 内的候选窗
│   ├── macos/                  # ObjC/Swift，基于 Squirrel 改造
│   └── linux/                  # Fcitx5 addon（+ 可选独立 UI 进程）
│
├── config-center/              # 配置中心
│   ├── shared/                 # 跨平台通用 YAML
│   ├── platforms/              # 各端专属 UI 配置
│   └── deploy.sh               # 分发脚本（仅桌面三端）
│
├── tools/
│   └── heng-cli/               # 命令行校验器（对照 rime_api_console 写）
│
└── docs/
    └── ARCHITECTURE.md
```

### 10.2 对外接口设计原则

| 原则 | 来源 | 说明 |
|---|---|---|
| 版本化结构体 | librime | 每个跨边界结构体带 `data_size` 首字段，配 `HENG_STRUCT_INIT()` 与 `HENG_API_AVAILABLE()` 宏 |
| C99 类型 + C 调用约定 | Keyman Core | ABI 内不出现 C++ 类型；字符串所有权规则显式写清 |
| 引擎无状态 | Keyman Core | 不在 FFI 边界维护跨调用状态，热路径显式传参 |
| 一份 IDL 两种绑定 | Mozc | C ABI 与本地 HTTP 从同一份 `heng.idl` 生成 |
| 候选列表按能力分层 | fcitx5 | 分 `Pageable` / `Bulk` / `CursorMovable` / `Actionable` 四类能力，避免一个巨型 struct |
| 热路径零阻塞 | Weasel | 热路径接口不做同步、AI、网络；IPC 协议只保留必要的六条消息 |
| 状态回传用调用方持有的结构体 | librime / Weasel | 避免跨 FFI 边界的所有权歧义 |
| 元数据自省 | Keyman Core | 暴露 `heng_describe()` 返回 JSON，设置界面据此动态生成控件 |

### 10.3 接口草案（供 M0 定稿）

```c
/* heng.h —— 统一能力模块对外接口（草案） */

typedef struct HengEngine HengEngine;

/* 所有跨边界结构体首字段为 data_size，用于版本兼容 */
typedef struct {
    int   data_size;
    char* preedit;
    int   cursor;
    char** candidates;
    char** comments;      /* 候选注释，直接映射 librime 的 comment */
    int   candidate_count;
    int   highlighted;
    int   page_no;
    int   page_total;
    int   has_prev;
    int   has_next;
} HengContext;

/* ---------- 生命周期 ---------- */
HengEngine* heng_create(const char* shared_dir, const char* user_dir);
void        heng_destroy(HengEngine* engine);

/* ---------- 热路径：零阻塞，同进程或走极简 IPC ---------- */
int  heng_process_key(HengEngine* engine, const char* session_id, int keysym, int modifiers);
int  heng_get_context(HengEngine* engine, const char* session_id, HengContext* out);
void heng_select_candidate(HengEngine* engine, const char* session_id, int index);
void heng_context_free(HengContext* ctx);

/* ---------- 会话（按 应用 + 输入上下文 二维管理） ---------- */
int  heng_start_session(HengEngine* engine, const char* app_id, char* session_id_out);
void heng_end_session(HengEngine* engine, const char* session_id);

/* ---------- 冷路径：可走 IPC / HTTP ---------- */
int  heng_reload_config(HengEngine* engine);
int  heng_sync_begin(HengEngine* engine);
int  heng_learn(HengEngine* engine, const char* word);

/* ---------- 元数据自省（设置界面据此生成控件） ---------- */
const char* heng_describe(void);   /* 返回 JSON */

/* ---------- 本地服务 ---------- */
int  heng_serve(HengEngine* engine, int port);
```

---

## 11. 模块职责与预测模块

**core（Rust 能力模块）**

- 向上：对五端外壳暴露 C ABI，对设置界面暴露 HTTP，对 Windows 外壳暴露本地 IPC
- 向下：封装 librime
- 承载：词库与词频、数据同步、跨端剪贴板、AI 预测、设置存储、per-machine 状态
- 这是唯一能存放差异化能力的位置

**从 librime 拿到的（不必自造）**：候选与注释、分页、组合串、状态与开关、方案管理、YAML 配置读写、部署维护、用户词典同步、按键序列模拟。

**predict（AI 预测）**——照 Keyman 的 lexical model 思路：

- **独立子模块、独立打包**，与引擎解耦
- **不参与热路径**。预测结果作为候选的附加来源异步汇入，绝不阻塞按键→候选的往返
- 可整体关闭（无网络 / 省电场景）

**shells（五端外壳）**——每端只做两件事：

1. 按键事件转发到 core
2. 渲染 core 返回的候选

**config-center**：`shared/` 放跨端通用 YAML；`platforms/` 放各端专属 UI 配置；`deploy.sh` 按平台分发。**注意只能覆盖桌面三端**——Android / 鸿蒙沙箱内无法脚本写入配置，需走应用内导入或 core 同步。

---

## 12. 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| **M0** | **地基**：Rust 骨架 + librime C API 绑定 + 版本化 C ABI + `heng-cli` 命令行校验器 + 本地 HTTP 服务 | 未开始 |
| **M0.5** | **决策门**：① 薄壳化试验（拿一个真实前端）② 跨端 GUI 工具包渲染候选窗的窗口属性验证 ③ 三前端行为差异清单 | 未开始 |
| **M1** | **Android 参考外壳**（产出标准外壳实现） | 未开始 |
| **M2** | 设置界面与配置中心 | 未开始 |
| **M3** | 词库与数据同步（Rime sync 机制 + 自有中继合并） | 未开始 |
| **M4** | 鸿蒙外壳（交叉编译 librime + NAPI + `InputMethodExtensionAbility`） | 未开始 |
| **M5** | Windows 外壳（TSF 薄壳 + 服务进程候选窗 + 命名管道 IPC） | 未开始 |
| **M6** | macOS 外壳（基于 Squirrel 改造） | 未开始 |
| **M7** | Linux（Fcitx5 addon + ClassicUI SVG 主题，不够再上独立 UI 进程） | 未开始 |
| **M8** | AI 预测（Keyman lexical model 式可插拔模块） | 未开始 |

**M0 细化**：

1. `heng-cli`：命令行模拟按键、打印候选 —— **直接对照 `rime_api_console.cc` 写**
2. 用 `RimeSimulateKeySequence` 做按键序列的自动化测试
3. C ABI 跑通，验证 `data_size` 版本化与所有权规则不泄露
4. 三段式异步初始化（`setup` → `initialize` → `start_maintenance`），验证部署不阻塞
5. 本地 HTTP 起得来，能读写 YAML 配置

> M0.5 是本次改造的核心改动：**它把「路线选择」从方案阶段推迟到有试验数据之后。** 先做试验，再选路线，而不是先选路线再做。

---

## 13. 风险与妥协清单

| 风险项 | 说明 | 应对 |
|---|---|---|
| **「改造前端使其变薄」可能比重写更贵** | Weasel / Trime 的平台逻辑与 UI 重度耦合。**全项目最大的未验证假设** | M0.5 决策门用真实试验判定 |
| **配置级统一有天花板** | 三前端对开关记忆范围等行为处理不同，差异在前端代码里 | 接受为路线 P 的边界；要突破只能走路线 F |
| 候选窗窗口属性未验证 | L2（跨端 GUI 工具包）需要「不抢焦点 + 置顶 + 不激活」，各工具包支持度未知 | M0.5 验证。不通过则退回 L1（用既有前端渲染） |
| Windows 候选窗必须外置 | TSF DLL 在宿主线程内，`WS_EX_NOACTIVATE` 同线程失效；UWP 沙箱禁共享内存与窗口消息 | 照抄小狼毫形态：DLL 极薄 + 服务进程渲染候选窗 + 命名管道 |
| CUAS 应用取不到文本位置 | Cicero 不兼容应用无法正确定位候选窗 | 记入已知坑，实现时做兜底（小狼毫有 workaround） |
| 逐像素一致性不可达 | 五端字体渲染引擎不同 | 目标定为「设计一致、比例一致、间距一致」 |
| Wayland 下候选窗受限 | 输入法 popup surface 受协议权限约束 | 使用 X11 / XWayland，与搜狗 / 微信 / 百度 / 讯飞一致 |
| Linux 无官方 RIME 前端 | Trime / fcitx5-rime 均社区维护 | 关注上游活跃度；严重时可 fork |
| **Maliit 式陷阱** | 造跨端输入法框架这条路已被走过且失败 | 明确不自研框架，只接宿主框架 |
| 鸿蒙调试证书 1 年过期 | 未实名仅 14 天 | 完成开发者实名认证；`.p12` 妥善备份 |
| 鸿蒙换机需卸载重装 | 证书本地生成，换机后旧证书装的应用无法覆盖 | 固定一台构建机；包名一次定死 |
| 鸿蒙 API 演进快 | IME Kit 仍在迭代 | 鸿蒙代码隔离在 `shells/harmony/` |
| 词库同步非实时 | Rime 内置机制需「导出 → 传播 → 再同步」 | core 走自有中继做主动同步（M3） |
| `installation_id` 不能统一下发 | 统一下发会导致多机共用同一 sync 目录、互相覆盖 | 放进 core 的 per-machine state |
| Android / 鸿蒙配置无法脚本写入 | 沙箱限制 | 走应用内导入或 core 同步，`deploy.sh` 只管桌面三端 |
| GPL 授权约束 | Weasel、Squirrel、Trime 等为 GPL 发布 | 自用且不分发不触发义务；若分发需重新评估 |
| 热路径 IPC 延迟 | Windows 端每次按键往返一次命名管道 | 协议精简到六条消息；小狼毫已验证可行，仍应在 M5 实测 |

---

## 附录 A · 现成前端清单

librime 仓库登记的前端（官方 3 个，其余社区维护）：

**官方**：Weasel（Windows）、Squirrel（macOS）、ibus-rime（Linux）

**社区（平台相关）**

- Android：Trime、YuyanIme、fcitx5-android
- macOS：XIME、fcitx5-macos
- Linux：fcitx-rime、fcitx5-rime
- Windows：PIME、rabbit
- iOS：Hamster
- Web：My RIME

**社区（编辑器与终端）**：ARIF、coc-rime、emacs-rime、rime.nvim、tmux-rime、zsh-rime、pyrime 等。

**结论：列表中无任何 HarmonyOS / OpenHarmony 前端。**

---

## 附录 B · 竞品架构速查表

| 实现 | 平台 | 引擎位置 | 候选窗位置 | 通信 | 引擎自研 |
|---|---|---|---|---|---|
| 搜狗 | 五端 | 各端同一份，Linux 版移植自 Windows | 独立面板进程（Qimpanel） | 自研 | 是 |
| 微信 | 五端 | 各端同一份 | 独立 UI Frontend 进程 | 自研 | 是 |
| 百度 | Win/mac/Linux/移动 | 自研 BaiduIME | 独立 `baidu-qimpanel` | 自研，三进程隔离 | 是 |
| 讯飞 | Win/mac/Linux/移动 | 自研 | 独立面板 | 自研 | 是 |
| **Weasel** | Windows | `WeaselServer.exe` | **`WeaselServer.exe`** | **命名管道** | 否（librime） |
| **Squirrel** | macOS | 同进程 | 同进程 NSPanel | C API | 否（librime） |
| **Trime** | Android | 同进程 | 同进程 | JNI | 否（librime） |
| **fcitx5-rime** | Linux | daemon 内 | fcitx5 UI | 无 | 否（librime） |
| Mozc | Win/mac/Linux/Android | 一份 C++，逐端编译 | Android 同进程；macOS 曾用 Qt 独立 renderer；Win/Linux 客户端-服务端 | **protobuf，三种方式** | 是 |
| Keyman | 六端 | Keyman Core（无状态 C） | 各平台原生 | C ABI | 是（规则引擎） |
| Gboard | Android/iOS/TV/Wear | 同一引擎 | 平台薄壳 | 平台特定 | 是 |
| Maliit | Linux/Windows | — | 插件 | D-Bus | 框架（已停滞） |

---

## 附录 C · 关键事实来源

| 事实 | 来源 |
|---|---|
| Weasel 双前端 + WeaselServer + 命名管道 IPC + 六条 IPC 消息 | DeepWiki `rime/weasel` System Architecture；WeaselTSF 页 |
| Weasel 候选窗刻意放在服务进程、避开 TSF 候选词接口 | 《Windows 输入法的 metro 应用兼容性改造》 |
| UWP 沙箱禁用共享内存与窗口消息（小狼毫崩溃根因） | 同上 |
| Squirrel 架构、同进程 librime、`SquirrelPanel`、命令行子命令、git submodule | DeepWiki `rime/squirrel` Core Architecture 与 Rime Integration |
| librime C API：`rime_get_api()`、`data_size` 版本化、`RimeTraits` / `RimeContext` / `RimeCandidate.comment`、函数分组、`RimeSimulateKeySequence` | DeepWiki `rime/librime` Public API Reference 与 RimeApi C Interface；`src/rime_api.cc` |
| librime 自带 `rime_api_console.cc` 参考实现、三段式异步初始化 | 同上 |
| Mozc 五层分层、一份 protobuf 三端三种通信、Android JNI 内嵌、macOS 用 Qt 渲染 | DeepWiki `google/mozc` Overview |
| Keyman Core：平台无关、C99、无状态引擎、客户端持有状态、JSON 自省、lexical model 解耦 | Keyman Core API 官方文档；keyman.com/engine |
| fcitx5 UI 多策略、`UserInterfaceManager`、候选列表能力分层、ClassicUI 支持 9-patch 与 SVG、测试框架 | DeepWiki `fcitx/fcitx5` User Interface 与 UI Component Architecture |
| Fcitx 官方对第三方引擎自绘 UI 的态度与推荐做法 | fcitx-im.org 开发者 Q&A |
| 百度 Linux 版基于 Fcitx、三进程隔离、`baidu-qimpanel` 路径、架构支持 | 百度百科；蓝点网安装说明；百度 Linux 版技术解析 |
| 搜狗 / 讯飞 Linux 版形态与架构支持 | 国产 Linux 桌面软件库汇总；多来源架构说明 |
| 搜狗 Linux 版引擎移植自 Windows 版 | 搜狗 Linux 版技术架构说明 |
| 微信输入法 Linux 版基于 Fcitx5、独立 UI Frontend 进程 | 技术问答站点故障剖析文章 |
| 微信输入法鸿蒙版时间线（2024-10 → 2026-07） | IT之家报道 |
| 搜狗 2026-07 全面适配鸿蒙 | 2026-07-09 多家媒体报道 |
| 鸿蒙 IME Kit：`InputMethodExtensionAbility`、`inputMethodEngine`、Panel、InputClient、KeyboardController、subtype、`editorAttributeChanged`、设备形态覆盖 | 华为开发者官方 IME Kit 文档；OpenHarmony Input Method Framework |
| 鸿蒙调试证书有效期与配额 | 华为开发者账号调试签名流程说明 |
| `WS_EX_NOACTIVATE` 语义与「同线程内失效」 | Microsoft Learn Extended Window Styles；多篇 Windows 焦点处理实践 |
| 候选窗非激活窗口的完整做法（`SW_SHOWNOACTIVATE` + `MA_NOACTIVATE` + `WS_EX_TOOLWINDOW`） | Windows 窗口焦点实践文献 |
| Rime sync 机制：`sync_dir` / `installation_id` 子目录 / `.userdb.txt` 文本导出 / 下次同步合并 / 同步范围 | oh-my-rime 多设备同步文档；Dvel 博客；Rime 社区实践 |
| 用户词典底层为 LevelDB（`.ldb` / `LOG` / `MANIFEST`） | 鼠须管用户目录结构实测文献 |
| 三前端「开关记忆范围」行为差异 | Dvel 博客（RIME 实践） |
| librime 前端清单、无鸿蒙前端 | rime/librime 仓库 README Frontends 段 |
| Maliit 已停滞、被 QtVirtualKeyboard 取代 | Maliit 项目版本历史与 KDE 迁移记录 |
| 跨端 GUI 工具包（Slint / egui / iced）平台覆盖与成熟度 | Rust GUI 库横向调研 |
