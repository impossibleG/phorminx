# Phorminx identity assets

This directory contains the production **Tensioned P / Laconic cut** family.
It is a deterministic redraw informed by the promoted `mark-laconic-p-v2`
concept, not a trace or vectorization of generated artwork.

## Construction

- `source/phorminx-mark-master.svg` is the authored 48-unit construction
  master: one connected P silhouette, three string apertures, one counter, and
  exactly one internal asymmetric tension cut.
- `source/phorminx-mark-{size}.svg` records the deliberate family member used
  at each Windows size. The 16 px variant has one string aperture; 20/24 px
  have two; 32 px and larger have three.
- `png/mark` contains transparent black and white pixel-hinted masters at 16,
  20, 24, 32, 40, 48, 64, 128, 256, and 512 px.
- `png/tray` contains transparent single-color light-background and
  dark-background assets at the three tray sizes.
- `png/app` contains the application family. It uses a full-bleed Abyss field
  for stable contrast and never introduces a rounded-square container,
  gradient, metallic edge, shadow, or bronze ornament.
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
that size's own geometry; it never downsamples the 48 px raster into smaller
assets. `--check` rebuilds into a temporary directory, compares every byte,
revalidates the connected silhouette and aperture cores, and parses every ICO
directory entry and embedded PNG dimension.

## Visual use

- Use black or white transparent marks on authored surfaces.
- Use the app ICO only for Windows surfaces that need a stable icon field.
- Use `tray-for-light-bg` on a light system surface and `tray-for-dark-bg` on a
  dark one; high-contrast integration should substitute the current Windows
  system foreground color from the same alpha mask.
- Do not add a helmet, shield, microphone, waveform, rounded-square shell,
  additional string, external leg, gradient, bevel, glow, or drop shadow.
- Do not recolor the tension cut independently. It is negative space.

The concept history and exact generation prompts are recorded in
`docs/BRAND-ASSET-LINEAGE.md`.
