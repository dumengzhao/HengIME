#!/bin/bash
# Squirrel 测试版部署：从 Debug 构建产物生成独立身份的测试版，
# 安装到 ~/Library/Input Methods/SquirrelDev.app（用户目录，免 sudo，
# 与 /Library 下的正式版完全隔离）。
#
# 身份差异：
#   Bundle ID / 输入源 ID: im.heng.ime.Squirrel.dev(.Hans/.Hant)
#   IMK 连接名:            SquirrelDev_Connection
#   rime 用户目录:         ~/Library/Rime-dev（Info.plist HengRimeUserDirName）
#   显示名:                衡Dev / Heng Dev（三语言 InfoPlist.strings 品牌化）
#
# 注意：必须在 /tmp staging 内完成全部修改并重签名后，
# 再以 rm -rf + cp -R 整包替换安装（app 内单文件原地修改会被 TCC 拦截）。
set -e
APP_SRC="${APP_SRC:-/Users/dmz/Library/Developer/Xcode/DerivedData/Squirrel-gbsdexgrlbwdqcdfplevxdnxqeis/Build/Products/Debug/Squirrel.app}"
APP_DST="$HOME/Library/Input Methods/SquirrelDev.app"
STAGING="/tmp/SquirrelDev-stage.app"
PB=/usr/libexec/PlistBuddy

# 1. /tmp staging: copy + re-identity
rm -rf "$STAGING"
cp -R "$APP_SRC" "$STAGING"
P="$STAGING/Contents/Info.plist"
plutil -convert xml1 "$P"
/usr/bin/perl -pi -e 's/im\.rime\.inputmethod\.Squirrel/im.heng.ime.Squirrel.dev/g' "$P"
$PB -c "Set :InputMethodConnectionName SquirrelDev_Connection" "$P"
$PB -c "Set :CFBundleName SquirrelDev" "$P"
$PB -c "Add :HengRimeUserDirName string Rime-dev" "$P" 2>/dev/null || $PB -c "Set :HengRimeUserDirName Rime-dev" "$P"
plutil -lint "$P"

# 1.5 InfoPlist.strings: align mode-ID keys with new bundle ID + unified brand name
for lp in zh-Hans zh-Hant en; do
  F="$STAGING/Contents/Resources/$lp.lproj/InfoPlist.strings"
  plutil -convert xml1 "$F"
  /usr/bin/perl -pi -e 's/im\.rime\.inputmethod\.Squirrel/im.heng.ime.Squirrel.dev/g' "$F"
  if [ "$lp" = "en" ]; then NAME="Heng Dev"; else NAME="衡Dev"; fi
  $PB -c "Set :CFBundleDisplayName $NAME" "$F"
  $PB -c "Set :CFBundleName $NAME" "$F"
  $PB -c "Set :im.heng.ime.Squirrel.dev $NAME" "$F"
  $PB -c "Set :im.heng.ime.Squirrel.dev.Hans $NAME" "$F"
  $PB -c "Set :im.heng.ime.Squirrel.dev.Hant $NAME" "$F"
  plutil -lint "$F"
done
echo "strings ok"

# dev icon (gray, distinct from prod blue; rm before cp to bypass file proxy)
DEV_ICNS="${HENG_DEV_ICNS:-$(dirname "$0")/icons/HengDev.icns}"
if [ -f "$DEV_ICNS" ]; then
  rm -f "$STAGING/Contents/Resources/Rime.icns"
  cp "$DEV_ICNS" "$STAGING/Contents/Resources/Rime.icns"
fi

codesign --force --deep -s - "$STAGING" 2>/dev/null
echo "staging ok"

# 2. stop old + remove old + install fresh
"$APP_DST/Contents/MacOS/Squirrel" --quit 2>/dev/null || true
pkill -f SquirrelDev 2>/dev/null || true
sleep 1
rm -rf "$APP_DST" && echo "old removed"
cp -R "$STAGING" "$APP_DST" && echo "installed"

# 3. register + enable + select
"$APP_DST/Contents/MacOS/Squirrel" --register-input-source
"$APP_DST/Contents/MacOS/Squirrel" --enable-input-source Hans
"$APP_DST/Contents/MacOS/Squirrel" --select-input-source Hans || true
echo "✅ SquirrelDev ready"
