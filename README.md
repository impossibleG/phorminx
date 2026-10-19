<p align="center">
  <img src="design/brand/png/app/phorminx-app-256.png" alt="Phorminx P logo" width="112" height="112">
</p>

<h1 align="center">Phorminx</h1>

<p align="center"><strong>Voice, disciplined.</strong></p>

<p align="center">
  Local-first dictation, meetings and research notes for Windows.<br>
  Speak, keep searchable text, and use AI formatting without a paid AI service.
</p>

<p align="center">
  <a href="https://github.com/impossibleG/phorminx/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/impossibleG/phorminx?label=release"></a>
  <a href="https://github.com/impossibleG/phorminx/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/impossibleG/phorminx/total"></a>
  <img alt="Windows 10 and 11" src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D4">
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

<p align="center">
  <a href="https://github.com/impossibleG/phorminx/releases/latest/download/Phorminx-x64-setup.exe"><img alt="Download for Windows" src="https://img.shields.io/badge/Download_for_Windows-x64_installer-a87542?style=for-the-badge&logo=windows&logoColor=white"></a>
  <br>
  <a href="https://phorminx.net">phorminx.net</a> ·
  <a href="docs/GETTING-STARTED.md">Getting started</a> ·
  <a href="docs/DEVELOPMENT.md">Build from source</a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/features/hero-dark.webp">
  <source media="(prefers-color-scheme: light)" srcset="docs/features/hero-light.webp">
  <img alt="Your voice, written and translated on your own PC: recent dictations and the meeting companion with an English transcript, its Portuguese translation and an assistant answer" src="docs/features/hero-light.webp">
</picture>

<p align="center">
  <a href="#dictation">Dictation</a> ·
  <a href="#formatting-and-custom-instructions">Formatting</a> ·
  <a href="#prompts-library">Prompts</a> ·
  <a href="#meetings-and-live-translation">Meetings</a> ·
  <a href="#companion-assistant">Assistant</a> ·
  <a href="#investigations">Investigations</a> ·
  <a href="#messages">Messages</a> ·
  <a href="#privacy-at-a-glance">Privacy</a>
  <br><sub>Product images are crops of the real app running on a demo profile with made-up content
  (<a href="docs/features/CAPTURE.md">how they are made</a>).</sub>
</p>

## Dictation

<img src="docs/features/dictation.webp" alt="Dictation: press Ctrl+Alt+Space, speak, the speech is recognized locally, optionally formatted by a local Ollama model, and inserted into the app you are using. Instant mode uses Vosk (fastest); Accurate mode uses Whisper (most accurate, optional Vulkan GPU).">

- Transcription starts while you speak; live text shows in the status pill.
- Hold-to-talk or toggle, from the launcher or a direct dictation shortcut.
- History is kept 7 days by default (1 day, 30 days, forever, or off).

### Review panel

Open the last dictation from the status pill's Expand button to edit it,
**Reformat** it with another strength or a one-off instruction, copy it, or
**Replace** the inserted text. Replace only touches text that is still there
and unchanged; otherwise the result goes to the clipboard.

## Formatting and custom instructions

<img src="docs/features/formatting.webp" alt="Speak loosely, paste it polished: raw speech becomes a formatted list after 2 local formatting passes, with lexicon words such as Priya and Kubernetes corrected, and an @ prompt chip inside the custom instructions field.">

- Strengths from **Raw** to **Light**, **Balanced**, **Strong**, **Structured** and **Custom**.
- **Formatting passes** run the local model more than once, each pass refining the last.
- A **lexicon** fixes names and jargon; **app profiles** change style, language or
  insertion per program, or turn dictation off there.

## Prompts library

<img src="docs/features/prompts.webp" alt="Write an instruction once, use it everywhere: the prompts library with tags, a selected prompt and its version history.">

Named, tagged, versioned instructions. Type `@` in any instruction field
(formatting, app profiles, the review panel, the meeting assistant or Actions)
to insert one; it always uses the latest version.

## Meetings and live translation

<img src="docs/features/meetings.webp" alt="Both sides of the call, transcribed and translated: your microphone and computer audio go to two local recognizers, and a local Ollama model adds a translation under each line while keeping the original.">

- Computer audio and your microphone are captured together, with timestamps.
- Pause either source for Phorminx without muting your call.
- Show Original, Translation or Both; saved sessions are searchable.

## Companion assistant

<img src="docs/features/assistant.webp" alt="Ask what you missed, while the call continues: a question in the companion and the assistant's answer listing what was decided and what is still open.">

A resizable companion keeps the transcript and assistant beside the call, in
Transcript, Together or Assistant view. Optional automatic commentary runs
only on a verified local Ollama model, and a failed answer never ends capture.

## Investigations

<img src="docs/features/investigations.webp" alt="Everything about one question, in one place: pasted text, screenshots read by local OCR, links and voice notes become cards, shown as a chronology or a diagram, and the assistant answers with citations to cards.">

A dedicated shortcut sends the clipboard to the active investigation from any
app. The assistant can suggest connections for you to accept or dismiss, and
any investigation can be exported as Markdown.

## Messages

<img src="docs/features/messages.webp" alt="Message people directly with no server in between: you and a contact connect through the Tor network. Off by default, no Phorminx server, neither side learns the other's IP address, history stays local.">

Add people with a one-time invite. A message only moves while both of you have
Phorminx running. See [ADR-018](docs/ADR-018-P2P-MESSAGING.md).

