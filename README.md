# Phorminx

Phorminx is a privacy-first Windows dictation utility. Hold a shortcut, speak naturally, and insert a faithful, clean transcript into the application that had focus.

The product is local-first:

- `whisper.cpp` performs speech recognition.
- Deterministic rules handle safe normalization and explicit aliases.
- Ollama optionally cleans and formats transcripts.
- Raw audio is not retained by default.
- Dictation remains usable when Ollama is missing or unavailable.

Start with [docs/BLUEPRINT.md](docs/BLUEPRINT.md), [docs/ROADMAP.md](docs/ROADMAP.md), [docs/PHASE-2-4-STATUS.md](docs/PHASE-2-4-STATUS.md), the Phase 0 benchmark results, and the [Phase 1 walking-skeleton guide](docs/PHASE-1.md).

## Status

The Phase 1 walking skeleton and the implementation work for Phases 2–4 are complete. Phorminx now has native settings and first-run setup, microphone selection and hotplug fallback, verified Whisper model downloads, Raw/Light/Balanced/Strong/Custom formatting, local Ollama discovery and model selection, private history with retention, an exact personal lexicon, per-application profiles, launch-at-login, sleep/resume recovery, and private-alpha release tooling.

This means the application is feature-complete for private-alpha qualification, not that every release gate has been certified. Code signing, a compiled installer, a real-model cleanup conformance run, the physical Windows compatibility matrix, and corpus-based performance measurements still require external tools, models, hardware, or deliberate manual testing. See the phase status document for the precise boundary.

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

# Packaged-style build: no console; use the tray menu to exit.
cargo build --release -p phorminx-app --features desktop
```

Runtime settings load from `%LOCALAPPDATA%\Phorminx\settings.toml`; local history and product data use `phorminx.db` beside it. Open the tray menu for Settings, History, Personal lexicon, and Application profiles. Settings discovers installed Ollama models without choosing one implicitly, controls model residency, and offers five formatting strengths. History defaults to disabled, raw audio is never stored, and unavailable Ollama always falls back to deterministic local output.

The model picker offers an explicit download for the pinned recommended English Whisper model. Downloads stream to a temporary file and must match the manifest size and SHA-256 before atomic promotion. Saving validates and atomically persists settings, then restarts Phorminx when runtime state must be reloaded. If the selected model is missing at startup, Phorminx remains in setup mode instead of terminating. Command-line values such as `--model`, `--language`, `--minimum-rms`, and `--formatting raw|light|balanced|strong|custom` override settings for one run; `--config PATH` selects a development/test settings file.

Whisper models belong in `models/` and personal recordings in `test-data/`; neither directory is committed to Git. The Phase 1 executable keeps the selected model resident and uses `Ctrl+Alt+Space` as its temporary hold-to-talk shortcut.

## Name

The project and product are named Phorminx.
