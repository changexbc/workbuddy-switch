#!/usr/bin/env python3
"""Bake macOS-like rounded corners into Windows icon assets.

macOS applies a squircle mask to square app icons. Windows shows the bitmap as-is,
so icon.ico and the Store/Start-menu Square*.png files need transparent corners.

Does not touch icon.png / icon.icns / 32x32.png / 128x128.png (macOS + Linux).
"""

from pathlib import Path

from PIL import Image, ImageDraw

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
    out = im.copy()
    out.putalpha(mask)
    return out


def main() -> None:
    src = Image.open(ROOT / "icon.png").convert("RGBA")
    rounded_src = rounded(src)
    rounded_src.save(ROOT / "icon-windows.png", "PNG")
    rounded_src.save(
        ROOT / "icon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    for name, size in WINDOWS_PNGS.items():
        rounded_src.resize((size, size), Image.Resampling.LANCZOS).save(ROOT / name, "PNG")


if __name__ == "__main__":
    main()
