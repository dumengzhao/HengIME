# 路线 P：core 前置 + 各端自研薄壳 —— 裁决与实施规划

> 裁决日期：2026-10-04
> 决策门输入：三份试验报告全部完成（见文末引用）
> 状态：已裁决走路线 P，本文为实施规划 v1
> 关联：`ARCHITECTURE.md` 第 9 章（两条路线定义）、`M0.5-frontend-diff.md`（差异证据）、`M0.5-slint-window-probe*.md`（UI 路线证据）

---

## 1. 裁决记录

**结论：走路线 P。** core 前置，五端外壳全部自研（Windows 改造 weasel、Linux 自研
fcitx5 addon、Android/macOS/鸿蒙各写薄壳），行为统一的责任全部收进 core。

### 依据（试验数据 → 结论）

| 输入 | 关键事实 | 对裁决的影响 |
|---|---|---|
| 薄壳化试验（Windows） | weasel 数据源换血为 heng-core C ABI 后真机可用，外壳只剩转发+渲染 | 路线 P 的单端成本 ≈ 一个数据源适配层，不是重写 |
| Slint 窗口属性（双端） | Windows 薄 shim 全 PASS；Linux 纯 Slint 不可达但 override-redirect 可行 | 候选窗自绘（L2）各端都有平台级出路，无阻断 |
| 前端差异清单 | 差异是机制性的（谁持有状态、传播到哪），实现成本小且可归纳（owner + 传播策略 + app_options 表） | 配置级统一（路线 F）到不了「由前端代码决定的行为」，天花板已实证 |
| 本机试用活案例（2026-10-04） | 悬浮高亮歧义（fcitx5 classicui 硬编码）、左右键/主题/模糊音两台机器手工同步 | 用户亲历路线 F 的两处天花板：UI 行为 + 配置分发 |

**裁决含金量最高的一条**：试用第一天就撞上两处只有路线 P 能消除的差异——
这不依赖任何推测，是实测结论。

---

## 2. 路线 P 的统一责任清单（core 要接管什么）

以 `M0.5-frontend-diff.md` §3 为基线，全部收进 core：

| # | 职责 | 现状 | 缺口 |
|---|---|---|---|
| U1 | 会话管理（per-IC 会话 + SetSessionOwner(app_id)） | ✅ v3 已有（core/src/global.rs SESSIONS/OWNER_MAP） | 无 |
| U2 | 开关传播策略（global / per-app / per-session） | ❌ 四前端各写各的 | 新增：策略配置 + 传播引擎 + ABI |
| U3 | app_options 按应用初始选项 | ❌ Weasel/Squirrel 有、其余无 | core 统一加载 + 焦点时机应用 |
| U4 | 跨重启记忆（开关持久化） | ❌ 四端都没有 | core 写 user.yaml，免费获得 |
| U5 | 候选窗行为统一（hover 语义、翻页、高亮跟随） | ❌ 框架写死 | L2 自绘候选窗才彻底；短期接受框架差异 |
| U6 | 样式/主题统一 | 半：weasel.yaml 样式通道 v4 已有 | 泛化为跨端样式源（tokens.yaml → 各端渲染） |
| U7 | 配置分发 | ❌ 两台机器手工同步 | config-center（见 §5） |

**原则**：外壳只做三件事——按键/事件转发、候选窗渲染、会话生命周期挂钩。
凡「这个行为该不该这样」的答案都不许写在外壳里。

---

## 3. ABI 演进：v5 → v6

现有 v5 已覆盖热路径与握手（heng_hello / heng_process_key_ex / config API 直通）。
路线 P 需要的增量（全部走增量追加，沿用 data_size 惯例）：

```
v6（已实现，2026-10-04）：
heng_set_propagation_policy(policy)                // 0=per_session 1=per_app 2=global
heng_get_propagation_policy()                      // 读当前策略
行为增强（签名不变）：
heng_set_option          → 按策略广播 + 白名单选项持久化（<user>/heng_options.yaml）
heng_start_session       → 自动应用持久化选项初值
heng_set_session_owner   → 自动应用 heng.yaml 的 app_options/<app>/（覆盖持久化值）
配置源：config-center/shared/heng.yaml（propagation/policy + app_options）
```

---

## 4. 各端外壳：现状与剩余工作

