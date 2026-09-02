# ADR-014: Resident Vosk streaming for Instant mode

Status: Accepted

## Context

Incremental Whisper still operates on independent batches. It can leave a tail
at key release, queue behind an in-flight batch, or discard partial work when
overlap reconciliation is ambiguous. That is useful in Accurate mode, but it
cannot guarantee low release-to-insert latency for frequent dictation.

## Decision

Phorminx exposes two persisted recognition modes. **Instant** owns the
transcript with a resident Vosk model and one streaming recognizer per held
dictation. **Accurate** retains Whisper. Schema 1-3 settings migrate to schema 4
with Accurate selected; `recognition.model_path` remains the Whisper path.
Instant adds an unpacked model directory and a native runtime-bundle directory.
Phorminx never downloads or executes native code implicitly.

The Vosk boundary dynamically loads its documented C ABI from `libvosk.dll`.
Function pointers never outlive the owning library. The runtime is represented
as a directory because Windows distributions may require adjacent DLLs.

The CPAL callback only downmixes and pushes into two bounded rings: the existing
120-second archival ring and a four-second streaming ring. The application
drains bounded batches into a bounded worker queue. Vosk decoding never runs in
the callback. Queue overflow or a stream gap degrades that session; key release
then decodes the intact archival clip once as a recoverable fallback. Normal
release feeds the callback-boundary tail into the same recognizer before its
terminal result.

Vosk endpoint `Result` text is appended exactly once. Mutable `PartialResult`
text is never committed. Release appends only the terminal `FinalResult`. This
prevents repetitions across natural pauses and long mid-hold silence.

Session IDs and cancellation discard stale work on resume, shutdown, or a
superseding activation. Audio and transcript content are never logged. Logs
contain only IDs, states, stages, durations, and categorical recovery reasons.

Raw and Light formatting remain deterministic. Balanced, Strong, and Custom
remain a separate optional Ollama latency stage and are never described as
instant.

## Readiness

Readiness is typed: ready (optionally with a PT-BR quality warning), missing
runtime, missing model, load failed, or unsupported language. Ready is announced
only after the library and model are loaded and resident. Instant v1 accepts
`en`, `en-us`, `pt`, and `pt-br`. English is recommended. PT-BR requires a
matching local model and carries a quality warning.

## Consequences

Normal release cost is the unconsumed callback tail, Vosk `FinalResult`,
deterministic formatting, and insertion. AI formatting may still add latency.
Missing assets enter recoverable setup instead of false readiness. Native
runtime and model licenses must be reviewed before distributing a bundle.
