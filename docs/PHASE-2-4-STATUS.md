# Phases 2–4 status

Date: 2026-08-30

Phorminx is feature-complete for private-alpha qualification. This document separates implemented and automated work from gates that require external infrastructure or deliberate physical testing.

## Implemented

### Phase 2 — Local product MVP

- Native tray, non-activating status overlay, settings, first-run setup, and verified model download.
- Exact microphone selection with Windows-default recovery when a saved or hot-unplugged device is unavailable.
- Versioned atomic settings with v1-to-v2 migration.
- Local SQLite history using WAL, disabled-by-default retention, immediate clear, and raw/normalized/cleaned/selected recovery views.
- Exact personal aliases with language and executable scope, explicit case policy, and no automatic promotion.
- Safe target snapshots, sensitive/elevated/changed-target refusal, and clipboard-only fallback.
- Single-instance enforcement and clean process restart after settings that require runtime reload.

### Phase 3 — Ollama enhancement

- Strict loopback-only Ollama client with redirects and environment proxies disabled.
- Installed-model discovery and explicit selection; Phorminx never guesses which installed model to use.
- Instant, Balanced, and Memory Saver residency modes with warm-up and unload behavior.
- Raw, Light, Balanced, Strong, and Custom formatting profiles.
- Deterministic generation settings, bounded connection/header/body/global timeouts, cancellation, and exact fallback.
- Protected-token validation for names and opaque tokens such as URLs, email addresses, paths, numbers, flags, placeholders, and code-like fragments.
- Per-application profiles keyed only by executable basename, with formatting, language, insertion, and deny overrides.
- Explicit Cleaning UI state while local model generation is in progress.

### Phase 4 — Hardening and release engineering

- 500-cycle runtime soak plus focused fault, stale-result, and wrong-target tests.
- Sleep/resume recovery that cancels owned recording/transcription state and rejects late worker output.
- Device-hotplug recovery on the next activation.
- Per-user HKCU launch-at-login support.
- Inno Setup specification for a non-elevated LocalAppData install and uninstall.
- Fail-closed signing gate for release builds.
- Content-free diagnostics bundle and compatibility-run harness.
- Automated installer-policy, diagnostics-privacy, Rust test, Clippy, CPU release-build, and executable smoke checks.

## Automated evidence from this milestone

- `cargo test --workspace`: 106 tests passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo clippy -p phorminx-app --features desktop --all-targets -- -D warnings`: passed.
- `scripts/Test-ReleaseAssets.ps1`: passed.
- Isolated optimized CPU desktop build: passed.
- Built executable `--smoke-test`: passed with the resident English Whisper model.

The Ollama tests use a controlled loopback HTTP service so timeouts, malformed responses, lifecycle requests, validation, and exact fallback are deterministic. They do not establish that a particular downloaded language model produces acceptable edits.

## External qualification gates still open

- Install Inno Setup 6 and compile/test the installer on a clean Windows account or machine.
- Configure a trusted Authenticode certificate and timestamp service; sign and verify the executable and installer. Unsigned artifacts are development builds only.
- Run the fixed physical compatibility matrix across Notepad, browsers, VS Code, Office/Electron-class apps, Terminal, RDP, elevated targets, password fields, focus races, clipboard races, multiple DPI configurations, sleep/resume, and microphone unplug/replug.
- Select at least one installed Ollama model and run the real cleanup conformance corpus. The model cannot be recommended before it passes meaning, polarity, and protected-token checks.
- Record and benchmark the consented private speech corpus to publish WER/CER, latency percentiles, real-time factor, memory use, and hardware recommendations.
- Install the Vulkan SDK before qualifying the optional Vulkan build. The CPU build is the currently verified release configuration.

These gates are intentionally not represented as passed by unit tests or generated harness files.

## Intentional safety choices

- Automatic paste is attempted once only when the original target remains verifiably safe; every uncertainty becomes clipboard-only.
- Phorminx leaves the successfully prepared payload on the clipboard. It does not attempt a delayed clipboard restore, avoiding a race that could overwrite a newer clipboard value.
- Raw audio is not retained. History content is opt-in through retention settings.
- Logs and default diagnostics exclude transcripts, prompts, clipboard content, window titles, full paths, user names, machine names, and device names.
