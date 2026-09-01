# Phorminx brand asset lineage

Status: production vector/raster family built from the user-selected reference

Date: 2026-09-01

## Decision record

The production family is the user-selected **Rounded P** shown in the attached
1024 × 1024 PNG reference (SHA-256
`ccfeed9f429d456b1261258335a80249d1f23e250168538f5fae84a829edaabe`).
The attachment is an approved visual source, not an instruction document.
Its measured color and optical relationships supersede the earlier promoted
`mark-laconic-p-v2.png` direction:

1. Preserve the inset rounded Abyss square and transparent outer corners.
2. Preserve the heavy Limestone P with its long, quiet vertical stem.
3. Preserve one black rectangular slit and one black rounded counter.
4. Preserve one restrained Bronze bar inside the counter.
5. Keep the mark flat: no gradient, lighting, texture, bevel, or shadow.

The attachment itself is not shipped as a production master. Its measured
geometry is normalized into a 48-unit authored construction in
`scripts/build-brand-assets.py`, emitted as the SVG family in
`design/brand/source`, and checked byte-for-byte by the same script.

## Production construction prompt

The deterministic source implements this exact reviewed brief:

> On a 48 × 48 transparent viewBox, draw an inset rounded square in Abyss
> `#0A0C0D`. Place a heavy Limestone `#E8E5DD` capital P from x≈10.31..38.44
> and y≈7.03..39.38. Use a straight stem, a broad cubic outer bowl centered at
> y≈17.58, a black rounded counter, and one separate black vertical slit. Add
> one narrow Bronze `#A87542` bar inside the left portion of the counter. Use
> flat fills only: no stroke, filter, gradient, blur, texture, or shadow.

All ten PNG sizes and all nine ICO entries are rendered directly from those
normalized coordinates. At small sizes the build guarantees an exact core
pixel for the black counter, black slit, and Bronze bar, while the transparent
tray mark retains open counter/slit cores. No raster master is reused or
reduced.

## Exact generated-concept prompts

The following prompts were sent through the built-in ImageGen tool. They are
preserved verbatim for provenance. The generated images contain no embedded
prompt metadata.

### `mark-tensioned-p-v1.png`

```text
Use case: logo-brand
Asset type: Phorminx Windows desktop application icon concept, first of three identity studies
Primary request: create an original abstract mark called “Tensioned P” for Phorminx, a private local speech-to-text application. Build an unmistakable capital-P silhouette from the structural geometry of an ancient Greek phorminx stringed instrument: one severe vertical arm, a controlled curved yoke/soundbox forming the bowl, and exactly three narrow negative-space string cuts implying disciplined audio cadence. The mark should feel subtly pretentious, elevated, laconic, privately powerful, and quietly confident—not loud or theatrical.
Style/medium: flat vector-friendly logo mark, monolithic geometric construction, excellent optical balance, strong silhouette, minimal forms
Composition/framing: one single centered symbol only, straight-on, generous padding, presented large on a plain off-white background
Color palette: near-black iron mark with one extremely restrained muted-bronze inner plane or edge; must also clearly work as one-color black
Constraints: no wordmark, no text, no letters besides the abstract P inherent in the mark, no gradients, no 3D, no mockup, no shadows, no watermark; must remain legible at 16px; no more than two visual metaphors
Avoid: Spartan helmet, warrior face, microphone, speech bubble, quotation marks, laurel wreath, Greek-key border, Roman columns, shield outline, red cape, gaming/esports logo aggression, generic SaaS sparkle, rounded app-square container
```

### `mark-stringed-lambda-v1.png`

```text
Use case: logo-brand
Asset type: Phorminx Windows desktop application icon concept, second identity study
Primary request: create an original abstract mark called “Stringed Lambda” for Phorminx, a local speech-to-text application. Construct a severe, compact ancient-instrument yoke silhouette whose negative space forms a subtle Greek lambda and whose exactly three tensioned vertical strings also read as a minimal voice cadence. The symbol should imply disciplined transformation from voice to text without depicting a microphone or letterform literally. It must feel elevated, laconic, privately powerful, and ownable.
Style/medium: truly flat vector logo mark, hard geometric silhouette with balanced negative space, contemporary Swiss-level restraint with a faint archaeological severity
Composition/framing: one single centered symbol only, straight-on, generous padding, plain warm off-white background
Color palette: solid near-black iron only; optional single tiny muted-bronze rectangular accent no larger than 8% of the mark
Constraints: no text, no wordmark, no gradients, no lighting, no bevel, no texture, no 3D, no shadows, no mockup, no watermark; maximum five major shapes; must remain legible at 16px and work as a one-color tray glyph
Avoid: Spartan helmet, warrior, face, shield circle, microphone, speech bubble, quotation mark, waveform squiggle, laurel, Greek-key border, columns, red, esports aggression, fantasy crest, generic SaaS sparkle, rounded app-square container
```

### `mark-laconic-cut-v1.png`

