# ADR-003: Phase 1 Windows walking skeleton

- Status: accepted
- Date: 2026-08-30

## Decision

Build the first end-to-end product path as a native Rust console shell before adding the tray and settings UI. It keeps one `WhisperContext` resident on a dedicated worker and uses a dedicated Win32 low-level-keyboard-hook message thread for `Ctrl+Alt+Space` hold/release events.

Capture the foreground window and focused child at the chord's key-down edge. On completion, put the transcript on the clipboard and inject `Ctrl+V` only if the exact target remains active and is a writable, visible, non-password classic Win32 `Edit` control. Unknown, custom, browser, Electron, elevated, changed, or otherwise uncertain targets degrade to clipboard-only.

Do not restore the prior clipboard in Phase 1. Restoration without holding the clipboard lock introduces a race that could overwrite a user's concurrent copy. Windows clipboard-history monitoring is disabled for the Phorminx payload where the platform API permits it.

## Rationale

The console shell isolates lifecycle and safety behavior from tray/UI work. A resident model removes per-dictation load latency. Separating the hook, audio callback, app state loop, and recognition worker prevents slow inference from blocking keyboard delivery or silently queuing later insertions.

Fail-closed insertion deliberately supports fewer applications at first. Foreground-window identity alone cannot prove that focus did not move into a password field inside the same application.

## Consequences

- The initial global shortcut is temporary and not configurable yet.
- Notepad-style Win32 edit controls can receive automatic paste.
- Most modern applications initially report `Ready to paste` and retain the text on the clipboard.
- Clipboard restoration, UI Automation sensitive-field inspection, application allowlists, tray UI, and overlay remain later work.
- Logs contain state, event name, correlation ID, and timings only; transcript content is never logged.
