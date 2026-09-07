# Extended Dictation and Setup/Repair acceptance gates

This document is the release-blocking evidence checklist for ADR-015 and
ADR-016. A checked implementation test is not a substitute for the physical
hardware gates at the end.

## Original rolling-capture evidence snapshot (2026-09-06)

The current change replaces full-recording scratch audio with a 45-second
in-memory backlog window. This snapshot distinguishes implemented regression
coverage from physical qualification; unchecked items below are not claims of
completed testing.

The counts and installers below describe the original rolling-capture release,
not qualification of subsequent continuity fixes. The follow-up regression
report records the newer evidence separately.

- [x] Ring wraparound, exact retained ranges, reclaimed-slot zeroization, and a
  three-hour synthetic capture through the real capture engine create no audio
  spool and retain only a bounded window.
- [x] Recognition backlog safe-stop preserves the retained prefix and permits
  the next recording; whole-session RMS includes reclaimed speech.
- [x] Accurate tests exercise timestamps across silence and speech, uncertain
  seams, repair overlap, repeated words, and release after a long owned prefix.
- [x] Instant tests exercise endpoint ownership, word-aware forced rollover,
  replay boundaries, and retained-tail failure recovery. The original
  energetic-empty rejection policy was incorrect: a successful native empty
  result is ordinary no-word coverage, not a decoder failure. Follow-up tests
  must cover leading/trailing ambient noise and long pauses.
- [x] Independent adversarial reviews produced regression fixes for both
  recognition modes. Native Vosk runtime smoke verifies the installed ABI and
  PCM input path; scripted hours-long recognition tests remain synthetic.
- [x] Native Vosk processed 70.355 seconds of neutral synthesized English
  through production streaming/release functions. Natural endpoints matched
  the full-clip baseline; six deliberately triggered decoder rollovers kept
  all twelve distinctive clauses in order and the three-second release tail.
  Reference errors were 10/241 words versus the baseline's 11/241, with a
  conjunction omitted at one restart seam. See
  [the reproducible spoken-rollover report](INSTANT-SPOKEN-ROLLOVER-VALIDATION.md)
  for the test timing override, exact bounds, and limitations.
- [x] Independent recovery-journal run passed seven Windows tests: coherent
  snapshot replacement, pending/late save versus delivery discard, Clear
  before first recognition, disabled-at-activation privacy, active-session
  cleanup on quit, unique identity after runtime reload, and bounded mailbox
  coalescing. Read-only UI review confirms Copy has no insertion target and
  individual Discard requires confirmation.
- [x] Final integrated workspace: 627 tests passed; documentation tests,
  formatting, strict all-targets workspace Clippy, and release-policy checks
  passed. The two opt-in spoken Vosk tests passed separately.
- [x] Recovery storage tests cover Unicode, tampered and swapped ciphertext,
  two-connection stale writes, restart invalidation, disable/re-enable,
  retention during normal dictation insertion, and metadata-only enumeration.
- [x] CPU and Vulkan development installers built with static CRT dependency
  checks and release-policy validation. See [release evidence](ROLLING-RELEASE-VALIDATION.md).
- [ ] Installed-binary verification: requires the previously running Phorminx
  process to exit before replacement and startup checks.
- [ ] Continuous native long-speech corpora in English and PT-BR, with seam
  accuracy compared against the same model's reference transcription.
- [ ] Physical latency samples and long microphone/device-loss/resource soaks
  for both engines on CPU and Vulkan where supported.

Text-only interrupted-dictation recovery follows the lifecycle in ADR-015.
The journal tests use real local persistence and DPAPI on Windows; they do not
simulate sudden power loss or establish the maximum recoverable prefix age
under a stalled disk. Recovery remains best effort, and audio ring tests do not
establish crash durability.

## Extended Dictation

- Successful complete text follows normal formatting and original-target
  insertion. Intentional repeated phrases cannot cause rejection or force
  retrieval from History; speculative repetition detection is diagnostic only.
- A successful independent recognition result with no words is not a native
  error. Audio energy alone cannot prove that words were spoken. Empty decoded
  intervals advance bounded audio coverage without changing already owned text;
  retained boundary overlap still applies to Accurate. Actual native errors,
  missing audio, and invalid ownership remain distinct retry/failure cases.
- Generated lifecycle traces permit one owner, terminal outcome, insertion,
  and history row, and reject every stale generation.
- Random callback sizes, channel layouts, sample formats, and 8–192 kHz input
  produce contiguous canonical 16 kHz coverage with no post-stop samples.
