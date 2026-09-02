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
   backend is compiled; explicit `Vulkan` fails visibly in a CPU-only build.
   `Cpu` remains selectable for diagnosis and benchmarks. Runtime proof requires
   an actual transcription whose returned backend is Vulkan; compilation alone
   is recorded separately.
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
   FIFO. The running partial supplies that flag to whisper.cpp's abort callback,
   so final work does not wait for obsolete partial inference. Aborted work is
   never accepted into the stable accumulator.
7. Whisper partials expose segment timestamps. Only segments ending before the
   overlap guard become stable. Later chunks append segments by their absolute
   audio interval and use a bounded prior-text prompt for decoder context. Text
   overlap remains a compatibility fallback, not the primary boundary proof.
8. Formatting, history, and insertion remain downstream of exactly one accepted
   final result. Ollama warm-up cannot block STT readiness or the transcription
   command loop.
9. Emit content-free stage telemetry: backend/model readiness, captured audio,
   partial audio/compute/abort, final tail audio/compute, full fallback reason
   and compute, formatting, insertion, and release-to-insert. Never log audio or
   transcript text.
10. Reject pathological repeated output before persistence/insertion. The guard
    detects three or more consecutive repetitions of a multi-word span and
    fails closed; it does not silently rewrite recognized speech.

## Consequences

- Accurate mode remains heavier than a true streaming recognizer but release
  latency is bounded by a short tail rather than an arbitrary queued partial.
- Segment timestamps eliminate most conservative full-clip fallbacks without
  pretending that Whisper token hypotheses are immutable.
- Explicit backend and model choices make CPU/Vulkan and Tiny/Base experiments
  reproducible.
- Vulkan packaging adds SDK/build complexity and must be proven on physical AMD
  hardware before claiming acceleration.
- The repetition guard can reject unusual intentionally repeated dictation; a
  visible failure is preferable to inserting a long hallucinated loop.

