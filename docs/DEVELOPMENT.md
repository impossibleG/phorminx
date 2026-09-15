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

Installers, compiled binaries, model weights, databases, and local recordings
are not source contributions. Check dependency/runtime/model licensing and
distribution notices separately before distributing bundled binaries.

## Contributing safely

Use synthetic fixtures. Do not commit `.env` files, tokens, private keys,
personal settings, recordings, transcripts, local diagnostic bundles, or
machine-specific QA notes. `.gitignore` is a convenience, not a secret scanner;
review staged changes before committing.