- Ledger property tests reject gaps, overlaps, reordered checkpoints, ambiguous
  seams, and duplicated results without advancing the committed frontier.
- 119.9, 120.0, and 120.1-second sessions have identical terminal semantics;
  longer sessions have no full-duration RAM allocation or release-time copy.
- Decoder, formatter, UI, and text-checkpoint stalls cannot block or allocate in
  the microphone callback. Work queues remain bounded and final work preempts
  obsolete speculative work.
- Release/cancel/finalize/sleep/device-loss/shutdown races produce no deadlock,
  stale insertion, use-after-free, or duplicate persistence.
- Production capture creates no audio spool file. Ordinary empty recognition
  results and temporary timestamp uncertainty must not be treated as terminal
  failures. Sustained genuine recognition failure or insufficient throughput
  can still exhaust the bounded backlog: capture must not silently overwrite
  uncommitted audio. This remaining resource-failure path is not an arbitrary
  recording-duration limit. A completed memory-only fault must permit the next
  recording; legacy spool corruption, quota, and cleanup tests qualify only the
  dormant compatibility facility.
- Long synthetic captures demonstrate bounded retained audio and decoder
  context. Text growth is reported separately; process handles/threads require
  a native soak. Terminal cleanup covers success, cancel, and failure.
- Text-only crash checkpoints honor history privacy and deletion controls;
  restart recovery never automatically inserts stale text into a new target.
- English and PT-BR seam corpora show zero checkpoint omissions or duplicates;
  accuracy does not materially regress against the same engine's reference.
- Raw/Light release latency is measured over at least 50 physical dictations;
  formatting time is measured separately.
- Both engines complete long hold/toggle soaks without queue drops, native
  leaks, stale results, or crashes.

## Everything Setup and Repair

- Inventory enumeration order cannot change the serialized plan.
- Every action/failure transition converges to Ready or a stable actionable
  Blocked state; retries are idempotent.
- Fresh, partial, corrupt, wrong-language, wrong-digest, custom-path, locked,
  and externally managed installations produce the minimal correct plan.
- Cancel, kill, or restart at every acquisition/verification/activation boundary
  resumes safely or rolls back while preserving the old working setup.
- Offline, TLS, redirect, short/oversize response, stall, proxy, and disk-full
  failures never activate unverified content.
- Archive extraction rejects traversal, rooted/device/ADS paths, links,
  case-colliding duplicates, deep paths, and decompression bombs.
- Network, native-code load, installer execution, background-process startup,
  elevation, and login persistence each require exact consent.
- Repair never deletes or overwrites custom/preexisting assets. Rollback deletes
  only transaction-owned staging recorded in its content-free journal.
- Double-clicks, multiple windows/processes, restart, and cancellation linearize
  to one settings commit.
- Recognition Ready comes only from a resident worker or full native probe;
  Ollama absence remains a neutral optional state.
- Setup and diagnostics are keyboard accessible and contain no transcript,
  audio, prompt, credential, private database copy, or unsafe path disclosure.

## Performance recommender

- Exact language, artifact identity, backend load, correctness, and resource
  gates exclude incompatible candidates before ranking.
- The versioned benchmark records cold/warm latency, production release tail,
  RTF, memory, backend/device identity, quality, protected-token behavior, and
  Whisper/Ollama contention without retaining calibration content.
- At least three valid runs produce median, dispersion, and confidence. Loaded
  or thermally unstable runs are rejected rather than presented as evidence.
- Identical evidence and policy produce identical recommendations and rejection
  reasons. Insufficient confidence yields a conservative explained default.
- A minor Tiny speed win cannot outrank a measured Base accuracy advantage.
- PT-BR Instant cannot be recommended without a pinned compatible Vosk asset
  and qualifying corpus evidence.
- Recommendation application is explicit and reversible. Evidence invalidates
  when its protocol, app, model digest, backend, device, or driver identity
  changes.

## Final physical and release gates

- Clean Windows standard-user VM, offline and online commissioning and repair.
- CPU-only reference plus a Vulkan-capable test system.
- English and PT-BR Accurate; every actually supported Instant language.
- No Ollama, Ollama on CPU, and Ollama sharing the GPU with Whisper.
- Install, upgrade, reboot, launch-at-login drift, repair, and uninstall.
- Full workspace tests, strict Clippy, formatting, release-asset policy, native
  startup smoke, memory/resource soak, and exact installed-binary audit.
