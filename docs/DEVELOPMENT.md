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
Do not run two app instances with the same shortcut or test against personal
transcripts. Quit the app through the tray before replacing its executable.

## Automated checks

Initialize the native developer shell first, then run:

```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets --features phorminx-app/desktop
cargo clippy --workspace --all-targets --features phorminx-app/desktop -- -D warnings
.\scripts\Test-ReleaseAssets.ps1
.\scripts\Test-PhorminxBootstrap.ps1
python .\scripts\build-brand-assets.py --check
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
shown; silent update installations do not launch the app or install models.

Daily checks use the latest stable GitHub Release and its
`phorminx-update.json` asset. This asset is a signed envelope, not a trusted
GitHub assertion. Build with `PHORMINX_UPDATE_PUBLIC_KEY` set to the base64 raw
Ed25519 public key; an unset key disables checks and installation in that
development build. Daily checks and automatic download/install are on by
default; onboarding offers an opt-out, and Settings → Updates keeps that
choice editable. Installation never interrupts an active recording or meeting
and runs only after exit.

The offline `phorminx_release` tool creates a signing key outside the source
repository and signs the exact installer bytes. Build it with
`cargo build -p phorminx-app --features desktop,vulkan,tauri-shell,release-tools
--bin phorminx_release` from the Phorminx developer shell.
Run `phorminx_release keygen <absolute-private-key.pk8> <public-key.txt>` once;
keep the private key offline, backed up, and never upload it. Build the public
app with that public-key text in `PHORMINX_UPDATE_PUBLIC_KEY`, then run
`phorminx_release sign <absolute-private-key.pk8> <version> <increasing-sequence>
<Phorminx-version-x64-setup.exe> <phorminx-update.json>`. The tool refuses to
overwrite outputs, checks the installer name and size, signs a five-year-valid
stable manifest, and verifies its own output. Upload both files to the matching
`v<version>` stable GitHub Release only after installer QA and Authenticode
signing. The first public release should be tested on a clean Windows account;
no release has been published by these instructions.

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
