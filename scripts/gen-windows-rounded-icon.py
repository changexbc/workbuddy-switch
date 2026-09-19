#!/usr/bin/env python3
"""Bake macOS-like rounded corners into Windows icon assets.

默认从 `icon-transparent.png`（透明底）生成：Windows 任务栏/开始菜单会直接显示位图，
若源图是不透明满幅方图，深色任务栏下会显示成带浅色底板的方块。
可用 --src 指定其他源图。

macOS applies a squircle mask to square app icons. Windows shows the bitmap as-is,
so icon.ico and the Store/Start-menu Square*.png files need transparent corners.

Does not touch icon.png / icon.icns / 32x32.png / 128x128.png (macOS + Linux).
"""

import argparse
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parents[1] / "src-tauri" / "icons"
RADIUS_RATIO = 0.2237
WINDOWS_PNGS = {
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}


def rounded(im: Image.Image, radius_ratio: float = RADIUS_RATIO) -> Image.Image:
    im = im.convert("RGBA")
    w, h = im.size
    scale = 4
    big = im.resize((w * scale, h * scale), Image.Resampling.LANCZOS)
    bw, bh = big.size
    radius = int(min(bw, bh) * radius_ratio)
    mask = Image.new("L", (bw, bh), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, bw - 1, bh - 1), radius=radius, fill=255)
    mask = mask.resize((w, h), Image.Resampling.LANCZOS)
    # 关键：圆角蒙版必须与源图 alpha **相乘**，不能整体覆盖——
    # 覆盖会把源图的透明背景变成不透明（其 RGB 多为黑色），在 Windows 上表现为黑底板。
    out = im.copy()
    out.putalpha(ImageChops.multiply(im.getchannel("A"), mask))
    return out


# ---------------------------------------------------------------------------
# 自写 ICO 打包：Pillow 的 ICO 保存会写出 AND 掩码全 0 的条目，导致透明区域
# 在 Windows（桌面图标、任务栏、小尺寸渲染路径）显示为黑底。这里改为：
#   * 每个尺寸写 32bpp BGRA 的 DIB（BITMAPINFOHEADER，biHeight = 2*h 以容纳掩码）
#   * AND 掩码按 alpha < 128 置位（1 = 透明），保证各渲染路径一致
# ---------------------------------------------------------------------------
def save_ico(im: Image.Image, path: Path, sizes) -> None:
    """写 ICO：256px 用 PNG 压缩条目，其余尺寸用 32bpp DIB + AND 掩码。

    为什么不用 Pillow 的 ICO 保存：它会把 **所有尺寸**都写成 PNG 条目，而 Windows
    仅对 256px 正式支持 PNG-in-ICO；小尺寸走 PNG 时资源管理器可能渲染异常（表现为黑底）。
    """
    import io as _io

    im = im.convert("RGBA")
    entries, blobs = [], []
    offset = 6 + 16 * len(sizes)
    for size in sizes:
        frame = im.resize((size, size), Image.Resampling.LANCZOS)
        if size == 256:
            buf = _io.BytesIO()
            frame.save(buf, "PNG")
            blob = buf.getvalue()
            entries.append(struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(blob), offset))
            offset += len(blob)
            blobs.append(blob)
            continue
        px = frame.load()
        # XOR 位图（自下而上）
        xor = bytearray()
        for y in range(size - 1, -1, -1):
            for x in range(size):
                r, g, b, a = px[x, y]
                xor += bytes((b, g, r, a))
        # AND 掩码（1bpp，行按 4 字节对齐，自下而上；alpha<128 → 1 = 透明）
        stride = ((size + 31) // 32) * 4
        mask = bytearray()
        for y in range(size - 1, -1, -1):
            row = bytearray(stride)
            for x in range(size):
                if px[x, y][3] < 128:
                    row[x // 8] |= 0x80 >> (x % 8)
            mask += row
        header = struct.pack(
            "<IiiHHIIiiII",
            40,          # biSize
            size,        # biWidth
            size * 2,    # biHeight（XOR + AND）
            1, 32, 0,    # planes, bitcount, compression
            len(xor) + len(mask), 0, 0, 0, 0,
        )
        blob = header + bytes(xor) + bytes(mask)
        entries.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(blob), offset))
        offset += len(blob)
        blobs.append(blob)
    with open(path, "wb") as fh:
        fh.write(struct.pack("<HHH", 0, 1, len(sizes)))
        for e in entries:
            fh.write(e)
        for b in blobs:
            fh.write(b)


def main() -> None:
    parser = argparse.ArgumentParser(description="生成 Windows 圆角图标资产")
    parser.add_argument(
        "--src",
        default="public/icon-transparent.png",
        help="源图路径（相对仓库根；默认使用透明底素材）",
    )
    args = parser.parse_args()
    repo_root = Path(__file__).resolve().parents[1]
    src = Image.open(repo_root / args.src).convert("RGBA")
    rounded_src = rounded(src)
    rounded_src.save(ROOT / "icon-windows.png", "PNG")
    save_ico(rounded_src, ROOT / "icon.ico", [16, 24, 32, 48, 64, 128, 256])
    for name, size in WINDOWS_PNGS.items():
        rounded_src.resize((size, size), Image.Resampling.LANCZOS).save(ROOT / name, "PNG")


if __name__ == "__main__":
    main()
