"""Regenerate desktop and web icons from the app's vector mark.

Run from the repo root after npm ci in the frontend. Requires Pillow and macOS iconutil.
"""

import math
import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

ICON_DIR = "crates/oc-gui/frontend/src-tauri/icons/"
PUBLIC_DIR = "crates/oc-gui/frontend/public/"
FRONTEND = Path("crates/oc-gui/frontend")
mark = (FRONTEND / "src/assets/logo/mark.svg").read_text()
art = mark.split(">", 1)[1].rsplit("</svg>", 1)[0].strip()
icon_svg = Path(ICON_DIR) / "logo.svg"
icon_svg.write_text(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" '
    'width="1024" height="1024">\n'
    '  <rect width="120" height="120" fill="#102a25"/>\n'
    '  <g transform="translate(7.2 7.2) scale(.88)" fill="#e5f6eb">\n'
    f"    {art}\n"
    "  </g>\n"
    "</svg>\n"
)

with tempfile.TemporaryDirectory(prefix="opencompose-icon-") as raster:
    subprocess.run(
        [
            "npm",
            "exec",
            "--offline",
            "--",
            "tauri",
            "icon",
            str(icon_svg.resolve()),
            "--output",
            raster,
            "--png",
            "1024",
        ],
        cwd=FRONTEND,
        check=True,
    )
    with Image.open(Path(raster) / "1024x1024.png") as source:
        plate = source.convert("RGBA")


def squircle_mask(size, body):
    S = 4
    N = size * S
    a = body * S / 2
    c = N / 2
    pts = []
    for i in range(4000):
        t = 2 * math.pi * i / 4000
        x, y = math.cos(t), math.sin(t)
        pts.append((c + a * math.copysign(abs(x) ** 0.4, x), c + a * math.copysign(abs(y) ** 0.4, y)))
    m = Image.new("L", (N, N), 0)
    ImageDraw.Draw(m).polygon(pts, fill=255)
    return m.resize((size, size), Image.LANCZOS)


def make(size, body):
    art = plate.resize((body, body), Image.LANCZOS)
    o = (size - body) // 2
    full = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    full.paste(art, (o, o))
    full.putalpha(squircle_mask(size, body))
    return full


mac = make(1024, 824)  # macOS/Linux grid: 824 body in 1024 canvas
win = make(1024, 1024)  # Windows/web: full-bleed rounded
mac.save(ICON_DIR + "icon-1024.png")


def resize_icon(image, size):
    return image.resize((size, size), Image.LANCZOS)


for name, s in [("icon.png", 512), ("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)]:
    resize_icon(mac, s).save(ICON_DIR + name)
for name in os.listdir(ICON_DIR):
    if name.startswith("Square") or name == "StoreLogo.png":
        s = Image.open(ICON_DIR + name).size[0]
        resize_icon(win, s).save(ICON_DIR + name)
win.save(ICON_DIR + "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
# The .icns includes the native size and Retina representation at each rung.
with tempfile.TemporaryDirectory(prefix="opencompose-iconset-") as temp:
    iconset = Path(temp) / "icon.iconset"
    iconset.mkdir()
    for size in [16, 32, 128, 256, 512]:
        resize_icon(mac, size).save(iconset / f"icon_{size}x{size}.png")
        resize_icon(mac, size * 2).save(iconset / f"icon_{size}x{size}@2x.png")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", ICON_DIR + "icon.icns"], check=True)
# web icons
win.save(PUBLIC_DIR + "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48), (256, 256)])
resize_icon(win, 16).save(PUBLIC_DIR + "favicon-16.png")
resize_icon(win, 32).save(PUBLIC_DIR + "favicon-32.png")

resize_icon(win, 180).save(PUBLIC_DIR + "apple-touch-icon.png")
