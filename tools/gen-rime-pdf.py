#!/usr/bin/env python3
"""生成输入法菜单栏用的 rime.pdf（22x16 pt），方案 P2。

背景
----
输入法在菜单栏 / 输入法选择器里显示的图标不是 Rime.icns，而是 Info.plist 中
  tsInputModeMenuIconFileKey
  tsInputModeAlternateMenuIconFileKey
  tsInputModePaletteIconFileKey
三者共同指向的 Resources/rime.pdf（三者共用同一文件）。

尺寸约束
--------
该PDF 页面固定 22x16 pt（与上游一致），菜单栏图标实际只有 16pt 高。
定稿的印章图形（1024 画布：描边 36、圆角 38.5）缩到 16pt 后，
描边只剩 0.56pt、圆角 0.6pt，低于 PDF 渲染阈值 —— 白色笔画会被整个抹掉，
菜单栏只显示一个深蓝色方块。

实测（像素级解析）：
  蓝底 132 像素、白色 0 像素

方案
----
P2：去掉外框（描边是最先被抹掉的元素），保留「三横一竖」骨架并显著加粗，
    让 16pt 下笔画仍能形成可见的白色块面。这是尺寸适配，不是重新设计：
    蓝底 #344dbe 与定稿一致，元素仍是三横 + 一竖。
"""
import os
import sys
import zlib

PAGE_W = 22.0
PAGE_H = 16.0
ICON = 16.0

# 底色：正式版蓝底 #344dbe，dev 版用灰底 #6E768C 区分
BG = os.environ.get("HENG_BG") or "#344dbe"
FG = "#ffffff"

# 1024x1024 画布内的笔画（y 向下，与 logo.svg 坐标系一致）
# 去框 + 加粗，保证 16pt 下每笔至少 ~2pt
BARS = [
    (250, 250, 150, 524, 75),   # 竖笔：贯通
    (470, 250, 150, 250, 75),   # 上横
    (470, 462, 150, 120, 60),   # 中横
    (470, 654, 150, 250, 75),   # 下横
]


def hex2rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) / 255.0 for i in (0, 2, 4))


def rr(x, y, w, h, r):
    """圆角矩形路径（y 已转成 PDF 坐标，原点在左下）。"""
    r = min(r, w / 2, h / 2)
    k = r * 0.4477
    return (
        f"{x + r:.2f} {y:.2f} m\n"
        f"{x + w - r:.2f} {y:.2f} l\n"
        f"{x + w - r + k:.2f} {y:.2f} {x + w:.2f} {y + r - k:.2f} {x + w:.2f} {y + r:.2f} c\n"
        f"{x + w:.2f} {y + h - r:.2f} l\n"
        f"{x + w:.2f} {y + h - r + k:.2f} {x + w - r + k:.2f} {y + h:.2f} {x + w - r:.2f} {y + h:.2f} c\n"
        f"{x + r:.2f} {y + h:.2f} l\n"
        f"{x + r - k:.2f} {y + h:.2f} {x:.2f} {y + h - r + k:.2f} {x:.2f} {y + h - r:.2f} c\n"
        f"{x:.2f} {y + r:.2f} l\n"
        f"{x:.2f} {y + r - k:.2f} {x + r - k:.2f} {y:.2f} {x + r:.2f} {y:.2f} c\n"
        "h\n"
    )


def build_content():
    s = ICON / 1024.0
    tx = (PAGE_W - ICON) / 2.0
    ty = (PAGE_H - ICON) / 2.0
    bg = hex2rgb(BG)
    fg = hex2rgb(FG)

    out = [f"q\n{s:.6f} 0 0 {s:.6f} {tx:.4f} {ty:.4f} cm\n"]
    # 满幅蓝底（显式闭合路径：re + f 在部分渲染器上不生效）
    out.append(f"{bg[0]:.4f} {bg[1]:.4f} {bg[2]:.4f} rg\n")
    out.append("0 0 m\n1024 0 l\n1024 1024 l\n0 1024 l\nh f\n")
    # 白色笔画
    out.append(f"{fg[0]:.4f} {fg[1]:.4f} {fg[2]:.4f} rg\n")
    for x, y, w, h, r in BARS:
        out.append(rr(x, 1024 - (y + h), w, h, r))
        out.append("f\n")
    out.append("Q\n")
    return "".join(out).encode("latin-1")


def build_pdf():
    content = zlib.compress(build_content())
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        (f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W:g} {PAGE_H:g}] "
         f"/CropBox [0 0 {PAGE_W:g} {PAGE_H:g}] /Resources << >> /Contents 4 0 R >>").encode(),
        b"<< /Length " + str(len(content)).encode() + b" /Filter /FlateDecode >>\nstream\n"
        + content + b"\nendstream",
    ]
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for i, body in enumerate(objs, start=1):
        offsets.append(len(out))
        out += str(i).encode() + b" 0 obj\n" + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 " + str(len(objs) + 1).encode() + b"\n0000000000 65535 f \n"
    for off in offsets[1:]:
        out += ("%010d 00000 n \n" % off).encode()
    out += (b"trailer\n<< /Size " + str(len(objs) + 1).encode() + b" /Root 1 0 R >>\nstartxref\n"
            + str(xref).encode() + b"\n%%EOF\n")
    return bytes(out)


if __name__ == "__main__":
    dest = sys.argv[1] if len(sys.argv) > 1 else "/tmp/rime.pdf"
    data = build_pdf()
    with open(dest, "wb") as f:
        f.write(data)
    print(f"wrote {dest} ({len(data)} bytes, MediaBox {PAGE_W:g}x{PAGE_H:g}pt, BG={BG})")