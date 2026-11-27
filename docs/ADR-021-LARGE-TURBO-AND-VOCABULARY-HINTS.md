# ADR-021: Large Turbo is the default Accurate model; the lexicon hints Whisper

## Status

Accepted.

## Context

Phorminx pinned only Whisper Tiny and Base. On about six minutes of real read
speech from one speaker (11 English and 3 Brazilian Portuguese passages: tech
terms, numbers, names, look-alike words, fast and quiet reading), each model
transcribed the same audio, segmented by the same speech detector:

| Model | English WER | Portuguese WER | CPU speed (4 cores) |
|---|---|---|---|
| Tiny English | 16.3% | n/a | 18x real time |
| Base English | 11.4% | n/a | 11x |
| Base Multilingual | 14.3% | 35.3% | 8 to 11x |
| Large v3 Turbo | 6.0% | 13.6% | 4 to 5x |
| Parakeet TDT 0.6B v3 | 9.9% | 18.5% | 16x |

The run used int8 ONNX exports through sherpa-onnx, not whisper.cpp, so the
figures are relative. Every model misheard the same product and tech words
("Phorminx", "Ollama", "Qwen"), which the lexicon only replaced after
recognition.

## Decision

1. Pin `ggml-large-v3-turbo-q8_0.bin` (874,188,075 bytes, whisper.cpp
   revision 5359861c) as `LargeV3Turbo`, model class `Large`. It covers both
   supported languages.
2. Setup acquires and activates Large Turbo for Accurate recognition in English
   and Brazilian Portuguese. Tiny and Base stay selectable on the Models page
   and in settings for low-memory machines.
3. Enabled lexicon entries become a Whisper initial prompt ("Glossary: A, B,
   C."), capped at 400 characters, for dictation and meetings. A caller's own
   prompt takes precedence. Decoded segments that only repeat the glossary are
   dropped (Whisper can echo its prompt on silence); a single spoken glossary
   word is kept. Lexicon replacement after recognition is unchanged.

## Consequences

- The first Accurate download grows from about 141 MiB to 834 MiB.
- Each 30-second encoder window costs far more on CPU than Base. On a machine
  without a usable Vulkan device, partial results arrive later; users can pick
  Base on the Models page.
- A two-source meeting loads two Large Turbo recognizers.
- Silero VAD (ADR-005) remains a separate step.
