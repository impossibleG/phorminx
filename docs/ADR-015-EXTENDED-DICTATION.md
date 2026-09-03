# ADR-015: Extended dictation uses checkpointed recognition and encrypted scratch audio

## Status

Accepted for implementation.

## Context

Phorminx currently preallocates a 120-second native-rate audio ring. Accurate
and Instant recognition both retain that complete recording as their final
correctness fallback. Raising the allocation only moves the duration cliff;
making the ring silently overwrite old samples makes fallback incomplete.

Lossless long capture, bounded memory, and recovery from decoder stalls cannot
all be provided without a bounded secondary store. Persisting recoverable raw
audio would violate the product's local-data expectations.

## Decision

Short dictations keep the existing in-memory full-recording fallback. Before
that archive can overflow, an active recording transitions without restarting
capture into an extended session with these properties:

1. Audio is resampled into canonical 16 kHz mono spans with absolute sample
   positions.
2. A transcript ledger accepts only generation-matched, ordered, contiguous
   checkpoints. Stable text is append-only and downstream formatting,
   insertion, and history still happen exactly once.
3. Canonical audio spans are appended to a transaction-owned encrypted scratch
   spool. Each bounded record uses authenticated encryption, an independent
   nonce, and authenticated range metadata.
4. The encryption key exists only in process memory and is never written to
   settings, history, logs, receipts, or the scratch directory. A crash can
   leave ciphertext but cannot leave a recoverable key.
5. Scratch files are created privately, deleted on every terminal path, and
   scavenged on later startup. Scavenging may identify only Phorminx-owned
   scratch names and never follows links or deletes user paths.
6. Memory keeps only the current capture lane, decoder windows, rollback
   overlap, checkpoint metadata, and accumulated text. Decoder queues are
   bounded and coalesce obsolete speculative work.
7. Physical release and resource-driven safe finalization use the same final
   priority path. Disk-full, corruption, quota, or unrecoverable coverage gaps
   fail closed: Phorminx never labels or inserts a truncated transcript as a
   successful complete dictation.
8. Extended scratch behavior is disclosed in setup/settings. Raw audio is
   never retained after the session and never enters history.

No component may describe the feature as literally unlimited. A documented
scratch quota, output limit, and graceful safe-stop policy remain resource
guards rather than a hidden time-based cliff.

## Recognition and formatting

Accurate mode uses timestamped Whisper segments and bounded decoder context to
advance the ledger. Instant mode drains finalized Vosk endpoints and tracks an
absolute acknowledged-audio cursor. Ambiguous seams schedule a bounded repair
range from the encrypted spool; stale or duplicated results cannot advance the
frontier.

Raw and Light formatting remain linear. Ollama-backed profiles operate on
ordered, boundary-safe document chunks with protected-token validation per
chunk and deterministic source-text fallback. Ollama work runs independently
from capture and recognition so cleanup cannot starve the audio pipeline.

## Required evidence

- Generated event traces prove one terminal outcome, insertion, and history row.
- Random sample formats and callback sizes prove exact canonical coverage.
- Long synthetic sessions demonstrate bounded RAM and complete spool cleanup.
- Tamper, truncation, quota, disk, cancellation, sleep, and device-loss tests
  fail closed without stale insertion.
- English and PT-BR seam corpora contain no checkpoint omission or duplication.
- Standard short-dictation latency and fallback behavior do not regress.

