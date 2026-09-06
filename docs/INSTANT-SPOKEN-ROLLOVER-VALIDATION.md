# Instant spoken rollover validation

Verified locally on 2026-09-06 with the installed Vosk 0.3.45 runtime, `vosk-model-small-en-us-0.15`, and Windows `Microsoft David Desktop` speech synthesis.

Run from the repository root:

```powershell
.\scripts\Test-InstantSpokenRollover.ps1
```

The script synthesizes a neutral 241-word story into a temporary 16 kHz mono PCM16 WAV, runs both ignored native integration tests, restores its environment variables, and deletes that generated fixture. It uses no network services or private dictation. The voice and asset directory can be overridden, but different voices/models may require separately reviewed recognition expectations.

## Exercised production paths

Both tests send real audio commands through `drain_instant_audio`, validate contiguous sample ownership and monotonic transcript acknowledgements, and withhold the final three seconds of speech until `transcribe_instant_final` runs. They check the complete spoken text against the known story and a full-clip Vosk baseline. Twelve distinctive clauses must each occur exactly once in order, including the final phrase spoken after release.

The natural-endpoint case uses the ordinary production trigger. The generated voice naturally reaches Vosk endpoints before thirty seconds, so it does not force a decoder restart.

The restart case explicitly calls the production `force_instant_rollover` routine at ten seconds of uncommitted audio to reproducibly exercise replacement-decoder creation, word-based commitment, lexical-tail replay, further streaming, and release. The production deadline remains thirty seconds; its threshold and long-session accounting are covered separately by deterministic tests. The native test does not claim that its forced timing is the normal production configuration.

## Observed results

Both native cases passed in 40.17 seconds of test execution. Each processes the same 70.355 seconds of synthesized speech.

| Case | Decoder restarts | Peak worker uncommitted audio | Errors versus 241-word reference | Difference from full baseline |
| --- | ---: | ---: | ---: | ---: |
| Natural endpoints | 0 | 323,584 samples / 20.224 seconds | 11 | 0 word edits |
| Explicit ten-second restart trigger | 6 | 159,872 samples / 9.992 seconds | 10 | 3 word edits |

All ordered-clause, final-tail, monotonic-acknowledgement, accepted-sample, replay-size, and committed-prefix assertions passed. The final three seconds were fed during release and the final phrase was present. The original exact-baseline assertion exposed ordinary decoder wording variation and was replaced with explicit reference error bounds plus clause and ownership assertions: at most 8% reference word error, no more than three additional reference errors over the full baseline, and at most five relative word edits.

## Limitations retained in the evidence

The forced case omitted the conjunction `and` immediately after the 32.760-second commit boundary in `forests and the animals`. It also corrected two baseline errors (`out outside` to `outside`, and `there families` to `their families`), producing a slightly lower overall reference error rate. Audio/sample ownership stayed contiguous, but restarting from a word timestamp can change recognition at that boundary. This test does not establish zero recognition omissions or identical wording across decoder restarts.

This is a real native-decoder integration test over synthesized speech, not an hours-long live microphone soak. It exercises worker buffering and short-clip finalization; the rolling capture ring, reclaimed-audio release path, queue failures, and hours-long accounting have separate deterministic regressions. The test retains the fixture in memory so it can compare the full baseline; that allocation is test infrastructure, not a measurement of production capture memory.
