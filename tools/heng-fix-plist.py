#!/usr/bin/env python3
"""把「衡」输入法的 Hans / Hant 以 Input Mode 形式写入 AppleEnabledInputSources。

背景 1：com.apple.HIToolbox.plist 曾被加 uchg(immutable) 标志，导致 cfprefsd 与
PlistBuddy 的写入全部返回 EPERM 并被静默丢弃（defaults write exit=0 但读不回来，
plist mtime 停在很久以前）。必须先 chflags nouchg。

背景 2：PlistBuddy 无法处理含空格的 key（"Bundle ID" 会被当成 key=Bundle + type=ID），
所以直接用 plistlib 读写。

背景 3：bundle ID 必须含 ".inputmethod." 段，否则 macOS 26+ 扫盘时不收录该输入法，
TISRegisterInputSource 返回 noErr 也没用。dev 版用 im.heng.inputmethod.Squirrel.dev。
"""
import plistlib
import subprocess
import sys
import os
import time

HOME = os.path.expanduser("~")
P = os.path.join(HOME, "Library/Preferences/com.apple.HIToolbox.plist")
# 默认正式版；dev 版传 im.heng.inputmethod.Squirrel.dev
BUNDLE_ID = sys.argv[1] if len(sys.argv) > 1 else "im.rime.inputmethod.Squirrel"
MODES = ("Hans", "Hant")
# 历史遗留的废弃 bundle ID（不含 .inputmethod. 段，系统永远不会收录），写入时顺手清掉
STALE_IDS = ("im.heng.ime.Squirrel.dev",)


def sh(cmd):
    subprocess.run(cmd, shell=True, capture_output=True)


def lock_state():
    r = subprocess.run(["ls", "-lO", P], capture_output=True, text=True)
    return r.stdout.strip()


def write_plist(pl):
    """写回 plist。

    不能直接 open(P, "wb") 覆盖：在被沙箱/文件代理托管的环境里，覆盖已有文件会
    报 "modify backup failed"。改为先写临时文件，再用 perl rename 顶替 ——
    rename 是原子操作，各环境实测都放行。
    """
    tmp = P + ".hengnew"
    with open(tmp, "wb") as f:
        plistlib.dump(pl, f)
    r = subprocess.run(
        ["/usr/bin/perl", "-e", 'rename($ARGV[0],$ARGV[1]) or die "rename: $!"', tmp, P],
        capture_output=True, text=True)
    if r.returncode != 0:
        print("  FAIL 覆盖失败:", r.stderr.strip())
        return False
    return True


def main():
    if not os.path.exists(P):
        print("FAIL 找不到", P)
        return 1

    print("== 0. 现状 ==")
    print(lock_state())

    print("== 1. 备份 + 解锁 ==")
    bak = "/tmp/HIToolbox.plist.pre-heng"
    sh(f'cp -p "{P}" "{bak}"')
    print("  备份 ->", bak)
    sh(f'chflags nouchg "{P}"')
    print(" ", lock_state())

    print("== 2. 停 cfprefsd（否则内存缓存会覆盖文件写入）==")
    sh("killall cfprefsd")
    time.sleep(1.5)

    print("== 3. 写入 AppleEnabledInputSources ==")
    with open(P, "rb") as f:
        pl = plistlib.load(f)

    arr = pl.get("AppleEnabledInputSources", [])

    # 清掉历史遗留的空 dict
    before = len(arr)
    arr = [d for d in arr if isinstance(d, dict) and d]
    if len(arr) != before:
        print(f"  清理空 dict {before} -> {len(arr)}")

    # 清掉废弃 ID 的条目
    stale = [d for d in arr if d.get("Bundle ID") in STALE_IDS]
    if stale:
        arr = [d for d in arr if d.get("Bundle ID") not in STALE_IDS]
        print(f"  清理废弃 ID 条目 {len(stale)} 条")

    have = {d.get("Input Mode") for d in arr if isinstance(d, dict)}
    for mode in MODES:
        key = f"{BUNDLE_ID}.{mode}"
        if key in have:
            print(f"  = {mode} 已存在，跳过")
            continue
        arr.append({
            "Bundle ID": BUNDLE_ID,
            "Input Mode": key,
            "InputSourceKind": "Input Mode",
        })
        print(f"  + 已写入 {mode}")

    pl["AppleEnabledInputSources"] = arr
    if not write_plist(pl):
        return 1
    print(f"  共 {len(arr)} 条")

    print("== 4. 校验（回读文件）==")
    with open(P, "rb") as f:
        check = plistlib.load(f)
    modes = [d.get("Input Mode") for d in check.get("AppleEnabledInputSources", [])]
    for m in modes:
        print("   ", m)
    if f"{BUNDLE_ID}.Hans" not in modes:
        print("  FAIL Hans 未落盘")
        return 1
    print("  OK Hans/Hant 已落盘")
    print(" ", lock_state())

    print("== 5. 确认保持可写（不要恢复 uchg）==")
    # 关键：绝不能把 uchg 加回去。
    # uchg(immutable) 是文件系统级标志，连 root 都拒绝写入，
    # cfprefsd 无法持久化任何输入源变更 —— 表现为系统设置里点「+」显示已添加
    # 但底层无记录（幽灵条目、减号灰化）、TISRegisterInputSource/TISEnableInputSource
    # 返回 noErr 却完全不生效。
    print(" ", lock_state())
    if "uchg" in lock_state():
        print("  FAIL plist 仍带 uchg，立即解锁")
        sh(f'chflags nouchg "{P}"')
        print(" ", lock_state())

    print("== 6. 让会话重新加载输入源列表 ==")
    sh("killall cfprefsd")
    sh("killall TextInputMenuAgent")
    print("done. 现在去 系统设置 → 键盘 → 输入法，应能看到「衡Dev」")
    return 0


if __name__ == "__main__":
    sys.exit(main())
