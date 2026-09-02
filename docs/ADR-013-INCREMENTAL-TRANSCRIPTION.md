# ADR-013: Adaptive incremental transcription

- Status: Accepted
- Date: 2026-09-01

## Decision

Keep the existing microphone stream and 120-second bounded native-rate ring as
the source of truth. Short dictations remain single-shot. Once a recording has
at least four seconds of uncommitted audio, the existing application-loop wakeup
may schedule a bounded Whisper-only partial job. Prefer a boundary after an
800 ms low-energy observation; force a boundary after eight seconds of
continuous audio. Start every later chunk 800 ms before the prior stable end.
Allow only one partial job to be outstanding.

Snapshotting is read-only. `ActiveRecording` observes one occupied length,
copies only an explicitly validated range into an owned vector, releases all
ring-buffer borrows, and resamples away from CPAL's callback. It never advances
the consumer during capture. The complete recording therefore remains intact
for finalization and correctness fallback, and each extra allocation is bounded
to at most the maximum chunk plus overlap.

Partial jobs run only Whisper. They do not apply aliases, deterministic
normalization, Ollama formatting, persistence, insertion, or completion UI.
The single resident worker serializes partial jobs and the final job, preserving
audio order without a socket or a second recognizer. On release, the final job
transcribes audio from 800 ms before the last stable boundary through release,
reconciles it with stable partial text, then runs the existing formatting and
insertion path exactly once.

Reconciliation is deliberately fail-closed. A boundary forced through speech
requires two or more exact normalized words across the transcript suffix and
prefix. A single matching word is ambiguous because it may be deliberate
repetition. A measured-silence boundary may concatenate unmatched text, but
still refuses an ambiguous single-word match. If a snapshot, submission,
partial recognition, ordering check, or overlap check is uncertain, discard the
incremental assembly and transcribe the untouched full clip once.

## Rationale

This removes most post-release STT latency from longer dictations without
changing the reliable path for short dictations. It keeps capture callback work
constant, bounds memory and worker backlog, and prevents chunk-level cleanup
from producing inconsistent tone or duplicate history entries.

The energy observation is only a scheduling hint. It neither trims endpoints
nor classifies the whole recording as speech, so ADR-005's plan to use a
separately managed Silero VAD model remains valid.

## Operational evidence

Content-free diagnostics expose:

- `incremental_partial_submitted` with sequence, boundary kind, and audio time;
- `incremental_partial_completed` with success and inference time;
- `incremental_final_tail` when the low-latency path is used;
- `incremental_fallback` with a non-content reason when full-clip recovery runs;
- `incremental_cancelled` on abandoned listening sessions.

No transcript or audio content is logged or persisted by this feature.

## Consequences

- Dictations shorter than four seconds have no additional inference or copy.
- Long continuous speech produces at most one queued partial every four to
  eight seconds, never an unbounded worker backlog.
- Conservative reconciliation can choose the slower full-clip path when
  Whisper renders an overlap differently. Correctness wins over latency.
- Formatting remains a final, globally coherent operation.
- A future real VAD can replace the low-energy boundary hint without changing
  the worker protocol or final fallback invariant.
