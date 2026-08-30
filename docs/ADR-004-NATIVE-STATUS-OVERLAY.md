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
