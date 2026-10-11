# 设置窗口

> 七页结构已全部上线。本文记录当前结构、配置读写取舍与 Slint 开发注意事项。
> 在整体架构中的位置见 [ARCHITECTURE.md](ARCHITECTURE.md) §5。

---

## 1. 为什么是 Slint 原生窗口

设置界面为 Slint 原生窗口，未采用 HTTP + Web 方案。对比：

| 维度 | HTTP + Web | Slint 原生窗口 |
|---|---|---|
| 视觉统一 | 与候选窗两套语言 | 同一 Slint 栈、同一设计语言 |
| 桌面体验 | 浏览器打开 localhost:9371 | 原生窗口（可聚焦） |
| 端口/防火墙 | 依赖 9371 监听 | 无 |
| 五端覆盖 | 理论 ×5 | 桌面三端 + Android；**鸿蒙例外** |

- **HTTP 通道保留**：9371 仍是 CLI/诊断通道，不承载设置 UI。
- `heng_describe()` 自省保留，但不用于动态生成控件 —— 页面手写更可控，
  它只服务「关于」页显示版本与能力清单。
- **鸿蒙单独决策**：设置页用 ArkUI 原生还是复用 Slint，到鸿蒙里程碑再定，
  配置数据层不变即可平滑接入。

---

## 2. 数据层：读与写不对称

这是与最初设想不同的地方，写代码前要知道。

| 方向 | 走哪条路 | 为什么 |
|---|---|---|
| **读** | config API v4（`heng_config_*`），读 staging 编译结果 | 即生效值，不需要另建映射 |
| **写** | **不走 ABI**，直接改 patch 源文件 | 设置 UI 就在 core 内，没有跨进程调用需求 |
| **读（配色）** | 直读 patch 源文件 | 样式不参与 librime 部署语义，源 patch 即真相 |

**没有** `heng_settings_set_*` 这类 ABI setter —— 最初设想的「语义化 setter 走 ABI」
被推翻了，实际是 core 内部 `settings.rs` 的 `set_path` / `set_custom_color` 等函数
被 `ui.rs` 同进程直接调用。

写入落点是 `*.custom.yaml` patch，不碰上游发行 YAML。写完是否 redeploy 分两种：

- **需要 redeploy**：方案列表、快捷键、标点等影响 librime 行为的项。
- **写后不 redeploy**（redeploy 会作废活动会话）：配色、字号、inline preedit 三项，
  写完直接 `set_*` + 重绘。

per-machine state（如 `installation_id`）仍归 librime/core 本地存储。

---

## 3. 窗口与页面结构

640×480（乘 `ui_scale()` 的 DPI 缩放），浅色 #f7f8fa 基调，左侧导航 + 右侧内容。
**窗口固定尺寸，不可缩放**，无最小/最大约束。窗口本身是 `WS_POPUP` 无边框方角，
8px 圆角是内部控件的。

**七页，页号是外壳契约**（`heng_settings_show(page)`）：

| 页 | 名称 | 内容 | 落点 |
|---|---|---|---|
| 0 | 输入方案 | schema 列表、默认方案 | `default.custom.yaml` |
| 1 | 外观 | **四项配色 + HSV 取色器**、字号、inline preedit | `weasel.custom.yaml` 的 `style/heng_*` |
| 2 | 快捷键 | 左右键行为、翻页键、简繁/标点/全半角切换键 | `default.custom.yaml`（key_binder） |
| 3 | 标点符号 | 半角标点模式（拨杆，立即生效） | `default.custom.yaml`（punctuator） |
| 4 | 词库 | 用户词典文件列表、打开用户目录、备份用户词典 | 用户目录 |
| 5 | 服务 | 重启/退出服务、重新部署、词典管理、三个目录按钮 | — |
| 6 | 关于 | 版本、ABI、目录信息（只读） | — |

第 5 页是从托盘/语言栏菜单搬过来的运维功能（原菜单已精简为设置/关于/检查更新三项）。
「服务」页里的进程级动作由外壳执行 —— core 是宿主加载的 DLL，自己重启/退出没意义，
只能抛意图过去（`heng_set_service_handler`）。

配色不采用 `preset_color_schemes` 预设列表：自绘候选窗不读 `style/color_scheme`
（那是 weasel 原生 UI 的键），预设链路对本项目无效。实际使用四项自定义颜色
（`style/heng_*`）+ HSV 取色器。

---

## 4. 入口

- **托盘/语言栏菜单「设置」** → `heng_settings_show(0)`（输入方案页）
- **托盘/语言栏菜单「关于」** → `heng_settings_show(6)`
- **候选窗工具条 ☰ 菜单「设置」** → 打开外观页

**没有全局快捷键**。Ctrl+Shift+P 已分配给「标点切换键」，不可用于设置入口。

---

## 5. Slint 踩坑记录（改设置窗时会遇到）

1. **`if cond:` 不能套 `for`** —— Slint 的 `if` 只能跟单个元素作为子节点。
   给页专属的循环加守卫，要写进迭代表达式：
   ```slint
   for ci[i] in (root.page == 1 ? 4 : 0): Rectangle { ... }
   ```
   （这不是显式 if 块，是给 for 传 0 或 4 个元素）
2. **HorizontalLayout 里的 Text 会吃掉全部剩余宽度**，把后面的控件挤走。
   `horizontal-stretch: 0` + 显式 `width` **无效**，只能改绝对定位。
3. **GridLayout 的 Row 不自动换行**，横向排不下会被裁掉。要网格就用绝对定位算
   `x: Math.mod(k,8)*32px` / `y: Math.floor(k/8)*32px`。
4. **长度 × 长度 = px² 编译失败**：`26px * 3px` 不行，写 `26px * 3`。
5. **颜色字面量必须 6 或 8 位十六进制**，`#0000008080`（10 位）报 Invalid color literal。
6. 设置窗**没有键盘事件通路** —— 平台层只处理指针事件，Slint `TextInput`
   收不到字符。所以没有文本输入框，hex 值也是只读显示。
7. **裸 Text 放进 VerticalLayout 会被拉伸占满剩余高度**，标题行要显式钉高
   + 内部 Text 垂直居中。

---

## 6. 已知问题

- 设置窗在 Visual Studio 里单步会被中断（工具链冲突），但不影响编译和真机使用。
- 进程级动作（重启/退出服务、打开程序目录）依赖外壳注册 handler，
  未注册时会静默失败并在页头提示。