# ADR-015: Extended dictation commits text and reclaims audio

## Status

Accepted; revised 2026-09-06 after the rolling-capture implementation and
independent recognition QA. Supersedes the 120-second archival capture and
full-recording fallback decisions in ADR-005, ADR-013, and both ADR-014 tracks.

The initial version of this ADR proposed retaining the recording in an
encrypted scratch spool. The product decision changed: healthy dictation
retains only bounded uncommitted audio and boundary context in memory. The
spool implementation remains a legacy compatibility/test facility and is
disabled in the production capture configuration.

## Context

A complete recording is no longer needed once recognition owns its text.
Keeping every sample made storage and release-time fallback grow with the
recording and preserved the original two-minute failure boundary. Raising a
duration constant would only postpone that failure.

Continuous dictation still needs a bound on audio that has not been recognized:
a decoder that stalls indefinitely cannot be supported by finite memory. That
bound must describe backlog, preserve already committed text, and avoid silently
overwriting speech.

## Decision

1. The microphone callback writes to bounded queues; canonical resampling,
   recognition, and persistence happen outside the callback. Audio positions
   are absolute half-open sample ranges at 16 kHz mono.
2. Production capture uses a preallocated 45-second canonical audio ring, with
   disk spill disabled. The ring is a bound on outstanding audio, not a limit
   on the total dictation duration. Healthy recordings can continue while the
   recognizer advances its committed frontier.
3. A worker acknowledgement releases only samples whose text is owned, keeping
   the recognition mode's required boundary context. The ring advances in
   constant time and zeroizes reclaimed slots. Queued decoder jobs and copies
   remain bounded independently of total recording duration.
4. Accurate mode validates Whisper segment timestamps against the actual audio.
   Measured silence and small timestamp alignment jitter may close coverage
   gaps; unresolved speech prevents frontier advancement. A bounded repair
   window re-decodes uncertain boundaries, and two seconds of retained context
   remain available after reclamation. The release path actually decodes that
   context. Ambiguous text seams preserve potentially meaningful words instead
   of deleting them; this can preserve a duplicate rendering.
5. Instant mode commits finalized Vosk endpoints with exact sample positions.
   Mutable partial hypotheses never become owned text. Continuous speech uses
   a rollover target of 30 seconds, word timestamps, and a two-second overlap:
   only complete words before the cutoff are committed, and the remainder is
   replayed into the next recognizer. A word crossing the cutoff is retained.
6. Release combines the owned prefix with the bounded remaining audio and
   applies formatting, history, and insertion through the existing single
   final-result path. Once audio has been reclaimed, no recovery may request a
   full recording from sample zero. Queue gaps, decoder failures, and energetic
   empty output use the retained frontier and explicit recovery/failure rules.
7. If recognition cannot commit audio before the ring fills, capture stops
   before overwriting uncommitted samples and finalizes the retained prefix.
   A backlog stop must preserve recoverable text and release the next recording
   permit. It must not present unrecoverable omitted speech as a successful
   complete dictation. Device loss, cancellation, shutdown, and stale results
   retain their typed lifecycle handling.
8. Whole-session energy statistics are accumulated while audio passes through.
   A long silence at release cannot erase the evidence that earlier, reclaimed
   audio contained speech.
9. Audio never enters history, logs, or the production scratch store. Content-free
   diagnostics may record counts, timing, memory bounds, and categorical errors.
   Text grows with dictation length; bounded audio does not mean bounded total
   process memory or guaranteed recognition accuracy for arbitrary speech.

## Recognition and formatting

Whisper and Vosk models remain resident according to the selected residency
policy. CPU recognition remains supported, and Auto may fall back from Vulkan
when needed. Recognition speed still determines whether the uncommitted-audio
budget can be maintained on a particular machine.

Raw and Light formatting remain deterministic. Ollama-backed profiles operate
on ordered document chunks with protected-token validation and source-text
fallback. Formatting runs after recognition in a separate worker so it does
not block capture or live recognition.

The platform-neutral transcript ledger and encrypted spool are reusable
foundations. Production recognition currently enforces its ownership protocol
through its mode-specific accumulators; ledger unit tests alone are not proof
of the end-to-end capture integration.

## Text-only interrupted-dictation recovery

When History is enabled at recording start, committed text may be checkpointed
in the existing local database. Only text protected by Windows DPAPI for the
current account crosses the database boundary; audio is never checkpointed.
This is best-effort recovery of a coherent committed prefix, not a promise to
recover the last spoken word or an unfinished formatting request.

The recording captures its history eligibility and privacy epoch before audio
capture. An asynchronous writer coalesces snapshots so disk and encryption work
do not run on the microphone callback or recognition worker. Clearing or
disabling History invalidates earlier epochs and removes saved drafts; normal
retention expiry includes these drafts. Successful paste or clipboard delivery
and a no-speech result discard the session checkpoint; normal quit also removes
the current active session. Recognition failure or suspend may preserve an
interrupted prefix. Retired sessions cannot be recreated by a late writer.
Normal shutdown drains and joins the writer.

Active recording checkpoints are hidden from the interrupted list. After a
crash, previous-process checkpoints become available as incomplete recovered
text. History and diagnostics must not imply that a recovered prefix contains
all audio through release. Recovery actions are explicit and never re-use an
old dictation target for automatic insertion. Protection or persistence failure
must not stop dictation or fall back to storing plaintext recovery data.

## Evidence and limitations

Automated coverage includes ring wraparound and zeroization, long simulated
capture with no audio spool, backlog finalization and permit reuse, Accurate
timestamp gaps and repair boundaries, Instant word-cutoff replay, repeated
phrases, release tails, and recognition failure recovery. Both recognition
tracks received independent adversarial review and regression fixes.

Hours-long tests use synthetic audio and/or scripted recognition results. They
prove ownership and resource invariants, not hours of native speech accuracy.
The native spoken-rollover test additionally exercises 70.355 seconds of
synthesized English through Vosk, including six deliberately triggered
rollovers and the final release tail. It detected ordinary wording changes and
one omitted conjunction at a restart seam despite contiguous audio ownership;
see `INSTANT-SPOKEN-ROLLOVER-VALIDATION.md` for the measured error bounds.
These tests do not substitute for continuous English and PT-BR speech corpora,
physical microphone/device-loss tests, or release-latency measurements. The
current evidence and remaining gates are recorded in
`EXTENDED-SETUP-ACCEPTANCE.md`.
