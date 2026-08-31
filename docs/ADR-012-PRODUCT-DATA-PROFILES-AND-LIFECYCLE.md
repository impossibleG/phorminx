# ADR-012: Product data, per-application profiles, and lifecycle recovery

Status: accepted

## Context

The walking skeleton deliberately avoided persistence. A usable local product needs recoverable transcripts, explicit vocabulary corrections, application-specific behavior, and predictable recovery after Windows lifecycle changes. These features must not expand the privacy boundary or let asynchronous work insert into a stale target.

## Decision

### Persistence and identity

- Store product data in a local SQLite database beside the settings file, using schema migrations and WAL mode.
- Keep history disabled by default. Enforce 24-hour, 7-day, 30-day, or indefinite retention during normal writes, and delete immediately when history is disabled or cleared.
- Store no audio.
- Identify application profiles and scoped aliases by validated executable basename only. Reject full paths and window titles at the repository boundary.

### Personal lexicon

- Apply enabled exact aliases deterministically before optional Ollama cleanup.
- Scope entries by language and optional executable basename.
- Preserve explicit case policy and require user-authored CRUD changes; never learn or promote aliases automatically.

### Application profiles

- Allow an executable-specific formatting profile, custom instructions, language override, insertion preference, or deny rule.
- A profile may make insertion more conservative by selecting clipboard-only, but cannot bypass target-safety checks.
- Reload profile and lexicon state through a clean application restart after edits.

### Lifecycle

- Enforce one Phorminx process per Windows session with a named mutex.
- On Windows resume notifications, cancel any owned recording or pending transcription state, drop captured resources, transition to Idle, and display Ready.
- Treat a worker completion received after recovery as stale and never insert it.
- If an exact saved microphone cannot start, retry once with the current Windows default device.

## Consequences

- Successful transcriptions can be recovered without retaining audio or sending content off-device.
- Profiles cannot become a source of full-path or window-title telemetry.
- Resume favors losing one in-flight dictation over risking insertion from pre-suspend state.
- Runtime restarts after lexicon/profile edits are visible but keep concurrency and cache invalidation simple for the private alpha.

## Validation

- Persistence integration tests cover migrations, retention, immediate clearing, content round-trips, exact scoped aliases, basename rejection, profiles, and error redaction.
- Runtime tests cover 500 mixed dictation cycles, fault recovery, explicit Cleaning state, resume during recording, and rejection of a late completion after resume.
- Windows tests cover the single-instance mutex, safe target identity, insertion fallbacks, and native-window value mappings.
