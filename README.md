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

The end-to-end walking skeleton and its stability gate are complete: microphone capture, resident local Whisper, guarded insertion, the non-activating overlay, 500-cycle safety soaks, and a native tray lifecycle are validated on the reference machine. Product-shell work is in progress; versioned settings and Raw/Light formatting behavior are implemented, while the settings window and onboarding still remain.

## Initial platform

- Windows 10/11 x64
- Rust on the current stable toolchain
- Lightweight native Win32 shell written in Rust
- Per-user installation

## Development

```powershell
cargo test --workspace
cargo run --release -p phorminx-bench -- devices

. .\scripts\Enter-PhorminxDevShell.ps1 -Vulkan
cargo run --release -p phorminx-app --features vulkan
```

Runtime settings load from `%LOCALAPPDATA%\Phorminx\settings.toml`. A missing file uses the current defaults without creating anything. Command-line values such as `--model`, `--language`, `--minimum-rms`, and `--formatting raw|light` override settings for one run; `--config PATH` selects a development/test settings file.

Whisper models belong in `models/` and personal recordings in `test-data/`; neither directory is committed to Git. The Phase 1 executable keeps the selected model resident and uses `Ctrl+Alt+Space` as its temporary hold-to-talk shortcut.

## Name

The project and product are named Phorminx.
