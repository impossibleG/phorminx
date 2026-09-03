# Extended Dictation and Setup/Repair acceptance gates

This document is the release-blocking evidence checklist for ADR-015 and
ADR-016. A checked implementation test is not a substitute for the physical
hardware gates at the end.

## Extended Dictation

- Generated lifecycle traces permit one owner, terminal outcome, insertion,
  and history row, and reject every stale generation.
- Random callback sizes, channel layouts, sample formats, and 8–192 kHz input
  produce contiguous canonical 16 kHz coverage with no post-stop samples.
- Ledger property tests reject gaps, overlaps, reordered checkpoints, ambiguous
  seams, and duplicated results without advancing the committed frontier.
- 119.9, 120.0, and 120.1-second sessions have identical terminal semantics;
  longer sessions have no full-duration RAM allocation or release-time copy.
- Decoder, formatter, UI, and scratch-store stalls cannot block or allocate in
  the microphone callback. Work queues remain bounded and final work preempts
  obsolete speculative work.
- Release/cancel/finalize/sleep/device-loss/shutdown races produce no deadlock,
  stale insertion, use-after-free, or duplicate persistence.
- Scratch short writes, quota/disk exhaustion, permission loss, corruption,
  truncation, crash residue, and cleanup failures fail closed and never expose
  recoverable raw audio without the in-memory session key.
- Long synthetic captures demonstrate bounded RAM, bounded handles/threads,
  documented scratch growth, and cleanup on success, cancel, error, and later
  startup scavenging.
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