| 端 | 宿主 | 现状 | 剩余 |
|---|---|---|---|
| **Linux** | fcitx5 addon | ✅ **试用版已通**（本机打字验证，v5 热路径） | U2-U4 跟进；候选窗 L2（Slint + override-redirect，M0.5 已验证可行）；悬浮歧义在 L2 中自然消失 |
| **Windows** | weasel 0.17.4 改造 | ✅ 深度改造完成（数据源换血 + v4 样式 + app_options 通道，真机验证） | U2-U4 跟进；heng.h 同步 v5/v6；L2 候选窗（SW_SHOWNA + WS_EX_NOACTIVATE 已验证） |
| **Android** | 自研 IMS | ❌ 未开始 | Kotlin + JNI（libheng_core.so 交叉编译 aarch64）；参考 Trime 的 IMS 骨架但**数据源换血**；SetSessionOwner 触发时机是主要未知（Trime 用键盘粒度代偿） |
| **macOS** | Squirrel 改造 | ❌ 未开始 | IMK 前端数据源 → heng-core（对齐 weasel 改造模式）；NSPanel 候选窗（M0.5 遗留验证项） |
| **鸿蒙** | 自研 IME Kit | ❌ 未开始 | ArkTS + NAPI；交叉编译 librime 是前置硬活；排在最后 |

**参考实现锚点**：Linux fcitx5 外壳（`shells/linux/fcitx5/`，~400 行）是第一个完整
走通「转发 + 渲染 + 会话挂钩」的薄壳，Android/macOS 外壳按它对齐接口划分。

---

## 5. 里程碑（重排，替代 ARCHITECTURE.md 原 M1-M8 顺序）

| 里程碑 | 内容 | 依赖 | 备注 |
|---|---|---|---|
| **M-P0** | core 行为统一层：传播策略 + app_options 统一加载 + 开关持久化；ABI v6 定稿；config-center MVP（shared/ YAML + 分发脚本，消掉两台机器手工同步） | 无 | 配置中心提前进来：今天的漂移是现成用例 |
| **M-P1** | Linux 外壳完结：跟进 U2-U4；Slint override-redirect 候选窗 PoC（渲染贴到无 WM 干预窗口 + 点击命中） | M-P0 | 产出**标准外壳规范**（转发/渲染/生命周期挂钩的接口划分） |
| **M-P2** | Windows 外壳收尾：v6 ABI 同步、U2-U4、L2 候选窗替换 classicui 式外置窗 | M-P0/P1 | weasel 改造已深，收尾即可 |
| **M-P3** | Android 外壳：aarch64 交叉编译 + IMS 骨架 + JNI | M-P1 规范 | 路线图原 M1，含触屏键盘形态决策 |
| **M-P4** | macOS 外壳：Squirrel 数据源换血 + NSPanel | M-P1 规范 | 需要 macOS 机器 |
| **M-P5** | 鸿蒙外壳：librime 交叉编译 + NAPI + IME Kit | M-P3 | 最重的一端，放最后 |
| **M-P6** | settings-web（设置界面）：读写 U2-U4 全部配置 + 主题选择 | M-P0 | HTTP/token 已就绪 |
| **M-P7** | 词库同步 + AI 预测（原 M3/M8，不变） | 无强依赖 | |

**L2（自绘候选窗）的策略**：按端分阶段，不做一刀切。Linux 在 M-P1 解决（今天已
实证必要性），Windows 在 M-P2（shim 已验证），macOS 在 M-P4（NSPanel 待验）。
各端未上 L2 前，先用宿主主题过渡（Linux 的 heng_blue 主题即此模式）。

---

## 6. 已知风险与开放问题

1. **Slint 嵌入 override-redirect 的工程化**：✅ **已解除**（2026-10-04）——
   PoC 打穿完整链路：软件渲染 + override-redirect 窗口 + 鼠标事件 + 焦点不抢，
   三项真机验证全 PASS（`docs/M-P1-slint-or-probe.md`）。悬浮歧义在此路线下
   天然消失。剩余为工程化收尾（光标跟随、DirtyRegion 局部刷新、与 addon 集成）。
2. **Android 焦点事件流**：IMS 没有现成的「应用切换」事件，SetSessionOwner 的
   触发时机需要试验（InputConnection 生命周期 / EditorInfo 兜底）。
3. **fcitx5 补丁的去留**：悬浮高亮补丁暂缓（用户已接受现状）；若做，走
   `patches/fcitx5-*.patch` 固化，M-P1 的 L2 候选窗上线后自然作废。
4. **每端一个宿主版本锚定**：weasel 0.17.4 / fcitx5 5.1.7（Ubuntu 24.04）/ Trime /
   Squirrel / 鸿蒙 IME Kit——升级跟随策略：只锚定、不追新，宿主升级需回归
   abitest + 真机冒烟。
5. **本机 librime 1.10 vs 目标 1.17**：Linux 试用环境用发行版包（已验证 data_size
   兼容）；发布/打包时统一为自编译 1.17。

---

## 7. 试验报告引用

- 薄壳化试验：`docs/M0.5-thin-shell-experiment.md`（Windows 真机验证）
- Slint 窗口属性：`docs/M0.5-slint-window-probe.md`（Windows）+ `M0.5-slint-window-probe-linux.md`（Linux）
- 前端行为差异：`docs/M0.5-frontend-diff.md`（含 file:line 证据）
