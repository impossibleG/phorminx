# Development

## Requirements

- Windows x64 and Rust through rustup; `rust-toolchain.toml` pins the toolchain.
- Visual Studio Build Tools with the Desktop development with C++ workload
  and Windows SDK.
- CMake and LLVM/libclang for native dependencies.
- Vulkan SDK and Ninja for GPU-enabled Whisper builds.
- Inno Setup 6 to build an installer.
- Python 3 and Pillow only when rebuilding the brand assets.

The developer-shell script discovers the installed native tools and reports
missing dependencies. It does not download models or install those tools.

## Run locally

From the repository root, in PowerShell:

```powershell
. .\scripts\Enter-PhorminxDevShell.ps1 -Vulkan
$env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'PhorminxBuild'
cargo run --release -p phorminx-app --features vulkan
```

Omit `-Vulkan` and `--features vulkan` for a CPU-only development build.
Configure speech models in the application's setup screens. Use a separate
`--config PATH` when testing settings rather than overwriting a normal profile.
A `--config` profile other than the default gets its own single-instance
slot, so it can run beside an installed Phorminx; give it different
shortcuts, because two instances cannot register the same ones. Do not test
against personal transcripts. Quit the app through the tray before replacing
its executable.

Each profile keeps a small `logs/lifecycle.log` next to its settings file:
start, exit, settings restarts and cleanup errors, with the user profile path
redacted. Check it first when the app closed or did not come back.

To compare formatting prompts and local models, run the formatting bench
against an installed Ollama model. It uses synthetic dictations only:

```powershell
cargo run -p phorminx-bench -- format-bench --model qwen2.5:7b-instruct `
    --custom "Use short bullet points." --output format-bench.md
```

Published benchmarks live in `docs/benchmarks/` and are shown, read-only, by
the app's Benchmarks page and the website
([ADR-020](ADR-020-BENCHMARKS-SECTION.md), [README](benchmarks/README.md)).
The suite needs a folder of model files (Vosk runtime and model, the Whisper
files, the OCR models) and the Ollama models under test. Build it with
`--features vulkan` to include Accurate on the GPU, then run each feature; each
merges its results into `docs/benchmarks/benchmarks.json`:

```powershell
$assets = 'C:\bench-assets'
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets fixtures
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets dictation
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets formatting
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets meetings
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets investigations
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets messaging --tor
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets app --exe <phorminx-app.exe>
& cargo run --release -p phorminx-bench --features vulkan -- suite --assets $assets finalize
```

Run it on an otherwise idle machine: a build, a second model in GPU memory or
a busy desktop can double latency, and each run records decode speed so a
model that spilled out of GPU memory is visible. Edit the `summary` in the
JSON by hand if the findings need words, then run `finalize` again. Commit
the JSON files; never commit personal recordings or transcripts.

## Automated checks

Initialize the native developer shell first, then run:

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets --features phorminx-app/desktop
cargo clippy --workspace --all-targets --features phorminx-app/desktop -- -D warnings
.\scripts\Test-ReleaseAssets.ps1
.\scripts\Test-PhorminxBootstrap.ps1
python .\scripts\build-brand-assets.py --check
node .\design\tauri-preview\markdown.test.js
node .\design\tauri-preview\investigation-model.test.js
node .\design\tauri-preview\benchmarks.test.js
```

To measure OCR accuracy and speed after changing `phorminx-ocr`, download the
pinned models (Models → Screenshot text recognition, or the URLs in
`investigation_ocr.rs`) and run the spike against synthetic screenshots, each
`name.png` beside its expected `name.txt`:

```powershell
cargo run --release -p phorminx-ocr --example ocr_spike -- det.onnx rec-latin.onnx latin-dict.txt .\fixtures
```

Tests requiring a microphone, installed model, native recognition runtime, or
other physical setup may be explicitly ignored. A passing default suite does
not certify those paths. Never claim hardware checks passed merely because
the deterministic tests passed.

`Test-InstantSpokenRollover.ps1`, `Test-AccurateSpokenStreaming.ps1`, and
`Test-MeetingStreaming.ps1` provide opt-in native recognition checks. Inspect
their parameters and prerequisites first; they need local assets and may use
Windows speech synthesis to generate neutral test audio. Keep generated
results in ignored temporary/artifact directories.

Use the [Windows compatibility protocol](WINDOWS-COMPATIBILITY-PROTOCOL.md)
for physical hotplug, focus, sleep/resume, DPI, and insertion checks.

## Build an installer

```powershell
.\scripts\Build-PhorminxInstaller.ps1 -Version '0.2.0-alpha.5'
```

The script defaults to a Vulkan-enabled desktop build with the static MSVC
runtime. It uses temporary short-path drive mapping to avoid native compiler
path-length limits. `-Cpu` produces a CPU-only build; `-OutputDirectory` chooses
the artifact destination. Choose the intended release version explicitly.
Signing is optional tooling, not automatic certification: do not describe an
unsigned build as signed or a local build as a published release.

