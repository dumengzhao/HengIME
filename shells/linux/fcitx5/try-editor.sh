#!/usr/bin/env bash
# 打开接入 fcitx5 的测试编辑器（不影响系统默认的 ibus，只对这个进程生效）
cd "$(dirname "$0")"

# 若 fcitx5 未运行则先启动
./start-fcitx5.sh

# GTK_IM_MODULE=fcitx 让这个编辑器把按键交给 fcitx5（而非系统默认 ibus）
GTK_IM_MODULE=fcitx QT_IM_MODULE=fcitx XMODIFIERS=@im=fcitx gnome-text-editor "$@" &
disown
echo "编辑器已打开：点进文本区 → Ctrl+Space 切换到「衡」→ 输入 nihao 看候选"
