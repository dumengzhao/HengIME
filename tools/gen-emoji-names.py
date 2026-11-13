#!/usr/bin/env python3
"""生成 emoji → 中文名（CLDR 官方注解）。

为什么不用 opencc/emoji.txt：那是「中文词 → emoji」的输入映射表，左列是
触发词不是名字（❣️ 叫「心叹号」、😒 叫「郁闷」），而且 79% 的 emoji 有多个
触发词，取第一个经常不是最贴切的那个。

用法：python3 tools/gen-emoji-names.py   （需要联网抓 CLDR zh 注解，约 400KB）
输出：runtime/rime-shared/others/emoji-names-zh.json
"""
import json, os, sys, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "runtime/rime-shared/others/emoji-names-zh.json")

# CLDR 的 tts 有时过于直译（🎉=拉炮彩带、☕=热饮），人工改几个高频的
OVERRIDE = {
    "🎉": "庆祝",
    "🎊": "庆祝",
    "☕": "咖啡",
    "🥵": "热",
    "😅": "冷汗",
    "😩": "疲惫",
    "✅": "对勾",
    "❌": "错号",
}
URL = "https://cdn.jsdelivr.net/npm/cldr-annotations-full/annotations/zh/annotations.json"


def fetch():
    with urllib.request.urlopen(URL, timeout=30) as r:
        return json.loads(r.read().decode("utf-8"))


def main() -> int:
    data = fetch()
    ann = data.get("annotations", {}).get("annotations", {})
    names = {}
    for ch, v in ann.items():
        if not isinstance(v, dict):
            continue
        # tts 是 CLDR 为朗读准备的短名，比 default 首个关键词更像「名字」
        name = (v.get("tts") or [""])[0]
        if not name:
            name = (v.get("default") or [""])[0]
        if name:
            names[ch] = name
    # 去掉变体选择符（U+FE0F）再存一份：词表里的 ⚠️/❤️ 与 CLDR 的 ⚠/❤ 常不一致
    for ch, name in list(names.items()):
        plain = ch.replace("\ufe0f", "")
        if plain and plain not in names:
            names[plain] = name
    names.update(OVERRIDE)
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(names, f, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    print(f"{len(names)} 条 → {OUT}（{os.path.getsize(OUT)} 字节）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
