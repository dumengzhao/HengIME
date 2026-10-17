#!/usr/bin/env bash
# HengIME fcitx5 开发版启动脚本（用户级安装，无 sudo）
#
# 只加载**衡Dev**：插件目录指向 ~/.local/lib/fcitx5-dev（只放 libhengdev.so），
# 不含 ~/.local/lib/fcitx5 —— 正式版装在系统级 /usr/lib/x86_64-linux-gnu/fcitx5，
# 那是 fcitx5 的默认插件路径、开发模式也要用（classicui 等基础插件在那），
# 故正式版与开发版可共存而不互相覆盖。
# 用法：
#   ./start-fcitx5.sh        启动/重启带 hengdev 插件的 fcitx5
#   ./try-editor.sh          打开一个接入 fcitx5 的文本编辑器
set -e
cd "$(dirname "$0")"

export FCITX_ADDON_DIRS="$HOME/.local/lib/fcitx5-dev:/usr/lib/x86_64-linux-gnu/fcitx5"
export RUST_BACKTRACE=1

# X 环境：从任意 shell 启动时自动补齐（GNOME Wayland 的 XWayland 授权文件
# 每次开机随机后缀，必须实时探测）。缺失时 fcitx5 与自绘候选窗都无法连 X。
if [ -z "$DISPLAY" ]; then
    export DISPLAY=:0
fi
if [ -z "$XAUTHORITY" ]; then
    AUTH_FILE=$(ls /run/user/$(id -u)/.mutter-Xwaylandauth.* 2>/dev/null | head -1)
    if [ -n "$AUTH_FILE" ]; then
        export XAUTHORITY="$AUTH_FILE"
    elif [ -f "$HOME/.Xauthority" ]; then
        export XAUTHORITY="$HOME/.Xauthority"
    fi
fi

# 已在运行时必须核对插件目录：曾两次遇到「systemd 抢拉一个不带
# FCITX_ADDON_DIRS 的 fcitx5」，脚本的 pgrep 守卫判定"已在运行"而跳过，
# 结果落在一个 heng 插件根本没加载的实例上（fcitx5-remote -n 仍返回
# hengdev —— 那是配置里的名字，不代表插件可用）。
if RUNNING=$(pgrep -x fcitx5) && tr '\0' '\n' < "/proc/$RUNNING/environ" 2>/dev/null \
        | grep -q 'FCITX_ADDON_DIRS=.*fcitx5-dev'; then
    echo "fcitx5 已在运行（PID $RUNNING，插件目录含 fcitx5-dev）"
else
    [ -n "${RUNNING:-}" ] && { pkill -x fcitx5; sleep 2; }
    fcitx5 -d 2>/tmp/fcitx5-heng.log
    sleep 1
    echo "fcitx5 已启动（日志：/tmp/fcitx5-heng.log）"
fi
echo "当前输入法：$(fcitx5-remote -n 2>/dev/null || echo '（无焦点）')"
echo "切换到衡Dev： fcitx5-remote -s hengdev   或 Ctrl+Space"
