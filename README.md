# Phorminx

Phorminx is a privacy-first Windows dictation utility. Hold a shortcut, speak naturally, and insert a faithful, clean transcript into the application that had focus.

The product is local-first:

- `whisper.cpp` performs speech recognition.
- Deterministic rules handle safe normalization and explicit aliases.
- Ollama optionally cleans and formats transcripts.
- Raw audio is not retained by default.
- Dictation remains usable when Ollama is missing or unavailable.

Start with [docs/BLUEPRINT.md](docs/BLUEPRINT.md), [docs/ROADMAP.md](docs/ROADMAP.md), the Phase 0 benchmark results, and the [Phase 1 walking-skeleton guide](docs/PHASE-1.md).

## Status

Phase 0 is complete: microphone capture, local Whisper, CPU fallback, and Vulkan acceleration are validated on the reference machine. `base.en` is the initial English model. The Phase 1 walking skeleton now includes guarded push-to-talk insertion and a non-activating status overlay; stability hardening remains before the tray UI.

## Initial platform

- Windows 10/11 x64
- Rust on the current stable toolchain
- Native Rust UI with `egui`/`eframe` and a lightweight tray application
- Per-user installation

## Development

```powershell
cargo test --workspace
cargo run --release -p phorminx-bench -- devices

. .\scripts\Enter-PhorminxDevShell.ps1 -Vulkan
cargo run --release -p phorminx-app --features vulkan
```

Whisper models belong in `models/` and personal recordings in `test-data/`; neither directory is committed to Git. The Phase 1 executable keeps the selected model resident and uses `Ctrl+Alt+Space` as its temporary hold-to-talk shortcut.

## Name

The project and product are named Phorminx.
