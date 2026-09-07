# Phorminx

Phorminx is a privacy-first Windows dictation utility. Open the P launcher, choose Dictate, speak naturally, and press the shortcut again to insert the transcript into the original application. An optional direct shortcut supports toggle or hold-to-record.

The product is local-first:

- `whisper.cpp` performs speech recognition.
- An optional resident Vosk stream provides Instant mode when a local runtime
  bundle and language-matched unpacked model are explicitly configured.
- Deterministic rules handle safe normalization and explicit aliases.
- Ollama optionally cleans and formats transcripts.
- Library finds retained dictations by exact words or meaning using an optional
  local embedding model. Indexing runs in the background and yields to dictation.
- Longer dictations are transcribed while you speak. Committed audio is erased
  from a fixed rolling memory buffer; only uncommitted audio and boundary
  context remain. Recording duration does not determine audio memory usage.
- Accurate mode repairs overlapping Whisper segments; Instant mode uses Vosk
  word timestamps to preserve whole words across decoder rollovers.
- The production capture path writes no audio recordings to disk.
- With history enabled, interrupted text is periodically encrypted for the
  current Windows account. History offers explicit Copy and Discard actions;
  the latest unrecognized or uncheckpointed speech may be missing after a crash.
- Clearing or disabling history also removes interrupted text. History Off
  keeps dictated text in memory only.
- Dictation remains usable when Ollama is missing or unavailable.

Start with [docs/BLUEPRINT.md](docs/BLUEPRINT.md), [docs/ROADMAP.md](docs/ROADMAP.md), [docs/PHASE-2-4-STATUS.md](docs/PHASE-2-4-STATUS.md), the Phase 0 benchmark results, and the [Phase 1 walking-skeleton guide](docs/PHASE-1.md).

## Status

The current local-workspace release adds a refined unified UI, configurable
shortcuts, the P action launcher, and local semantic search. See the
approved scope and
[validation and manual test guide](docs/LOCAL-WORKSPACE-VALIDATION.md).

The default `Ctrl+Alt+Space` shortcut now opens the launcher; press `1` or `Enter`
to start, and the shortcut again to stop. Configure bindings under Settings >
Shortcuts, and the light/dark preference under Settings > Appearance. Optional
semantic search is configured under Models and used from Library. Choose an
installed dedicated embedding model; normal chat models may not support it.
Exact-word search needs no AI model. No remote AI provider is enabled.

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
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'PhorminxBuild'
cargo run --release -p phorminx-app --features vulkan

# Packaged-style build: no console; use the tray menu to exit.
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release -p phorminx-app --features desktop,vulkan
```

Runtime settings load from `%LOCALAPPDATA%\Phorminx\settings.toml`; local history and product data use `phorminx.db` beside it. Open the tray menu for Settings, History, Personal lexicon, and Application profiles. Settings discovers installed Ollama models without choosing one implicitly, controls model residency, and offers five formatting strengths. New application settings retain history for seven days; select History Off to disable transcript persistence. Production capture keeps audio in memory, and unavailable Ollama falls back to deterministic local output.

The model picker offers an explicit download for the pinned recommended English Whisper model. Downloads stream to a temporary file and must match the manifest size and SHA-256 before atomic promotion. Saving validates and atomically persists settings, then restarts Phorminx when runtime state must be reloaded. If the selected model is missing at startup, Phorminx remains in setup mode instead of terminating. Command-line values such as `--model`, `--language`, `--minimum-rms`, and `--formatting raw|light|balanced|strong|custom` override settings for one run; `--config PATH` selects a development/test settings file.

Instant mode's Settings action imports user-selected official Vosk runtime and
English-model ZIPs only after pinned size/SHA-256 verification and safe
extraction. Ready requires an actual native model and recognizer probe. The
release-to-insert timer begins at the physical hold-release or toggle-stop event,
and history keeps content-free stage timings for performance analysis.

Whisper models belong in `models/` and development audio fixtures in `test-data/`; neither directory is committed to Git. Speech models can remain resident according to the selected runtime policy.

## Name

The project and product are named Phorminx.
