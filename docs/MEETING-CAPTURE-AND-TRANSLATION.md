# Meeting capture and translation

## Contract

New meetings capture computer audio and microphone together. The Meeting settings screen can save separate output and input devices; each falls back to the Windows default when unset. These choices are saved explicitly before the next meeting starts. Each input owns its own capture pump, recognizer, resampling, committed frontier, and cutoff queue. This deliberately uses more recognition memory/compute than a single source.

The companion has independent controls to pause Phorminx's transcription of either source without ending capture or muting a Teams/Zoom call. Mute transitions are stamped at each source's capture frontier and applied to recognition input even if decoding lags behind a later unmute. Meeting capture does not stop when the companion is hidden or when the assistant or translation worker fails.

The dual coordinator never mixes waveforms or guesses speaker identities. Source-labelled text carries the original per-source sample timestamp. The capture-open monotonic offset aligns startup skew to the meeting clock. It does **not** correct hardware clock-rate drift. Labels mean audio source, not diarization. Headphones are recommended; acoustic echo cancellation and cross-source echo suppression are not implemented.

Committed transcript batches advance at the lesser active source watermark. A faster stream's finalized text is visible as provisional text while the other stream catches up. A question seals both source ledgers; per-source limits prevent the faster stream's next portion leaking into that question. The merged batch interval is a delivery ledger; structured source rows retain original source-local start/end samples and clock offsets separately.

An unavailable input produces an explicit warning. The remaining input continues if available. No missing audio is reconstructed. Stop seals both sources. Existing microphone-only and computer-only paths retain their original behavior. Silence and AI completion are not stop commands.

The legacy scalar batch ledger cannot represent overlapping device intervals directly. Successive barriers therefore reserve at least one new delivery tick when source clock maxima are equal; Stop also allocates a final delivery tick for unsent text behind an earlier barrier. These are ordering markers, not extra captured audio. Use structured source-local samples plus their offsets (also rendered in source labels) for original audio times.

## Translation

Translation is disabled by default and separately selects a target language and local Ollama model in Meeting settings. The companion can show Original, Translation, or Both, with a status when translation is waiting or unavailable. Translation uses the loopback local provider, including the existing local-model/cloud-alias validation; it never inherits a cloud assistant selection or its credentials.

One background worker translates finalized phrases with an eight-job bounded queue and bounded result queue. Submission is nonblocking. Translation shares the local Ollama compute slot with the assistant; a manual question takes priority and may cancel or skip a translation. Queue overflow, unavailable models, contention, empty results, and oversized phrases produce a visible notice; original recognition continues. This is best-effort near-real-time translation, not a guaranteed translation of every phrase under overload. Native inference/GPU contention still needs hardware testing.

Original text is never replaced. Successful translations persist in an additive table keyed to their original transcript segment; deleting transcripts cascades translations. Source metadata is deleted with transcript-category deletion. Late writes cannot recreate a deleted source collection. Changing/opening sessions cancels the previous translation worker, including queued work; shutting down does not wait for translation at the expense of recording finalization. Saved pages display completed translations next to originals. Assistant context remains the original source-labelled transcript.

## Verification

Deterministic tests cover simultaneous source text, capture-start skew, barrier isolation, source failure with surviving capture, finite polling, source-specific mute timing and atomic controls, translation queue saturation, saved device validation/retry, legacy configuration defaults, source/translation persistence, and privacy deletion. Existing meeting lifecycle and UI tests remain required.

Before a release claims hardware validation, test both recognition engines on synthetic loopback + virtual microphone input: startup and stop during loading, initial silence, hours-long runs, source disconnection, overlapping speech, slow translation, source-device drift, and low-memory/GPU contention. No personal audio is needed for these tests.
