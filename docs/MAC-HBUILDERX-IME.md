# macOS HBuilderX 中 HengIME 不可用：问题定位与验证方案

## 1. 现象

- HengIME（squirrel 内核 + heng 补丁）在 macOS HBuilderX 编辑器中：第一个字母键可以输入（实测为 a），之后所有字母键与数字键全部无响应。
- 同一台机器上，微信输入法、搜狗输入法在 HBuilderX 编辑器内完全正常（微信输入法为日常写代码所用）；社区亦确认系统拼音可用。
- HengIME 在其他应用（如微信）正常。

## 2. 环境

已知：HBuilderX 为 DCloud 自研编辑器，官方口径 C++ 实现（"Implemented with C++, excellent architecture than Java or Electron"）。有第三方资料称其界面基于 CEF 深度定制版，未经官方确认。

Mac 侧待采集：

- macOS 版本：`sw_vers`
- HBuilderX 版本（Help → About）与 bundle ID：`osascript -e 'id of app "HBuilderX"'`
- 已安装 HengIME 构建：`ls ~/Library/Input\ Methods/`（区分 GitHub 正式版与本地构建）
- 部署配置：squirrel.yaml 与主题配置文件，重点记录 app_options 段与 `style/inline_preedit` 的值
- 可用对照输入法：系统拼音、搜狗、微信输入法、鼠须管（若有）

## 3. 外部证据

| 来源 | 环境 | 现象 |
|---|---|---|
| DCloud 官方社区 ask.dcloud.net.cn/question/216975（2026-01-04） | M4 + macOS 26.3 + HBuilderX 最新版 | 鼠须管、落格输入法「被拦截」，只有字母 a 作首字母能打字，其他字母数字不识别 |
| 同帖跟帖 | Ventura 13.7.8 + HX 4.87 + 鼠须管 1.1.2 | 同样症状，Intel 老系统复现 |
| ask.dcloud.net.cn/question/220643 | HX 最新版 | 豆包输入法：整个键盘失效 |
| ask.dcloud.net.cn/question/131369 | macOS 11.6 + HX 3.2.3 | 百度五笔/拼音：中文输入时英文叠输（输入法消费了键、app 又自行插入） |
| HBuilderX 3.2.16 changelog | Windows | 复制操作导致 QQ/搜狗无法输入标点，IME 兼容问题跨平台长期存在 |
| 本机验证（用户 Mac） | HBuilderX 当前使用版本 | 微信输入法、搜狗输入法在编辑器内正常；HengIME 仅首键可输入 |
| JetBrains JCEF 支持线程 | — | CEF 离屏渲染（OSR）模式下原生输入法事件不进入 Chromium，JCEF 中文输入故障；关闭 OSR 后恢复 |
| RIMES（rime 系 macOS 输入法）架构文（第三方博客） | — | 组字期间 marked-text 会话必须常驻；无会话时光标矩形 API 返回零值；对 iTerm2 类应用采用全角空格占位策略 |

推论：上游 squirrel（鼠须管）在 HBuilderX 上同样失效，多位用户、多个 HX 版本、Intel/ARM 均复现。该问题不是 HengIME 补丁引入。

## 4. macOS 第三方输入法（IMK）契约

app 侧必须实现三件事，缺项与症状对应：

1. **按键转交**：文本视图经 `interpretKeyEvents:` 调输入法的 `handle(_:client:)`。缺失 → 按键被吞，表现为「输入法被拦截」。
2. **上屏/组字回调**：`insertText(_:replacementRange:)` 与 `setMarkedText(_:selectionRange:replacementRange:)`。不尊重 `handle` 返回值 → 中英文叠输。
3. **光标矩形查询**：`attributes(forCharacterIndex:lineHeightRectangle:)`。返回零值 → 候选窗沉在屏幕角落。

HBuilderX 的 C++ 编辑器在上述三项上都有已知缺失记录（对应社区不同症状）。

## 5. HengIME / squirrel 代码坐标

squirrel 源码：`third_party/src/squirrel/sources/`

- `SquirrelInputController.swift:32-124` `handle(_:client:)`：IMK 按键入口；L97-99 放行 Command 快捷键
- `SquirrelInputController.swift:48-51`：bundleId 变化时触发 per-app 配置
- `SquirrelInputController.swift:377-392` `updateAppOptions()`：读取 `app_options/<bundle_id>` 并逐项 set_option，per-app rime 选项机制
- `SquirrelInputController.swift:22, 461-463`：`inlinePreedit` 默认 false，来自主题 `style/inline_preedit`
- `SquirrelInputController.swift:516-528`：组字显示分支。inlinePreedit 为真 → `setMarkedText` 真实 preedit；为假 → `setMarkedText` 全角空格占位（L524-526，代码注释原意为防 iTerm2 回显原始按键）
- `SquirrelInputController.swift:562-584` `commit()`：L581 `insertText(string, replacementRange: .empty)`；L565-579 `force_marked_text_for_direct_commit` 选项为直通型提交先补一个 marked text 会话——squirrel 既有按 app 调整 IMK 用法的先例
- `SquirrelInputController.swift:586-606` `show(preedit:)`：L605 为 setMarkedText 调用点
- `SquirrelInputController.swift:609-619` `showPanel()`：L612 经 `attributes(forCharacterIndex:0, lineHeightRectangle:)` 取光标矩形定位候选窗