## And also

<img src="docs/features/more.webp" alt="Library search with exact words or local semantic search, Actions that send a dictation to an HTTP endpoint, hash-verified models and guided setup, and automatic updates: a signed manifest is checked, the installer hash is verified, and it installs after you exit or while idle, never during a recording.">

- **Library search:** exact-word and optional local semantic search; saved meetings have their own search.
- **Actions:** saved HTTP requests with transcript substitution or local-model JSON, on launcher keys 3 to 9.
- **Models and setup:** Whisper, Vosk and OCR downloads are hash-verified; **Prepare everything** sets up the starter bundle.
- **Automatic updates:** daily checks and automatic install can both be turned off in Settings → Updates.

## Privacy at a glance

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/features/privacy-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/features/privacy-light.svg">
  <img alt="Speech recognition, local formatting, search, OCR and your saved text stay on your computer. Feedback, a remote assistant, Actions, Messages and downloads leave only when you choose." src="docs/features/privacy-light.svg" width="820">
</picture>

<details>
<summary><b>Privacy details</b></summary>

Speech recognition runs locally. The production capture path keeps audio in
memory rather than saving recordings to disk. Retained text and settings live
under `%LOCALAPPDATA%\Phorminx`; use the application's history, session, and
deletion controls to manage stored text. Disabling dictation history does not
mean that separately saved meeting sessions have been deleted.

Local Ollama formatting and embeddings use local compute. Model downloads use
the network. Optional remote assistant providers and HTTP actions send the
configured text/context to the selected provider or destination. These options
are not a local-only privacy guarantee: remote providers may charge for usage
and apply their own data policies. Review the destination before enabling them.

Meeting translation uses a separately selected local Ollama model. Originals
remain available even if translation is unavailable or cannot keep up. Fresh
settings recommend `nomic-embed-text:latest`; existing embedding selections and
indexes are not silently replaced.

Investigations are kept until you delete them; dictation history retention does
not apply to them. Screenshots are stored as re-encoded PNG files beside the
database. Screenshot text recognition runs locally after an optional, verified
model download (12.6 MB). A pasted link is fetched once, without cookies, for its
title and preview; local and private-network addresses are never fetched.

**Messages** (off by default) sends text directly to people you add with a
one-time invite. Turning it on connects Phorminx to the Tor network through
the embedded Arti client; there is no Phorminx server, and neither side learns
the other's IP address. Contacts can see when you are online, and a message
only moves while both of you have Phorminx running. History stays on this
computer and can be deleted per contact. See
[ADR-018](docs/ADR-018-P2P-MESSAGING.md).

Launcher **0 · Give feedback** records a draft locally. Only clicking **Send**
submits its reviewed text to `phorminx.net`; optional diagnostics are shown
separately. Audio and logs are not attached automatically. Release-enabled
builds check stable GitHub Releases daily by default; automatic download and
install after exit are on by default, with an onboarding opt-out and a Settings
control. The app verifies signed metadata and installer hashes and
waits for exit before installing, never stopping an active recording. Builds
without a configured release-verification key cannot enable updates. Neither
feature makes an unconfigured feedback server ready.

Do not include real transcripts, recordings, credentials, or personal settings
in bug reports or contributions. Use synthetic examples instead.

</details>

## Getting started

1. **Install.** [Download the installer](https://github.com/impossibleG/phorminx/releases/latest/download/Phorminx-x64-setup.exe)
   for Windows 10/11 x64. Alpha builds are not Authenticode-signed yet, so
   SmartScreen may warn on first run.
2. **Prepare.** The setup window opens after installation. Choose
   **Prepare everything** for the English starter bundle (speech models,
   Ollama, a small local formatting model and local embeddings), or run
   individual steps. Ollama is optional; dictation works without AI formatting.
3. **Dictate.** Press `Ctrl+Alt+Space` to open the P launcher, choose Dictate,
   speak, and press the shortcut again to finish.

Phorminx is alpha software; accuracy, latency and compatibility depend on the
device, audio source and model. The [getting started guide](docs/GETTING-STARTED.md)
covers the PowerShell starter bundle, model choices and meeting audio tips.

## Documentation

- [Getting started](docs/GETTING-STARTED.md)
- [Build, test, and package](docs/DEVELOPMENT.md)
- [Windows compatibility checks](docs/WINDOWS-COMPATIBILITY-PROTOCOL.md)
- [Architecture decision records](docs/README.md)
- [Production brand assets](design/brand/README.md)
- [Assistant transport contracts](crates/phorminx-assistant/README.md)
- [Meeting capture and translation](docs/MEETING-CAPTURE-AND-TRANSLATION.md)
- [Setup and operations release gates](docs/ADR-017-RELIABLE-SETUP-AND-MEETINGS.md)
- [Setup and meetings acceptance checklist](docs/SETUP-MEETINGS-TEST-CHECKLIST.md)
- [Investigations and local OCR](docs/ADR-018-INVESTIGATIONS-AND-LOCAL-OCR.md)
- [Peer-to-peer messaging](docs/ADR-018-P2P-MESSAGING.md)
- [Prompt library](docs/ADR-019-PROMPT-LIBRARY.md)
- [Recreating the README visuals](docs/features/CAPTURE.md)

## License

Phorminx's original source is licensed under the [MIT License](LICENSE).
Third-party libraries, runtimes, and downloaded models retain their respective
licenses and notices; the project license does not replace those terms.
