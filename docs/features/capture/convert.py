"""Converts raw PNG captures into the README's WebP stills.

python docs/features/capture/convert.py <folder-with-pngs>
"""
import sys
from pathlib import Path

from PIL import Image

# README image id -> raw capture name (see CAPTURE.md)
STILLS = {
    "hero-dark": "companion-dark",
    "hero-light": "companion",
    "dictation": "home",
    "formatting": "settings-dictation",
    "prompts": "prompts",
    "meetings": "saved-dark",
    "assistant": "companion-assistant",
    "investigations": "inv-chron",
    "investigations-diagram": "inv-diagram",
    "messages": "messages",
    "library": "history",
    "actions": "action-editor",
    "models": "models",
}
LIMIT = 250 * 1024

source = Path(sys.argv[1])
target = Path("docs/features")
for still, raw in STILLS.items():
    image = Image.open(source / f"{raw}.png").convert("RGB")
    for quality in (88, 80, 72, 64):
        out = target / f"{still}.webp"
        image.save(out, "WEBP", quality=quality, method=6)
        if out.stat().st_size <= LIMIT:
            break
    print(f"{out.name}: {image.width}x{image.height}, {out.stat().st_size // 1024} KB")
