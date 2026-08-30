# Phorminx — Product and Engineering Blueprint

## 1. Product thesis

Phorminx should make voice input feel like a native input method while keeping the user's audio and text under their control. The essential loop is:

1. Hold a global shortcut.
2. Speak.
3. Release the shortcut.
4. Receive a faithful transcript, optionally cleaned locally.
5. Insert it into the original safe target, or leave it ready to paste.

The quality bar is not merely transcription accuracy. The product succeeds when activation, latency, cleanup, insertion, and recovery are predictable enough that users stop thinking about the application.

## 2. MVP scope

### Included

- Windows 10/11 x64 tray application.
- Push-to-talk and toggle recording modes.
- Default and selectable microphone support.
- Local transcription using `whisper.cpp`.
- English as the first validated language; Brazilian Portuguese follows through the multilingual model path.
- Fixed-language mode and optional automatic detection.
- Optional local cleanup through an existing Ollama installation.
- Automatic Ollama model discovery with explicit user selection.
- Conservative deterministic normalization and personal aliases.
- Plain-text insertion into common Windows applications.
- Raw-versus-cleaned review, copy, paste, and undo affordances.
- Local history with configurable retention; audio retention off by default.
- Per-application cleanup profiles.
- First-run hardware benchmark and model recommendation.
- Diagnostics that exclude transcript/audio content by default.

### Explicit non-goals for v1

- macOS, Linux, iOS, or Android clients.
- Meeting recording, system-audio capture, diarization, or summaries.
- Cloud accounts, synchronization, team administration, or billing.
- Real-time Vosk preview.
- Automatic learning from corrections without confirmation.
- Rich-text-preserving insertion.
- Injection into password fields, secure desktop, or elevated applications.
- Shipping or managing Ollama itself.
- A proprietary speech or language model.

## 3. Core decisions

### Speech recognition

Use `whisper.cpp` as the only production STT backend in v1. It is native, distributable on Windows, quantized, and avoids bundling Python. Keep `ISpeechRecognizer` replaceable.

Do not include Vosk initially. Vosk is attractive for low-cost partial results, but a Vosk-preview/Whisper-final design doubles model and reconciliation complexity. Add it later only if measured user research shows that a live transcript is more valuable than a simple listening waveform/status.

Suggested first-run choices:

- Default: multilingual `small` quantized model selected after benchmarking.
- Low-resource option: multilingual `base` quantized model.
- English-only option: corresponding `.en` model.
- Larger models: opt-in, never silently downloaded.

Models are downloaded from a pinned manifest containing source, license, size, SHA-256, language class, quantization, minimum RAM, and supported accelerators.

### Voice activity detection

Use Silero VAD to trim silence and reject noise-only recordings. Push-to-talk always ends on key-up; VAD must not prematurely terminate deliberate pauses. Maintain approximately 300 ms of pre-roll so the first phoneme is not clipped.

### Text cleanup

Use a three-stage pipeline:

1. Raw transcript from STT.
2. Deterministic normalization: whitespace, punctuation spacing, explicit aliases, and explicit spoken commands.
3. Optional Ollama transform: filler removal, false-start cleanup, punctuation, and profile formatting.

Ollama is enhancement, not infrastructure. If it is unavailable, slow, invalid, or unsafe, Phorminx inserts the deterministic result and preserves the raw result.

The cleanup system prompt must say, in substance:

> Edit only for punctuation, capitalization, filler words, false starts, and requested formatting. Preserve all claims, names, numbers, URLs, commands, and technical terms. Do not answer the text or add information. Return only the edited transcript.

Use temperature zero, disable reasoning where supported, cap output growth, and validate the response before accepting it.

Reject or warn on cleanup output when it:

- Drops or changes numbers, URLs, email addresses, or protected terms.
- Is empty when the input is non-empty.
- Exceeds a configurable input/output length ratio.
- Contains prompt wrappers, explanations, or structured chatter.
- Times out or is cancelled.

### Ollama discovery and lifecycle

