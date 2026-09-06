# ADR-005: Bounded callback-safe audio capture

> Historical decision. ADR-015 supersedes the 120-second recording ceiling,
> full-recording retention, and post-stop-only resampling described below.
> Production now uses bounded rolling capture and reclaims recognized audio.
> Callback constraints and the PCM-format policy remain applicable.

- Status: Accepted
- Date: 2026-08-30

## Decision

Capture native-rate mono audio into a preallocated single-producer/single-consumer ring buffer. The CPAL data callback only converts, downmixes, writes into that buffer, and updates an atomic drop counter. Backend notifications are also counted atomically; neither callback locks a mutex, allocates a transcript-sized buffer, or performs resampling.

Bound each recording to 120 seconds. If the ring fills, reject the recording instead of transcribing or inserting truncated audio. This bounds capture storage to about 22 MiB at 48 kHz mono `f32` while leaving ample room for normal push-to-talk dictation.

After the stream stops, drain the buffer and use Rubato's synchronous FFT resampler to produce 16 kHz mono audio. The one-shot path performs band-limited conversion and removes its own startup delay and tail padding.

Support every PCM sample format exposed by CPAL 0.18 and reject DSD input explicitly through the unsupported-format path.

## Silence handling

Keep the existing duration and whole-clip RMS rejection as the temporary fail-closed no-speech check. Do not add energy-based endpoint trimming: it is difficult to tune across quiet microphones and can clip soft initial or final phonemes.

Endpoint trimming and stronger no-speech classification will use whisper.cpp's standalone Silero VAD through `whisper-rs`, with a separately managed local VAD model and corpus-based English and Brazilian Portuguese tuning.

## Consequences

- Capture memory is predictable and audio callbacks avoid blocking locks.
- A recording that exceeds the limit fails visibly rather than yielding a partial transcript.
- Resampling quality is suitable for accuracy benchmarking.
- VAD remains a separate, testable milestone and requires a local model asset.
