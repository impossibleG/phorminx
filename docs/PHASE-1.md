# Phase 1: push-to-talk walking skeleton

> Historical milestone and reference-machine evidence. ADR-015 supersedes the
> 120-second capture ceiling and complete-recording retention below. Later
> phases also expanded insertion compatibility and the product UI; this file
> is not the current feature or release-acceptance checklist.

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

## Status feedback

A small native overlay appears near the bottom center of the active monitor. It is topmost, click-through, and uses `WS_EX_NOACTIVATE` plus no-activate show/position calls, so displaying it cannot become the dictation target.

It presents only fixed application states:

- Loading and ready
- Listening
- Transcribing
- Inserted
- Ready to paste
- No clear speech
- Error

No transcript or audio content is sent to or rendered by the overlay. Result and error states auto-hide; active work states remain visible until the runtime advances.

## Safety policy

- Target identity is captured at key-down and checked again immediately before paste.
- Injected keyboard events are ignored by the global hook.
- A busy runtime rejects new activations instead of queuing them.
- Quiet or shorter-than-200-ms recordings are rejected.
- Capture uses a preallocated, callback-safe ring buffer and rejects recordings beyond 120 seconds rather than using truncated audio.
- Native microphone audio is converted to 16 kHz with a band-limited FFT resampler after capture stops.
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

# Visual overlay QA without loading a speech model:
cargo run --release -p phorminx-app --features vulkan -- --overlay-demo
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

After the overlay milestone, a second physical Notepad dictation validated the combined UI path:

- Listening, transcribing, and inserted states were visible in the native overlay.
- The user confirmed the overlay never stole focus and the sentence appeared correctly.
- Guarded insertion still targeted the original Notepad edit control.
- Resident Vulkan inference completed in 922 ms.

The walking skeleton and its status-feedback layer are functionally validated. The automated 500-cycle stability gates are complete; Phase 1 remains open only for the physical Windows wrong-target matrix documented in `PHASE-1-SOAK.md`.

The audio-hardening milestone then replaced callback mutexes and unbounded growth with a preallocated SPSC ring, replaced linear interpolation with band-limited FFT resampling, and expanded capture to every PCM format exposed by CPAL 0.18. Automated tests cover bounded-buffer overflow accounting, passband preservation from 8–192 kHz sources, and rejection of above-Nyquist energy. Three consecutive physical default-microphone capture lifecycles completed successfully after the change, each producing a valid 16 kHz mono clip.

The qualification harness subsequently completed 500 deterministic mixed orchestration cycles with zero wrong-target fake pastes, nine fake-backed insertion-policy cases, 500 real WASAPI start/finish cycles, and 25 cancellation-by-drop cycles. The native soak completed without a crash, invalid clip, or capture overflow. See `PHASE-1-SOAK.md` for the exact automated/manual boundary and content-free metrics.
