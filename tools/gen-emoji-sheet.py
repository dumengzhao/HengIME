#!/usr/bin/env python3
"""生成工具窗口用的彩色 emoji 雪碧图。

为什么需要：Slint 软件渲染器走 fontconfig 的 glyf 轮廓路径，画不出
Noto Color Emoji 的 CBDT 彩色位图字形 —— 落到单色 fallback 字体后，
新 emoji 没字形就是占位方块，老 emoji 是黑色描边。把彩色位图预渲染成
雪碧图，core 解码后直接贴到自绘帧缓冲，绕开文字渲染。

用法（仓库根运行）：
    python3 tools/gen-emoji-sheet.py
输出：
    runtime/rime-shared/others/emoji-sheet.png   RGBA 雪碧图
    runtime/rime-shared/others/emoji-sheet.json  分组 + 顺序（Rust 读它，
        保证 flat_items 下标与雪碧图格子下标严格一一对应）

字体：Noto Color Emoji 是固定 strike 的位图字体，只有 109px 这一档能被
FreeType 加载（PIL truetype size 必须给 109，否则 invalid pixel size）。
"""

import json
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EMOJI_TXT = os.path.join(ROOT, "runtime/rime-shared/opencc/emoji.txt")
FONT_PATH = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"
# 位图字体缺字形的兜底：货币符号（¥ € £ …）等普通文本字符走常规字体，
# 渲染成深色单色文字位图；两者都无字形的（系统字体版本太旧的新 emoji）
# 直接剔除，不留空白格。
FALLBACK_PATH = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
OUT_DIR = os.path.join(ROOT, "runtime/rime-shared/others")
CELL = 40
COLS = 64
GLYPH_SIZE = 36

# 与 core/src/tools_data.rs 的 ORDER / emoji_group 完全一致
GROUPS = [
    "笑脸与情感", "人物与身体", "动物与自然", "活动与物品",
    "交通与地点", "符号与旗帜", "旗帜", "其他",
]


def emoji_group(cp: int) -> str:
    if 0x1F600 <= cp <= 0x1F64F:
        return "笑脸与情感"
    if 0x1F300 <= cp <= 0x1F3FF:
        return "活动与物品"
    if 0x1F400 <= cp <= 0x1F4FF:
        return "动物与自然"
    if 0x1F900 <= cp <= 0x1F9FF:
        return "人物与身体"
    if 0x1F680 <= cp <= 0x1F6FF:
        return "交通与地点"
    if 0x1F1E6 <= cp <= 0x1F1FF:
        return "旗帜"
    if (0x2600 <= cp <= 0x27BF or 0x2B00 <= cp <= 0x2BFF
            or 0x1F000 <= cp <= 0x1F2FF):
        return "符号与旗帜"
    return "其他"


def parse() -> list:
    buckets = {name: [] for name in GROUPS}
    seen = set()
    with open(EMOJI_TXT, encoding="utf-8") as f:
        for line in f:
            if "\t" not in line:
                continue
            rhs = line.split("\t", 1)[1]
            for i, token in enumerate(rhs.split()):
                if i == 0 or not token:
                    continue  # 跳过原词
                # 纯中文/ASCII 的说明文字不当 emoji（与 Rust 同规则）
                if all(c.isascii() or "\u4e00" <= c <= "\u9fff" for c in token):
                    continue
                if token in seen:
                    continue
                seen.add(token)
                cp = ord(token[0])
                buckets[emoji_group(cp)].append(token)
    return [
        {"label": name, "items": buckets[name]}
        for name in GROUPS
        if buckets[name]
    ]


def render(token: str, font, fallback) -> Image.Image | None:
    """把单个 emoji 渲染成 CELL×CELL 的 RGBA 小图；完全无字形返回 None。"""
    # 109px strike 下字形大约占 117×112（bbox 9,7..126,119），给足画布
    big = Image.new("RGBA", (160, 160), (0, 0, 0, 0))
    ImageDraw.Draw(big).text((0, 0), token, font=font, embedded_color=True)
    bbox = big.getbbox()
    if bbox:
        glyph = big.crop(bbox)
        w, h = glyph.size
        scale = min(GLYPH_SIZE / w, GLYPH_SIZE / h)
        nw, nh = max(1, round(w * scale)), max(1, round(h * scale))
        glyph = glyph.resize((nw, nh), Image.LANCZOS)
        cell = Image.new("RGBA", (CELL, CELL), (0, 0, 0, 0))
        cell.paste(glyph, ((CELL - nw) // 2, (CELL - nh) // 2), glyph)
        return cell
    # 兜底：仅对普通文本码点（¥ € £ …）用常规字体画深色文字。
    # 高位 emoji 码点（0x1F000+）在常规字体里只会画出 .notdef 方框，
    # 那正是用户看到的"占位"，必须剔除而不是兜底。
    if fallback is not None and ord(token[0]) < 0x2000:
        mono = Image.new("RGBA", (CELL, CELL), (0, 0, 0, 0))
        d = ImageDraw.Draw(mono)
        d.text((CELL / 2, CELL / 2), token, font=fallback,
               fill=(31, 35, 40, 255), anchor="mm")
        if mono.getbbox():
            return mono
    return None


def main() -> int:
    if not os.path.exists(FONT_PATH):
        print(f"找不到 {FONT_PATH}（需要 fonts-noto-color-emoji）", file=sys.stderr)
        return 1
    groups = parse()
    font = ImageFont.truetype(FONT_PATH, 109)
    fallback = (ImageFont.truetype(FALLBACK_PATH, 28)
                if os.path.exists(FALLBACK_PATH) else None)

    # 第一遍：渲染全部，找出两者都无字形的 token
    images = []
    missing = set()
    for g in groups:
        for token in g["items"]:
            img = render(token, font, fallback)
            if img is None:
                missing.add(token)
            images.append((token, img))
    if missing:
        print(f"剔除 {len(missing)} 个系统字体无字形的 emoji: "
              + " ".join(sorted(missing)), file=sys.stderr)
        groups = [
            {"label": g["label"], "items": [t for t in g["items"] if t not in missing]}
            for g in groups
        ]
        groups = [g for g in groups if g["items"]]
    image_map = {t: img for t, img in images}
    flat = [t for g in groups for t in g["items"]]

    rows = math.ceil(len(flat) / COLS)
    sheet = Image.new("RGBA", (COLS * CELL, rows * CELL), (0, 0, 0, 0))
    for idx, token in enumerate(flat):
        img = image_map[token]
        if img is not None:
            sheet.paste(img, ((idx % COLS) * CELL, (idx // COLS) * CELL), img)

    os.makedirs(OUT_DIR, exist_ok=True)
    png = os.path.join(OUT_DIR, "emoji-sheet.png")
    sheet.save(png, optimize=True)
    meta = {
        "cell": CELL,
        "cols": COLS,
        "count": len(flat),
        "groups": groups,
    }
    with open(os.path.join(OUT_DIR, "emoji-sheet.json"), "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, separators=(",", ":"))
    print(f"{len(flat)} emoji, {rows} 行, {os.path.getsize(png)} 字节")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
