#!/bin/bash
# Squirrel 测试版部署：从 Debug 构建产物生成独立身份的测试版，
# 安装到 ~/Library/Input Methods/SquirrelDev.app（用户目录，免 sudo，
# 与 /Library 下的正式版完全隔离）。
#
# 身份差异：
#   Bundle ID / 输入源 ID: im.heng.inputmethod.Squirrel.dev(.Hans/.Hant)
#   IMK 连接名:            im_heng_inputmethod_Squirrel_dev_Connection
#   rime 用户目录:         ~/Library/Rime-dev（Info.plist HengRimeUserDirName）
#   显示名:                衡Dev / Heng Dev（三语言 InfoPlist.strings 品牌化）
#
# 关键约束 1 —— bundle ID 必须含 ".inputmethod." 段：
#   macOS 26/27 系统扫盘时只收录 bundle ID 里带 ".inputmethod." 的第三方输入法。
#   不含该段时 TISRegisterInputSource 仍返回 noErr，但 TIS 列表里永远没有它，
#   系统设置「+」里也看不到 —— 完全静默失败。故 dev 版用
#   im.heng.inputmethod.Squirrel.dev，而不是 im.heng.ime.Squirrel.dev。
#
# 关键约束 2 —— app 级输入法键必须齐：
#   上游 Squirrel 缺 tsInputMethodIconFileKey / tsInputMethodCharacterRepertoireKey，
#   后果是 (a) 输入源列表里 app 级条目没有图标，
#        (b) 系统无法把该输入法归入正确语言分组。
#   WeType 两个键都有。这里按同样方式补上。
#
# 关键约束 3 —— 同一个 bundle ID 在 LaunchServices 里只能有【一条】记录：
#   系统按 bundle ID 解析第三方输入法。只要 LS 里存在第二条同 ID 记录
#   （旧的 staging 副本、挪走的旧包、已删除但记录还在的死路径……），
#   解析就会产生歧义 —— 症状是「注册上了、TIS 里能看到，过一会儿自己消失，
#   甚至直接整个不见了」。所以 staging 用完要 lsregister -u，收尾还要按 ID 全量去重。
#   脚本末尾有自检：该 ID 的记录数必须为 1。
#
# 关键约束 4 —— app 包内的写权限（实测）：
#   新建文件 ✅ / cp 到【新】路径 ✅ / perl rename ✅
#   cp -f 覆盖【已有】文件 ❌ / rm ❌ / mv 覆盖 ❌ / Python open(...,"w") ❌
#   → 覆盖包内已有文件一律「落到 .new 再 perl rename」，见 put_in_bundle()。
#   → 整个旧包【不挪不删】，一律原地 ditto 覆盖（见「关键约束 5」）。
#
# 关键约束 5 —— 【不要给 app 包留备份副本】：
#   本机 `rm` 会把 app 包路由到 `~/.Trash/`，而 LaunchServices 会立刻把废纸篓里
#   那份按【同一个 bundle ID】重新注册。于是 LS 里该 ID 的记录数凭空暴涨
#   —— 实测从 1 跳到 6，再跳到 13；连不带 `.app` 后缀的备份目录
#   （stage-bak.* / stage.*）也照注册不误。
#   症状：输入源在「注册上了 ↔ 自己消失 ↔ 整个不见了」之间反复。
#   → 所以安装一律【原地 ditto 覆盖】，不 mv 备份、不 rm 旧包。
#   → 收尾必须 lsr_dedup_id 兜底，并用 lsregister_count 自检必须为 1。
#
# 已排除、不必再试的伪变量（实测均与收录无关）：
#   * 签名档位（adhoc + spctl rejected 照样收录）、entitlements 少 get-task-allow
#   * 安装位置（~/Library 与 /Library 都会被扫）
#   * com.apple.inputsources 的 AppleEnabledThirdPartyInputSources 第三方白名单
#     —— 曾误判为「不在名单就活不下来」。实测：dev 版不在该名单里，
#     照样扛过 killall TextInputMenuAgent + killall cfprefsd 的完整列表重建。
#     而且该域是系统托管 + com.apple.* 前缀受文件代理保护，根本写不进去。
set -e
# 必须指向本仓 -derivedDataPath build 的产物。曾指向 Xcode DerivedData，
# 那里是陈旧构建 → 「编译成功但装上去的包没变化」，白排查半天。
SQUIRREL_DIR="$(cd "$(dirname "$0")/../third_party/src/squirrel" && pwd)"
APP_SRC="${APP_SRC:-$SQUIRREL_DIR/build/Build/Products/Debug/Squirrel.app}"
APP_DST="$HOME/Library/Input Methods/SquirrelDev.app"
STAGING="/tmp/SquirrelDev-stage.app"
ASIDE="/tmp/heng-uninstalled"
DEV_ID="im.heng.inputmethod.Squirrel.dev"
PB=/usr/libexec/PlistBuddy
PY=/usr/bin/python3
LSR=/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister
mkdir -p "$ASIDE"

