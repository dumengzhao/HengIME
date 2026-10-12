#!/usr/bin/env python3
"""把输入法 .app 改标签：换 bundle id、名称、连接名，并同步改掉
ComponentInputModeDict 里所有模式 ID。改完必须重新签名。"""

import os
import plistlib
import sys


def main():
    app = os.path.abspath(sys.argv[1])
    new_id = sys.argv[2]
    new_name = sys.argv[3] if len(sys.argv) > 3 else None

    p = os.path.join(app, "Contents", "Info.plist")
    with open(p, "rb") as f:
        d = plistlib.load(f)

    old_id = d["CFBundleIdentifier"]
    if old_id == new_id and not new_name:
        print("no-op: already %s" % new_id)
        return

    d["CFBundleIdentifier"] = new_id
    if new_name:
        d["CFBundleName"] = new_name
    d["InputMethodConnectionName"] = new_id.replace(".", "_") + "_Connection"

    cid = d.get("ComponentInputModeDict")
    if cid:
        lst = cid.get("tsInputModeListKey", {})
        new_lst = {}
        for k, v in lst.items():
            nk = k.replace(old_id, new_id)
            if isinstance(v, dict) and "TISInputSourceID" in v:
                v["TISInputSourceID"] = v["TISInputSourceID"].replace(old_id, new_id)
            new_lst[nk] = v
        cid["tsInputModeListKey"] = new_lst
        if "tsVisibleInputModeOrderedArrayKey" in cid:
            cid["tsVisibleInputModeOrderedArrayKey"] = [
                x.replace(old_id, new_id)
                for x in cid["tsVisibleInputModeOrderedArrayKey"]
            ]

    if "TISInputSourceID" in d:
        d["TISInputSourceID"] = new_id

    with open(p, "wb") as f:
        plistlib.dump(d, f)

    modes = list((cid or {}).get("tsInputModeListKey", {}).keys())
    print("relabelled: %s -> %s" % (old_id, new_id))
    print("  connection: %s" % d["InputMethodConnectionName"])
    print("  modes: %s" % ", ".join(modes))


if __name__ == "__main__":
    main()
