# ADR-016: Setup, repair, and recommendations are deterministic transactions

## Status

Accepted for implementation.

## Context

Phorminx has individually careful model downloads, readiness probes, settings
saves, Vosk import, Ollama discovery, and startup integration, but no single
owner for a clean-machine or repair operation. A boolean onboarding marker is
not evidence that the microphone, resident recognizer, or optional formatter is
usable. An “Everything” action also cannot imply blanket consent to download
native code, execute installers, start processes, or change login behavior.

## Decision

Introduce a platform-neutral setup domain with the persisted lifecycle:

```text
Inventory -> Plan -> Consent -> Acquire -> Verify -> Stage -> Activate
          -> Benchmark -> Recommend -> Commit
```

1. Inventory is read-only, independently generated per capability, and never
   labels file presence as runtime readiness.
2. Planning is deterministic and versioned. It states vendor, artifact,
   version/digest, bytes, license, disk change, network effect, native-code or
   process execution, elevation, and rollback behavior.
3. Consent is typed per side-effect class. Declining one action leaves existing
   settings and assets unchanged and does not block core Light-format dictation.
4. Acquisition uses fresh same-volume staging. Exact size, digest, archive
   safety, native loading, model/session creation, or external signer checks
   complete before activation.
5. Working/custom assets are never mutated in place. Settings switch only after
   replacement validation; rollback removes only transaction-owned artifacts.
6. A content-free journal records only active action identity, phase, and owned
   staging location. Restart resumes a safe phase or removes abandoned staging.
7. External tools such as Ollama remain shared software. Phorminx never silently
   uninstalls them or deletes models it cannot prove it acquired.
8. Async observations and actions carry generations; stale results cannot
   overwrite newer state. A single-operation lock linearizes duplicate clicks,
   windows, and processes.

## Recommendation policy

Recommendations come from a versioned local benchmark protocol rather than
hardware-name heuristics. Language compatibility, successful resident loading,
protected-token correctness, hallucination behavior, and resource ceilings are
hard eligibility gates. Among candidates meeting the selected latency goal,
Phorminx prefers measured fidelity; a small speed improvement cannot justify a
known word loss.

Evidence includes exact model identity, active backend/device, cold and warm
latency, release-tail percentiles, RTF, memory pressure, and accuracy against
displayed or bundled non-sensitive calibration material. User microphone audio
is transient and requires separate disclosure. Only aggregate content-free
metrics are persisted. Recommendations are explainable, confidence-scored,
invalidated when their evidence identity changes, and applied only after an
explicit user action.

## Required evidence

- Inventory order cannot change a plan or recommendation.
- Every interruption boundary resumes or rolls back without mixed activation.
- Corrupt, oversized, redirected, incomplete, or malicious assets never load.
- Optional Ollama absence never blocks core readiness.
- Runtime readiness comes only from resident or full authoritative probes.
- Clean, partial, offline, low-disk, permission-denied, and repair installations
  converge to Ready or a stable actionable state.
- CPU-only and Vulkan hardware plus English and PT-BR policies are qualified.
- Setup and diagnostic artifacts contain no transcript, audio, prompt, token,
  private database copy, or user-owned path deletion authority.

