# M-P1：Slint override-redirect 候选窗 PoC 报告

> 试验工程：`experiments/slint-or-probe/`（独立 workspace，Slint 1.18 软件渲染器 + x11rb，无 winit）。
> 运行：`cd experiments/slint-or-probe && DISPLAY=:0 ./target/debug/slint-or-probe`（自终端启动）。
> 日期：2026-10-04，GNOME Wayland + XWayland（Ubuntu 24.04）。

## 1. 验证目标

`docs/ROUTE-P.md` §6 风险 1：M0.5 只验证了窗口属性，本 PoC 打穿
「Slint 渲染 → 自建无 WM 干预 X 窗口」完整链路。

## 2. 实现要点

| 项 | 方案 |
|---|---|
| 渲染 | `slint::platform::set_platform` 自定义 Platform + `MinimalSoftwareWindow`；`SoftwareRenderer::render` 写入 3 字节 RGB 缓冲 |
| 窗口 | x11rb `create_window` + `override_redirect=1`（绕过 WM）；24bpp ZPixmap 全量 `put_image` |
| 事件 | X 事件（Motion/ButtonPress/ButtonRelease/Leave）→ `window().dispatch_event(WindowEvent::*)` |
| 主循环 | 轮询 X → `update_timers_and_animations` → `draw_if_needed` → blit，8ms 一帧 |
| 事件 | 字体经 slint `software-renderer-systemfonts` feature（系统 fontconfig） |

## 3. 结果（2026-10-04 真机验证）

| 验证点 | 结论 | 证据 |
|---|---|---|
| P1 窗口属性 | **PASS** | override-redirect 窗口正常显示 Slint 内容（xwd 采样：白底 `0xffffff`、选中块 `0x2164f1` 均正确）；浮于一切窗口之上、无任务栏条目、无 WM 干预 |
| P2 事件链路 | **PASS** | 鼠标悬浮/点击均到达 Slint（TouchArea clicked 回调触发）；悬浮高亮语义由外壳自定义——悬浮只显示细蓝边框提示，实心选中块不跟随（对比 classicui 硬编码 `hoverIndex_` 优先的行为） |
| P3 焦点不抢 | **PASS** | 点击候选前后 `GetInputFocus` 一致（日志打印 `焦点未变=true`） |

## 4. 结论

**L2 路线在 Linux 的工程化链路已打穿，风险 1 解除。** Slint 自绘候选窗在
XWayland 上完全可行，且悬浮歧义（试用第一天撞到的 classicui 天花板）在
此路线下天然消失——高亮语义只有一个来源（引擎高亮），hover 表现由外壳定义。

## 5. PoC → 生产（shells/linux 后续工作）

1. 候选窗数据源接 heng-core：preedit/candidates/highlight 来自
   `heng_process_key_ex` 返回的 context，点击回调接
   `heng_select_candidate_on_current_page`
2. 窗口定位：跟随输入光标（需要 fcitx5 上报光标位置）
3. DirtyRegion 局部 `put_image`（PoC 全量 blit，性能足够但可优化）
4. 与 fcitx5 addon 集成方式：addon 保留按键转发与会话管理，InputPanel 候选
   部分改由自绘窗接管（关闭 classicui 候选区或用空 panel 占位）
5. 按需隐藏/显示（无组合串时 unmap）
