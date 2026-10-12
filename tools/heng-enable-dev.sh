#!/bin/bash
# 把 dev 版输入法写进 macOS 26 的第三方输入源注册表并激活。
#
# 为什么要这个脚本：
#   macOS 26 起，第三方输入法「是否启用」的真正来源是
#   ~/Library/Preferences/com.apple.inputsources.plist 的
#   AppleEnabledThirdPartyInputSources（WeType 就在里面）。
#   com.apple.HIToolbox.plist 的 AppleEnabledInputSources 已经只是遗留数据，
#   系统不再据此渲染系统设置里的输入法列表。
#
# 为什么不能在 WorkBuddy 沙箱里跑：
#   WorkBuddy 的文件代理会拦截对 ~/Library/Preferences/com.apple.* 前缀
#   偏好域的一切内容覆盖（plutil / PlistBuddy / defaults / Python 全部
#   Operation not permitted）。在你自己的 Terminal 里没有这层代理，可以正常写。
#
# 用法：在你的 Terminal 里执行
#   bash /Volumes/ssd/DMZ/Work/HengIME/tools/heng-enable-dev.sh
set -e
DEV_ID="im.heng.inputmethod.Squirrel.dev"
P="$HOME/Library/Preferences/com.apple.inputsources.plist"

[ -f "$P" ] || { echo "找不到 $P"; exit 1; }
cp -n "$P" "$P.hengbak" 2>/dev/null || true
echo "== 0. 备份 -> $P.hengbak =="

echo "== 1. 停 cfprefsd（否则内存缓存会覆盖文件写入）=="
killall cfprefsd 2>/dev/null || true
sleep 1

echo "== 2. 写入 AppleEnabledThirdPartyInputSources =="
/usr/bin/python3 - "$DEV_ID" "$P" <<'PY'
import plistlib, sys, os
dev, p = sys.argv[1], sys.argv[2]
with open(p, 'rb') as f:
    pl = plistlib.load(f)
arr = pl.get('AppleEnabledThirdPartyInputSources', [])
have = {(d.get('Bundle ID'), d.get('InputSourceKind'), d.get('Input Mode')) for d in arr}
want = [
    {'Bundle ID': dev, 'InputSourceKind': 'Keyboard Input Method'},
    {'Bundle ID': dev, 'Input Mode': dev + '.Hans', 'InputSourceKind': 'Input Mode'},
    {'Bundle ID': dev, 'Input Mode': dev + '.Hant', 'InputSourceKind': 'Input Mode'},
]
added = 0
for d in want:
    key = (d['Bundle ID'], d['InputSourceKind'], d.get('Input Mode'))
    if key in have:
        print(f"  = {d['Bundle ID']} / {d['InputSourceKind']} / {d.get('Input Mode','-')} 已存在")
        continue
    arr.append(d)
    added += 1
    print(f"  + {d['Bundle ID']} / {d['InputSourceKind']} / {d.get('Input Mode','-')}")
pl['AppleEnabledThirdPartyInputSources'] = arr
with open(p, 'wb') as f:
    plistlib.dump(f=pl, fmt=plistlib.FMT_BINARY) if False else plistlib.dump(pl, f)
print(f"  共新增 {added} 条")
PY

echo "== 3. 回读校验 =="
/usr/bin/defaults read com.apple.inputsources AppleEnabledThirdPartyInputSources \
  | /usr/bin/grep "Bundle ID" | sed 's/^/    /'

echo "== 4. 让输入子系统重新加载 =="
killall TextInputMenuAgent 2>/dev/null || true
killall cfprefsd 2>/dev/null || true
sleep 2

echo "== 5. 再开一次注册（防止上面重启进程把注册冲掉）=="
"/Users/dmz/Library/Input Methods/SquirrelDev.app/Contents/MacOS/Squirrel" \
  --register-input-source 2>/dev/null || true

echo
echo "✅ 完成。现在打开 系统设置 → 键盘 → 输入法，看「衡Dev」是否出现。"
echo "   如果还没有：注销并重新登录一次（会强制 TIS 全量重扫输入法目录）。"