- Probe the local API at `http://localhost:11434`.
- Use `GET /api/tags` to list installed models.
- Inspect candidate details and benchmark them; never silently select an arbitrary model.
- Store one explicitly selected cleanup model.
- Warm it after the tray, hotkey, audio, and STT services become usable.
- Default `keep_alive` to 15 minutes.
- Offer `Instant` (resident), `Balanced` (15 minutes), and `Memory saver` modes.
- Do not block application startup while warming.
- Do not automatically pull a model without explicit consent.
- Treat local and cloud-tagged models distinctly and warn before any non-local processing.

### Windows UI

Use native Rust with `egui`/`eframe` for settings, history, onboarding, and the non-activating status overlay. Use a Rust tray library and keep Win32 interoperability isolated in a Windows platform crate.

One per-user process owns:

- A single-instance mutex and named-pipe activation endpoint.
- A dedicated low-level-keyboard-hook message thread.
- Audio capture and buffering.
- STT and cleanup job queues.
- Target tracking and insertion.
- Local persistence.

### Hotkeys

True hold/release requires `WH_KEYBOARD_LL`; `RegisterHotKey` alone does not provide dependable key-up semantics. The hook callback must enqueue an event and return immediately. Ignore injected events, debounce auto-repeat, and unhook deterministically.

Provide toggle mode using `RegisterHotKey` as an accessibility fallback. Do not swallow keys by default. Detect conflicts during shortcut configuration.

### Safe insertion

Capture the foreground window and focused UI Automation element at key-down, before showing any UI. The overlay must not activate or steal focus.

Default insertion sequence:

1. Confirm the original target remains foreground and is not sensitive.
2. Snapshot the clipboard with bounded retries.
3. Put Unicode transcript text on the clipboard.
4. Send `Ctrl+V` using `SendInput`.
5. Restore the snapshot only if the clipboard still contains Phorminx's payload.

Clipboard restoration is inherently racy. Never overwrite clipboard content changed by another process. Provide an option to leave the dictated text on the clipboard.

UI Automation `ValuePattern.SetValue` is not the default because it replaces entire control values and is inconsistent across Chromium, Electron, editors, and terminals. Unicode keystroke injection is a last resort for short plain text.

Never inject when:

- Focus moved to another application after recording.
- The focused element is a password field.
- The target is on the secure desktop.
- Integrity levels prevent safe insertion.
- The target closed or its identity changed.

Safe failure means showing `Ready to paste` and keeping the result on the clipboard, never forcing text into an uncertain target.

## 4. Runtime state machine

```text
Starting
   ↓
Idle ⇄ Listening
          ↓
   FinalizingAudio
          ↓
      Transcribing
          ↓
       Normalizing
          ↓
   Cleaning (optional)
          ↓
    ReadyToInsert
          ↓
       Inserting
          ↓
         Idle
```

Every dictation has a correlation ID and cancellation token. `Cancelled` and `Faulted` can be entered from every active state. A new activation while processing uses an explicit policy: cancel the unfinished dictation or reject the new activation; v1 should not silently queue multiple insertions.

Readiness is reported separately:

- Audio ready/unavailable.
- STT absent/downloading/loading/ready/faulted.
- Ollama absent/model absent/loading/ready/faulted.

## 5. Solution structure

```text
src/
  LocalFlow.Desktop/                 WPF, tray, composition, onboarding
  LocalFlow.Core/                    state machine, contracts, domain types
  LocalFlow.Windows.Infrastructure/  hooks, WASAPI, HWND/UIA, clipboard, startup
  LocalFlow.Engine.Whisper/          whisper.cpp adapter
  LocalFlow.Transform.Ollama/        discovery, lifecycle, cleanup adapter
  LocalFlow.Persistence/             SQLite repositories and settings
tests/
  LocalFlow.Core.Tests/
  LocalFlow.Engine.Tests/
  LocalFlow.Windows.IntegrationTests/
  LocalFlow.Benchmarks/
tools/
  ModelManifest/
  CompatibilityHarness/
docs/
```

Primary contracts:

