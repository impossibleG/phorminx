#!/usr/bin/env python3
"""Build and validate the deterministic Phorminx identity asset family.

The raster masters are rendered independently at each requested size. Small
sizes use their own hinted geometry; they are not reductions of the 48 px
master. Pillow is the only non-standard dependency.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
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
BLACK = (0, 0, 0, 255)
WHITE = (255, 255, 255, 255)


Point = tuple[float, float]


@dataclass(frozen=True)
class Geometry:
    size: int
    outer: tuple[Point, ...]
    counter: tuple[Point, ...]
    strings: tuple[tuple[Point, ...], ...]
    tension: tuple[Point, ...]


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


def full_geometry(size: int) -> Geometry:
    """Scale the authored 48-unit geometry directly onto a requested canvas."""
    scale = size / 48.0

    def s(points: Iterable[Point]) -> tuple[Point, ...]:
        return tuple((x * scale, y * scale) for x, y in points)

    outer: list[Point] = [(7, 4), (25, 4)]
    outer += cubic((25, 4), (35.2, 4), (41, 9.4), (41, 17.5))
    outer += cubic((41, 17.5), (41, 25.4), (35.0, 30.2), (26.2, 31.0))
    outer += cubic((26.2, 31.0), (22.1, 31.4), (18.7, 30.6), (16, 29.4))
    outer += [(16, 44), (7, 44)]

    counter: list[Point] = [(29, 11)]
    counter += cubic((29, 11), (32.5, 11.1), (34.5, 13.4), (34.5, 17.5), steps=12)
    counter += cubic((34.5, 17.5), (34.5, 21.4), (32.5, 24.3), (29, 25), steps=12)

    strings = (
        ((16.5, 10), (18.5, 10.5), (18.5, 25), (16.5, 25)),
        ((21, 10), (23, 10.25), (23, 24.5), (21, 24.5)),
        ((25.5, 10), (27.5, 10.5), (27.5, 24), (25.5, 24)),
    )
    tension = (
        (26, 27.6),
        (23.5, 29.5),
        (20.5, 31.5),
        (18, 32.6),
        (18, 30.0),
        (21.2, 29.8),
        (23.8, 28.8),
    )
    return Geometry(size, s(outer), s(counter), tuple(s(item) for item in strings), s(tension))


def hinted_geometry(size: int) -> Geometry:
    """Return independently hinted geometry for each Windows raster size."""
    if size >= 40:
        return full_geometry(size)

    if size == 32:
        outer: list[Point] = [(5, 3), (16.5, 3)]
        outer += cubic((16.5, 3), (23.5, 3), (27, 6.5), (27, 11.5), steps=12)
        outer += cubic((27, 11.5), (27, 17), (23, 20), (17.2, 20.7), steps=12)
        outer += cubic((17.2, 20.7), (14.7, 21), (12.5, 20.4), (11, 19.7), steps=8)
        outer += [(11, 29), (5, 29)]
        return Geometry(
            size,
            tuple(outer),
            ((19.5, 7), (21.5, 7), (23, 8.5), (23, 11.5), (23, 14.5), (21.5, 16.5), (19.5, 16.8)),
            (
                ((11, 6.5), (13, 6.5), (13, 17), (11, 17)),
                ((14.5, 6.5), (16.5, 6.8), (16.5, 16.5), (14.5, 16.5)),
                ((17.8, 6.5), (19.8, 6.8), (19.8, 16), (17.8, 16)),
            ),
            ((17.7, 18.4), (15.8, 19.8), (12.2, 22.0), (12.2, 20.1), (14.5, 20), (16.2, 19.2)),
        )

    if size == 24:
        outer: list[Point] = [(3.5, 2), (12.5, 2)]
        outer += cubic((12.5, 2), (18, 2), (21, 5), (21, 9), steps=10)
        outer += cubic((21, 9), (21, 13.1), (17.8, 15.5), (13.2, 15.8), steps=10)
        outer += cubic((13.2, 15.8), (11, 16), (9.4, 15.5), (8, 15), steps=6)
        outer += [(8, 22), (3.5, 22)]
        return Geometry(
            size,
            tuple(outer),
            ((15.7, 5.5), (17.5, 5.5), (18.5, 7), (18.5, 9), (18.5, 11.2), (17.3, 12.7), (15.7, 12.8)),
            (
                ((8.2, 5), (10.2, 5), (10.2, 13), (8.2, 13)),
                ((11.8, 5), (13.8, 5.3), (13.8, 12.8), (11.8, 12.8)),
            ),
            ((13.8, 14), (12.2, 15.2), (9.2, 17.0), (9.2, 15.5), (11.2, 15.3), (12.5, 14.6)),
        )

    if size == 20:
        outer: list[Point] = [(3, 1.5), (10.4, 1.5)]
        outer += cubic((10.4, 1.5), (15, 1.5), (17.5, 4), (17.5, 7.5), steps=10)
        outer += cubic((17.5, 7.5), (17.5, 11), (15, 13), (11, 13.3), steps=10)
        outer += cubic((11, 13.3), (9.2, 13.5), (7.8, 13), (6.5, 12.5), steps=6)
        outer += [(6.5, 18.5), (3, 18.5)]
        return Geometry(
            size,
            tuple(outer),
            ((13, 4.5), (14.4, 4.5), (15.2, 5.8), (15.2, 7.5), (15.2, 9.3), (14.3, 10.5), (13, 10.7)),
            (
                ((6.7, 4), (8.7, 4), (8.7, 10.8), (6.7, 10.8)),
                ((10.2, 4), (12.2, 4.2), (12.2, 10.5), (10.2, 10.5)),
            ),
            ((11.7, 11.8), (10.3, 12.8), (7.6, 14.5), (7.6, 13.1), (9.3, 13.0), (10.5, 12.3)),
        )

    if size == 16:
        outer: list[Point] = [(2, 1), (8.2, 1)]
        outer += cubic((8.2, 1), (12, 1), (14, 3), (14, 6), steps=8)
        outer += cubic((14, 6), (14, 9.2), (11.8, 10.8), (8.6, 11), steps=8)
        outer += cubic((8.6, 11), (7.2, 11.1), (6.1, 10.8), (5, 10.3), steps=5)
        outer += [(5, 15), (2, 15)]
        return Geometry(
            size,
            tuple(outer),
            ((10, 3.5), (11.3, 3.5), (12, 4.5), (12, 6), (12, 7.5), (11.2, 8.5), (10, 8.6)),
            (((6, 3), (8, 3), (8, 9), (6, 9)),),
            ((9.2, 9.2), (8, 10.1), (6.1, 11.5), (6.1, 10.2), (7.4, 10.0), (8.3, 9.5)),
        )

    raise ValueError(f"unsupported hinted size: {size}")


def svg_points(points: Sequence[Point]) -> str:
    return " ".join(f"{x:.3f},{y:.3f}".rstrip("0").rstrip(".") for x, y in points)


def svg_for(geometry: Geometry) -> str:
    holes = [geometry.counter, *geometry.strings, geometry.tension]
    path_parts = ["M " + " L ".join(svg_points([point]) for point in geometry.outer) + " Z"]
    path_parts.extend("M " + " L ".join(svg_points([point]) for point in hole) + " Z" for hole in holes)
    path = " ".join(path_parts)
    return (
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        f"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{geometry.size}\" height=\"{geometry.size}\" "
        f"viewBox=\"0 0 {geometry.size} {geometry.size}\">\n"
        "  <title>Phorminx Tensioned P / Laconic cut</title>\n"
        "  <desc>One-color master. Three size-dependent string apertures and one internal asymmetric tension cut.</desc>\n"
        f"  <path fill=\"#000000\" fill-rule=\"evenodd\" d=\"{path}\"/>\n"
        "</svg>\n"
    )


def scaled(points: Sequence[Point], multiplier: int) -> list[tuple[int, int]]:
    return [(round(x * multiplier), round(y * multiplier)) for x, y in points]


def alpha_mask(geometry: Geometry) -> Image.Image:
    large = Image.new("L", (geometry.size * SUPERSAMPLE, geometry.size * SUPERSAMPLE), 0)
    draw = ImageDraw.Draw(large)
    draw.polygon(scaled(geometry.outer, SUPERSAMPLE), fill=255)
    for hole in (geometry.counter, *geometry.strings, geometry.tension):
        draw.polygon(scaled(hole, SUPERSAMPLE), fill=0)
    mask = large.resize((geometry.size, geometry.size), Image.Resampling.LANCZOS)

    # Preserve unambiguous transparent cores in every aperture after antialiasing.
    hinted = ImageDraw.Draw(mask)
    for string in geometry.strings:
        left = math.ceil(min(x for x, _ in string))
        right = math.ceil(max(x for x, _ in string)) - 1
        top = math.ceil(min(y for _, y in string))
        bottom = math.ceil(max(y for _, y in string)) - 1
        if right >= left and bottom >= top:
            hinted.rectangle((left, top, right, bottom), fill=0)
    cx = round(sum(x for x, _ in geometry.tension) / len(geometry.tension))
    cy = round(sum(y for _, y in geometry.tension) / len(geometry.tension))
    hinted.point((cx, cy), fill=0)
    return mask


def colored_mark(mask: Image.Image, color: tuple[int, int, int, int]) -> Image.Image:
    image = Image.new("RGBA", mask.size, color)
    image.putalpha(mask)
    return image


def app_icon(mask: Image.Image) -> Image.Image:
    image = Image.new("RGBA", mask.size, ABYSS)
    mark = Image.new("RGBA", mask.size, LIMESTONE)
    mark.putalpha(mask)
    image.alpha_composite(mark)
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
    label(draw, (24, 18), "PHORMINX / ACTUAL-SIZE MASTERS / 100%", BLACK)
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
    label(draw, (24, 78), "APP ICO FIELD (SELECTED SIZES, ACTUAL)", BLACK)
    ax = 24
    for size in (16, 20, 24, 32, 40, 48, 64, 128, 256):
        sheet.alpha_composite(apps[size], (ax, 104))
        ax += size + 14
    return sheet


def small_contact_sheet(black_marks: dict[int, Image.Image], white_marks: dict[int, Image.Image]) -> Image.Image:
    sizes = (16, 20, 24, 32, 40, 48)
    zoom = 8
    cell_w, cell_h = 420, 460
    sheet = Image.new("RGBA", (cell_w * 3, cell_h * 2 + 56), WHITE)
    draw = ImageDraw.Draw(sheet)
    label(draw, (20, 18), "PHORMINX / SMALL-SIZE PROOF / 800% NEAREST + ACTUAL", BLACK)
    for index, size in enumerate(sizes):
        col, row = index % 3, index // 3
        x, y = col * cell_w, 56 + row * cell_h
        dark = index % 2 == 0
        bg = ABYSS if dark else WHITE
        fg = white_marks[size] if dark else black_marks[size]
        draw.rectangle((x, y, x + cell_w - 1, y + cell_h - 1), fill=bg)
        text_color = LIMESTONE if dark else BLACK
        label(draw, (x + 18, y + 16), f"{size}px / {'DARK' if dark else 'LIGHT'}", text_color)
        enlarged = fg.resize((size * zoom, size * zoom), Image.Resampling.NEAREST)
        sheet.alpha_composite(enlarged, (x + 18, y + 48))
        sheet.alpha_composite(fg, (x + cell_w - size - 28, y + 22))
        # Taskbar-gray proof below the enlargement.
        tray_y = y + 48 + size * zoom + 18
        draw.rectangle((x + 18, tray_y, x + cell_w - 18, tray_y + 54), fill=(72, 76, 80, 255))
        sheet.alpha_composite(white_marks[size], (x + 34, tray_y + (54 - size) // 2))
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
    for index, aperture in enumerate(geometry.strings, start=1):
        cx = round(sum(x for x, _ in aperture) / len(aperture))
        cy = round(sum(y for _, y in aperture) / len(aperture))
        if pixels[cx, cy] != 0:
            raise AssertionError(f"{geometry.size}px string {index} has no transparent core")
    tx = round(sum(x for x, _ in geometry.tension) / len(geometry.tension))
    ty = round(sum(y for _, y in geometry.tension) / len(geometry.tension))
    if pixels[tx, ty] != 0:
        raise AssertionError(f"{geometry.size}px tension cut has no transparent core")
    stem_x = max(0, round(geometry.size * 7 / 48))
    stem_y = max(0, round(geometry.size * 36 / 48))
    if pixels[stem_x, stem_y] < 128:
        raise AssertionError(f"{geometry.size}px stem sample is absent")


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
        geometry = hinted_geometry(size)
        mask = alpha_mask(geometry)
        validate_mask(geometry, mask)
        black_marks[size] = colored_mark(mask, BLACK)
        white_marks[size] = colored_mark(mask, WHITE)
        apps[size] = app_icon(mask)
        (source / f"phorminx-mark-{size}.svg").write_text(svg_for(geometry), encoding="utf-8", newline="\n")
        write_png(png_root / "mark" / f"phorminx-mark-black-{size}.png", black_marks[size])
        write_png(png_root / "mark" / f"phorminx-mark-white-{size}.png", white_marks[size])
        write_png(png_root / "app" / f"phorminx-app-{size}.png", apps[size])

    # The 48-unit source is the canonical construction master; the remaining
    # SVGs are its deliberately simplified or directly scaled family members.
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
    write_png(brand / "validation" / "small-size-contact-sheet.png", small_contact_sheet(black_marks, white_marks))

    files = sorted(
        [*source.rglob("*.svg"), *png_root.rglob("*.png"), ico_path, *(brand / "validation").rglob("*.png")]
    )
    manifest = {
        "asset_family": "Tensioned P / Laconic cut",
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
