# ADR-004: Native non-activating status overlay

- Status: accepted
- Date: 2026-08-30

## Decision

Implement Phase 1 status feedback as a tiny native Win32/GDI popup on its own message thread. The overlay accepts only fixed `OverlayStatus` enum values and never receives transcript text.

Use `WS_EX_NOACTIVATE`, `WS_EX_TOOLWINDOW`, `WS_EX_TOPMOST`, and `WS_EX_TRANSPARENT`, return `HTTRANSPARENT` during hit testing, and show or position the window only with no-activate flags. Place the DPI-scaled rounded panel near the bottom center of the active monitor.

Loading, listening, and transcribing remain visible until superseded. Ready, inserted, clipboard-ready, no-speech, and error results use generation-safe timers so a stale queued timer cannot hide a newer status.

## Rationale

The first live walking-skeleton test showed that invisible processing encourages duplicate activations and window switching. A fixed-status overlay resolves that ambiguity without introducing the memory footprint or focus behavior of the future settings UI.

Keeping rendering native and content-free also reduces privacy and target-stealing risk. The later `egui` settings and history windows can remain independent from this non-activating surface.

## Consequences

- Phase 1 gains clear feedback with no new UI framework dependency.
- The design is intentionally simple and Windows-specific.
- Styling, animation, accessibility settings, and configurable placement remain Phase 2 work.
- Live testing must confirm that the overlay never becomes foreground or changes the captured target.

## Amendment (October 2026): live text, notes and Expand

The pill may now show content next to the fixed label:

- **Live text** while dictating (Listening, Transcribing, Cleaning): the
  newest recognized words, from Vosk's partial result in Instant mode and
  from committed partial transcripts in Accurate mode. The pill grows to two
  lines and drops leading words until the text fits. Settings > General >
  "Show live text while dictating" (on by default) turns it off. The text
  stays in memory on the overlay thread and is cleared when the next
  dictation starts.
- A short **note** on result states, such as why AI formatting was skipped.
  Notes are fixed, content-free strings.
- An **Expand** button on Inserted and Ready to paste, for a few seconds
  longer than the bare result. Only the button's rectangle answers hit
  testing with `HTCLIENT`; the rest of the pill stays click-through and the
  window still never activates (`MA_NOACTIVATE`). A click hides the pill and
  opens the review panel.

The review panel is an ordinary Tauri window. Copy is its default action.
Replace runs only when pressed, and only for text that was pasted into a
classic Win32 Edit control: the control's text is read back, the inserted
passage is located (preferring the one ending at the caret) and only that
range is replaced with `EM_SETSEL`/`EM_REPLACESEL`. No keys are sent and
nothing is undone. If the passage was edited, removed or is ambiguous, the
control is left alone and the new text is copied instead.