```csharp
pub trait AudioCapture
{
    Task StartAsync(TimeSpan preRoll, CancellationToken cancellationToken);
    Task<AudioClip> StopAsync(CancellationToken cancellationToken);
}

pub trait SpeechRecognizer
{
    Task LoadAsync(ModelSpec model, DeviceSpec device, IProgress<double> progress,
        CancellationToken cancellationToken);
    Task<TranscriptResult> TranscribeAsync(AudioClip clip,
        TranscriptionOptions options, CancellationToken cancellationToken);
    Task UnloadAsync(CancellationToken cancellationToken);
}

pub trait TextTransformer
{
    Task<TransformResult> TransformAsync(string input, TransformContext context,
        CancellationToken cancellationToken);
}

pub trait TextInjector
{
    Task<InsertionResult> InsertAsync(TargetSnapshot target, string text,
        CancellationToken cancellationToken);
}
```

Audio capture uses WASAPI shared mode through NAudio. Convert off the audio callback thread to mono 16 kHz samples. Use pooled buffers and bounded channels; never run recognition or cleanup on UI, hook, or audio callback threads.

## 6. Local data model

SQLite with WAL mode stores:

- `Dictations`: ID, timestamps, app-profile ID, language, raw text, normalized text, cleaned text, selected output, timings, model versions, warning flags.
- `LexiconEntries`: written form, optional spoken aliases, language, app scope, case policy, enabled state, source, confirmation time.
- `AppProfiles`: executable identity/path hash, style prompt, language override, insertion preference, deny flag.
- `Models`: provider, model name, digest/version, capability results, benchmark results, last used.
- `Settings`: typed settings schema with migration version.

Raw audio is never persisted by default. If diagnostic recording is enabled, show an unmistakable indicator, encrypt files at rest using Windows DPAPI-protected keys, enforce short retention, and support immediate deletion.

History defaults to local-only and should offer disabled, 24-hour, 7-day, 30-day, and indefinite retention. `Clear history` must be immediate and auditable locally.

## 7. UX flows

### First run

1. Explain local processing and microphone/accessibility behavior.
2. Request microphone permission.
3. Choose and test a shortcut.
4. Benchmark hardware.
5. Recommend an STT model and obtain download consent.
6. Detect Ollama; optionally select and test a cleanup model.
7. Run a test dictation without automatic insertion.
8. Offer launch-at-login.

### Normal dictation

- Key-down: quiet sound/status overlay; capture target and begin audio.
- While held: waveform/listening indicator, cancel affordance.
- Key-up: show transcribing, then cleaning if enabled.
- Success: insert if target is still safe; otherwise show ready-to-paste.
- Failure: preserve the best available raw text and a concise recovery action.

### Correction

History displays raw, deterministic, and cleaned variants. Users may copy/paste raw, restore the last insertion, or propose an alias. No automatic dictionary promotion occurs without confirmation.

## 8. Performance and reliability objectives

- Capture acknowledgement: p95 under 50 ms.
- No clipped initial phoneme in at least 99.5% of controlled trials.
- Warm STT, key-up to transcript ready for utterances up to 10 seconds: p50 <= 350 ms, p95 <= 900 ms on reference hardware.
- Warm cleanup: p95 <= 700 ms for a typical short dictation on the recommended local model.
- Cold base/small STT readiness: <= 5 seconds on reference hardware.
- STT p95 real-time factor below 0.25 on reference hardware.
- Zero wrong-window insertions in automated and manual release suites.
- No keyboard-hook leaks after 1,000 activation cycles.
- No silent transcript loss; every failure yields raw text, clipboard text, or an explicit error.

These are targets to validate, not promises. If hardware misses the latency target, recommend a smaller model rather than concealing the delay.

## 9. Test strategy

### Speech benchmark corpus

Build a consented private corpus of approximately 300 clips covering:

- 0.3–2 second commands, 2–10 second dictation, and 10–60 second paragraphs.
- English for the v1 release corpus; Brazilian Portuguese in the subsequent multilingual corpus.
- Multiple accents, quiet rooms, fans, music, laptop and headset microphones.
- Names, technical terms, numbers, URLs, email addresses, code, and false starts.
- At least 100 silence/noise-only clips for hallucination testing.

