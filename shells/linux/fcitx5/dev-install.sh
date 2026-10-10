#!/usr/bin/env bash
# dev 版并行安装：hengdev 插件（用户级），与已安装的正式版 heng 并存互不影响。
# - 用户数据隔离：runtime-dev/rime-user（dev 打字/配置不污染正式版词库）
# - fcitx5 中出现两个输入法：衡（正式）+ 衡Dev（开发），Ctrl+Space 独立切换
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

mkdir -p ~/.local/lib/fcitx5 \
         ~/.local/share/fcitx5/addon \
         ~/.local/share/fcitx5/inputmethod
cp build-dev/libhengdev.so ~/.local/lib/fcitx5/
cp conf-dev/addon.conf ~/.local/share/fcitx5/addon/hengdev.conf
cp conf-dev/im.conf ~/.local/share/fcitx5/inputmethod/hengdev.conf

pkill -x fcitx5 2>/dev/null || true
sleep 1
if [ -x ./start-fcitx5.sh ]; then ./start-fcitx5.sh; else fcitx5 -d 2>/tmp/fcitx5-heng.log; fi
echo "已安装 衡Dev（hengdev，用户级）。"
echo "添加输入法：fcitx5-configtool → 输入法 → 搜索「衡Dev」"
echo "与正式版「衡」并存：Ctrl+Space / Win+Space 在两者间独立切换。"
