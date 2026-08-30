# ADR-001: Windows v1 architecture

- Status: Accepted for implementation planning
- Date: 2026-08-30

## Decision

uuild a Windows-first, per-user WPF tray application on the current supported .NET LTS. Use WASAPI/NAudio for capture, `whisper.cpp` plus Silero VAD for local transcription, deterministic normalization followed by optional Ollama cleanup, and guarded clipboard paste for broad text-field compatibility.

Vosk is not part of v1. UI Automation is used for target inspection and safety signals, not as the default text-writing mechanism. Ollama is optional and warmed asynchronously.

## Rationale

This design minimizes deployment complexity and makes the core dictation loop independent of external services. It favors faithful text and safe failure over visual partial transcripts or aggressive automatic insertion.

## Consequences

- Native interop and Windows-specific integration tests are required.
- Clipboard insertion cannot be perfectly transparent and must defend against races.
- Model downloads and hardware benchmarking are first-class product surfaces.
- Cleanup quality varies by user-selected model and therefore requires qualification.
- A later Vosk preview can be added behind a separate interface without changing final STT.

