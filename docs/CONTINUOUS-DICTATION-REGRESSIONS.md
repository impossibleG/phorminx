# Continuous dictation regression investigation — 2026-09-06

## Reports and reproduced causes

The reports concern Accurate stopping without an explicit stop, Instant sometimes
failing around startup or release, and completed long text remaining in History
instead of reaching normal insertion. Recovery is not the desired delivery path.

The following are separate code paths, not evidence that every reported incident
had the same cause. No private transcript contents were used for this investigation.

- Accurate: native `base.en` recognized synthesized speech successfully, but the
  application stopped incremental processing after its timestamp-repair window
  exceeded 12 seconds. In the reproducer, the accepted frontier froze at 10.8
  seconds and the application declared failure at 25.8 seconds. This eventually
  exhausts the rolling audio buffer even though the decoder itself is healthy.
- Instant: a successful native empty endpoint was treated as failure if an RMS
  threshold was exceeded. Ambient noise is not proof that words were spoken.
  The same heuristic could reject a complete committed prefix at finalization.
- Delivery: a text-only repeated-phrase detector rejected successful transcripts,
  including Vosk transcripts, after saving the final text snapshot. Intentional
  repeated speech must remain eligible for ordinary formatting and insertion.
- Next recording: a completed memory-only capture failure could permanently hold
  the shared finalizer permit, preventing later starts until application restart.
- Memory cleanup: discarding bounded in-memory audio unnecessarily spawned a
  separate worker. A spawn failure or timeout could veto successful recognition
  even though no disk operation needed to complete.
- Accurate content QA also found mismatched text/audio ownership: confirming
  the full text of a provisional window while advancing only to its guarded
  boundary could add its final words again. Independent repair could likewise
  revisit more already-owned audio than the intended two-second overlap.
- Native quality testing isolated a larger contributor: supplying accumulated
  transcript text as a prompt for each tiny Whisper audio window. With the same
  ownership fixes and fixture, removing that prompt reduced the observed edit
  count from 371 to 20. Acoustic overlap remains available for context.

## Required behavior

Successful native silence/no-word results, repeated speech, and repairable timing
uncertainty must not be interpreted as instructions to end a recording. Successful
text follows normal formatting and insertion into the captured target. Ambiguous
injection results must not trigger blind retries that could paste twice or into
another window.

Accurate retries preserve their sequence and committed prefix. Timestamp repair
can use a bounded independent native decode; a successful empty result is
no-word coverage, not a decoder error inferred from signal energy. Successful
nonempty output is preserved. Confirmed full-window text owns that window only
after new right-hand context is available, and repair begins at the current
ownership boundary minus its retained overlap. These bounds apply to individual
work units, not the length of a recording.

Memory-only audio is zeroized synchronously on cleanup. A completed failed pump
releases its permit; a genuinely still-running finalizer keeps ownership until it
returns. Legacy disk cleanup and its privacy safeguards are not bypassed.

## Validation status

- Integrated workspace suite: 642 tests passed, zero failures; documentation
  tests passed. Seven asset-dependent tests are ignored by default; the native
  tests described below were run explicitly with installed local models.
- Audio crate: 46 tests passed, including repeated failed-pump/next-start cycles,
  zero-deadline memory cleanup, immediately releasing abandoned memory captures,
  still-running finalizer exclusion, and the existing long rolling-buffer tests.
  Strict audio-crate Clippy passed.
- Native Instant: all four opt-in tests passed in 99.82 seconds. The startup
  matrix covers 1/5/15/29 seconds before speech, each with silence and two room-noise
  levels. It explicitly exercises successful empty endpoints above the old RMS
  rejection threshold. Natural endpoints and six forced rollovers use 70.355
  seconds of synthesized speech.
- Native Instant extended release: 150.71 seconds of synthesized speech plus
  trailing ambient noise, through both healthy streaming and degraded sequential
  finalization. Both results retained the ordered clause anchors in both copies
  and the last clause. Results were 483/482 words, each two word edits from the
  continuous native baseline. The native baseline also splits one word under
  noise; the test compares content rather than requiring that spelling to match
  the source script exactly. Snapshot reads stay within retained audio and the
  30-second request bound.

- Native Accurate CPU: 219.06 seconds of speech, 723 reference words, 743 output
  words and 20 word edits (2.77%). The minimum alignment contains insertions only;
  this is not perfect transcription. All 14 clause anchors appear in order in
  each of the three passages. A whole-pass native baseline of the same spoken
  fixture, repeated for comparison, has zero edits. Incremental compute was
  115.51 seconds; peak uncommitted audio was 12.14 seconds, with a further two
  seconds retained as overlap. These are local regression measurements, not
  guaranteed latency on other machines.
- Native Accurate also passed 84 seconds of ambient audio, preserving already
  committed text while advancing no-word coverage. A deliberately aborted
  nonzero-sequence decode retried without losing its prefix or advancing audio
  ownership on failure. Four additional deterministic ownership tests passed.
- Accelerated Accurate: the static-CRT release test used Vulkan on the local
  Vulkan-capable GPU. The final run delivered 745 words for the same 723-word
  reference, with 22 edits (3.04%, insertions only in a minimum alignment),
  versus zero baseline edits. All 42 ordered clause anchors passed, as did
  initial silence, native abort/retry, final-tail delivery, and 84 seconds of
  ambient audio. Compute was 93.46 seconds for 219.06 seconds of speech;
  peak uncommitted audio was 11.35 seconds plus the two-second overlap.
  A preceding passing GPU run produced 768 words, demonstrating run-to-run
  variation; its exact edit count was not retained. The native regression
  retains a baseline-relative 10-percentage-point error gate rather than
  presenting one measured score as a universal accuracy guarantee.
- One earlier throughput run was invalidated because Windows slept for about
  24 minutes, confirmed by System events. The native test script now holds a
  temporary system-awake request and releases it in `finally`; no persistent
  power settings or production power behavior are changed.

Workspace Clippy with `--all-targets --locked -- -D warnings` passed after
equivalent style cleanups. The final integrated rerun again passed all 642 tests.
Formatting, diff checks, and `scripts/Test-ReleaseAssets.ps1` passed.
Existing dictations and History are not modified by testing;
native audio fixtures are synthesized neutral text, not microphone recordings.

## Packaged result

The updated per-user installer is an unsigned local development artifact:

- `artifacts/installer/Phorminx-0.1.0-x64-setup.exe`, 12,829,179 bytes.
- Installer SHA-256: `A57FB7270BEFC76AEC57859835D2A32CFA901F01138258D48A4F20E5F9191FBE`.
- Packaged executable SHA-256: `B29A303A78BC0A58F75E21F34E7DA50C1C86FEE1A28F33686B252942F4667DF0`.

The build includes Vulkan and CPU fallback and passed the native dependency
inspection. Temporary test/build drive mappings and awake requests were released.
The installed application was still running the preceding executable
(`7E6FEDBBBCAB290B92812BF775593CED0375BD617ED4D16821104FD9D91D7654`)
when this package was verified. It was not terminated or replaced during testing.
Quit that app before installing the new build. Live microphone and target-app
confirmation on the installed update remain the final manual check.

## Scope of the guarantee

These fixes address the reproduced software-triggered stops and delivery vetoes.
They do not claim perfect speech recognition or continued microphone capture
through hardware loss, manual system sleep, or an indefinite native decoder
outage. Pending audio remains bounded; its capacity is not a recording-duration
limit. Long-term processing that cannot keep up with incoming audio still needs
an explicit resource/fallback policy, not a false success indicator.
