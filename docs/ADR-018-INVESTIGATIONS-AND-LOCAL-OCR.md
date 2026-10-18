# ADR 018: Investigations and local screenshot OCR

Status: implemented on `feature/investigations`; hardware acceptance pending
(microphone voice notes, real global-shortcut use and real web pages).

## Scope

Add an **Investigations** tab to the Tauri product shell. An investigation is a
long-lived, local collection of material about one question: pasted text,
typed notes, screenshots (pasted, dropped or opened; text extracted locally),
links (with a one-time preview), voice notes (text only) and pinned assistant
answers. Views are a chronology and a diagram; an assistant answers from the
cards with citations. A dedicated global shortcut files the clipboard into the
active investigation.

## Decisions

### Storage

- Additive tables in the existing schema version, like meetings:
  `investigations`, `investigation_items`, `investigation_ocr_lines`,
  `investigation_nodes`, `investigation_edges` and `investigation_messages`.
  Older binaries ignore them. Deleting an investigation cascades in one
  transaction.
- Investigations are explicit user documents. Dictation **history retention
  does not apply**, and the category-deletion action (dictations, meeting
  transcripts, chats) does not touch them. Data stays until the user deletes
  the card or the investigation.
- Image bytes never enter SQLite. Every image (screenshot, link preview,
  favicon) is decoded, bounded (32 MB input, 40 MP), **re-encoded as PNG** —
  which drops EXIF location and embedded text chunks — and stored as
  `<data folder>/investigations/<investigation id>/<sha256>.png`. The file is
  removed when the last card in that investigation referencing it is deleted,
  and the folder is removed with the investigation. Orphaned folders are
  cleaned at startup. The window only receives an image as a `data:` URL after
  the host checks the hash belongs to the requested investigation.
- The quick-capture target and shortcut live in `persistence_settings`, so the
  shared `settings.toml` schema is unchanged.

### OCR: PaddleOCR ONNX models run by RTen

- No operating-system OCR (Windows.Media.Ocr or similar) and no native
  inference runtime. Models are **downloaded on request** like speech models:
  pinned repository revision, exact byte count and SHA-256, streamed into a
  temporary file and atomically activated (shared helpers from `model.rs`).
  Files are re-hashed before every engine load.
  - Detection: PP-OCRv4 mobile (`ch_PP-OCRv4_det_infer.onnx`, 4,745,517 bytes,
    `d2a7720d…9da9`), multilingual, from `SWHL/RapidOCR` at
    `1cfba2e9…c173`.
  - Recognition: PP-OCRv5 Latin mobile (`rec.onnx`, 7,862,832 bytes,
    `614ffc2d…4409`) and its dictionary (1,634 bytes, `3c0a8a79…13ff`) from
    `monkt/paddleocr-onnx` at `7b02d0a3…de65`. Covers English, Spanish and 30
    other Latin-script languages.
  - All Apache-2.0. Total download 12.6 MB, shown in **Models** and **Setup &
    repair**.
- Inference uses **RTen 0.27** (pure Rust, loads `.onnx` directly). This
  avoids shipping or downloading ONNX Runtime (an ~80 MB archive per release)
  while still running the preferred PaddleOCR models. `ocrs` was not needed.
- `phorminx-ocr` implements the standard pipeline: BGR/ImageNet-normalized
  detection capped at 2,048 px, DB post-processing (0.3 binary threshold, 0.6
  box score, 1.5 unclip) with axis-aligned 8-connected components, rows
  grouped by vertical overlap and split at column gaps, recognition at the
  model's height and greedy CTC with the trailing space class.
- A background worker processes the oldest pending screenshot, pauses while
  dictation is listening/transcribing, unloads the engine after five idle
  minutes, and never overwrites text the user already typed into the card.

Spike (release build, this development PC, generated screenshots, English and
Spanish UI/document/code text plus a 1920×1080 mail client):

| Fixture | Size | Time | CER |
|---|---|---|---|
| English UI | 583×255 | 268 ms | 0.00% |
| Spanish document (accents, ¿¡ñü) | 588×285 | 282–372 ms | 0.00% |
| Dark code/terminal | 576×216 | 216–284 ms | 0.62% (`0K` for `OK`) |
| Small mixed EN/ES | 473×200 | 185–226 ms | 0.00% |
| 1920×1080 mail client | 1920×1080 | 1.2–2.0 s | 0.00% (order-insensitive) |

Overall 0.08% character error rate. Model load is ~20–70 ms. Synthetic
rendering is easier than real photographs or scaled screenshots; treat these
numbers as a regression baseline, not a promise.

