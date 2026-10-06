# 设置界面规划（CW2 / M2 重定方向）

> 2026-10-05 决策：**放弃 HTTP + Web 设置页（原 M2 方向），改用 Slint 设置窗口。**
> 原案依据（ARCHITECTURE.md §5.5）的「Slint 未验证」前提已被 CW 线推翻；
> 桌面端浏览器开设置的体验割裂（端口/防火墙/非原生）不可接受。

## 1. 决策与理由

| 维度 | 原案：HTTP + Web | 新案：Slint 设置窗口 |
|---|---|---|
| 视觉统一 | 与候选窗两套语言 | **同一 Slint 栈、同一设计语言** |
| 桌面体验 | 浏览器打开 localhost:9371 | 原生窗口（可聚焦，属性要求远低于候选窗） |
| 端口/防火墙 | 依赖 9371 监听 | 无 |
| 五端覆盖 | 理论 ×5 | 桌面三端 + Android（Slint 支持）；**鸿蒙例外** |
| 状态 | 一行未写（无沉没成本） | 复用已真机闭环的栈 |

- **鸿蒙决策门**（与 M0.5 同款逻辑）：不为没影的平台预付复杂度，到 M-鸿蒙 里程碑再定
  （ArkUI 原生设置页 / 其它），配置数据层不变即可平滑接入。
- HTTP 通道**不删**：9371 仍是 CLI/调试/未来词库同步通道，只是不再承载设置 UI。
- `heng_describe()` 自省保留：不再用于动态生成控件（自用输入法手写页面更可控），
  用于「关于/诊断」页显示版本与能力清单。

## 2. 数据层先焊死（UI 换什么都不影响）

- 配置读写只走 core；UI 层零配置知识。
- 现状缺口：config API v4（`heng_config_*`）**只读 staging**；写配置需要新增：
  - `heng_settings_set_*` 语义化 setter（候选窗样式/翻页键/标点/方案切换…），
    落盘到 `*.custom.yaml` patch（`default.custom.yaml` / `weasel.custom.yaml` /
    `<schema>.custom.yaml`），不碰上游发行 YAML；
  - 写入后 `start_maintenance(TRUE)` 触发重新部署（M1 已验证该路径：配置变化自动部署）。
- 读回：复用 config API v4（staging 编译结果即生效值）。
- per-machine state（installation_id 等）仍归 core 本地存储（ARCHITECTURE.md §8 既定）。

## 3. 窗口与页面结构

窗口：普通可聚焦 Slint 窗口，**640×480**（最小 560×420），浅色 #f7f8fa 基调与候选窗一致，
8px 圆角，左侧导航 + 右侧内容（微信输入法式简洁风）。运行于 core UI 线程（与候选窗同循环，独立窗口）。

导航板块（六页）：

| # | 页面 | 内容 | 数据落点 |
|---|---|---|---|
| 1 | 输入方案 | schema 列表（启用/停用）、默认方案、中英文/简繁开关 | default.custom.yaml（schema_list） |
| 2 | 候选窗样式 | 配色方案（preset_color_schemes 39 套，点选即预览）、横/竖排、字号、单行候选数、inline preedit、页大小 | weasel.custom.yaml + schema patch |
| 3 | 快捷键 | 翻页键（-=/ 方向键）、左右键行为、全半角、简繁切换、二次确认 | default.custom.yaml（key_binder） |
| 4 | 标点符号 | 全/半角标点、自定义标点映射（常用几组预设） | default.custom.yaml（punctuator） |
| 5 | 词库与同步 | 用户词典导入/导出、词频同步（远期，接 8.4 词库同步） | 用户目录 + 同步服务 |
| 6 | 关于与诊断 | 版本（abi_version 自省）、部署日志位置、手动重新部署、重启服务 | 只读 + 动作按钮 |

每页右侧即时生效（写 patch → 自动 redeploy → 候选窗热刷新），无需「保存」按钮。

入口：
- WeaselServer 托盘菜单「设置」（现有托盘逻辑接线）；
- 默认快捷键（如 Ctrl+Shift+P 之类，S2 定）不与现有 hotkeys 冲突；
- 首版只做托盘入口。

## 4. 里程碑（S 线）

| 里程碑 | 内容 | 验收 |
|---|---|---|
| S0 数据层 | core 设置 setter API + custom.yaml patch + redeploy；托盘「设置」开空窗口 | abitest 增项；改一项配置真机生效 |
| S1 样式页 | 窗口骨架（导航+内容区）+ 第 2 页候选窗样式（配色预览最直观，先做熟） | 真机点选配色/字号/横竖排即改即见 |
| S2 方案+快捷键+标点 | 第 1/3/4 页 | 真机切方案、改翻页键生效 |
| S3 诊断页 + 词库管理 | 第 5/6 页（词库可与 8.4 同步合并做） | 导出用户词典成功 |
| S-Android 决策门 | Android 用 Slint 或原生；鸿蒙定方向 | — |

## 5. 与既有文档的关系

- ARCHITECTURE.md §5.5「设置界面用 Web」、§settings-web/ 目录规划 → 本文档取代；
  v3 修订说明待 M2 收尾时并入主文档。
- M2 里程碑改名：设置界面与配置中心（Slint 窗口方向）。
