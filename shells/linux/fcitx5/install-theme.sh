#!/usr/bin/env bash
# 安装 heng_blue 主题到 fcitx5（用户级）并启用。
# 用法：./install-theme.sh
# 卸载：rm -rf ~/.local/share/fcitx5/themes/heng_blue 后 fcitx5 -r

set -e
THEME_DIR="$HOME/.local/share/fcitx5/themes/heng_blue"
CONF="$HOME/.config/fcitx5/conf/classicui.conf"

mkdir -p "$(dirname "$THEME_DIR")"
cp -r "$(dirname "$0")/theme/heng_blue" "$THEME_DIR"

# classicui.conf 设置 Theme=heng_blue（不存在则创建；存在则替换 Theme= 行，保留其余配置）
if [ -f "$CONF" ]; then
    if grep -q "^Theme=" "$CONF"; then
        sed -i 's/^Theme=.*/Theme=heng_blue/' "$CONF"
    else
        echo "Theme=heng_blue" >> "$CONF"
    fi
else
    mkdir -p "$(dirname "$CONF")"
    cat > "$CONF" << 'EOF'
# 衡 heng_blue 主题（fcitx5 外壳）
Theme=heng_blue
# 横排候选（对齐 Windows 端 style/horizontal: true）
VerticalCandidateList=False
# 字体：Ubuntu 24.04 自带；与 Windows 端 font_point 14 对应
Font="Noto Sans CJK SC 14"
# 每屏 DPI 适配高分屏
PerScreenDPI=True
EOF
fi

echo "heng_blue 主题已安装到 $THEME_DIR"
echo "classicui.conf: $(grep '^Theme=' "$CONF")"
echo "重载 fcitx5 生效：fcitx5 -r"
