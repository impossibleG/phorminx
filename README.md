# Phorminx

Phorminx is a privacy-first Windows dictation utility. Hold a shortcut, speak naturally, and insert a faithful, clean transcript into the application that had focus.

The product is local-first:

- `whisper.cpp` performs speech recognition.
- Deterministic rules handle safe normalization and explicit aliases.
- Ollama optionally cleans and formats transcripts.
- Raw audio is not retained by default.
- Dictation remains usable when Ollama is missing or unavailable.

This repository currently contains the implementation blueprint. Start with [docs/BLUEPRINT.md](docs/BLUEPRINT.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

## Status

Planning complete; implementation has not started.

## Initial platform

- Windows 10/11 x64
- Rust on the current stable toolchain
- Native Rust UI with `egui`/`eframe` and a lightweight tray application
- Per-user installation

## Working name

`Phorminx` is a temporary internal name. Product naming and trademark review are outside the MVP.