Measure WER/CER, protected-entity exact match, empty/hallucination rate, language detection, latency percentiles, real-time factor, RAM/VRAM, cold load, CPU, and battery impact.

### Cleanup conformance corpus

Each case contains raw input, allowed transformations, and protected tokens. Every supported Ollama model must pass:

- Meaning and polarity preservation.
- Exact preservation of names, numbers, URLs, emails, and code tokens.
- Output-only instruction following.
- Filler and false-start removal.
- Formatting without unsolicited answers.
- Timeout and cancellation behavior.

### Windows compatibility matrix

Manually and automatically exercise:

- Notepad, Office, browser inputs/contenteditable, VS Code, Slack/Teams-class Electron apps, Windows Terminal, and RDP.
- Emojis, non-Latin scripts, multiline text, and large selections.
- Focus changes, closed targets, elevated targets, password fields, UAC, concurrent clipboard changes.
- Microphone unplug/replug, device changes, sleep/resume, multiple monitors and DPI scales.
- No Ollama, stopped Ollama, missing selected model, model eviction, and insufficient memory.

## 10. Security and privacy boundaries

- Never capture system audio in v1.
- Never read surrounding text without explicit opt-in.
- Never inject into known password fields or secure desktop.
- Never upload audio, transcripts, diagnostics, or model prompts.
- Clearly distinguish local Ollama models from any cloud model endpoint.
- Bind local integrations only to loopback; do not expose an unauthenticated network listener.
- Treat model output as untrusted data.
- Sign release binaries and verify all downloaded model artifacts.
- Redact transcript content, paths, window titles, and clipboard data from logs by default.

Primary threat scenarios are wrong-target disclosure, clipboard destruction, malicious model output, compromised model downloads, sensitive-field capture, and overly broad diagnostics.

## 11. Packaging and distribution

Start with a self-contained x64 Rust release build and signed per-user installer under LocalAppData. WiX Toolset or Inno Setup can provide install/uninstall, Start Menu entry, and launch-at-login using HKCU. Do not require administrator privileges.

Abstract updates from the start, but do not build auto-update in the first vertical slice. Production releases need code signing to reduce SmartScreen friction. Evaluate MSIX later if Store distribution or packaged identity becomes valuable.

Models are separate downloads and are not committed to Git or embedded in the installer.

## 12. Principal risks

| Risk | Consequence | Mitigation |
|---|---|---|
| Wrong-target insertion | Sensitive data disclosure | Snapshot at key-down, require same safe target, otherwise clipboard-only |
| LLM changes meaning | Incorrect communication | Protected-token validation, raw fallback, transform conformance tests |
| Clipboard race | Lost user data | Sequence checks, bounded restore, never overwrite externally changed clipboard |
| Slow local hardware | Product feels broken | Benchmark, recommend smaller models, optional cleanup, background warm-up |
| Elevated target/UIPI | Injection failure | Detect and fall back to manual paste; do not elevate whole app |
| Model supply chain | Code/data compromise | Pinned manifest, HTTPS, SHA-256, source/license metadata |
| Hook/audio instability | Missed input or crashes | Dedicated threads, minimal callbacks, soak and hotplug tests |
| Product scope expansion | Delayed usable loop | Enforce v1 non-goals and vertical-slice milestones |

## 13. Definition of MVP done

The MVP is ready for a private alpha when:

- A clean Windows machine can install and complete onboarding without developer tools.
- The recommended STT model can be downloaded and verified.
- Push-to-talk works reliably across the compatibility matrix.
- Dictation works entirely without Ollama.
- At least one small Ollama model passes the cleanup conformance gate.
- Unsafe or stale targets always fall back to ready-to-paste.
- Raw output is recoverable for every successful transcription.
- Privacy defaults and retention controls match this document.
- Performance targets are measured on declared reference hardware.
- Installer, binaries, manifest, and release artifacts are signed.