heng 补丁：`patches/macos/squirrel-heng.patch`

- 新增 `HengWindowRenderer.swift`（patch L155 起，595 行）：候选窗/字母区为 `.nonactivatingPanel` + `CGShieldingWindowLevel`（L482-494），不抢焦点；core 只出帧、窗口操作回主线程执行；诊断日志落盘 `~/Library/Logs/HengIME/window.log`（L245-263）
- InputController 改动：`observeCandidateClick()`（听 core `hengCoreCommitReady` 通知完成点击提交）、caretAnchor 屏幕坐标锚点、`heng_ui_hide` 防残影
- 按键消费路径与上游 squirrel 逐行相同，补丁不改 `handle` 的返回值逻辑

rime 日志：`SquirrelApplicationDelegate.swift:141-153` 将 `RIME_LOG_DIR` 设为 `NSTemporaryDirectory()/rime.squirrel`。

## 6. 假说与判据

**假说 A（首选）：marked-text 会话触雷。** squirrel 组字必调 `setMarkedText`（占位模式也调）。HBuilderX 编辑器收到后进入坏死的组字态：不再把后续 keyDown 转给输入法，自身也不回显，表现为「首键能进、之后全死」。微信输入法不走该路径，故全程正常。

**假说 B：候选窗弹出后编辑器丢键。** heng 候选窗为 nonactivating + shielding 层级。上游 squirrel 候选窗为同级同类 NSPanel，若该假说成立则鼠须管同样中招，与社区报告不矛盾，未排除。

**假说 C（已排除）：编辑器不给第三方输入法转键。** 搜狗、微信输入法、系统拼音均正常，编辑器可托管第三方 IMK 输入法，与假说矛盾，排除。

**输入法簇划分**：可用簇为搜狗、微信输入法、系统拼音；失效簇为 squirrel/heng、落格输入法、豆包输入法（社区报告）。两簇差异点按嫌疑排序：

1. marked-text 组字会话：squirrel 组字必调 setMarkedText（占位模式亦调用）；失效簇其余成员同样使用自有预编辑 + marked text 会话，可用簇成员不走该路径。
2. 客户端能力探测：squirrel 无 `respondsToSelector` 式能力查询（源码 grep 零命中），不按 app 调整策略；商业输入法普遍带适配层。
3. 次要：`insertText(_:replacementRange:)` 变体提交；候选窗每帧调 `attributesForCharacterIndex:`。
4. 候选窗层级假说基本排除：可用簇成员同样弹候选窗且正常。

已排除项：

- secure input：会全局封锁所有第三方输入法；HengIME 在微信等应用正常，排除。复查命令：HBuilderX 激活时 `ioreg -l | grep SecureInput`
- 辅助功能权限：输入法可切换且在其他应用正常，基础链路完好
- heng 补丁：按键路径与上游一致，上游鼠须管同样中招（见 §3）
- 仓库 app_options：仅 Spotlight/Alfred/Quicksilver 的 ascii_mode，无 HBuilderX 条目；Mac 上的部署配置需单独确认

## 7. 验证步骤（按序执行）

**E1 日志（最关键）。** HBuilderX 编辑器内按 a，随后：

```
tail -30 ~/Library/Logs/HengIME/window.log
```

有 show/blit 记录 → 键进了 squirrel 且 core 在出帧；无记录 → 键未进输入法。

**E2 首键 + 空格。** 按 a 后立刻按空格，不打第二个字母。能上屏汉字 → 仅多键组字坏（组字态吞后续键）；不能 → 提交链路（insertText）也坏。

**E3 下划线观察。** 按 a 后观察编辑器：a 是否带下划线标记。注意占位模式下 marked text 是全角空格，下划线可能落在不可见字符上。有标记 → 编辑器收到 setMarkedText 并进入组字态，坐实假说 A；无 → 转入 E2 判读。

**E4 取消。** 按 a 后按 ESC。组字能取消且无残留 → 会话清理正常；a 残留 → marked text 未被正确清除。

**E5 候选窗。** 按 a 后观察候选窗位置与可见性。沉在屏幕左上角 (0,0) → 光标矩形 API 返零（锚点问题，heng 侧可修）；不可见 → 窗口层/焦点问题（假说 B 域）。

**E6 分区对照。** 在 HBuilderX 内置浏览器/预览窗（web 内容）输入拼音。正常 → 问题限于编辑器组件；同样失效 → 外壳级问题。

