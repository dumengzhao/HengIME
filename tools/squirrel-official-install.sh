#!/bin/bash
# 正式版安装：从 Debug 构建产物生成带品牌化显示名（衡/Heng）与正确 entitlements 的
# 正式版，整包替换 /Library/Input Methods/Squirrel.app。
#
# 与测试版（tools/squirrel-dev-install.sh）的差异：不改 bundle ID / 连接名 / 词库目录，
# 只做显示名品牌化 + 重签 + 整包替换。
#
# 用法（替换 /Library 需要管理员权限）：
#   sudo bash tools/squirrel-official-install.sh
set -e
APP_SRC="${APP_SRC:-/Users/dmz/Library/Developer/Xcode/DerivedData/Squirrel-gbsdexgrlbwdqcdfplevxdnxqeis/Build/Products/Debug/Squirrel.app}"
APP_DST="/Library/Input Methods/Squirrel.app"
STAGING="/tmp/Squirrel-brand-stage.app"
ENTITLEMENTS="${HENG_ENTITLEMENTS:-/tmp/squirrel.entitlements}"
[ -f "$ENTITLEMENTS" ] || ENTITLEMENTS="$(dirname "$0")/../third_party/src/squirrel/resources/Squirrel.entitlements"
PB=/usr/libexec/PlistBuddy

LSR=/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister

