# core 自绘候选窗（Slint）——跨平台完成规划

> 规划日期：2026-10-05
> 前置：`docs/ROUTE-P.md`（路线 P）、`docs/M-P1-slint-or-probe.md`（可行性 PoC）
> 状态：Linux/Windows 已上线试用，本文规划剩余工程项与里程碑（CW 系列）

---

## 1. 现状盘点

### 1.1 已达成（随 d2cb7b5 / 8a2da62 入库）

| 能力 | 状态 |
|---|---|
| 共享渲染管线 | Slint `SoftwareRenderer` → 预乘 ARGB 缓冲，一套代码三端复用 |
| 平台接缝 | `UiBackend` trait（configure / set_mapped / blit / poll_events），平台差异全部关在 trait 后面 |
| X11 后端 | x11rb，override-redirect + depth 32 ARGB + colormap，GNOME Wayland/XWayland 实测通过 |
| Win32 后端 | WS_POPUP 分层窗口（LAYERED\|TOPMOST\|NOACTIVATE\|TOOLWINDOW）+ CreateDIBSection + UpdateLayeredWindow，真机通过 |
| 交互语义 | 悬浮只出细边框提示（不抢高亮）、点击选词（core 内部完成 + take_ui_commit 回传）、焦点隐藏 |
| 主题观感 | heng_blue 白底蓝块白字、横排、容器/选中块同 8px 圆角（ARGB 透明角） |
| DPI | Windows 按 LOGPIXELSX 缩放；Linux 固定 1.0 |
| 调试设施 | `heng-cli uitest` 脱壳复现 + `HENG_UI_DUMP=1` 渲染缓冲落盘 |

### 1.2 平台坑档案（已解决，防止回归）

1. XWayland 丢弃**未映射窗口**的 PutImage（无报错）→ 先 map 再画
2. depth 32 窗口必须**显式 border_pixel**，否则 CreateWindow BadMatch
3. depth 32 窗口的 PutImage depth 必须为 32
4. `TargetPixel::background()` 默认实现是不透明黑 → 圆角外黑角，需覆写为预乘透明
5. Slint `ui.show()` 不调用则永不渲染；首帧前缓冲未分配会越界 panic

---

## 2. 决策点（需要拍板）

### D1：宽度自适应的实现方式
- **A（推荐）**：渲染两遍——首遍用零宽候选跑一次布局拿到各 Text 的实际宽度，二遍正式渲染。准确、无字体依赖，代价是多一次毫秒级渲染
- B：cosmic-text 测量 API 手动算——省一次渲染，但要自己维护测量与 Slint 布局的一致性
- C：维持字符估算（现状）——长候选/生僻字会有偏差，只够试用

### D2：主题数据源
- **A（推荐）**：`config-center/shared/heng.yaml` 增 `ui/tokens` 段（背景/高亮/文字/圆角/字号/横竖排），config API 读取，作为跨端唯一样式真源；weasel.yaml 的 39 个配色作为「迁移名单」逐步收编
- B：各端继续读宿主配置（Windows 读 weasel.yaml、Linux 读 heng.yaml）——真源分裂，路线 P 要消除的东西

### D3：事件循环模式
- **A（推荐）**：事件驱动——X11 用 pollfd 挂进现有等待、Win32 用 MsgWaitForMultipleObjects，消灭常驻 8ms 轮询（当前空转烧 CPU，笔记本上可测）
- B：维持轮询（实现最简单，先跑通功能再优化）

### D4：渲染器
- **维持 SoftwareRenderer**（推荐）：候选窗只有几百像素宽，软件渲染绰绰有余，且是三端唯一零 GPU 依赖的方案；femtovg/skia 引入 GPU 上下文初始化差异，收益不成比例

---

## 3. 里程碑

| 里程碑 | 内容 | 依赖 | 验收 |
|---|---|---|---|
| **CW0 收尾** | D1 宽度实测（A 方案）；D2 主题 tokens 进 heng.yaml 并三端读取；D3 事件驱动化；线程 panic 后自动重建（当前 UI 线程死亡即候选窗永久失效）；abitest 补 v7 用例（Linux 侧回归） | 无 | 笔记本空转 CPU 占用 ≈0；换主题改 heng.yaml 一处生效 |
| **CW1 功能完备** | 竖排候选（配置切换）；翻页（候选超宽 → ›/‹ 与 PgUp/PgDn）；中英状态小图标（微信同款角标）；注释（comment）列显示；**工具条菜单（就地展开：设置入口→settings-web、开关切换→set_option，见红线 5）** | CW0 | 微信键盘横排可见功能对齐 |
| **CW2 macOS 后端** | `UiBackend` NSPanel 实现：nonactivating panel + NSBitmapImageRep/layer 内容 + NSEvent 轮询；配合 M-P4 Squirrel 的 `ui/builtin` 开关（对齐 weasel 集成模式：宿主面板隐藏 + heng_ui_sync/hide/take_ui_commit） | M-P4 Squirrel 骨架 | Mac 真机候选窗三交互（悬浮/点击/焦点）与 Win/Linux 一致 |
| **CW3 Android 后端** | SurfaceControl/WindowManager 弹窗 + JNI 桥接 blit；触屏点击（无 hover）；配合 M-P3 IMS 骨架 | M-P3 | 真机横排候选 + 点击上屏 |
| **持续项** | 多屏/fractional DPI（Linux per-screen、Wayland 原生路径评估）；partial blit（X11 按 DirtyRegion 局部 PutImage，Win 分层窗保持全量）；候选数据超长截断策略 | — | — |

---

## 4. 架构红线（演进时不得破坏）

1. **后端保持愚蠢**：`UiBackend` 只做「给缓冲就显示、给坐标就挪、吐指针事件」——任何语义（悬浮该不该动高亮、点击选谁）都留在 core 共享层。classicui 悬浮歧义的教训
2. **一处样式真源**：tokens 只进 heng.yaml（config-center 分发），禁止各端再写死颜色
3. **无 GPU 依赖**：SoftwareRenderer 路线锁定，D4 不翻案
4. **平台坑档案随代码走**：新后端实现必须先读 §1.2，新增坑必须补录
5. **功能优先「就地展开」**：菜单/工具条/面板一律做进同一窗口的 Slint 场景（状态切换），禁止为功能新建平台窗口——保住「一端加功能全端都有」；仅系统能力（托盘图标等）允许进后端，且语义仍须留在 core

---

## 5. 样式同步的保证与边界

**保证**：所有走 tokens（D2）的样式，改 `config-center/shared/heng.yaml` 一处，三端观感一致。机制由 CW0（D2）建立，能力由 CW2（macOS 后端）补齐。

**边界（三条，防止期望错位）**：
1. **分发是拉模式**：改样式需 commit → 各端 pull → deploy.sh；无服务器实时推送。真·云同步属 M-P6（settings-web + 配置云）
2. **观感一致 ≠ 逐像素一致**：三端字体不同（思源黑体/微软雅黑/苹方），文字度量与抗锯齿有原生差异，属正确代价
3. **tokens 之外的面**：内联拼音样式（宿主应用绘制）、app_options 按应用覆盖值，不在候选窗同步范围

---

## 6. 与主里程碑的关系

CW0/CW1 属 M-P1/M-P2 的完善尾巴，可与 M-P3（Android IMS）并行；CW2 随 M-P4 走；CW3 随 M-P3 走。候选窗五端共用一套 Slint 场景是路线 P「全量统一」最直观的展示面。