### Links

- A pasted URL (http/https, no credentials) becomes a link card at once; a
  worker fetches the page **once**: 10 s budget, 2 MB page, 6 MB preview
  image, 512 KB favicon, no cookies or credentials, at most 5 redirects
  followed manually. Every hop is refused when it names this computer or a
  private, link-local, CGNAT or single-label host, or resolves to one (a
  best-effort DNS-rebinding guard, not a sandbox).
- Only `<title>`, `<meta>` and `<link>` tags in the document head are parsed;
  nothing is executed. Open Graph values win over plain title/description.
- Failure leaves a plain URL card with **Retry**.

### Voice notes

- Voice notes reuse the meeting workspace's text-only note capture, the same
  path as Record-for-action, which uses the configured recognition engine. Only
  the final recognized text becomes a card; no audio is stored by this feature.
- A watcher thread follows the capture to completion even if the window
  closes, so a started note is not lost silently. A running meeting blocks a
  voice note with an explanation.

### Quick capture

- A dedicated `RegisterHotKey` shortcut (default `Ctrl+Alt+Shift+I`,
  configurable or off from the tab) on its own message thread. It sees only its
  own key combination. It refuses the launcher and direct-dictation shortcuts,
  and keeps the working shortcut if a new one cannot be registered. It does not
  touch the dictation hotkey or launcher slots.
- Clipboard text is preferred over a clipboard image, because rich text copied
  from documents often carries a bitmap of the same text. A lone URL becomes a
  link card.
- Confirmation is a small unfocused, always-on-top window in the work-area
  corner. The dictation overlay is untouched.

### Diagram

- Plain SVG with HTML node bodies (`foreignObject`). Pan, wheel zoom, drag, Fit.
  Viewport is saved per investigation. New cards land in an **Unplaced** tray.
  Removing a card node returns it to the tray; the card itself is kept.
- Every node has a **+** button that adds a note, link, image, voice note or
  concept already connected to it. Dragging from a node's lower handle onto
  another node connects them. Connections are undirected and de-duplicated.

### Assistant

- Same provider configuration as Meetings (`assistant.json`; local Ollama by
  default), including the optional fallback model. Context is the outline
  (every card id, kind and label; concepts; user-drawn connections) plus up to
  eight cards ranked by question-word overlap, bounded to 24k characters.
  Card text is marked as untrusted material.
- Answers stream into `investigation_messages` and cite `[#id]`; the window turns
  known ids into chips that jump to the card. Answers can be pinned as cards.
- **Suggest connections** is user-initiated. The reply is parsed as JSON and
  validated against existing nodes and connections. Suggestions are stored as
  dashed `suggested` edges that the user accepts or dismisses; nothing is
  connected automatically.

### Export

- Markdown export: chronological cards with kind, time (UTC), text, extracted
  text and URLs, plus accepted connections. Copy or save from the window.

## Shared-file footprint

New code is in new files: `phorminx-ocr`, `investigation*.rs`,
`tauri_investigations.rs`, `registered_hotkey.rs`, `investigations.{js,css}`,
`investigation-model.js`, `toast.*`. Edits to shared files are small and
additive:

- workspace and app `Cargo.toml`: new crate and dependencies; `arboard`
  `image-data`; `image` `ico`.
- `tauri_shell.rs`: manage the state, register commands, install the shortcut,
  and pause OCR while dictating.
- `tauri.conf.json`: `img-src 'self' data:`, and HTML5 file drop
  (`dragDropEnabled: false`).
- `product.html` and `product.js`: navigation, section, Models/Setup cards,
  and a route event.
- `model.rs`: two helpers made `pub(crate)`.
- `tauri_meetings.rs`: a read-only note-state accessor.

## Verification

Automated: persistence integration tests (CRUD, cascade, retention
independence, OCR queue, edits preserved, image reference counting, diagram
rules, message lifecycle, validation); OCR unit tests (CTC, DB post-processing,
row grouping, bounds); service tests (capture classification, image storage and
cleanup, + attachment, quick capture, export); link tests (URL normalization,
private-target refusal, metadata parsing, charsets); an assistant end-to-end
test against a fake local Ollama (retrieval context, streaming, citations,
pinning, suggestions as dashed edges); registered-hotkey registration and
release; and Node tests for window helpers.

Not verified automatically: real microphone voice notes, the global shortcut
from other applications, toast placement across DPI/multi-monitor setups,
previews of real websites, and OCR on real-world screenshots beyond the spike.
