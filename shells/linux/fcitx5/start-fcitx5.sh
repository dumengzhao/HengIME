#!/usr/bin/env bash
# HengIME fcitx5 试用启动脚本（用户级安装，无 sudo）
# 用法：
#   ./start-fcitx5.sh        启动/重启带 heng 插件的 fcitx5
#   ./try-editor.sh          打开一个接入 fcitx5 的文本编辑器
set -e
cd "$(dirname "$0")"

export FCITX_ADDON_DIRS="$HOME/.local/lib/fcitx5:/usr/lib/x86_64-linux-gnu/fcitx5"

if ! pgrep -x fcitx5 > /dev/null; then
    fcitx5 -d 2>/tmp/fcitx5-heng.log
    sleep 1
    echo "fcitx5 已启动（日志：/tmp/fcitx5-heng.log）"
else
    echo "fcitx5 已在运行"
fi
echo "当前输入法：$(fcitx5-remote -n 2>/dev/null || echo '（无焦点）')"
echo "切换到衡： fcitx5-remote -s heng   或 Ctrl+Space"
