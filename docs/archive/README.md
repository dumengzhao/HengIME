# 归档文档

本目录收录已完成的决策记录与一次性试验报告。相关结论已并入生产代码，
文档保留作为依据，其中的状态描述停留在写作时点。

现行文档：

| 主题 | 文档 |
|---|---|
| 系统当前结构 | [../ARCHITECTURE.md](../ARCHITECTURE.md) |
| 功能范围与进度 | [../FEATURES.md](../FEATURES.md) |
| 设置窗口 | [../SETTINGS-PLAN.md](../SETTINGS-PLAN.md) |

## 归档清单

| 文档 | 主题 | 结论落点 |
|---|---|---|
| `ROUTE-P.md` | 决策门裁决（core 前置路线） | ARCHITECTURE §2 / §3.2 |
| `M0.5-thin-shell-experiment.md` | Weasel 薄壳化可行性试验 | ARCHITECTURE §2.1（Windows 外壳结构） |
| `M0.5-slint-window-probe.md` | Slint 窗口属性（Windows：置顶、不激活） | ARCHITECTURE §3.1 |
| `M0.5-slint-window-probe-linux.md` | 同上，Linux/X11 | ARCHITECTURE §3.1 |
| `M0.5-frontend-diff.md` | 三前端行为差异清单 | ARCHITECTURE §3.2（行为统一层） |
| `PLAN-candidate-window.md` | 候选窗工程项（D1-D4 + CW 系列） | ARCHITECTURE §4、§10 |
| `M-P1-slint-or-probe.md` | override-redirect 窗口 PoC | ARCHITECTURE §3.1、§10 |

## 未完成项

`PLAN-candidate-window.md` 的 D3（事件驱动）、DirtyRegion 局部刷新、UI 线程重建
三项尚未实现，记录在 ARCHITECTURE §10「已知局限」。

`ROUTE-P.md` 的 M-P0～M-P7 编号体系与 `README.md` 的 M0～M8 不是同一套，
两套里程碑均已完成大半，排期时以 `README.md` 为准。