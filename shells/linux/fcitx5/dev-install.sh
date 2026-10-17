#!/usr/bin/env bash
# dev 版安装：hengdev 插件（用户级），与系统级正式版 heng 互不覆盖。
# - 插件目录独立：~/.local/lib/fcitx5-dev（只放 libhengdev.so），
#   不碰 ~/.local/lib/fcitx5（正式版用户级安装位）与系统级目录
# - start-fcitx5.sh 只把 fcitx5-dev 加入插件路径 → 开发模式**仅**加载衡Dev
# - 用户数据隔离：runtime-dev/rime-user（dev 打字/配置不污染正式版词库）
# 用法：./dev-install.sh   （改完 core/插件代码后重新跑一遍即可）
set -e
cd "$(dirname "$0")"
REPO_ROOT="$(cd ../../../ && pwd)"

if [ ! -f "$REPO_ROOT/target/release/libheng_core.so" ]; then
    echo "错误：先在仓库根执行 cargo build --release -p heng-core" >&2
    exit 1
fi

cmake -B build-dev -DHENG_DEV=ON -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build build-dev >/dev/null

mkdir -p ~/.local/lib/fcitx5-dev \
         ~/.local/share/fcitx5/addon \
         ~/.local/share/fcitx5/inputmethod
cp build-dev/libhengdev.so ~/.local/lib/fcitx5-dev/
cp conf-dev/addon.conf ~/.local/share/fcitx5/addon/hengdev.conf
cp conf-dev/im.conf ~/.local/share/fcitx5/inputmethod/hengdev.conf

pkill -x fcitx5 2>/dev/null || true
sleep 2
if [ -x ./start-fcitx5.sh ]; then ./start-fcitx5.sh; else fcitx5 -d 2>/tmp/fcitx5-heng.log; fi
echo "已安装 衡Dev（hengdev → ~/.local/lib/fcitx5-dev）。"
echo "添加输入法：fcitx5-configtool → 输入法 → 搜索「衡Dev」"
echo "开发模式只加载衡Dev；正式版「衡」走系统级目录，互不覆盖。"
