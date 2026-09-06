# ADR-011: Per-user Windows release and privacy-safe diagnostics

Status: accepted for development and private testing

## Context

Phorminx needs an installable Windows build, an explicit launch-at-login control, a repeatable compatibility record, and a support bundle that does not silently collect dictated content. These facilities must preserve the product's local-first boundary and must not require administrator privileges.

Code signing is a release gate, but no signing identity or trusted timestamping service is currently configured. Physical compatibility scenarios also cannot be certified by unit tests alone.

## Decision

### Installation

- Use an Inno Setup 6 script for an x64-compatible, per-user installation under `{localappdata}\Programs\Phorminx`.
- Set `PrivilegesRequired=lowest` and provide no elevation override.
- Install only the application executable. Speech and cleanup models remain separate, explicitly consented downloads.
- Statically link the Microsoft C/C++ runtime in packaged builds. Rust and the older whisper.cpp CMake project are both pinned to the static runtime; the release script inspects the final PE dependency table and refuses to package an executable that still imports the unbundled MSVC/UCRT redistributable.
- Build native dependencies through a temporary drive-root mapping backed by versioned per-user LocalAppData storage. This keeps Vulkan shader-generator paths below legacy MSVC limits; the mapping is removed in a `finally` boundary and is never the installer source path.
- Add Start Menu and uninstall entries.
- Offer launch-at-login as an unchecked installer task that writes only the `Phorminx` value under the current user's Run key.
- Treat an unsigned installer as a development artifact. `Build-PhorminxInstaller.ps1 -RequireSignedBinary` fails closed unless both the input binary and produced installer have valid Authenticode signatures. Signing configuration remains external to the repository and no signing success is claimed by this ADR.

### Runtime launch-at-login control

- `phorminx-windows` owns a small registry adapter for `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
- Values contain one quoted absolute executable path and no user content.
- Reads distinguish disabled, enabled for the expected executable, and a different/stale command.
- Disable deletes only the named `Phorminx` value and is idempotent.
- Runtime UI wiring is deliberately separate from this adapter so settings composition can evolve without placing Win32 details in the app crate.

### Diagnostics

- The default diagnostic bundle contains exactly `system.json`, `runtime.json`, and `manifest.json`.
- It includes coarse OS/runtime compatibility facts, app presence/version/signature status, process count, settings-file presence, and a boolean loopback Ollama reachability probe.
- It excludes audio, transcripts, cleanup results, clipboard contents, window titles, focused-control text, custom instructions, model names, prompts, raw logs, environment variables, usernames, machine names, and filesystem paths.
- Arbitrary notes and raw logs cannot be appended by the default collector. If content-bearing evidence is ever added, it must be a separately named, explicit opt-in flow with a review step.
- The collector refuses to overwrite an existing ZIP and cleans its exact temporary directory.

### Compatibility evidence

- Use a content-free JSON run record with fixed scenario IDs, fixed outcomes, and fixed issue codes.
- Do not store target application names, window titles, clipboard payloads, transcripts, audio, device names, or free-form notes.
- Mark physical scenarios as such. A generated run record is evidence scaffolding, not evidence that a scenario was performed.

## Consequences

- Developers can validate packaging privacy, static-runtime, and policy invariants without installing Inno Setup.
- Producing an installer still requires Inno Setup 6; producing a distributable installer additionally requires an externally managed code-signing process.
- Launch-at-login is available as a tested platform primitive and installer option, but its final Settings checkbox depends on app composition work.
- Compatibility completion requires deliberate manual execution on declared Windows hardware and target applications.

## Validation

Run:

```powershell
.\scripts\Test-ReleaseAssets.ps1
cargo test --locked -p phorminx-windows
cargo clippy --locked -p phorminx-windows --all-targets -- -D warnings
```

The first command validates installer invariants, creates and inspects a default diagnostic bundle, and exercises compatibility-run persistence. It does not compile an installer, sign artifacts, or perform physical compatibility tests.

To produce both private-test installers, build CPU first into a separate
output directory and then rebuild the default Vulkan edition:

```powershell
.\scripts\Build-PhorminxInstaller.ps1 -Cpu -OutputDirectory .\artifacts\installer-cpu
.\scripts\Build-PhorminxInstaller.ps1
```

Both builds use the same intermediate executable path. The separate CPU output
directory preserves its installer, and the final default build leaves the
Vulkan executable and installer in the normal release locations. Do not use
`-SkipBuild` to switch editions: it packages whichever executable currently
occupies that shared path. The Vulkan edition still supports the CPU runtime
backend and Auto fallback. Neither command signs the development artifacts.
