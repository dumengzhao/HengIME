#!/usr/bin/env bash
# 为 CI 裁剪 default.yaml 的 schema_list，只留 rime_ice。
#
# 与 make_ci_default.py 等价，但不依赖 python —— Windows Git Bash 里的
# `python` 可能是 Microsoft Store 的占位符（存在但执行即失败），
# runner 上也没有 unzip 可用。
#
# 为什么需要裁剪：schema_list 列了 9 个方案（rime_ice + t9 + 7 个双拼），
# 但只有 rime_ice 的 schema 文件随 full.zip 到位。librime 部署时遇到缺失的
# schema 会以 deploy: failure 收场，之后所有分词与 config 断言一起失败。
#
# 做法：把除 rime_ice 外的 `  - schema: xxx` 行整行注释掉，保留可逆性。
# 已经是注释行的（以 # 开头）不动——重复跑不会加第二层 #。
#
# 用法：make_ci_default.sh <输入> <输出>
set -euo pipefail

if [ $# -ne 2 ]; then
  echo "用法: $0 <输入> <输出>" >&2
  exit 1
fi

src="$1"
dst="$2"
tmp="$dst.tmp.$$"

# 只处理行首恰好两个空格 + "- schema: " 的条目，且只注释标识不是 rime_ice 的。
# 用 awk 而非 sed -E：sed 没有条件替换，只能全注释或全不注释。
#
# 行尾必须原样保留。full.zip 里的 default.yaml 是 CRLF，awk 在 Windows
# 的 Git Bash 下会输出 LF，改行尾等于把上游文件的格式换掉。
# 做法是去掉 awk 输出里的 \r 再写回——但那会连内容里的 \r 一起动，
# 所以改成：先记下原文件是 CRLF 还是 LF，最后统一补回去。
crlf=no
# grep 的 $'\r' 在 Git Bash 下不可靠，直接看第二个字节是不是 CR。
first_line=$(head -1 "$src" | od -An -tx1 | tr -d ' \n')
case "$first_line" in
  *0d*) crlf=yes ;;
esac

awk '
  { sub(/\r$/, "") }
  /^  - schema: / {
    ident = $3
    if (ident != "rime_ice") {
      print "#" $0
      next
    }
  }
  { print }
' "$src" > "$tmp"

if [ "$crlf" = yes ]; then
  # unix2dos 不一定存在，用 sed 的 \r 追加
  sed -i 's/$/\r/' "$tmp"
fi

if ! grep -qE '^  - schema: rime_ice' "$tmp"; then
  echo "schema_list 一条都没保留，正则多半没匹配上。输入前 5 行：" >&2
  head -5 "$src" >&2
  rm -f "$tmp"
  exit 1
fi

# 已注释掉的条目数，用于确认裁剪真的生效了。
hidden=$(grep -cE '^#  - schema: ' "$tmp" || true)
mv "$tmp" "$dst"
echo "schema_list 保留 rime_ice，注释掉 ${hidden} 条"