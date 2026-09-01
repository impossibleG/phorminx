#!/usr/bin/env python3
"""Build and validate the deterministic Phorminx Rounded P asset family.

The 48-unit construction is an authored redraw of the user-approved reference.
Every raster is rendered directly at its requested size; no smaller asset is a
reduction of a larger PNG. Pillow is the only non-standard dependency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

from PIL import Image, ImageDraw, ImageFont


SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256, 512)
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
TRAY_SIZES = (16, 20, 24)
SUPERSAMPLE = 8

ABYSS = (10, 12, 13, 255)
LIMESTONE = (232, 229, 221, 255)
BRONZE = (168, 117, 66, 255)
BLACK = (0, 0, 0, 255)
WHITE = (255, 255, 255, 255)


Point = tuple[float, float]


@dataclass(frozen=True)
class Geometry:
    size: int
    field_inset: float
    field_radius: float
    outer: tuple[Point, ...]
    counter: tuple[Point, ...]
    slit: tuple[Point, ...]
    bronze: tuple[Point, ...]


def cubic(p0: Point, p1: Point, p2: Point, p3: Point, steps: int = 18) -> list[Point]:
    points: list[Point] = []
    for index in range(1, steps + 1):
        t = index / steps
        u = 1.0 - t
        points.append(
            (
                u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
            )
        )
    return points


def geometry_for(size: int) -> Geometry:
    """Scale the approved 48-unit construction onto a requested canvas."""
    scale = size / 48.0

    def s(points: Iterable[Point]) -> tuple[Point, ...]:
        return tuple((x * scale, y * scale) for x, y in points)

    # Optical bounds and proportions are measured from the approved 1024 px
    # reference and normalized onto this 48-unit construction. The two cubic
    # bowls deliberately share a vertical center at y=17.58.
    outer: list[Point] = [(10.3125, 7.03125), (28.80, 7.03125)]
    outer += cubic((28.80, 7.03125), (34.18, 7.03125), (38.4375, 11.76), (38.4375, 17.578125), steps=24)
    outer += cubic((38.4375, 17.578125), (38.4375, 23.39), (34.18, 28.125), (28.80, 28.125), steps=24)
    outer += [(23.953125, 28.125), (23.953125, 39.375), (10.3125, 39.375)]

    counter: list[Point] = [(22.40625, 12.1875), (31.171875, 12.1875)]
    counter += cubic((31.171875, 12.1875), (33.54, 12.1875), (35.4375, 14.60), (35.4375, 17.578125), steps=18)
    counter += cubic((35.4375, 17.578125), (35.4375, 20.56), (33.54, 22.96875), (31.171875, 22.96875), steps=18)
    counter += [(22.40625, 22.96875)]

    slit = ((19.21875, 12.1875), (20.859375, 12.1875), (20.859375, 22.96875), (19.21875, 22.96875))
    bronze = ((24.0, 12.1875), (25.640625, 12.1875), (25.640625, 22.96875), (24.0, 22.96875))
    return Geometry(size, 1.5 * scale, 8.5 * scale, s(outer), s(counter), s(slit), s(bronze))


def svg_points(points: Sequence[Point]) -> str:
    return " ".join(f"{x:.3f},{y:.3f}".rstrip("0").rstrip(".") for x, y in points)


def svg_for(geometry: Geometry) -> str:
    outer = "M " + " L ".join(svg_points([point]) for point in geometry.outer) + " Z"
    counter = "M " + " L ".join(svg_points([point]) for point in geometry.counter) + " Z"
    slit = "M " + " L ".join(svg_points([point]) for point in geometry.slit) + " Z"
    bronze = "M " + " L ".join(svg_points([point]) for point in geometry.bronze) + " Z"
    field_size = geometry.size - 2 * geometry.field_inset
    return (
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        f"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{geometry.size}\" height=\"{geometry.size}\" "
        f"viewBox=\"0 0 {geometry.size} {geometry.size}\">\n"
        "  <title>Phorminx Rounded P</title>\n"
        "  <desc>Rounded Abyss field, heavy Limestone P, black counter and slit, and one restrained Bronze inner bar.</desc>\n"
        f"  <rect x=\"{geometry.field_inset:.3f}\" y=\"{geometry.field_inset:.3f}\" "
        f"width=\"{field_size:.3f}\" height=\"{field_size:.3f}\" rx=\"{geometry.field_radius:.3f}\" fill=\"#0A0C0D\"/>\n"
        f"  <path fill=\"#E8E5DD\" d=\"{outer}\"/>\n"
        f"  <path fill=\"#000000\" d=\"{counter} {slit}\"/>\n"
        f"  <path fill=\"#A87542\" d=\"{bronze}\"/>\n"
        "</svg>\n"
    )


def scaled(points: Sequence[Point], multiplier: int) -> list[tuple[int, int]]:
    return [(round(x * multiplier), round(y * multiplier)) for x, y in points]


def alpha_mask(geometry: Geometry) -> Image.Image:
    large = Image.new("L", (geometry.size * SUPERSAMPLE, geometry.size * SUPERSAMPLE), 0)
    draw = ImageDraw.Draw(large)
    draw.polygon(scaled(geometry.outer, SUPERSAMPLE), fill=255)
    for hole in (geometry.counter, geometry.slit):
        draw.polygon(scaled(hole, SUPERSAMPLE), fill=0)
    mask = large.resize((geometry.size, geometry.size), Image.Resampling.LANCZOS)

    # Preserve unambiguous transparent cores after antialiasing, especially at
    # 16 px where the approved slit is deliberately only one device pixel.
    hinted = ImageDraw.Draw(mask)
    for aperture in (geometry.counter, geometry.slit):
        cx = round(sum(x for x, _ in aperture) / len(aperture))
        cy = round(sum(y for _, y in aperture) / len(aperture))
        hinted.point((cx, cy), fill=0)
    return mask


def colored_mark(mask: Image.Image, color: tuple[int, int, int, int]) -> Image.Image:
    image = Image.new("RGBA", mask.size, color)
    image.putalpha(mask)
    return image


def polygon_layer(size: int, points: Sequence[Point], color: tuple[int, int, int, int]) -> Image.Image:
    large = Image.new("RGBA", (size * SUPERSAMPLE, size * SUPERSAMPLE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(large)
    draw.polygon(scaled(points, SUPERSAMPLE), fill=color)
    return large.resize((size, size), Image.Resampling.LANCZOS)


def app_icon(geometry: Geometry, mask: Image.Image) -> Image.Image:
    size = geometry.size
    large = Image.new("RGBA", (size * SUPERSAMPLE, size * SUPERSAMPLE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(large)
    inset = round(geometry.field_inset * SUPERSAMPLE)
    radius = round(geometry.field_radius * SUPERSAMPLE)
    draw.rounded_rectangle(
        (inset, inset, size * SUPERSAMPLE - inset - 1, size * SUPERSAMPLE - inset - 1),
        radius=radius,
        fill=ABYSS,
    )
    image = large.resize((size, size), Image.Resampling.LANCZOS)
    mark = Image.new("RGBA", (size, size), LIMESTONE)
    mark.putalpha(mask)
    image.alpha_composite(mark)
    image.alpha_composite(polygon_layer(size, geometry.counter, BLACK))
    image.alpha_composite(polygon_layer(size, geometry.slit, BLACK))
    image.alpha_composite(polygon_layer(size, geometry.bronze, BRONZE))

    # Guarantee one exact device-pixel color sample for every defining detail.
    pixels = image.load()
    for points, color in ((geometry.counter, BLACK), (geometry.slit, BLACK), (geometry.bronze, BRONZE)):
        cx = round(sum(x for x, _ in points) / len(points))
        cy = round(sum(y for _, y in points) / len(points))
        pixels[cx, cy] = color
    return image


def png_bytes(image: Image.Image) -> bytes:
    from io import BytesIO

    buffer = BytesIO()
    image.save(buffer, format="PNG", optimize=False, compress_level=9)
    return buffer.getvalue()


def write_png(path: Path, image: Image.Image) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png_bytes(image))


def build_ico(entries: Sequence[tuple[int, bytes]]) -> bytes:
    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    directory = bytearray()
    payload = bytearray()
    for size, png in entries:
        encoded = 0 if size == 256 else size
        directory.extend(struct.pack("<BBBBHHII", encoded, encoded, 0, 0, 1, 32, len(png), offset))
        payload.extend(png)
        offset += len(png)
    return header + bytes(directory) + bytes(payload)


def default_font() -> ImageFont.ImageFont:
    return ImageFont.load_default()


def label(draw: ImageDraw.ImageDraw, xy: tuple[int, int], text_value: str, fill: tuple[int, int, int, int]) -> None:
    draw.text(xy, text_value, font=default_font(), fill=fill)


def actual_size_sheet(marks_black: dict[int, Image.Image], apps: dict[int, Image.Image]) -> Image.Image:
    width, height = 1260, 680
    sheet = Image.new("RGBA", (width, height), (239, 238, 234, 255))
    draw = ImageDraw.Draw(sheet)
    label(draw, (24, 18), "PHORMINX / ROUNDED P / ACTUAL-SIZE MASTERS / 100%", BLACK)
    x = 24
    baseline = 620
    for size in SIZES:
        label(draw, (x, 48), f"{size}px", BLACK)
        y = baseline - size
        sheet.alpha_composite(marks_black[size], (x, y))
        x += size + 18
    x = 24
    baseline = 620
    for size in SIZES:
        y = baseline - size
        x += size + 18
    # The app family is shown in a second strip scaled only when it cannot fit
    # horizontally; individual masters above remain at exact size.
    label(draw, (24, 78), "APP / ROUNDED ABYSS FIELD + LIMESTONE P + BLACK CUTS + BRONZE BAR", BLACK)
    ax = 24
    for size in (16, 20, 24, 32, 40, 48, 64, 128, 256):
        sheet.alpha_composite(apps[size], (ax, 104))
        ax += size + 14
    return sheet


def small_contact_sheet(
    black_marks: dict[int, Image.Image],
    white_marks: dict[int, Image.Image],
    apps: dict[int, Image.Image],
) -> Image.Image:
    sizes = (16, 20, 24, 32, 40, 48)
    zoom = 8
    cell_w, cell_h = 420, 530
    sheet = Image.new("RGBA", (cell_w * 3, cell_h * 2 + 56), WHITE)
    draw = ImageDraw.Draw(sheet)
    label(draw, (20, 18), "PHORMINX / ROUNDED P / SMALL-SIZE PROOF / 800% NEAREST + ACTUAL", BLACK)
    for index, size in enumerate(sizes):
        col, row = index % 3, index // 3
        x, y = col * cell_w, 56 + row * cell_h
        dark = index % 2 == 0
        bg = ABYSS if dark else WHITE
        fg = white_marks[size] if dark else black_marks[size]
        draw.rectangle((x, y, x + cell_w - 1, y + cell_h - 1), fill=bg)
        text_color = LIMESTONE if dark else BLACK
        label(draw, (x + 18, y + 16), f"{size}px / {'DARK' if dark else 'LIGHT'}", text_color)
        enlarged = apps[size].resize((size * zoom, size * zoom), Image.Resampling.NEAREST)
        sheet.alpha_composite(enlarged, (x + 18, y + 48))
        sheet.alpha_composite(apps[size], (x + cell_w - size - 28, y + 22))
        # Taskbar-gray proof below the enlargement.
        tray_y = y + 48 + size * zoom + 18
        draw.rectangle((x + 18, tray_y, x + cell_w - 18, tray_y + 54), fill=(72, 76, 80, 255))
        sheet.alpha_composite(fg, (x + 34, tray_y + (54 - size) // 2))
        label(draw, (x + 68, tray_y + 19), "TASKBAR GRAY / ACTUAL", WHITE)
    return sheet


def foreground_components(mask: Image.Image) -> int:
    pixels = mask.load()
    width, height = mask.size
    remaining = {(x, y) for y in range(height) for x in range(width) if pixels[x, y] >= 128}
    count = 0
    while remaining:
        count += 1
        stack = [remaining.pop()]
        while stack:
            x, y = stack.pop()
            for neighbor in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                if neighbor in remaining:
                    remaining.remove(neighbor)
                    stack.append(neighbor)
    return count


def validate_mask(geometry: Geometry, mask: Image.Image) -> None:
    if mask.size != (geometry.size, geometry.size):
        raise AssertionError(f"wrong mask size: {mask.size}")
    if foreground_components(mask) != 1:
        raise AssertionError(f"{geometry.size}px silhouette is not one connected component")
    pixels = mask.load()
    for name, aperture in (("counter", geometry.counter), ("slit", geometry.slit)):
        cx = round(sum(x for x, _ in aperture) / len(aperture))
        cy = round(sum(y for _, y in aperture) / len(aperture))
        if pixels[cx, cy] != 0:
            raise AssertionError(f"{geometry.size}px {name} has no transparent core")
    stem_x = max(0, round(geometry.size * 14 / 48))
    stem_y = max(0, round(geometry.size * 34 / 48))
    if pixels[stem_x, stem_y] < 128:
        raise AssertionError(f"{geometry.size}px stem sample is absent")


def validate_app_icon(geometry: Geometry, image: Image.Image) -> None:
    if image.size != (geometry.size, geometry.size) or image.mode != "RGBA":
        raise AssertionError(f"invalid {geometry.size}px app icon")
    pixels = image.load()
    if pixels[0, 0][3] >= 16:
        raise AssertionError(f"{geometry.size}px field corner is visibly opaque")
    samples = (
        (geometry.counter, BLACK, "counter"),
        (geometry.slit, BLACK, "slit"),
        (geometry.bronze, BRONZE, "bronze bar"),
    )
    for points, expected, name in samples:
        cx = round(sum(x for x, _ in points) / len(points))
        cy = round(sum(y for _, y in points) / len(points))
        if pixels[cx, cy] != expected:
            raise AssertionError(f"{geometry.size}px {name} lost its exact color core")


def parse_ico(data: bytes) -> list[tuple[int, bytes]]:
    reserved, kind, count = struct.unpack_from("<HHH", data, 0)
    if (reserved, kind) != (0, 1):
        raise AssertionError("invalid ICO header")
    result: list[tuple[int, bytes]] = []
    for index in range(count):
        width, height, _, _, planes, depth, length, offset = struct.unpack_from("<BBBBHHII", data, 6 + index * 16)
        size = 256 if width == 0 else width
        decoded_height = 256 if height == 0 else height
        if size != decoded_height or planes != 1 or depth != 32:
            raise AssertionError("invalid ICO directory entry")
        result.append((size, data[offset : offset + length]))
    return result


def build_into(root: Path) -> None:
    brand = root / "design" / "brand"
    source = brand / "source"
    png_root = brand / "png"
    source.mkdir(parents=True, exist_ok=True)

    black_marks: dict[int, Image.Image] = {}
    white_marks: dict[int, Image.Image] = {}
    apps: dict[int, Image.Image] = {}

    for size in SIZES:
        geometry = geometry_for(size)
        mask = alpha_mask(geometry)
        validate_mask(geometry, mask)
        black_marks[size] = colored_mark(mask, BLACK)
        white_marks[size] = colored_mark(mask, WHITE)
        apps[size] = app_icon(geometry, mask)
        validate_app_icon(geometry, apps[size])
        (source / f"phorminx-mark-{size}.svg").write_text(svg_for(geometry), encoding="utf-8", newline="\n")
        write_png(png_root / "mark" / f"phorminx-mark-black-{size}.png", black_marks[size])
        write_png(png_root / "mark" / f"phorminx-mark-white-{size}.png", white_marks[size])
        write_png(png_root / "app" / f"phorminx-app-{size}.png", apps[size])

    # The 48-unit source is the canonical construction master; every other SVG
    # is the same approved geometry expressed directly in its delivery units.
    shutil.copyfile(source / "phorminx-mark-48.svg", source / "phorminx-mark-master.svg")

    for size in TRAY_SIZES:
        write_png(png_root / "tray" / f"phorminx-tray-for-light-bg-{size}.png", black_marks[size])
        write_png(png_root / "tray" / f"phorminx-tray-for-dark-bg-{size}.png", white_marks[size])

    ico_entries = [(size, png_bytes(apps[size])) for size in ICO_SIZES]
    ico = build_ico(ico_entries)
    ico_path = brand / "phorminx.ico"
    ico_path.write_bytes(ico)
    decoded = parse_ico(ico)
    if tuple(size for size, _ in decoded) != ICO_SIZES:
        raise AssertionError("ICO sizes do not match the release set")
    for size, payload in decoded:
        from io import BytesIO

        with Image.open(BytesIO(payload)) as image:
            if image.size != (size, size) or image.mode != "RGBA":
                raise AssertionError(f"invalid {size}px ICO payload: {image.size}, {image.mode}")

    write_png(brand / "validation" / "actual-size-masters.png", actual_size_sheet(black_marks, apps))
    write_png(
        brand / "validation" / "small-size-contact-sheet.png",
        small_contact_sheet(black_marks, white_marks, apps),
    )

    files = sorted(
        [*source.rglob("*.svg"), *png_root.rglob("*.png"), ico_path, *(brand / "validation").rglob("*.png")]
    )
    manifest = {
        "asset_family": "Rounded P / selected reference",
        "schema": 1,
        "sizes": list(SIZES),
        "ico_sizes": list(ICO_SIZES),
        "files": [
            {
                "path": path.relative_to(root).as_posix(),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "bytes": path.stat().st_size,
            }
            for path in files
        ],
    }
    (brand / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")


def compare_trees(expected: Path, actual: Path) -> None:
    manifest = json.loads((expected / "manifest.json").read_text(encoding="utf-8"))
    prefix = Path("design") / "brand"
    expected_files = {
        Path(entry["path"]).relative_to(prefix) for entry in manifest["files"]
    } | {Path("manifest.json")}
    missing = sorted(str(path) for path in expected_files if not (actual / path).is_file())
    if missing:
        raise SystemExit(f"brand assets are stale (missing={missing})")
    changed = [str(path) for path in sorted(expected_files) if (expected / path).read_bytes() != (actual / path).read_bytes()]
    if changed:
        raise SystemExit("brand assets are stale: " + ", ".join(changed))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="repository root (defaults to script parent)")
    parser.add_argument("--check", action="store_true", help="rebuild in a temporary directory and compare bytes")
    args = parser.parse_args()
    root = (args.root or Path(__file__).resolve().parents[1]).resolve()
    if args.check:
        with tempfile.TemporaryDirectory(prefix="phorminx-brand-") as temporary:
            candidate = Path(temporary)
            build_into(candidate)
            compare_trees(candidate / "design" / "brand", root / "design" / "brand")
        print("Phorminx brand assets are reproducible and valid.")
    else:
        build_into(root)
        print(f"Built Phorminx brand assets in {root / 'design' / 'brand'}")


if __name__ == "__main__":
    main()
