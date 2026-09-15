# Phorminx identity assets

This directory contains the production **Rounded P / selected reference**
family. Versioned vector sources and deterministic exports define the
production identity.

## Construction

- `source/phorminx-mark-master.svg` is the authored 48-unit color construction
  master: a rounded Abyss square, a heavy Limestone P, one black counter, one
  black vertical slit, and one restrained Bronze inner bar.
- `source/phorminx-mark-{size}.svg` records the same construction directly in
  each Windows delivery size.
- `png/mark` contains transparent black and white pixel-hinted masters at 16,
  20, 24, 32, 40, 48, 64, 128, 256, and 512 px.
- `png/tray` contains transparent single-color light-background and
  dark-background assets at the three tray sizes.
- `png/app` contains the selected application family. Its inset rounded Abyss
  field keeps transparent outer corners and stable contrast; the inner bar is
  Bronze `#A87542` and all remaining geometry is flat color.
- `phorminx.ico` embeds independently rendered 16, 20, 24, 32, 40, 48, 64,
  128, and 256 px PNG entries.
- `validation` contains the actual-size master strip and an 800% nearest-
  neighbor small-size proof.
- `manifest.json` records output sizes, byte counts, and SHA-256 hashes.

## Rebuild

From the repository root:

```powershell
python .\scripts\build-brand-assets.py
python .\scripts\build-brand-assets.py --check
```

The build needs Python 3 and Pillow. It renders every requested canvas from
the normalized 48-unit geometry; it never downsamples a larger raster into a
smaller asset. `--check` rebuilds into a temporary directory, compares every
byte, revalidates the connected silhouette and defining color cores, and
parses every ICO directory entry and embedded PNG dimension.

## Visual use

- Use black or white transparent marks on authored surfaces.
- Use the app ICO only for Windows surfaces that need a stable icon field.
- Use `tray-for-light-bg` on a light system surface and `tray-for-dark-bg` on a
  dark one; high-contrast integration should substitute the current Windows
  system foreground color from the same alpha mask.
- Preserve the selected construction: rounded Abyss field, heavy Limestone P,
  black counter and slit, and one Bronze inner bar.
- Do not add a helmet, shield, microphone, waveform, additional slit, external
  leg, gradient, bevel, glow, or drop shadow.

The versioned source geometry and output manifest are the reproducible
reference for this asset family.
