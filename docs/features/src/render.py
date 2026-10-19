"""Renders the README visuals from the HTML templates in this folder.

From the repository root:

    python docs/features/src/render.py

Needs Python 3 with Pillow and Microsoft Edge (or set EDGE to a Chromium
binary). Each template is screenshotted headless and saved as WebP in
docs/features/, then a contact sheet of all of them is written to
docs/features/src/contact-sheet.webp.
"""
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from PIL import Image

SRC = Path(__file__).resolve().parent
OUT = SRC.parent
EDGE = os.environ.get("EDGE", r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
LIMIT = 250 * 1024

# output name -> (template, theme, height)
VISUALS = {
    "hero-dark": ("hero.html", "dark", 800),
    "hero-light": ("hero.html", "light", 800),
    "dictation": ("dictation.html", "dark", 800),
    "formatting": ("formatting.html", "dark", 800),
    "prompts": ("prompts.html", "dark", 800),
    "meetings": ("meetings.html", "dark", 800),
    "assistant": ("assistant.html", "dark", 800),
    "investigations": ("investigations.html", "dark", 1100),
    "messages": ("messages.html", "dark", 800),
    "more": ("more.html", "dark", 800),
}


def render(name: str, template: str, theme: str, height: int, scratch: Path) -> Path:
    png = scratch / f"{name}.png"
    url = (SRC / template).as_uri() + f"?theme={theme}"
    subprocess.run(
        [EDGE, "--headless=new", "--disable-gpu", "--hide-scrollbars",
         "--force-device-scale-factor=1", "--allow-file-access-from-files",
         f"--user-data-dir={scratch / 'profile'}", f"--window-size=1600,{height}",
         "--virtual-time-budget=3000", f"--screenshot={png}", url],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    # msedge.exe can return before its headless child has written the file.
    for _ in range(100):
        if png.exists() and png.stat().st_size:
            break
        time.sleep(0.2)
    time.sleep(0.3)
    image = Image.open(png).convert("RGB")
    out = OUT / f"{name}.webp"
    for quality in (90, 84, 78, 70, 62):
        image.save(out, "WEBP", quality=quality, method=6)
        if out.stat().st_size <= LIMIT:
            break
    print(f"{out.name}: {image.width}x{image.height}, {out.stat().st_size // 1024} KB")
    return out


def contact_sheet(paths: list[Path]) -> None:
    thumbs = [Image.open(p).convert("RGB") for p in paths]
    width = 760
    rows = [(t.resize((width, round(t.height * width / t.width))), p.stem) for t, p in zip(thumbs, paths)]
    columns, gap = 2, 24
    heights = [max(r[0].height for r in rows[i:i + columns]) for i in range(0, len(rows), columns)]
    sheet = Image.new("RGB", (columns * width + (columns + 1) * gap, sum(heights) + (len(heights) + 1) * gap), "#2a2f32")
    y = gap
    for row_index, row_height in enumerate(heights):
        for column, (thumb, _) in enumerate(rows[row_index * columns:(row_index + 1) * columns]):
            sheet.paste(thumb, (gap + column * (width + gap), y))
        y += row_height + gap
    sheet.save(SRC / "contact-sheet.webp", "WEBP", quality=80, method=6)
    print("contact-sheet.webp written")


if __name__ == "__main__":
    only = set(sys.argv[1:])
    with tempfile.TemporaryDirectory() as scratch:
        outputs = [render(name, *spec, Path(scratch)) for name, spec in VISUALS.items() if not only or name in only]
    if not only:
        contact_sheet(outputs)