The build embeds the requested version in both the application and its Windows
resources. `-SkipBuild` rejects a binary with a different embedded version.
The installer opens the branded Phorminx setup window after installation. The
user can choose **Prepare everything** to run the verified starter bootstrap
behind that window, or use individual repair actions. The Start menu's
**Prepare Phorminx** shortcut reopens setup. No setup PowerShell window is
shown; silent update installations do not open setup or install models.

Daily checks use the latest stable GitHub Release and its
`phorminx-update.json` asset. This asset is a signed envelope, not a trusted
GitHub assertion. Build with `PHORMINX_UPDATE_PUBLIC_KEY` set to the base64 raw
Ed25519 public key; an unset key disables checks and installation in that
build, so `Build-PhorminxInstaller.ps1` refuses to run without it unless
`-AllowMissingUpdateKey` is passed for a local QA build. Daily checks and
automatic download/install are on by default; onboarding offers an opt-out,
and Settings → Updates keeps that choice editable.

Installation never interrupts an active recording or meeting. Because
Phorminx lives in the tray and starts with Windows, it rarely exits on its
own, so a verified update is applied in one of three ways:

- **Restart to update** in Settings → Updates installs now and reopens the app.
- With automatic updates on, Phorminx restarts itself into the tray after the
  user has been away for 30 minutes, nothing is recording, and no Phorminx
  window is open.
- Otherwise the update installs the next time Phorminx is closed.

The install helper re-hashes the installer before running it silently and
writes `updates/packages/last-install.json`. Settings and models live in
Local AppData and are untouched by the installer.

### Release checklist

The offline `phorminx_release` tool signs the exact installer bytes. The
private key lives outside the repository (for example
`C:\Users\<you>\PhorminxReleaseKeys\phorminx-release.pk8`) with an offline
backup; never commit it, upload it, or copy it to the website server. The
public-key text beside it is safe to share.

1. One time only: from the developer shell, build the tool with
   `cargo build --release -p phorminx-app --features desktop,vulkan,tauri-shell,release-tools --bin phorminx_release`
   and run `phorminx_release keygen <absolute-private-key.pk8> <public-key.txt>`.
2. Bump the version, run the library tests, and merge to `main` together with
   the user-facing notes in `docs/releases/v<version>.md` (format in
   [docs/releases/README.md](releases/README.md)). If the release adds or
   changes a feature, update `docs/features/features.json` in the same pull
   request: set `since` on new features and keep the text true to the code.
   The website reads both files at the release tag.
3. Build the installer with the key compiled in:
   ```powershell
   $env:PHORMINX_UPDATE_PUBLIC_KEY = (Get-Content -Raw <public-key.txt>).Trim()
   .\scripts\Build-PhorminxInstaller.ps1 -Version 0.2.1
   ```
4. Installer QA: install over the previous release and check the tray icon,
   the setup window, a dictation, and that settings and models survived.
5. Publish with the next sequence number. It must be higher than every earlier
   release, because the updater rejects anything at or below the highest
   sequence it has seen:
   ```powershell
   .\scripts\Publish-PhorminxRelease.ps1 -Version 0.2.1 -Sequence 2 `
       -PrivateKeyPath <absolute-private-key.pk8> -ReleaseTool <phorminx_release.exe>
   ```
   This signs `phorminx-update.json`, adds the fixed-name copy
   `Phorminx-x64-setup.exe` (so
   `https://github.com/impossibleG/phorminx/releases/latest/download/Phorminx-x64-setup.exe`
   always serves the newest build), and creates a **draft** `v0.2.1` release.
   The release body is `docs/releases/v0.2.1.md`; the script stops if that
   file is missing (`-GenerateNotes` uses GitHub's pull request list instead,
   but the website changelog still needs the file).
   Review the draft on GitHub, then publish it (or re-run with `-Publish`).
   It must be a full release marked latest, not a pre-release, or both the
   updater and the website link keep pointing at the previous one.
6. Publishing runs the **Notify website** workflow, which calls the Render
   deploy hook in the `RENDER_DEPLOY_HOOK_PHORMINX` repository secret.
   phorminx.net then rebuilds with the new version, features and changelog.
   If the secret is unset the workflow only leaves a notice; trigger a
   manual deploy in Render instead.
7. Verify: on a machine running the previous version, Settings → Updates →
   Check now offers the new version, downloads it, and **Restart to update**
   reopens the new version with settings intact. Check that phorminx.net
   shows the new version and its changelog entry.
8. Record the sequence number used so the next release uses a larger one.

Alpha releases currently ship without Authenticode signing, so Windows
SmartScreen warns on first run. Do not describe them as signed.

Ed25519 metadata signing is separate from a Windows Authenticode code-signing
certificate or signing service. The latter identifies the publisher in Windows
and is still needed before treating an installer as a polished public release.
The private Ed25519 key must not be placed on the website/server. The website
feedback API is independent of update distribution.

Installers, compiled binaries, model weights, databases, and local recordings
are not source contributions. Check dependency/runtime/model licensing and
distribution notices separately before distributing bundled binaries.

## Contributing safely

Use synthetic fixtures. Do not commit `.env` files, tokens, private keys,
personal settings, recordings, transcripts, local diagnostic bundles, or
machine-specific QA notes. `.gitignore` is a convenience, not a secret scanner;
review staged changes before committing.
