#!/usr/bin/env python3
"""Package committed artwork into desktop/web icons (Python 3 + Pillow).

Artwork is generated separately; this script only resizes, encodes and exports
platform assets. Like WB Switch, tray pixels are embedded as raw RGBA so Tauri
does not need a runtime PNG decoder. Run after `npm ci`.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets/branding"
ICONS = ROOT / "src-tauri/icons"
PUBLIC = ROOT / "public"


def resized(image, size):
    # Premultiplied alpha prevents colored fringes around transparent edges.
    return image.convert("RGBa").resize((size, size), Image.Resampling.LANCZOS).convert("RGBA")


def export_tray(image, stem, size, ink=None):
    small = resized(image, size)
    if ink is not None:
        alpha = small.getchannel("A")
        small = Image.new("RGBA", small.size, (*ink, 0))
        small.putalpha(alpha)
    alpha = small.getchannel("A")
    assert alpha.getextrema()[0] == 0 and alpha.getextrema()[1] >= 240, stem
    assert all(alpha.getpixel(point) == 0 for point in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1)]), stem
    small.save(ICONS / f"{stem}.png")
    (ICONS / f"{stem}.rgba").write_bytes(small.tobytes())


def main():
    ICONS.mkdir(parents=True, exist_ok=True)
    PUBLIC.mkdir(parents=True, exist_ok=True)
    app = Image.open(SOURCE / "app-source.png").convert("RGBA")
    color = Image.open(SOURCE / "transparent-source.png").convert("RGBA")
    mono = Image.open(SOURCE / "tray-template-source.png").convert("RGBA")
    assert app.width == app.height
    # The local, lockfile-pinned Tauri CLI supplies the platform container formats.
    # Generate in a temporary directory: mobile icons are not shipped by this app.
    with tempfile.TemporaryDirectory(prefix="companion-icons-") as directory:
        subprocess.run([
            str(ROOT / "node_modules/.bin/tauri"), "icon",
            str(SOURCE / "app-source.png"), "--output", directory,
        ], cwd=ROOT, check=True)
        for path in Path(directory).iterdir():
            if path.is_file():
                shutil.copy2(path, ICONS / path.name)
    for size in (16, 32, 64, 128, 256, 512, 1024):
        resized(app, size).save(ICONS / f"{size}x{size}.png")
    export_tray(mono, "tray-icon-template", 36, (0, 0, 0))
    export_tray(mono, "tray-icon-template-18", 18, (0, 0, 0))
    export_tray(mono, "tray-icon-mono-black", 32, (17, 17, 19))
    export_tray(mono, "tray-icon-mono-white", 32, (255, 255, 255))
    export_tray(color, "tray-icon-color", 32)
    export_tray(color, "tray-icon-color-16", 16)
    resized(app, 32).save(PUBLIC / "favicon.png")
    app.save(PUBLIC / "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48)])
    resized(app, 180).save(PUBLIC / "apple-touch-icon.png")
    resized(app, 512).save(PUBLIC / "icon.png")
    resized(color, 512).save(PUBLIC / "icon-transparent.png")
    print("Generated desktop PNG/ICNS/ICO, transparent trays and web icons.")


if __name__ == "__main__":
    main()
