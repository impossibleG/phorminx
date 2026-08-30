# Phase 1 stability and wrong-target qualification

Phase 1 uses separate automated and physical gates. Synthetic tests can prove state ownership and fail-closed routing; they cannot prove machine-global Windows focus or input behavior.

## Automated gates

### Orchestration soak

Run 500 deterministic mixed dictation cycles through fake audio, recognition, target, insertion, and status boundaries. The mix includes successful paste, changed and missing targets, short and quiet recordings, transcription failure, empty output, insertion failure, audio start/finish failure, busy activation, and stale or duplicate worker results.

Every cycle must end in `Idle` with no recording, target, or worker result retained. Dictation IDs must remain correlated, worker depth must stay bounded at one, and failed or stale cycles must never call the injector.

### Insertion policy suite

Exercise the production decision order with fake clipboard, target, modifier, and input backends. Every uncertain condition must return clipboard-only before injection. Only a verified stable target with released modifiers may call the injector, exactly once.

### Native audio soak

Run 500 serial default-microphone start/finish cycles plus 25 start/drop cancellation cycles in one release process. Audio is held only in memory and immediately discarded. Every completed clip must be finite 16 kHz mono, and no cycle may overflow its bounded capture buffer. Report only counts and latency percentiles.

## Physical Windows matrix

Run this separately on a disposable desktop session. Never automate real `SendInput` against an uncontrolled foreground application.

| Scenario | Expected result |
|---|---|
| Classic Notepad writable edit remains focused | Paste once into the original edit |
| Focus moves to another Notepad edit during transcription | Clipboard-only; no injected paste |
| Foreground application changes during transcription | Clipboard-only; no injected paste |
| Password, read-only, disabled, or hidden classic edit | Clipboard-only |
| Browser, VS Code, Slack/Teams, Terminal, Office, WPF, WinUI, UWP | Clipboard-only in Phase 1 |
| Shortcut modifiers remain pressed beyond the release deadline | Clipboard-only |
| Elevated or secure-desktop target | Clipboard-only |
| Overlay changes state on any supported DPI/monitor | Foreground and focused control remain unchanged |

Record only application class/outcome categories, correlation IDs, timing, and pass/fail. Do not record window titles, transcript text, clipboard contents, or audio.

## Exit rule

The Phase 1 exit criterion is met only when all automated gates pass and the physical wrong-target matrix has no injection into an unverified target. Application coverage beyond classic Win32 `Edit` remains a Phase 2 compatibility feature, not a reason to weaken the fallback policy.

## Reference-machine results

On 2026-08-30, the deterministic orchestration soak completed 500 mixed cycles with zero wrong-target fake pastes, no stale insertion, bounded worker depth, and clean `Idle` ownership after every cycle. The fake-backed insertion policy suite passed nine fail-closed and success-path cases without accessing the system clipboard or calling `SendInput`.

The release-mode native soak then completed 500 real default-microphone start/finish cycles and 25 start/drop cancellation cycles in 74.134 seconds. It validated and discarded 692,480 finite 16 kHz frames (43.280 seconds of audio) without a crash, empty clip, capture overflow, or retained recording. Start latency was 12.716 ms p50 and 15.089 ms p95; finish latency was 3.205 ms p50 and 4.100 ms p95. A mid-run snapshot showed 25.4 MiB private memory, 11.7 MiB working set, 218 handles, and 8 threads.

The aggressive device reopen loop observed 163 recoverable WASAPI notifications. Every affected iteration still returned valid audio. Treat the count as a stress characteristic to monitor during longer natural dictation and hotplug testing, not as lost or silently truncated content.

These automated gates are complete. The physical Windows wrong-target matrix remains open.