**E7 输入法对照矩阵。** 同一编辑器位置依次切换：系统拼音 / 鼠须管（若有）/ 微信输入法 / HengIME。系统拼音也失效 → 编辑器不给第三方输入法转键（只能反馈 DCloud 或绕行）；系统拼音正常而 squirrel 系失效 → squirrel 特定 IMK 用法问题，落入假说 A/B 域。

## 8. 判读决策树

- E1 无记录 → 走 E7。系统拼音不行 → 编辑器侧转键缺陷（反馈 DCloud / 绕行）；系统拼音行 → squirrel 入口问题（罕见，查会话创建与输入源切换）。
- E1 有记录 + E3 有下划线 + 后续键死 → 假说 A 坐实。
- E1 有记录 + E3 无下划线 + E2 空格能上屏 → 假说 A 变体：组字态吞键但 insertText 通路正常。
- E1 有记录 + E2 空格不能上屏 → 编辑器 NSTextInputClient 的 insertText/replacementRange 实现缺陷（编辑器侧）。
- E5 候选窗沉角 → 光标矩形返零，锚点问题（heng 侧可修，与 A/B 相互独立）。
- E6 内置浏览器正常 → 问题限于编辑器组件。

## 9. 假说 A 坐实后的处置选项

编辑器侧缺陷无法从输入法修复，可选路径：

1. **squirrel 侧 per-app 跳过 marked-text 会话**：`app_options` 已有 per-app rime 选项机制（`updateAppOptions` L377-392），新增选项（如 `no_marked_text`）在指定 app 下不调 setMarkedText，组字完全走自有候选窗。squirrel 既有 `force_marked_text_for_direct_commit` 这类按 app 调整 IMK 用法的先例。需配套候选窗锚点 fallback：无 marked-text 会话时光标矩形 API 返零是已记载行为（§3 RIMES 架构文），fallback 取鼠标位置或屏幕中心。
2. **最小验证**：在 Mac 上切换部署主题的 `style/inline_preedit`（true/false 各试），观察症状是否变化，确认 setMarkedText 的内容策略是否影响触雷。
3. **向 DCloud 反馈**：附版本矩阵（HX 4.87/Ventura 与最新版/macOS 26.3 均复现）+ 「鼠须管同样失效」的社区证据 + E1-E5 结果。
4. **绕行**：HBuilderX 内使用微信输入法或系统拼音（当前可用方案）。

## 10. 现状小结

HBuilderX 编辑器可正常托管第三方 IMK 输入法（搜狗、微信输入法、系统拼音均可用）。失效簇（squirrel/heng、落格、豆包）的共同嫌疑为 marked-text 组字会话触发编辑器缺陷，squirrel 的必调 `setMarkedText` 是首要触雷点；上游鼠须管同样中招，非 HengIME 补丁引入。可行动的一侧在 squirrel 的 IMK 用法：per-app 跳过 marked-text 会话（复用 app_options 机制）加候选窗锚点 fallback，工作量以天计。重写不改变结论——新实现只要仍使用 marked text 同样中招；补丁的效果是在问题 app 内切换为可用簇的用法模式。§9 方案未采用，实际修复见 §11。

## 11. 已修复（commit 70de5d0 / 1d3ab84）

根因：HBuilderX 为 Qt/QtWebEngine 应用。squirrel 的 `endComposition` 以 `insertText("")` 结束组合，Qt 下空提交为 no-op，组合态从未清除，宿主在首个按键之后不再把 keyDown 转给输入法。假说 A 的方向成立，但机制是「会话未正确结束」，而非「会话不应建立」。

修复（70de5d0）：

- `endComposition` 改用 `setMarkedText("")`
- `isNonTextKey` 将空串计为非文本键（Qt 下 Esc 的 `charactersIgnoringModifiers` 为空串，漏判会把 `\u{1B}` 插入文档）
- `refreshPlaceHolder` 有选区时跳过（避免 QtWebEngine 用组合串替换选区）
- 按住 ⌘ 期间挂起零宽占位
- 回车/退格/Tab 等控制键交回宿主
- 补丁补入 `data/squirrel.yaml` 与 `SquirrelPanel.swift`，补齐 Makefile 固定签名身份

独立问题与修复（1d3ab84）：⌘F/⌘S 在代码区失效。Qt 以输入源的 `kTISPropertyUnicodeKeyLayoutData` 构建快捷键映射表，第三方输入法该属性为 nil，映射表为空。修法为 `data/squirrel.yaml` 置 `keyboard_layout: default`，外壳 `activateServer` 调 `overrideKeyboard(com.apple.keylayout.ABC)`。

遗留：`runtime/rime-user/squirrel.yaml`、`runtime/rime-user/build/squirrel.yaml`、`runtime-dev/rime-shared/squirrel.yaml` 三处仍为 `keyboard_layout: last`（均未入库）。重新部署会覆盖回 last，需同步。
