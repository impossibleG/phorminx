# Getting started

Phorminx targets Windows 10/11 x64. It is alpha software; recognition accuracy,
latency, and compatibility depend on the device, audio source, and model.

Download the latest installer from
[GitHub Releases](https://github.com/impossibleG/phorminx/releases/latest), or
build from source using the [development guide](DEVELOPMENT.md).
Speech models and Vosk runtime assets are separate downloads and are not
included in this source repository. Configure them in the application's setup
and model screens. Ollama is optional; dictation can run without AI formatting.

## Installer and guided setup

The branded installer opens Phorminx's setup window after installation. Choose
**Prepare everything** to install the local starter bundle without a terminal,
or review individual repair steps. Progress and errors appear in Phorminx; a
safe stop finishes the current step first. Use **Prepare Phorminx** in the Start
menu to retry. Silent updates skip this onboarding. Setup preserves existing
settings and saved text.

## Starter bundle from PowerShell

For an already installed app, an English starter bundle can be downloaded and
configured automatically. Close Phorminx (including its tray icon), then run
this from the repository folder in **64-bit PowerShell as your normal user**:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Initialize-Phorminx.ps1
```

The script downloads Vosk's English model/runtime, Whisper base.en, installs
Ollama and the Vulkan runtime if missing, and pulls `qwen2.5:1.5b` for formatting
and `nomic-embed-text` for semantic search. Allow several GB of free disk space
and time for downloads. Windows may request administrator approval for Vulkan.
The speech assets are hash-verified and installers are publisher-verified.
On a fresh installation it configures Accurate mode, custom two-pass formatting,
and local semantic search. Existing settings and saved text are preserved; existing
installations may need to select the downloaded models in Settings/Models.
The script can be rerun after a failed download. Add `-Plan` to preview its scope
without changing anything. It is also standalone: the script file can be shared
alongside the app installer, without requiring a repository checkout.

This does **not** install Phorminx itself, source-build tools, or GPU drivers.
Vulkan acceleration needs a compatible GPU/vendor driver; installing the Vulkan
runtime alone does not establish GPU compatibility. This starter downloads
English models, not every available language or model variant.

## First dictation and next steps

The default `Ctrl+Alt+Space` shortcut opens the P launcher. Choose Dictate, speak,
then press the shortcut again to finish. Settings provides shortcut and
appearance preferences. Meetings and Actions have their own workspaces.

Models also offers curated English Vosk choices ranging from a small download to
multi-gigabyte models. Downloads do not switch the active model: explicitly choose
**Use for English Instant** after setup. Native validation runs in the background.
Larger models require more memory and are not guaranteed to improve every voice.
Dual-source meetings run two recognizers; headphones help avoid speaker audio
being transcribed a second time through the microphone. Meeting settings save
the input and output devices separately for the next session. A configured,
installed small local fallback model may retry a failed assistant response once;
assistant failure never ends capture.
