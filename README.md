# Phorminx

Phorminx is a privacy-first Windows dictation utility. Hold a shortcut, speak naturally, and insert a faithful, clean transcript into the application that had focus.

The product is local-first:

- `whisper.cpp` performs speech recognition.
- Deterministic rules handle safe normalization and explicit aliases.
- Ollama optionally cleans and formats transcripts.
- Raw audio is not retained by default.
- Dictation remains usable when Ollama is missing or unavailable.

Start with [docs/BLUEPRINT.md](docs/BLUEPRINT.md), [docs/ROADMAP.md](docs/ROADMAP.md), the [Phase 0 benchmark guide](docs/PHASE-0.md), and the current benchmark results.

## Status

Phase 0 is in progress. The first Rust workspace provides a command-line harness for microphone capture and local Whisper benchmarking before the tray application is built.

## Initial platform

- Windows 10/11 x64
- Rust on the current stable toolchain
- Native Rust UI with `egui`/`eframe` and a lightweight tray application
- Per-user installation

## Development

```powershell
cargo test --workspace
cargo run --release -p phorminx-bench -- devices
```

Whisper models belong in `models/` and personal recordings in `test-data/`; neither directory is committed to Git. See the Phase 0 guide for capture and transcription commands.

## Name

The project and product are named Phorminx.
