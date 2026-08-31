# Windows private-alpha compatibility protocol

This protocol creates auditable, content-free results for scenarios that need real Windows hardware and applications. It must not be treated as complete merely because the harness starts successfully.

## Start and inspect a run

```powershell
$run = .\scripts\Invoke-PhorminxCompatibility.ps1 -Start -RunDirectory .\artifacts\compatibility
.\scripts\Invoke-PhorminxCompatibility.ps1 -List
```

Use non-sensitive test text made specifically for QA. Never dictate passwords, personal messages, tokens, customer data, or production source code during these exercises.

Record a result with a fixed issue code; the harness intentionally accepts no free-form notes:

```powershell
.\scripts\Invoke-PhorminxCompatibility.ps1 `
  -Record `
  -RunFile $run.FullName `
  -Scenario focus_change `
  -Outcome pass `
  -IssueCode none

.\scripts\Invoke-PhorminxCompatibility.ps1 -Summary -SummaryRunFile $run.FullName
```

## Scenario acceptance checks

| Scenario | Action | Pass condition |
|---|---|---|
| `sleep_resume` | Start Phorminx, sleep Windows, resume, then dictate twice. | Tray and hotkey recover without duplicate triggers; both new captures complete. |
| `microphone_hotplug` | Unplug the active microphone while idle and while listening; reconnect it. | No crash or hang; clear unavailable status; capture recovers or asks for a valid device. |
| `default_device_change` | Change the Windows default input device between dictations. | Selected/default-device policy is predictable and the next recording uses the reported device. |
| `focus_change` | Start in target A, move focus to target B before transcription completes. | No automatic insertion into either uncertain target; result remains ready to paste. |
| `closed_target` | Start recording, close the captured target before completion. | No automatic insertion; result remains recoverable. |
| `clipboard_race` | Change the clipboard after release while Phorminx is processing. | Phorminx never overwrites the newer external clipboard value during restoration. |
| `elevated_target` | Dictate toward an elevated test editor while Phorminx is not elevated. | No forced injection or elevation prompt; safe clipboard/manual-paste fallback is shown. |
| `password_field` | Capture a target classified as a password field using non-secret QA text. | Phorminx never injects automatically. |
| `multi_monitor_dpi` | Move the target across monitors/DPI scales and dictate. | Overlay remains visible/non-activating and the original safe target contract holds. |
| `ollama_unavailable` | Stop Ollama and use every cleanup profile. | Raw/deterministic output remains usable; startup and dictation do not depend on Ollama. |
| `ollama_model_eviction` | Evict/stop the selected model between cleanup operations. | Timeout/error degrades to deterministic output without transcript loss. |

For every failure, choose the closest fixed issue code. Put screenshots, dumps, audio, transcripts, clipboard values, window titles, paths, or free-form investigative notes in a separately access-controlled location only after explicit consent; do not add them to the JSON run record or the default diagnostic bundle.

## Required declarations before an alpha claim

- Windows edition/build, hardware class, and application versions used for the run.
- Whether each scenario was physically executed, blocked, or not applicable.
- Exact Phorminx commit and installer SHA-256 kept in release engineering records, not embedded in content-bearing diagnostics.
- Authenticode result for both binary and installer.

No current repository automation proves physical hotplug, sleep/resume, elevated-target, multi-monitor, application compatibility, or code-signing success.

