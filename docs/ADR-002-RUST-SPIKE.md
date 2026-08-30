# ADR-002: Rust implementation and Phase 0 spike

- Status: Accepted
- Date: 2026-08-30

## Decision

Implement Phorminx primarily in Rust. Use `egui`/`eframe` with the lean `glow` renderer for the eventual native interface, CPAL over WASAPI for portable microphone capture, and `whisper-rs` over vendored `whisper.cpp` for free local transcription.

English is the v1 language. Benchmark `base.en` and quantized `small.en` before selecting the default. Brazilian Portuguese follows through a multilingual Whisper model and language-specific evaluation corpus.

The first executable is a command-line feasibility harness rather than the tray application. It must enumerate microphones, record 16 kHz mono WAV, transcribe an existing WAV, and record then transcribe in one command.

## Rationale

Rust keeps the application core portable and memory-conscious while still providing direct access to Windows APIs. A command-line spike isolates audio and inference failures from tray, UI, hotkey, and insertion concerns.

## Temporary constraints

- CPU-only Whisper build.
- Default microphone only.
- Fixed-duration capture instead of push-to-talk.
- Simple linear resampling, which must be replaced with a band-limited resampler before accuracy results become a release gate.
- No VAD, Ollama, UI, persistence, or automatic model download.