```text
Use case: logo-brand
Asset type: Phorminx Windows desktop application icon concept, third identity study
Primary request: create an original compact abstract mark called “Laconic Cut” for Phorminx, a private local speech-to-text application. Begin with a single heavy geometric vertical slab and carve one disciplined upper counter so the silhouette hints at a capital P. Inside that counter, use exactly three thin vertical negative-space cuts suggesting tensioned strings and ordered voice. Add one sharp diagonal terminal that quietly evokes the Spartan lambda without drawing a shield or weapon. The result should be unmistakable, severe, intelligent, premium without announcing itself, and highly recognizable as a tiny desktop/tray icon.
Style/medium: truly flat vector logo mark, reductive modernist geometry, one contiguous silhouette, balanced negative space, no ornamental detail
Composition/framing: one single centered symbol only, straight-on, generous padding, plain warm off-white background
Color palette: solid iron black mark; one optional flat muted-bronze cut or inset no larger than 10% of total area
Constraints: no text, no wordmark, no gradient, no lighting, no bevel, no texture, no 3D, no shadow, no mockup, no watermark; must work perfectly in pure monochrome and remain legible at 16px
Avoid: literal lyre illustration, Spartan helmet, warrior, face, shield outline, microphone, speech bubble, quotes, waveform, laurel, Greek-key border, columns, red, esports emblem, fantasy crest, generic SaaS icon, rounded app-square container
```

### `mark-laconic-p-v2.png` (edit of Laconic Cut)

```text
Use case: precise-object-edit
Asset type: refined Phorminx logo concept
Input images: Image 1 is the edit target, the “Laconic Cut” mark
Primary request: change only the lower-right diagonal leg so the mark reads unmistakably as a capital P rather than an R. Remove the protruding leg and bronze triangle completely. Preserve the heavy vertical stem, upper bowl, and exactly three vertical negative-space string cuts. Introduce a single subtle lambda-like diagonal cut inside the lower edge of the upper bowl if needed, contained fully inside the P silhouette and still legible at 16px.
Style/medium: truly flat vector-friendly logo, solid geometric silhouette
Color palette: pure solid iron black on warm off-white; no accent color in this revision
Constraints: preserve the overall proportions, centered composition, generous padding, and severe quiet-confidence character; one contiguous P silhouette; no gradients, highlights, shadows, outlines, texture, 3D, text, mockup, or watermark; change only the lower-right leg/terminal and flatten all rendering
Avoid: R silhouette, literal lyre, helmet, shield, microphone, speech bubble, ornament
```

### `shell-home-reference-v1.png`

```text
Use case: ui-mockup
Asset type: high-fidelity desktop product UI reference for implementation
Primary request: design the unified Home screen for PHORMINX, a private local Windows speech-to-text application, governed by this brand north star: subtly pretentious but not arrogant, elevated, laconic, quietly confident, every element intentional, nothing that could belong to a mediocre SaaS dashboard. The app should feel like a disciplined instrument at rest.
Scene/backdrop: one landscape Windows desktop application window only, approximately 1120x760 proportions, dark authored theme
Subject: custom integrated title bar with a tiny severe abstract P/string mark and the exact word “PHORMINX”; narrow left navigation with exact labels “Home”, “History”, “Lexicon”, “Profiles”, “Models”, “Settings”; main Home page with exact status “Ready”, shortcut “CTRL  ALT  SPACE”, a single restrained “Test dictation” action, and compact local readiness rows for “Microphone”, “Whisper”, and “Ollama”; bottom navigation note “LOCAL ONLY”
Style/medium: realistic shippable Windows product UI, restrained editorial hierarchy, precise spacing, one-pixel separators, few large quiet planes, sharp modern geometry with subtle Windows-compatible corner radii
Composition/framing: straight-on full-window view, no surrounding laptop or device mockup, large negative space, deliberate asymmetry, no card grid
Color palette: abyss #0A0C0D, iron #121619, tempered #1B2024, limestone #E8E5DD, ash #A8ADB0, one restrained bronze #A87542 active accent
Typography: clean Windows sans serif, sparse confident copy, monospace only for shortcut and technical tokens
Constraints: render the listed text exactly and no marketing copy; one primary action only; accessible contrast; no gradients, glass, glossy effects, illustrations, photos, shadows, watermark, browser chrome, fake analytics, charts, productivity scores, tips, avatars, account controls, or promotional panels
Avoid: generic SaaS dashboard, cards everywhere, pill-shaped everything, neon waveform, purple/blue gradient, glassmorphism, Spartan helmet, columns, Greek key, laurel, gaming UI, cyberpunk, clutter
```

## Reproduction and validation

Run:

```powershell
python .\scripts\build-brand-assets.py
python .\scripts\build-brand-assets.py --check
```

The build performs these automated checks:

- dimensions and independent geometry at all ten PNG master sizes;
- one connected foreground component;
- an open transparent core in every expected string aperture;
- an open transparent core in the sole tension cut;
- exact ICO directory order, 32-bit entry metadata, embedded PNG dimensions,
  and alpha-capable RGBA decoding;
- byte equality between a clean temporary rebuild and committed outputs.

Human validation uses:

- `design/brand/validation/actual-size-masters.png` at 100%; and
- `design/brand/validation/small-size-contact-sheet.png` at 100%, where each
  small master is also shown at 800% nearest-neighbor enlargement.

The automated and visual asset proofs satisfy construction and reproducibility
work. They do not substitute for the five-person accidental-resemblance test,
Windows High Contrast system-color integration, or executable/installer shell
inspection required by `docs/UI-ACCEPTANCE.md`.
