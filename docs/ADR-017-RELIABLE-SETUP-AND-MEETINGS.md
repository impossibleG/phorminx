# ADR 017: Reliable setup, richer meetings, and product operations

Status: local alpha test build; hardware acceptance and production rollout pending.

## Scope

Implement recording reliability fixes; independent settings validation and local
Ollama recovery; integrated verified setup; recommended local embeddings; speech
model choices; dual-source meetings; optional local translation; explicit voice
feedback; and authenticated software-update distribution. Peer messaging is
excluded and requires a separate architecture/security review.

## Invariants

- Initial silence and an utterance endpoint do not mean the user stopped recording.
- Audio ownership is tracked by sample ranges, never blanket repeated-word removal.
- Recognition remains independent from formatting, translation, feedback and updates.
- Original meeting text, source identity and timestamps survive translation and saves.
- Microphone/computer labels identify sources, not individual remote speakers.
- A stopped local service is not proof that a selected model is uninstalled.
- Downloads/installation require informed setup consent; existing user data survives.
- Feedback is reviewed and explicitly sent. No automatic transcripts/audio/log upload.
- Network endpoints are public security boundaries, not secrets embedded in clients.
- Updates require trusted signatures and an idle installation boundary. No generated
  development key may become an implicit production trust root.
- Server deployments, signing credentials and paid storage provisioning are separate
  approval boundaries. A disabled/unconfigured path must never claim to be ready.

## Workstreams and sequencing

1. Reliability: reproduce silent starts and chunk overlap; preserve valid repetition;
   separate persistent settings validity from transient dependency availability;
   implement verified local Ollama start/retry with actionable readiness states.
2. Setup/models: reuse the verified starter bundle in installation/onboarding; retain
   user settings; expose recommended embeddings and resource-appropriate speech
   options. Published download checksums and licenses are prerequisites for catalogs.
3. Meetings: source-isolated capture and recognition, a common meeting timeline,
   persisted source labels, and independent bounded translation work. Test clock
   drift, simultaneous speech, one source failing, and playback leaking into mic.
4. Product operations: isolated feedback API/storage and explicit desktop submission;
   signed update metadata, verification, staging and idle handoff. No production
   traffic during development. AI ticket classification/testimonials follow core
   intake; submissions must never become executable agent instructions.

Reliability, meeting development and server contracts can proceed independently.
Shared desktop integration is coordinated; integration precedes independent QA.

## Verification and release gates

Developer tests precede an independent adversarial QA review. Findings return to
the owning developer and are rerun until resolved or explicitly blocked. Use only
synthetic fixtures. Run deterministic unit/integration suites and formatting/lint
checks. Native recognition and physical audio tests are separate: ignored tests
are not evidence of hardware success. Clean-machine installation, initial silence,
long speech, overlap, offline/stopped Ollama, interrupted downloads, dual audio,
translation contention, feedback retry/auth/host isolation, tampered updates,
active-recording update deferral and data migration all need coverage.

## Known external prerequisites

The website is static behind a shared hostname gateway. Durable feedback needs an
explicit persistent store; an ephemeral hosting filesystem is not durable. Update
delivery needs a chosen release signing authority and distribution policy. Do not
silently provision either, publish unsigned updates, or deploy gateway changes.

## Implemented boundaries

- Installer preparation is an explicit post-install option with a Start-menu retry
  shortcut. Failed preparation leaves the app installed and reports incomplete
  model setup. Cached complete downloads are reusable; partial downloads restart.
- Local Ollama recovery verifies the installed executable's publisher before
  starting it. A stopped service no longer invalidates unchanged saved model
  selections. Recovery does not silently substitute or download a different model.
- Curated English Vosk downloads span small, lgraph, full English and GigaSpeech
  models. Activation is explicit and uses a background native probe. Download
  archive identity and cached layout checks are distinct from runtime readiness.
  Existing native Windows runtime remains pinned; newer source-only releases do
  not constitute a tested Windows binary upgrade.
- Fresh settings recommend Nomic embeddings; existing configurations/indexes are
  preserved. Translation is separately opt-in and never replaces original text.
- Dual capture preserves per-source samples and start offsets. Ordering markers
  for assistant cutoffs are separate from original audio timestamps. This is
  source attribution, not identification of individual remote speakers.
- Voice feedback is a local reviewable draft, with explicit Send and separately
  optional minimal diagnostics. The server acknowledges only durable writes and
  deduplicates retry IDs. No automatic testimonial publication or AI triage.
- Updates are opt-in and fail closed without a compiled verification key. Signed
  metadata, monotonic release sequence, expiry, exact size/hash and constrained
  release URLs precede staging. Installation waits for normal application exit;
  it does not terminate recording or force-close the app. Stable metadata rejects
  prereleases. Public distribution also requires a reviewed signing/release process.

## Remaining validation and operational limits

Automated fixtures cannot certify all devices. Initial-silence native Vosk tests
did not reproduce the reported immediate stop; content-free stop-origin diagnostics
were added, and a live reproduction is still required. No claimed universal fix
should be inferred from a passing synthetic suite.

Both-source mode loads two recognizers. Headphones are recommended: acoustic echo
cancellation and device clock-rate drift correction are not implemented. Translation
is bounded/best-effort and reports skipped work instead of blocking capture. Original
text queues are not globally bounded during prolonged source/storage failures.
Clean-machine bootstrap, physical dual-device capture, long-running clock drift,
translation quality under contention, and an actual signed installer lifecycle are
explicit release gates, not tests silently counted as passing.

The current server is not deployed by this work. Durable feedback storage, an admin
secret, edge rate limiting, privacy/retention operations and release signing remain
operator prerequisites. User-facing feedback should not be advertised as live until
that deployment is verified. Peer messaging remains out of scope.
