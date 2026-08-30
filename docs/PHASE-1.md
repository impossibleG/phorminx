# Phase 1: push-to-talk walking skeleton

The current executable implements:

```text
hold Ctrl+Alt+Space
    → capture target and microphone
release shortcut
    → reject silence
    → transcribe on the resident local Whisper worker
    → deterministically normalize spacing
    → guarded paste or clipboard-only fallback
```

## Run

```powershell
. .\scripts\Enter-PhorminxDevShell.ps1 -Vulkan

cargo run --release -p phorminx-app --features vulkan -- `
  --model models/ggml-base.en.bin
```

The model loads before `Ready` appears. Hold `Ctrl+Alt+Space` in the target application, speak, then release all three keys. Press `Ctrl+C` in the Phorminx console to exit.

## Safety policy

- Target identity is captured at key-down and checked again immediately before paste.
- Injected keyboard events are ignored by the global hook.
- A busy runtime rejects new activations instead of queuing them.
- Quiet or shorter-than-200-ms recordings are rejected.
- Automatic paste is limited to conventional writable, visible, unmasked Win32 `Edit` controls.
- Every other target receives a clipboard-only result.
- Clipboard contents are deliberately not restored yet, avoiding a concurrent-copy overwrite race.
- Transcript and audio content never appears in structured logs.

## Verification

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Optimized service-lifecycle smoke test:
cargo run --release -p phorminx-app --features vulkan -- `
  --model models/ggml-base.en.bin `
  --smoke-test
```

Manual insertion testing starts with classic Notepad. Browser and Electron controls are expected to fall back to `Ready to paste` until UI Automation and explicit sensitive-target policies are implemented.

## Live validation

On 2026-08-30, the reference machine completed a physical `Ctrl+Alt+Space` hold-to-talk cycle into classic Notepad:

- The hook captured key-down and key-up without swallowing input.
- The microphone recording remained usable after a recoverable WASAPI overrun notification.
- Resident `base.en` inference on Vulkan completed in 542 ms.
- The original focused Notepad edit control passed target and password-style revalidation.
- Guarded paste was injected and the user confirmed that the dictated sentence appeared correctly.

Earlier attempts also exercised the fail-closed paths for a held modifier, an unavailable target, a busy reactivation, and a recoverable audio backend warning. No uncertain attempt injected text.

The walking skeleton is functionally validated. Phase 1 remains open until the status-feedback layer and 500-dictation stability/wrong-target soak are complete.
