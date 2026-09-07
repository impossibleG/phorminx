# Shortcut save feedback regression

## Report and confirmed defects

A captured trigger failed on Save with only `Settings could not be saved.` Other
settings could be saved. The exact reported combination has been requested but
has not yet been supplied, so that particular input is not claimed reproduced.

Two defects were reproduced in synthetic tests:

- `SettingsError::InvalidShortcut` fell through to the generic settings error,
  hiding unsupported modifiers, reserved combinations, and duplicate bindings.
- Capture accepted inputs that native validation rejected, including Alt+Space,
  Shift+A, F12, and Ctrl+F4.

## Changes

- Save now reports the specific shortcut validation reason, including duplicates
  between the launcher and direct dictation. Persistence failures remain redacted.
- Capture rejects the unsupported combinations immediately, retains the previous
  draft and active capture, and allows retry or Escape to cancel.
- Capture feedback clears on success, cancellation, navigation, and focus loss.
- Existing native shortcut restrictions and registration checks are unchanged.
  No recognition, formatting, microphone, or delivery behavior was changed.

## Validation

- `cargo test -p phorminx-ui -p phorminx-app -p phorminx-windows`: passed (410
  tests passed, 7 opt-in native tests ignored).
- `cargo clippy -p phorminx-ui -p phorminx-app -p phorminx-windows --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Regressions exercise all five shortcut error categories, unchanged persisted
  settings after rejection, error publication to the shell, capture retry/cancel,
  and saving a valid Ctrl+Shift+F9 preference without a speech-model reload.
- Independent read-only QA found no blocking issue in this scoped change.
- Tests use temporary settings and generated UI events; no personal transcripts
  or installed application settings were changed.

## Remaining confirmation and existing limitation

Confirm the originally attempted shortcut and whether it targets the launcher or
direct dictation, then verify its expected result with the updated installed app.
This is not a native physical-keyboard verification.

egui-winit does not expose the Windows logo modifier through `egui::Modifiers`
on Windows. Physical Win-key capture therefore has a pre-existing limitation:
the logo modifier can be omitted. Typed Win combinations are explicitly rejected
by native validation. This patch does not claim to fix physical Win-key capture or
add support for Windows-key shortcuts.

The updated installer is built separately in `artifacts/shortcut-save-fix/`; the
previous installer is preserved. Installing this patch remains an explicit action.

The desktop/Vulkan release and installer build passed. Artifact:
`artifacts/shortcut-save-fix/Phorminx-0.1.0-x64-setup.exe` (12,920,009 bytes), SHA256
`D5ABB6FB713C85B552E1206CA1CEADB5351AD5DF5AFDD2DEFC87FBCB19603FB7`.
This is an unsigned local development installer, not a public distribution build.
`scripts/Test-ReleaseAssets.ps1` passed.
