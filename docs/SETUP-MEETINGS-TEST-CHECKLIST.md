# Setup and meetings test checklist

This is an alpha acceptance checklist, not a completed hardware certification.
Use synthetic speech and a test profile. Do not send real feedback during local QA.

## Local acceptance

1. **Initial silence:** choose Instant, start recording, wait 1, 5, 15 and 29
   seconds, then speak. Recording should remain active until explicitly stopped.
   If it fails, preserve the content-free stop reason; native fixture success does
   not establish the cause of a failure on an installed app.
2. **Accurate boundaries:** dictate for several minutes, including pauses, quiet
   trailing speech and intentionally repeated words. Check for duplicated chunk
   boundaries without treating legitimate repetitions as errors. Compare CPU and
   Vulkan explicitly; CPU throughput is not established by the GPU fixture.
3. **Ollama unavailable:** with no capture running, stop the local service. Verify
   unrelated settings still save and verified local-service recovery is reported
   honestly. Missing models must not be silently substituted or downloaded.
4. **First-run setup:** on a clean Windows test machine, consent to installer
   preparation. Interrupt a download, retry from **Prepare Phorminx**, and verify
   existing settings/text are preserved. Declining preparation and silent updates
   must not install models. GPU vendor drivers remain a separate prerequisite.
5. **Instant model choices:** download deliberately, then choose Use. Check that
   the UI remains responsive during native validation, including hiding/showing
   the main window. Missing assets must not disable their own setup controls.
   Test insufficient disk/RAM and corrupt cached files with disposable assets.
6. **Both-source meetings:** use headphones. Play synthetic computer speech and
   speak through a test microphone, including simultaneous speech. Confirm both
   labels/timestamps, saved originals, assistant cutoffs and final trailing text.
   Disconnect one source and verify a warning with the surviving source continuing.
7. **Translation:** select a local model and target language. Originals must stay
   visible/searchable. Slow/unavailable translation should show its status without
   stopping capture. Check language quality separately from pipeline correctness.
8. **Embedding defaults:** fresh settings recommend Nomic; existing selections and
   indexes remain unchanged. An unavailable embedding model must not block recording.
9. **Feedback:** launcher 0 opens a local draft. Record or type, review, and inspect
   the optional diagnostics checkbox. Nothing is sent before Send. Use a dedicated
   test service for receipts/retries; production deployment is not part of local QA.
10. **Updates:** only a deliberately keyed test build can enable updates. Test
    signature/hash/expiry/rollback rejection and a real installer in a disposable
    Windows environment. Active capture must not be terminated. Silent updates
    must skip model bootstrap and report installation failure on next launch.

## Automated evidence versus remaining gates

The workspace unit/integration suites, independent adversarial regressions,
bootstrap safety fixtures and website gateway tests cover local state and service
contracts. Native Vosk delayed-start fixtures and a release/Vulkan streaming
fixture were also exercised with synthetic audio. The latter does not certify
CPU throughput: a debug/CPU run exceeded the recognition backlog budget.

Independent QA caught and verified fixes for two final-tail text-loss cases:
quiet speech following louder owned overlap, and a brief utterance diluted by
whole-window silence. The post-fix native Vulkan streaming/ambient checks passed;
the 219-second speech fixture had two word edits against 723 reference words in
the final run. A separate retained-prefix fixture appended no hallucinated words
for exact-zero and two nonzero quiet-room-tone levels. These are bounded examples,
not proof of perfect recognition or a universal speech/noise classifier.

Physical dual-device capture, echo, long-session clock-rate drift, translation
quality under contention, clean-machine installation and a real signed-update
lifecycle remain separate gates. One Windows symlink fixture could not execute
without symlink-creation privileges; do not count that branch as verified.

See [capture limitations](MEETING-CAPTURE-AND-TRANSLATION.md) and
[operational prerequisites](ADR-017-RELIABLE-SETUP-AND-MEETINGS.md) before release.
