# Rolling dictation release validation — 2026-09-06

Historical baseline for revision `2fb4037`. Later recording and delivery
regression fixes, current validation, and replacement artifact hashes are
tracked in [continuous dictation regressions](CONTINUOUS-DICTATION-REGRESSIONS.md).
The artifact hashes below describe that earlier build, not a later installer
written to the same output directory.

The integrated revision uses bounded rolling audio, timestamp-aware Whisper
repair, Vosk whole-word rollover/replay, and encrypted interrupted-text recovery.

## Automated evidence

- `cargo test --workspace --locked --quiet`: 627 tests passed; documentation
  tests passed. Three native asset-dependent tests are ignored by default.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `scripts/Test-ReleaseAssets.ps1`: passed after packaging.
- Two opt-in native spoken Vosk tests passed separately. See
  [spoken rollover evidence](INSTANT-SPOKEN-ROLLOVER-VALIDATION.md) for the
  70.355-second fixture, six forced restarts, exact audio ownership checks,
  preserved release tail, and measured recognition differences.
- Seven recovery actor tests passed independently. Seven storage unit tests
  plus 32 persistence integration tests cover encryption, Unicode, tampering,
  row binding, stale writes, deletion, retention, and restart recovery.

## Artifacts

Both installers are unsigned local development artifacts. The release build
checked their executable imports for unbundled Microsoft C/C++ runtimes.

| Artifact | SHA-256 |
| --- | --- |
| `artifacts/installer/Phorminx-0.1.0-x64-setup.exe` (Vulkan, 12,831,278 bytes) | `B968FAD98FDB1A43C22FB2030C9A9CCE6540D5A2B9B2BA81E6BE37ACC7477D5B` |
| `artifacts/installer-cpu/Phorminx-0.1.0-x64-setup.exe` (CPU, 8,022,553 bytes) | `D3A98526B1DB6B7B809DD18B46BC477447FDDE45277811946896707739A942A0` |
| Packaged Vulkan `phorminx-app.exe` | `7E6FEDBBBCAB290B92812BF775593CED0375BD617ED4D16821104FD9D91D7654` |

## Installation and remaining physical evidence

The installed application was still running when packaging completed. It has
not yet been replaced. Startup smoke checks for Accurate/CPU, Accurate/Vulkan,
and Instant are prepared with isolated settings and History Off; they must run
after the old process exits so the single-instance guard cannot yield a false
successful result.

Live microphone soaks, device-loss testing, broad English/PT-BR corpora, and
post-release physical latency measurements remain manual qualification. A
speech recognizer can change individual words across a replay boundary; these
tests establish neither perfect transcription nor unlimited resource capacity.
Recovery preserves the last successful encrypted text snapshot and may omit
recent speech not yet recognized or written, particularly during a disk stall.