# 覆盖 app 包内【已存在】的文件：cp -f 会被文件代理拒绝
# （目标在 _CodeSignature/CodeResources 里），必须先落到同目录的 .new 新文件，
# 再用 perl rename 顶替 —— 同卷 rename 原子且放行。
put_in_bundle() {   # $1 = 源文件（任意位置）   $2 = bundle 内目标路径
  cp -f "$1" "$2.new" || return 1
  /usr/bin/perl -e 'rename($ARGV[0],$ARGV[1]) or die "rename: $!"' "$2.new" "$2"
}

# 注销某路径在 LaunchServices 里的登记（同 ID 多记录的源头就是它）
lsr_forget() {      # $1 = app 路径
  [ -n "$1" ] || return 0
  "$LSR" -u "$1" 2>/dev/null || true
  # /tmp 是 /private/tmp 的软链，两种写法都要注销
  case "$1" in
    /tmp/*) "$LSR" -u "/private$1" 2>/dev/null || true ;;
    /private/tmp/*) "$LSR" -u "${1#/private}" 2>/dev/null || true ;;
  esac
}

# 保证某个 bundle ID 在 LaunchServices 里【只剩 $2 这一条】记录：
# 注销所有「ID 相同但路径不是 $2」的记录 —— 死路径、旧 staging、挪走的旧包、
# .Trash 里的副本都算。历史事故就是这类记录造成的解析歧义。
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

# 某个 bundle ID 在 LaunchServices 里的记录条数（正常必须为 1）
# 某个 bundle ID 在 LaunchServices 里的记录条数（正常必须为 1）
lsregister_count() {   # $1 = bundle ID
  "$LSR" -dump 2>/dev/null \
    | /usr/bin/grep -c -E "^[[:space:]]*identifier:[[:space:]]+$1[[:space:]]*$" || true
}

# 1. staging：原地 ditto 覆盖（不挪旧包、不留备份，见文件头「关键约束 5」）
[ -d "$APP_SRC" ] || { echo "❌找不到构建产物：$APP_SRC"; echo "   先编译：cd third_party/src/squirrel && make debug"; exit 1; }

# core 的 dylib 会被拷进 app 的 Frameworks/。若 core 改过而这里没同步，
# 链接期会报「symbol(s) not found for architecture arm64」——
# 看起来像 Swift 写错，其实是拿到旧的 dylib。改 core 后必须先同步。
CORE_SRC="$(cd "$SQUIRREL_DIR/../../.." && pwd)/target/release/libheng_core.dylib"
CORE_DST="$SQUIRREL_DIR/lib/libheng_core.dylib"
if [ ! -f "$CORE_SRC" ]; then
  echo "❌找不到 core：$CORE_SRC"; echo "   先编译：cargo build --release -p heng-core"; exit 1
fi
# 头文件也要同步：Squirrel 的 bridging header 是 #import <heng.h>，
# 走 HEADER_SEARCH_PATHS 指向 librime/include —— 那是 core/include/heng.h 的**副本**。
# 只同步 dylib 不同步头文件时，新加的 heng_* 函数在 Swift 侧报
# 「cannot find 'xxx' in scope」，而 dylib 里其实已经有实现了，最难查。
HDR_SRC="$(cd "$SQUIRREL_DIR/../../.." && pwd)/core/include/heng.h"
HDR_DST="$SQUIRREL_DIR/librime/include/heng.h"
CORE_TOUCHED=0
if [ -f "$HDR_SRC" ] && [ "$HDR_SRC" -nt "$HDR_DST" ]; then
  echo "  core 头文件有更新，同步 heng.h..."
  cp "$HDR_SRC" "$HDR_DST.new" && /usr/bin/perl -e 'rename($ARGV[0],$ARGV[1]) or die "rename: $!"' "$HDR_DST.new" "$HDR_DST"
  CORE_TOUCHED=1
fi
if [ "$CORE_SRC" -nt "$CORE_DST" ]; then
  echo "  core 有更新，同步 dylib..."
  cp "$CORE_SRC" "$CORE_DST.new" && /usr/bin/perl -e 'rename($ARGV[0],$ARGV[1]) or die "rename: $!"' "$CORE_DST.new" "$CORE_DST"
  CORE_TOUCHED=1
fi
if [ "$CORE_TOUCHED" = "1" ]; then
  echo "  ✅ core 同步完成 —— 请重新编译 Squirrel（xcodebuild），本次只更新了 dylib/头文件"
  exit 2
fi
SRC_BIN="$APP_SRC/Contents/MacOS/Squirrel"
NEWEST_SRC=$(/usr/bin/find "$SQUIRREL_DIR/sources" "$SQUIRREL_DIR/resources" -type f -newer "$SRC_BIN" 2>/dev/null | /usr/bin/head -3)
if [ -n "$NEWEST_SRC" ]; then
  echo "❌ 构建产物比源码旧，安装了也看不到改动。请重新编译："
  /usr/bin/echo "$NEWEST_SRC" | /usr/bin/sed 's/^/     /'
  exit 1
fi
/usr/bin/ditto "$APP_SRC" "$STAGING"
P="$STAGING/Contents/Info.plist"
plutil -convert xml1 "$P"
/usr/bin/perl -pi -e "s/im\\.rime\\.inputmethod\\.Squirrel/$DEV_ID/g" "$P"
$PB -c "Set :InputMethodConnectionName im_heng_inputmethod_Squirrel_dev_Connection" "$P"
$PB -c "Set :CFBundleName SquirrelDev" "$P"
$PB -c "Add :HengRimeUserDirName string Rime-dev" "$P" 2>/dev/null || $PB -c "Set :HengRimeUserDirName Rime-dev" "$P"

# 1.2 app 级输入法键（上游缺，见文件头「关键约束 2」）
$PB -c "Add :tsInputMethodIconFileKey string rime.pdf" "$P" 2>/dev/null \
  || $PB -c "Set :tsInputMethodIconFileKey rime.pdf" "$P"
if ! $PB -c "Print :tsInputMethodCharacterRepertoireKey" "$P" >/dev/null 2>&1; then
  $PB -c "Add :tsInputMethodCharacterRepertoireKey array" "$P"
fi
$PB -c "Add :tsInputMethodCharacterRepertoireKey:0 string Hans" "$P" 2>/dev/null \
  || $PB -c "Set :tsInputMethodCharacterRepertoireKey:0 Hans" "$P"
$PB -c "Add :tsInputMethodCharacterRepertoireKey:1 string Hant" "$P" 2>/dev/null \
  || $PB -c "Set :tsInputMethodCharacterRepertoireKey:1 Hant" "$P"
plutil -lint "$P"

# 1.5 InfoPlist.strings：以输入源 ID 为 key 的名字必须同步到新 bundle ID，
#     否则系统取不到名字，输入源列表里会退化成显示 bundle ID。
for lp in zh-Hans zh-Hant en; do
  F="$STAGING/Contents/Resources/$lp.lproj/InfoPlist.strings"
  plutil -convert xml1 "$F"
  /usr/bin/perl -pi -e "s/im\\.rime\\.inputmethod\\.Squirrel/$DEV_ID/g" "$F"
  if [ "$lp" = "en" ]; then NAME="Heng Dev"; else NAME="衡Dev"; fi
  $PB -c "Set :CFBundleDisplayName $NAME" "$F"
  $PB -c "Set :CFBundleName $NAME" "$F"
  $PB -c "Set :$DEV_ID $NAME" "$F"
  $PB -c "Set :$DEV_ID.Hans $NAME" "$F"
  $PB -c "Set :$DEV_ID.Hant $NAME" "$F"
  plutil -lint "$F"
done
echo "strings ok"

# dev 图标（灰底，区别于正式版蓝底）
DEV_ICNS="${HENG_DEV_ICNS:-$(dirname "$0")/icons/HengDev.icns}"
if [ -f "$DEV_ICNS" ]; then
  put_in_bundle "$DEV_ICNS" "$STAGING/Contents/Resources/Rime.icns" && echo "  Rime.icns ok"
fi

# 菜单栏 / 输入源列表图标 rime.pdf
#   Info.plist 里 tsInputMethodIconFileKey 与三个 per-mode 的
#   tsInputMode*IconFileKey 都指向 rime.pdf，输入源列表与菜单栏显示的都是它，
#   不是 Rime.icns。上游原版是纯黑方块（实测 22x16 渲染全黑），必须替换。
#   用 gen-rime-pdf.m（CoreText 矢量字形「衡」+ 圆角徽章，16x16pt 与 WeType 一致）；
#   gen-rime-pdf.py 只作兜底——它的细白杠 16pt 下低于渲染阈值，会退化成色块。
DEV_PDF_BG="${HENG_PDF_BG:-#6E768C}"
if [ -n "$DEV_PDF_BG" ]; then
  PDF_SRC="$(dirname "$0")/gen-rime-pdf.m"
  PDF_GEN="$ASIDE/gen-rime-pdf"
  if [ ! -x "$PDF_GEN" ] || [ "$PDF_SRC" -nt "$PDF_GEN" ]; then
    /usr/bin/clang -x objective-c -framework CoreText -framework CoreGraphics \
      -framework Foundation -o "$PDF_GEN" "$PDF_SRC" 2>/dev/null \
      || echo "  ⚠️ gen-rime-pdf.m 编译失败，退回 gen-rime-pdf.py"
  fi
  PDF_TMP="$ASIDE/rime-dev.$$.pdf"
  if [ -x "$PDF_GEN" ]; then
    "$PDF_GEN" "$PDF_TMP" "$DEV_PDF_BG"
  else
    HENG_BG="$DEV_PDF_BG" "$PY" "$(dirname "$0")/gen-rime-pdf.py" "$PDF_TMP"
  fi
  put_in_bundle "$PDF_TMP" "$STAGING/Contents/Resources/rime.pdf" && echo "  rime.pdf ok ($DEV_PDF_BG)"
fi

# 重签必须带 entitlements：Xcode 原产物含 disable-library-validation，
# codesign -s - 不带 --entitlements 会丢弃，系统随后无法激活该输入法。
ENTITLEMENTS="${HENG_ENTITLEMENTS:-$(dirname "$0")/../third_party/src/squirrel/resources/Squirrel.entitlements}"
codesign --force --deep -s - --entitlements "$ENTITLEMENTS" "$STAGING" 2>/dev/null
echo "staging ok"
echo "  icns    : $(stat -f%z "$STAGING/Contents/Resources/Rime.icns") bytes"
echo "  rime.pdf: $(stat -f%z "$STAGING/Contents/Resources/rime.pdf") bytes"

# 2. 停旧 + 原地 ditto 覆盖安装
"$APP_DST/Contents/MacOS/Squirrel" --quit 2>/dev/null || true
pkill -f SquirrelDev 2>/dev/null || true
sleep 1
/usr/bin/ditto "$STAGING" "$APP_DST" && echo "installed"
# staging 留在 /tmp 供下轮复用，但要把它从 LS 登记里摘掉，避免同 ID 两条
lsr_forget "$STAGING"
lsr_dedup_id "$DEV_ID" "$APP_DST"

# 3. register + enable
"$APP_DST/Contents/MacOS/Squirrel" --register-input-source

# 4. 修正 AppleEnabledInputSources
#    TISEnableInputSource 对 Input Mode 类型只改会话内存视图，不落盘；
#    Hans/Hant 必须以 "Input Mode" 条目存在于 AppleEnabledInputSources 才生效。
#    另注意 plist 可能带 uchg 标志，会让所有写入被静默丢弃。
#    这里用 Python 处理：PlistBuddy 无法写含空格的 key（"Bundle ID" 会被
#    解析成 key=Bundle + type=ID），且需要先 chflags nouchg。
"$PY" "$(dirname "$0")/heng-fix-plist.py" "$DEV_ID" || true

# 5. 最后再注册一次
#    heng-fix-plist.py 结尾会重启 cfprefsd / TextInputMenuAgent，
#    会把第 3、4 步之前注册的输入源冲掉，所以必须补这一步。
"$APP_DST/Contents/MacOS/Squirrel" --register-input-source || true
"$APP_DST/Contents/MacOS/Squirrel" --enable-input-source Hans || true
"$APP_DST/Contents/MacOS/Squirrel" --enable-input-source Hant || true

# 6. 收尾：再兜一次 LS 去重 + 把 staging 挪出 .app 形态
#    中途 heng-fix-plist.py 会 killall cfprefsd / TextInputMenuAgent，
#    而且这期间任何对 app 包的移动都可能让 LS 补登记，所以落盘后再兜一次底。
lsr_dedup_id "$DEV_ID" "$APP_DST"
lsr_forget "$STAGING"

# 7. 自检
echo "---- 自检 ----"
N=$(lsregister_count "$DEV_ID")
echo "  LaunchServices 中 $DEV_ID 记录数: $N （必须为 1）"
if [ "$N" != "1" ]; then
  echo "  ⚠️ 记录数不为 1。常见元凶：~/.Trash 里还躺着 SquirrelDev 副本"
  echo "     （本机 rm 会把 app 包送废纸篓，LS 会把废纸篓那份也注册进来）。"
  echo "     处理：清空废纸篓后重跑本脚本。"
  echo "     排查: $LSR -dump | grep -B4 \"identifier:.*$DEV_ID\""
fi
PROBE_SRC="$(dirname "$0")/tis-probe2.c"
if [ -f "$PROBE_SRC" ]; then
  PROBE_BIN="$ASIDE/tis-probe.$$"
  if /usr/bin/clang -x objective-c -o "$PROBE_BIN" "$PROBE_SRC" \
       -framework Carbon -framework Foundation 2>/dev/null; then
    echo "  TIS 里的条目:"
    "$PROBE_BIN" 2>/dev/null | /usr/bin/grep -F "$DEV_ID" | /usr/bin/sed 's/^/    /' || true
    rm -f "$PROBE_BIN" 2>/dev/null || true
  fi
fi

echo "✅ SquirrelDev ready ($DEV_ID)"
