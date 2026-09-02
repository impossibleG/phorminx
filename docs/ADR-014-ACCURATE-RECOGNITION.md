# ADR-014: Accurate recognition is resident, preemptible, timestamp-stable, and backend-observable

## Status

Accepted for the Accurate recognition track.

## Context

The first incremental implementation reduced work after long recordings, but it
still treated Whisper as a sequence of unrelated text-producing batch jobs. A
partial could occupy the only worker after key release, chunks shorter than four
seconds received no work, the final tail could contain 8.8 seconds, and lexical
overlap disagreement caused a complete retranscription. The application also
reported a model file as "ready" without proving that whisper.cpp loaded it or
which compute backend was active. Finally, the release binary was CPU-only even
on a Vulkan-capable GPU.

The custom short `audio_ctx` heuristic is not an accuracy-preserving streaming
primitive. It changes the model encoder context and was active on the real
8.18-second sample that produced a repeated phrase. Accurate mode must use the
model default unless an explicit benchmark command asks for an override.

## Decision

1. Keep one Whisper model resident. Loading completes before STT readiness is
   announced. Readiness records requested backend, active backend, model load
   duration, and whether GPU use was compiled in. A disk file is described only
   as available, never as a loaded recognizer.
2. Ship Vulkan in the Windows Accurate release. `Auto` selects Vulkan when that
   backend is compiled and a device enumerates. If Vulkan context/model loading
   fails, `Auto` creates a fresh CPU context and exposes the fallback; explicit
   `Vulkan` fails visibly. `Cpu` remains selectable for diagnosis and
   benchmarks. The binding proves configuration through device enumeration plus
   successful context creation; actual kernel execution is proven separately by
   whisper.cpp runtime logs and a hardware benchmark.
3. Pin four verified upstream artifacts: Tiny English, Base English, Tiny
   Multilingual, and Base Multilingual. Existing `model_path` remains
   authoritative. A pinned variant is inferred only after filename, byte count,
   and SHA-256 all match the embedded manifest; otherwise it is Custom. English
   models must not be presented as Portuguese-capable.
4. Replace duration-derived `audio_ctx` overrides in production with the Whisper
   model default. Keep the benchmark-only override for controlled experiments.
5. Start partial work after 1.5 seconds, target silence boundaries, force a
   boundary at 3 seconds, and retain 500 ms overlap. This bounds the ordinary
   release tail to 3.5 seconds while preserving the 120-second capture ceiling.
6. A final request publishes a shared release-priority flag before entering the
   FIFO. A tombstone also aborts older partial commands that have not reached
   `begin_partial` yet. The running partial supplies the same flag to
   whisper.cpp's abort callback, so final work does not wait for obsolete
   partial inference. The tombstone clears only when the final command dequeues.
   Aborted work is never accepted into the stable accumulator.
7. Whisper partials expose segment timestamps. Only segments ending before the
   overlap guard become stable. Later chunks append segments by their absolute
   audio interval and use a bounded prior-text prompt for decoder context. A
   segment that crosses the stability guard or already-accepted frontier freezes
   admission at the earliest unresolved instant; the final tail resumes there,
   so a later timestamp can never jump across and permanently lose audio. Text
   overlap remains a compatibility fallback, not the primary boundary proof.
8. Formatting, history, and insertion remain downstream of exactly one accepted
   final result. Ollama warm-up cannot block STT readiness or the transcription
   command loop.
9. Emit content-free stage telemetry: backend/model readiness, captured audio,
   partial audio/compute/abort, final tail audio/compute, full fallback reason
   and compute, formatting, insertion, and release-to-insert. Never log audio or
   transcript text.
10. Recover from pathological repeated incremental output with one clean
    full-clip pass before persistence/insertion. The high-specificity guard
    requires four consecutive repetitions of a phrase at least five words long,
    so intentional short repetition such as `red green blue` three times remains
    valid. A repeated full-clip result fails closed; recognized speech is never
    silently rewritten.
11. The Models page and both settings surfaces expose all four pinned variants
    and Auto/Vulkan/CPU. Downloads use the selected manifest and switch the
    authoritative model path only after size and SHA-256 verification.

## Consequences

- Accurate mode remains heavier than a true streaming recognizer but release
  latency is bounded by a short tail rather than an arbitrary queued partial.
- Segment timestamps eliminate most conservative full-clip fallbacks without
  pretending that Whisper token hypotheses are immutable.
- Explicit backend and model choices make CPU/Vulkan and Tiny/Base experiments
  reproducible.
- Vulkan packaging adds SDK/build complexity and must be proven on physical AMD
  hardware before claiming acceleration.
- Very long intentional verbatim repetition can still meet the conservative
  loop threshold; it receives a full-clip recovery pass before any visible
  failure.
