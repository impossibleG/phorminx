# Development Roadmap

## Phase 0 — Evidence and spikes (1–2 weeks)

Deliverables:

- Hardware probe and reproducible benchmark harness.
- `whisper.cpp` interop spike using English and Portuguese samples.
- WASAPI capture spike with pre-roll and VAD.
- Low-level hotkey and safe clipboard-paste spike.
- Ollama discovery, warm-up, cancellation, and cleanup conformance spike.
- Compatibility results for Notepad, browser, VS Code, Slack, and Terminal.

Exit criteria: no unresolved feasibility problem in capture, recognition, cleanup, or insertion; default STT model/quantization selected from measurements.

## Phase 1 — Walking skeleton (1–2 weeks)

Build one end-to-end path with no persistence or polished UI:

```text
hold shortcut → record → transcribe → deterministic normalize → safe paste
```

Add correlation IDs, structured content-free logs, cancellation, and the runtime state machine immediately.

Exit criteria: 500 repeated dictations without a crash or wrong-target insertion.

## Phase 2 — Local product MVP (2–3 weeks)

Implementation status: complete. The clean-machine install/uninstall and non-developer onboarding exit gate remains part of private-alpha qualification.

- Tray, non-activating overlay, settings, onboarding, and model download.
- Microphone selection and recovery.
- History and retention controls.
- Personal lexicon and exact aliases.
- Safe target detection and ready-to-paste fallback.
- Unit and Windows integration suites.

Exit criteria: a non-developer can install, configure, dictate, recover raw text, and uninstall.

## Phase 3 — Ollama enhancement (1–2 weeks)

Implementation status: complete. Fake loopback-service conformance and failure-mode tests pass; qualifying at least one real installed Ollama model remains an external alpha gate.

- Installed-model discovery and capability test.
- Explicit model selection and lifecycle modes.
- Conservative cleanup profiles.
- Protected-token and output validation.
- Per-app profile selection.
- Clear degraded mode when unavailable.

Exit criteria: dictation behavior is unchanged when Ollama is removed, and supported cleanup models pass the conformance suite.

## Phase 4 — Hardening and private alpha (2–3 weeks)

Implementation status: engineering complete. Release tooling and automated policy checks pass. Signing and the physical compatibility/performance matrices are still required before calling the private-alpha exit gate complete.

- Full application compatibility matrix.
- Sleep/resume, device hotplug, clipboard race, focus-change, and elevated-target tests.
- Performance profiling and smaller-model recommendations.
- Signed per-user installer and launch-at-login.
- Privacy review, threat model, and diagnostic bundle review.

Exit criteria: all MVP definition-of-done items in the blueprint are satisfied.

## Phase 5 — Unified product experience and identity

Status: planned in `UI-BRAND-BLUEPRINT.md`; implementation has not started.

- Replace the separate Settings, History, Lexicon, and Profiles windows with one routed application shell.
- Establish the Phorminx mark, wordmark, color, type, geometry, iconography, copy, and motion systems.
- Route tray commands into the relevant shell page.
- Rebuild first-run commissioning, operational settings, model readiness, and local-data workspaces inside the same design language.
- Produce multi-resolution application, tray, installer, and overlay assets.
- Validate keyboard navigation, DPI scaling, reduced motion, contrast, empty/error states, and long content.

Exit criteria: every current product capability remains available in one coherent, branded, keyboard-accessible shell, and no content route creates an independent native window.

## Parallel workstreams

After Phase 0 contracts are frozen, development can run concurrently:

| Workstream | Ownership | Dependencies |
|---|---|---|
| Windows shell | tray, egui, hotkeys, overlay, lifecycle | Core state contracts |
| Audio/STT | WASAPI, resampling, VAD, whisper adapter, models | Audio and recognizer contracts |
| Insertion | target snapshot, sensitive-field rules, clipboard/SendInput | Target/injector contracts |
| Cleanup | Ollama discovery, benchmark, prompts, validation | Transformer contract |
| Persistence/UX | SQLite, settings, history, onboarding | Domain schema |
| QA/release | corpus, compatibility harness, installer, signing | Walking skeleton |

No two workstreams should edit the composition root or shared contracts without coordination. Freeze contract changes through short architecture decisions.

## Initial backlog

### P0

- Establish the Cargo workspace, crates, and CI.
- Define domain contracts and state machine.
- Build capture, pre-roll, VAD, and WAV diagnostic path.
- Bind and benchmark `whisper.cpp`.
- Implement target snapshot and safe clipboard insertion.
- Implement raw-result overlay and cancellation.
- Implement verified model manifest/downloader.
- Add content-free structured diagnostics.

### P1

- Ollama discovery and lifecycle.
- Cleanup conformance validator.
- History and retention.
- Lexicon and explicit aliases.
- Per-app profiles.
- Device selection and recovery.
- First-run benchmark/onboarding.
- Installer and autostart.

### P2 after alpha evidence

- Vosk live preview experiment.
- GPU backend selection improvements.
- Richer application adapters.
- Optional offline bundled cleanup runtime instead of requiring Ollama.
- ARM64 builds.
- macOS feasibility work.

## Decision gates

1. **STT gate:** choose model and backend only after corpus benchmark.
2. **Insertion gate:** no alpha until wrong-target tests are clean.
3. **Cleanup gate:** no model is recommended until it passes protected-token tests.
4. **Distribution gate:** do not publicly distribute unsigned binaries.
5. **Vosk gate:** add only if user testing demonstrates material benefit from partial text.
