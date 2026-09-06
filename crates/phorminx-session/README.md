# phorminx-session

Platform-neutral foundations for dictation sessions longer than the live in-memory capture window.

Production capture uses a 45-second rolling in-memory audio ring and reclaims
samples after recognition owns their text, with boundary context retained.
Production disk spill is disabled. See `docs/ADR-015-EXTENDED-DICTATION.md` for
the current integration; this crate's reusable ledger and legacy spool are
not by themselves the production recognition state machine.

## Invariants

- Every audio position is an absolute half-open sample range at 16 kHz mono.
- Transcript checkpoints advance one contiguous frontier. Their speech and silence blocks must
  partition the checkpoint coverage exactly; a gap or overlap is rejected without mutation.
- Speech blocks carry their exact separator-before boundary. Assembly never guesses spaces, and
  separator bytes count against the same transcript bound as recognized text.
- Worker generations advance explicitly and monotonically. Sequences start at zero for each
  generation and are accepted in order. Identical retries are idempotent; conflicting retries,
  stale generations, and future results fail closed.
- A failed checkpoint creates an unresolved frontier. Transcript assembly is forbidden until a
  recovery checkpoint names that exact failure and covers forward from the frontier.
- The capture subsystem seals the ledger with its authoritative final sample. Transcript assembly
  requires complete coverage through that exact frontier and is a one-shot finalization operation,
  preventing truncated or duplicate persistence/insertion by construction.

## Encrypted audio spool

This is a legacy compatibility/test facility. Healthy production dictation
does not create or append audio spool files. Its tests qualify the facility
itself and must not be represented as proof of rolling-capture integration.

Each spool receives a fresh 256-bit key from `ring::rand::SystemRandom`. The key is held only in
memory and is never serialized. A random nonce prefix and monotonic record counter give every
AES-256-GCM record a unique nonce under that key. The file header and every record header are
authenticated as associated data, while audio samples are encrypted independently per bounded
record.

Spool creation and scavenging require a validated owner marker in an application-owned root; a
non-empty unmarked directory is refused, and live exclusively locked files are skipped. The
in-memory index is authoritative for exact range reads. Quotas are checked before a write,
partial writes are truncated back to the last committed offset, reads have an independent memory
bound, and malformed, tampered, or truncated records never return audio. Normal cleanup closes and
deletes the file; startup scavenging exclusively locks filename-matched crash orphans without
attempting to decrypt them. Because their keys died with the prior process, orphan bytes cannot be
decrypted. Raw
keys and transient plaintext buffers use zeroizing guards on success and error paths.

This crate intentionally contains no logging. Integrators may emit content-free counters and
timings, but must never log transcript text, plaintext audio, encryption keys, or nonces.