# 注销某路径在 LaunchServices 里的登记
lsr_forget() {
  [ -n "$1" ] || return 0
  "$LSR" -u "$1" 2>/dev/null || true
  case "$1" in
    /tmp/*) "$LSR" -u "/private$1" 2>/dev/null || true ;;
    /private/tmp/*) "$LSR" -u "${1#/private}" 2>/dev/null || true ;;
  esac
}

# 保证某 bundle ID 在 LaunchServices 里只剩 $2 这一条记录：
# 注销所有「ID 相同但路径不是 $2」的记录。同 ID 多记录会让系统解析产生歧义，
# 症状是「注册上了、输入源列表里能看到，过一会儿自己消失」甚至整个不见了。
lsr_dedup_id() {    # $1 = bundle ID   $2 = 要保留的路径
  "$LSR" -dump 2>/dev/null | /usr/bin/awk -v want="$1" -v keep="$2" '
    /^bundle id:[[:space:]]/ { cur="" }
    /^path:[[:space:]]/ { match($0,/^path:[[:space:]]+/); p=substr($0,RLENGTH+1);
      if ((i=index(p," (0x"))>0) p=substr(p,1,i-1); cur=p }
    /^identifier:[[:space:]]/ { match($0,/^identifier:[[:space:]]+/); id=substr($0,RLENGTH+1);
      sub(/[[:space:]]+$/,"",id);
      if (id == want && cur != "" && cur != keep) print cur }
  ' | sort -u | while IFS= read -r p; do
      "$LSR" -u "$p" 2>/dev/null || true
      echo "  LS 去重: 注销同 ID 的旁支记录 $p"
    done
}

[ -d "$APP_SRC" ] || { echo "构建产物不存在: $APP_SRC"; exit 1; }
[ "$(id -u)" = "0" ] || { echo "请用 sudo 运行（需写 /Library/Input Methods）"; exit 1; }

# 1. /tmp staging：复制 + 显示名品牌化
rm -rf "$STAGING"
cp -R "$APP_SRC" "$STAGING"
for lp in zh-Hans zh-Hant en; do
  F="$STAGING/Contents/Resources/$lp.lproj/InfoPlist.strings"
  if [ "$lp" = "en" ]; then NAME="Heng"; else NAME="衡"; fi
  $PB -c "Set :CFBundleDisplayName $NAME" -c "Set :CFBundleName $NAME" \
      -c "Set :im.rime.inputmethod.Squirrel $NAME" \
      -c "Set :im.rime.inputmethod.Squirrel.Hans $NAME" \
      -c "Set :im.rime.inputmethod.Squirrel.Hant $NAME" "$F"
  plutil -lint "$F"
done

# 2. 重签：按 Xcode 顺序（嵌套先签、顶层后签），不用 --deep
#    --deep 会破坏嵌套签名链，IMKServer 校验输入法 app 时可能因此拒绝激活。
FW="$STAGING/Contents/Frameworks"
for d in "$FW"/*.dylib; do
  [ -e "$d" ] && codesign --force --sign - "$d" 2>/dev/null
done
for f in "$FW"/*.framework; do
  [ -e "$f" ] || continue
  for inner in "$f"/Versions/*/; do
    [ -d "$inner" ] || continue
    for lib in "$inner"*.dylib; do
      [ -e "$lib" ] && codesign --force --sign - "$lib" 2>/dev/null
    done
  done
  codesign --force --sign - "$f" 2>/dev/null
done
for exe in "$STAGING"/Contents/MacOS/*; do
  [ -f "$exe" ] && codesign --force --sign - "$exe" 2>/dev/null
done
codesign --force --sign - --entitlements "$ENTITLEMENTS" "$STAGING"
codesign -v --strict "$STAGING" || { echo "签名校验失败"; exit 1; }
codesign -dv --entitlements - "$STAGING" 2>&1 | grep -q "disable-library-validation" \
  && echo "✅ entitlements 已带入" || { echo "❌ entitlements 缺失"; exit 1; }

# 3. 整包替换（app 内单文件原地修改会被 TCC 拦，整包 rm+cp 可行）
"$APP_DST/Contents/MacOS/Squirrel" --quit 2>/dev/null || true
pkill -f "MacOS/Squirrel" 2>/dev/null || true
sleep 1
rm -rf "$APP_DST"
cp -R "$STAGING" "$APP_DST"
# LaunchServices 去重：确保 im.rime.inputmethod.Squirrel 只剩 /Library 这一条
lsr_forget "$STAGING"
lsr_dedup_id "im.rime.inputmethod.Squirrel" "$APP_DST"

# 4. 重新注册输入源
"$APP_DST/Contents/MacOS/Squirrel" --register-input-source
"$APP_DST/Contents/MacOS/Squirrel" --enable-input-source Hans || true

# 5. 修正 AppleEnabledInputSources
#    TISEnableInputSource 对 Input Mode 类型只改会话内存视图，不落盘；
#    Hans/Hant 必须以 "Input Mode" 条目存在于 AppleEnabledInputSources 才生效
#    （对比系统拼音：Bundle ID=SCIM / Input Mode=SCIM.ITABC）。
#    macOS 27 上系统设置「添加」可能写成幽灵条目：UI 显示已添加、减号灰化，但 plist 无记录。
TARGET_USER="${SUDO_USER:-$(stat -f%Su /dev/console)}"
HOME_DIR=$(dscl . -read "/Users/$TARGET_USER" NFSHomeDirectory | awk '{print $2}')
P="$HOME_DIR/Library/Preferences/com.apple.HIToolbox.plist"
echo "== 修正 AppleEnabledInputSources (user=$TARGET_USER) =="
[ -f "$P" ] && cp -n "$P" "$P.hengbak" 2>/dev/null
# cfprefsd 的内存缓存会覆盖文件写入，先停掉
sudo -u "$TARGET_USER" killall cfprefsd 2>/dev/null
sleep 1
for MODE in Hans Hant; do
  if $PB -c "Print :AppleEnabledInputSources" "$P" 2>/dev/null | grep -q "Squirrel\.$MODE"; then
    echo "  = $MODE 已存在，跳过"; continue
  fi
  N=$($PB -c "Print :AppleEnabledInputSources" "$P" 2>/dev/null | grep -c "InputSourceKind")
  $PB -c "Add :AppleEnabledInputSources:$N dict" "$P"
  $PB -c "Add :AppleEnabledInputSources:$N:Bundle ID string im.rime.inputmethod.Squirrel" "$P"
  $PB -c "Add :AppleEnabledInputSources:$N:Input Mode string im.rime.inputmethod.Squirrel.$MODE" "$P"
  $PB -c "Add :AppleEnabledInputSources:$N:InputSourceKind string Input Mode" "$P"
  echo "  + 已写入 $MODE (index $N)"
done
$PB -c "Print :AppleEnabledInputSources" "$P"
# 让会话重新加载输入源列表
sudo -u "$TARGET_USER" killall cfprefsd 2>/dev/null
sudo -u "$TARGET_USER" killall TextInputMenuAgent 2>/dev/null

echo "✅ 正式版已安装: $APP_DST"
echo "   系统设置 → 键盘 → 输入法 应出现「衡」；菜单栏可用 Ctrl+Space 切到「衡」"
