# Phorminx product roadmap and release gates

Phorminx should be dependable for someone who did not build it: installation must be understandable, everyday controls must behave consistently, and a successful action must be distinguishable from one that merely appeared to work. This is a living plan for the next waves, not a claim that every item is already implemented or a promise that priorities cannot change. Each wave ends with evidence and an explicit review of what remains.

## Current position

The branded installer, guided preparation, and signed GitHub Release update path have been implemented in source and exercised in a local QA build. That build is unsigned for Windows and has not passed a clean-machine installation or a published-release update test. Those gates remain before public distribution. Pushing code to GitHub alone does not update installed copies; only a tested, published release can do that.

Wave 0A QA checkpoint (2026-10-09): the previously blank Feedback window rendered in an isolated Tauri build; manual QA confirmed that X closes it, a typed unsent draft survives reopening, and a short recorded report becomes text when the QA profile points to a valid local Whisper model. The initial recording failure came from the intentionally empty QA profile, not from the recorder. No report was sent. The native library suite passed 360 tests (10 hardware/model-dependent tests ignored); bootstrap and release-asset scripts passed. The installed user's launch-at-login preference and the exact HKCU Run value agree, but an actual Windows sign-in was not tested. This is not yet a clean-machine or production-delivery sign-off.

Wave 0A follow-up (2026-10-09): changing history retention no longer depends on an available speech model. A regression test verifies that an unrelated retention and launch-at-login edit saves while the configured Whisper file is absent, updates the local retention policy, and emits the startup-registration effect. The active runtime now reloads privacy and startup fields with other non-recognition settings. The optimized app library suite passed 361 tests (10 device/model-dependent tests ignored). A live save/reopen, shortcut, and Windows sign-in check remains open; this source build has not replaced the installed copy.

Wave 0A speech-level follow-up (2026-10-09): isolated QA found that saving Minimum speech level could close the app without reopening it. The source now applies that threshold to the next dictation without a whole-app restart, and an unchanged non-AI settings save no longer restarts or requires a recognition model. The active AI model's pinned identity check remains intact. The optimized library suite passes 364 tests, with 10 hardware/model-dependent tests ignored. The rebuilt QA executable and a live save check are still pending; other model/backend changes still use the broader restart path, whose failure requires separate diagnosis before Wave 0A can close.

The order below gives blockers priority, but independent work may overlap when it does not obscure a regression. A wave is not complete because its screen looks finished: its behavior, persistence, failure path, and relevant native workflow must be verified. Findings can change this plan without erasing the original scope.

## Wave 0A Desktop reliability

First make the existing application trustworthy. Reproduce and fix the blank feedback window, its close behavior, and the case where it remains after Phorminx exits. A report must remain a local draft until an explicit Send; a failed send must retain the draft and must not claim delivery. Verify typed and recorded reports separately.

Audit the controls already in use: saving and reopening settings, launch-at-login registration at the next Windows sign-in, launcher and direct-dictation shortcut capture, custom-instruction persistence, history retention and local search, setup and repair, and the shared automatic-update preference shown in onboarding and Settings. Include a smoke pass over Actions, Models, Profiles, and Lexicon. Test initial, busy, success, error, cancel, restart, and quit states where applicable.

Exit when the reported feedback failure has a reproduced cause and a regression test; the relevant save, restart, shortcut, setup, and close paths pass; and remaining device-specific or external-service gaps are named rather than counted as successes. This wave changes desktop behavior only. It does not authorize a Render deployment.

## Wave 0B Feedback delivery

The desktop sends reviewed text to the Phorminx feedback API and expects a matching ticket receipt. The website and API live in the shared `impossible-g-site` repository, which also serves other sites. The checked-in server contract deliberately returns an unavailable response without durable feedback storage; the live Render configuration must be checked before claiming that submissions work.

Choose durable storage and its operating cost before enabling production intake. Verify retention, backups, privacy notice, rate limits, host isolation, admin access, duplicate retries, and honest error receipts using synthetic reports. Test in a safe environment before any production change. Render access, a paid plan or external database, secrets, and deployment each require a specific decision at that point; no credentials belong in source or chat. Website visual polish is a later, separate scope.

