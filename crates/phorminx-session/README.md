# phorminx-session

Platform-neutral foundations for dictation sessions longer than the live in-memory capture window.

## Invariants

- Every audio position is an absolute half-open sample range at 16 kHz mono.
- Transcript checkpoints advance one contiguous frontier. Their speech and silence blocks must
  partition the checkpoint coverage exactly; a gap or overlap is rejected without mutation.
- Worker generations advance explicitly and monotonically. Sequences start at zero for each
  generation and are accepted in order. Identical retries are idempotent; conflicting retries,
  stale generations, and future results fail closed.
- A failed checkpoint creates an unresolved frontier. Transcript assembly is forbidden until a
  recovery checkpoint names that exact failure and covers forward from the frontier.
- Transcript assembly is a one-shot finalization operation, preventing duplicate persistence or
  insertion by construction.

## Encrypted audio spool

Each spool receives a fresh 256-bit key from `ring::rand::SystemRandom`. The key is held only in
memory and is never serialized. A random nonce prefix and monotonic record counter give every
AES-256-GCM record a unique nonce under that key. The file header and every record header are
authenticated as associated data, while audio samples are encrypted independently per bounded
record.

The in-memory index is authoritative for exact range reads. Quotas are checked before a write,
partial writes are truncated back to the last committed offset, reads have an independent memory
bound, and malformed, tampered, or truncated records never return audio. Normal cleanup closes and
deletes the file; startup scavenging deletes filename-matched crash orphans without attempting to
open them. Because their keys died with the prior process, orphan bytes cannot be decrypted.

This crate intentionally contains no logging. Integrators may emit content-free counters and
timings, but must never log transcript text, plaintext audio, encryption keys, or nonces.
