#!/usr/bin/env bash
# config-center 分发脚本（M-P0 MVP）：把共享配置与 RIME 补丁同步到本机运行时。
# 用法：./config-center/deploy.sh
# 生效：fcitx5 -r（或重启 heng-server / heng-cli 首次调用时自动部署）

set -e
cd "$(dirname "$0")/.."
mkdir -p runtime/rime-shared

cp -v patches/rime-default.yaml     runtime/rime-shared/default.yaml
cp -v patches/rime-weasel.yaml      runtime/rime-shared/weasel.yaml
cp -v patches/rime-ice.schema.yaml  runtime/rime-shared/rime_ice.schema.yaml
cp -v config-center/shared/heng.yaml runtime/rime-shared/heng.yaml

echo "配置已同步到 runtime/rime-shared/（重载输入法生效）"