Exit when a deliberate test submission receives a durable ticket and can be retrieved through authorized administration, while unavailable storage and network failures never show false success. No real user feedback is sent as a test fixture.

## Wave 3 Product clarity and performance

### Wave 3.1 Interaction and field audit

Inventory each visible field and button: what it controls, valid values, where it saves, when it takes effect, and what failure looks like. Replace ambiguous free-text inputs with guided choices where the choices are knowable. Recognition language should reflect the selected speech model; local embeddings should show Off, installed models, and the recommended model; the direct-dictation shortcut should explain that it bypasses the launcher and use key capture rather than unexplained typing. Keep advanced manual entry only where it represents a real supported capability.

Give buttons immediate, restrained acknowledgement: pressed/focus state, a non-duplicating pending state, and visible success or error. Respect reduced-motion preferences. Explain Profiles as per-application dictation behavior and Lexicon as personal word substitutions. Restore controls that the backend still supports but the new UI hides, including meeting commentary cadence; its current engine range is 10 to 120 seconds with a 30-second default.

Exit with a field inventory and tested save/reopen behavior, not merely revised labels or animation.

### Wave 3.2 AI Center and model selection

Evolve Models into one source of truth for the models used by dictation, formatting, meeting transcription, translation, assistant replies, Actions, and semantic search. Show the selected model, installed or unavailable state, fallback, and relevant health or latency. Local-model selectors across the app must use the same installed catalog. External provider model names remain provider-specific rather than masquerading as installed local models.

Do not silently replace an existing selection or index. Exit when changing a model in one place is reflected everywhere it is used, unavailable models are explained, and the underlying feature still works after restart.

### Wave 3.3 Prompt library and guided Actions

Add named, reusable instruction presets for dictation, meeting assistance, and AI-authored Action payloads. Preserve and migrate existing free-text instructions. Make the active preset and any local override explicit so an edit does not silently alter an unrelated workflow.

Turn Action creation into a sequence: destination, recorded transcript, template or AI-written JSON, model and instructions when needed, payload preview, then an explicitly authorized test or send. Preview must not contact the destination. Exit with verified template substitution, generated-JSON validation, retry/error behavior, and no accidental requests.

### Wave 3.4 Measured speed and quality

Measure cold and warm latency on representative computers and realistic phrases, meetings, and Action payloads. Choose the fastest default that meets a defined quality floor for each feature; a fast but unusable translation is not an improvement. Show resource and fallback implications before changing defaults. Preserve user-selected models unless the user chooses to change them.

Exit with repeatable benchmark inputs, quality checks, and before-and-after timings rather than an impression of speed alone.

## Wave 4 Bounded regression and polish gate

Run the end-to-end matrix after the earlier waves: initial silence and long dictation, uninterrupted capture, chunk boundaries without duplicated or lost words, dual-source meeting capture, commentary cadence, translation latency, assistant replies, feedback, setup, updates, retained history and embeddings, shortcuts, prompts, and Actions. Include offline dependencies, failed downloads, restart, and clean installation. Automated fixtures do not substitute for physical-device or clean-machine checks.

Fix regressions introduced by these waves, complete a visual pass across windows and sizes, then freeze the milestone. New feature ideas discovered here go into a later scope unless they block an existing workflow. Report what passed, what needed a human test, and what remains blocked before calling the release ready.

## Wave 5 Messaging as a separate project

After the product gate, design peer messaging on its own terms: identity and invitations, authenticated encryption, connectivity through NAT, optional relay, offline delivery, abuse controls, blocking, history and deletion, and whether audio is ever sent. A rotating share string alone is not an identity or connectivity solution. Review the threat model and operating costs before implementation; do not add a server or expose a machine address by assumption.

## Decisions and checkpoints

- Start Wave 0A only after its scope is approved. At each wave boundary, report the observed behavior, tests, unresolved risks, and the next decision before expanding scope.
- Request Render access only when Wave 0B needs live configuration inspection or an approved deployment. A shared-site deployment has a wider blast radius than a desktop-only fix.
- Require separate approval before spending money, publishing a release, changing production infrastructure, or introducing a new external service.
- Keep website polish and other new feature ideas visible as later candidates, but outside these exit gates unless they directly block a wave.
