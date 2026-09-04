//! User-facing composition for trusted Ollama onboarding and measured setup.
//!
//! The egui thread owns only small immutable views and intent routing. Every
//! filesystem, model, network, native capture, and settings-CAS operation is
//! performed by a generation-bound worker.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use phorminx_audio::ActiveRecording;
use phorminx_ollama::{
    CancellationToken, CuratedLanguage, CuratedModel, CuratedModelCatalog, CuratedModelId,
    DaemonState, InstalledModelIdentity, ModelPullProgress, PullFailure, PullFailureKind,
    PullPhase, PullResidue,
};
use phorminx_setup::{
    BackendKind, BenchmarkEvidence, CalibrationKind, ContentFreeId, EngineKind, Language,
    ModelClass, Recommendation, RecommendationOutcome, RecommendationPolicy,
    RecommendationPreference,
};
use phorminx_ui::{
    BenchmarkCandidateView, BenchmarkEvidenceView, BenchmarkUnavailableView,
    CalibrationCaptureState, CalibrationPromptView, OllamaModelChoice, OllamaOperationState,
    OllamaSetupSnapshot, OllamaSetupState, PerformancePreference, PerformanceRecommendationView,
    PerformanceRollbackState, PerformanceRunState, PerformanceSetupSnapshot, SetupSnapshot,
};
use phorminx_windows::{
    OLLAMA_OFFICIAL_WINDOWS_DOWNLOAD, OllamaInstallReview, open_official_ollama_download,
};

use crate::ollama_onboarding::{OllamaHostState, OllamaOnboardingHost};
use crate::performance::{
    AppliedRecommendation, ApplyConsent, BenchmarkRunState, BenchmarkTicket, EvidenceStore,
    PerformanceBenchmarkService, PerformanceRecommender, PersistedRollbackState,
};
use crate::performance_runtime::{
    CandidateUnavailableReason, ProductionCandidateInventory, RuntimeActivityKind,
    RuntimeActivityLease, TransientCalibrationAudio, discover_installed_candidates,
    production_workload_coordinator,
};
use crate::settings::{OllamaModelIdentity, Settings, SettingsStore};

const BENCHMARK_DEADLINE: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeaturePoll {
    pub changed: bool,
    pub runtime_reload: bool,
    pub settings_refresh: bool,
}

enum FeatureEvent {
    OllamaInspected {
        generation: u64,
        state: OllamaHostState,
    },
    OllamaProgress {
        generation: u64,
        progress: ModelPullProgress,
    },
    OllamaPullFinished {
        generation: u64,
        result: Result<CuratedModel, PullFailure>,
    },
    OllamaAuthorizationFailed {
        generation: u64,
    },
    OfficialPageOpened {
        generation: u64,
        succeeded: bool,
    },
    OllamaActivated {
        generation: u64,
        result: Result<String, ()>,
    },
    CandidatesDiscovered {
        generation: u64,
        result: Result<ProductionCandidateInventory, ()>,
    },
    RollbackInspected {
        generation: u64,
        state: PersistedRollbackState,
    },
    CaptureStarted {
        generation: u64,
        case_id: ContentFreeId,
        result: Result<(ActiveRecording, RuntimeActivityLease), ()>,
    },
    CaptureFinished {
        generation: u64,
        case_id: ContentFreeId,
        result: Result<(), ()>,
    },
    BenchmarkStarted {
        generation: u64,
        result: Result<(PerformanceBenchmarkService, BenchmarkTicket), String>,
    },
    EvidenceCommitted {
        generation: u64,
        result: Result<Vec<BenchmarkEvidence>, ()>,
    },
    RecommendationApplied {
        generation: u64,
        recommender: PerformanceRecommender,
        result: Result<Box<AppliedRecommendation>, String>,
    },
    RecommendationReverted {
        generation: u64,
        result: Result<(), String>,
    },
    RollbackDiscarded {
        generation: u64,
        result: Result<(), String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EventLane {
    Ollama,
    Discovery,
    Rollback,
    Capture,
    BenchmarkStart,
    Evidence,
    Settings,
}

impl FeatureEvent {
    const fn lane(&self) -> EventLane {
        match self {
            Self::OllamaInspected { .. }
            | Self::OllamaProgress { .. }
            | Self::OllamaPullFinished { .. }
            | Self::OllamaAuthorizationFailed { .. }
            | Self::OfficialPageOpened { .. }
            | Self::OllamaActivated { .. } => EventLane::Ollama,
            Self::CandidatesDiscovered { .. } => EventLane::Discovery,
            Self::RollbackInspected { .. } => EventLane::Rollback,
            Self::CaptureStarted { .. } | Self::CaptureFinished { .. } => EventLane::Capture,
            Self::BenchmarkStarted { .. } => EventLane::BenchmarkStart,
            Self::EvidenceCommitted { .. } => EventLane::Evidence,
            Self::RecommendationApplied { .. }
            | Self::RecommendationReverted { .. }
            | Self::RollbackDiscarded { .. } => EventLane::Settings,
        }
    }

    const fn generation(&self) -> u64 {
        match self {
            Self::OllamaInspected { generation, .. }
            | Self::OllamaProgress { generation, .. }
            | Self::OllamaPullFinished { generation, .. }
            | Self::OllamaAuthorizationFailed { generation }
            | Self::OfficialPageOpened { generation, .. }
            | Self::OllamaActivated { generation, .. }
            | Self::CandidatesDiscovered { generation, .. }
            | Self::RollbackInspected { generation, .. }
            | Self::CaptureStarted { generation, .. }
            | Self::CaptureFinished { generation, .. }
            | Self::BenchmarkStarted { generation, .. }
            | Self::EvidenceCommitted { generation, .. }
            | Self::RecommendationApplied { generation, .. }
            | Self::RecommendationReverted { generation, .. }
            | Self::RollbackDiscarded { generation, .. } => *generation,
        }
    }
}

#[derive(Default)]
struct EventMailbox {
    slots: Mutex<BTreeMap<EventLane, FeatureEvent>>,
}

impl EventMailbox {
    fn push_terminal(&self, event: FeatureEvent) {
        if let Ok(mut slots) = self.slots.lock() {
            let lane = event.lane();
            if slots
                .get(&lane)
                .is_some_and(|existing| existing.generation() > event.generation())
            {
                return;
            }
            slots.insert(lane, event);
        }
    }

    fn push_progress(&self, generation: u64, progress: ModelPullProgress) {
        if let Ok(mut slots) = self.slots.lock() {
            let lane = EventLane::Ollama;
            let replace = slots.get(&lane).is_none_or(|existing| {
                existing.generation() < generation
                    || (existing.generation() == generation
                        && matches!(existing, FeatureEvent::OllamaProgress { .. }))
            });
            if replace {
                slots.insert(
                    lane,
                    FeatureEvent::OllamaProgress {
                        generation,
                        progress,
                    },
                );
            }
        }
    }

    fn drain(&self) -> Vec<FeatureEvent> {
        self.slots
            .lock()
            .map(|mut slots| std::mem::take(&mut *slots).into_values().collect())
            .unwrap_or_default()
    }
}

pub struct SetupFeatures {
    mailbox: Arc<EventMailbox>,
    store: SettingsStore,
    ollama: OllamaFeature,
    performance: PerformanceFeature,
    active: bool,
    settings_refresh_pending: bool,
}

struct OllamaFeature {
    generation: u64,
    cancel: Option<CancellationToken>,
    inspect_in_flight: bool,
    inspect_pending: bool,
    state: Option<OllamaHostState>,
    operation: OllamaOperationState,
    operation_detail: Option<String>,
    progress_percent: Option<u8>,
    selected: Option<String>,
    selected_identity: Option<OllamaModelIdentity>,
}

struct ActiveCapture {
    generation: u64,
    case_id: ContentFreeId,
    recording: ActiveRecording,
    _activity: RuntimeActivityLease,
}

struct PerformanceFeature {
    discovery_generation: u64,
    capture_generation: u64,
    benchmark_generation: u64,
    mutation_generation: u64,
    discovery_in_flight: bool,
    discovery_pending: bool,
    language: Language,
    preference: PerformancePreference,
    inventory: Option<ProductionCandidateInventory>,
    inventory_error: bool,
    selected_candidate: Option<ContentFreeId>,
    calibration: Arc<TransientCalibrationAudio>,
    capture_states: BTreeMap<ContentFreeId, CalibrationCaptureState>,
    capture_details: BTreeMap<ContentFreeId, String>,
    active_capture: Option<ActiveCapture>,
    capture_starting: bool,
    benchmark: Option<(PerformanceBenchmarkService, BenchmarkTicket)>,
    benchmark_starting: bool,
    benchmark_cancel_pending: bool,
    run_state: PerformanceRunState,
    run_detail: String,
    progress: (u32, u32),
    evidence: Vec<BenchmarkEvidence>,
    recommendation: Option<Recommendation>,
    apply_consent: Option<ApplyConsent>,
    recommender: Option<PerformanceRecommender>,
    applied: Option<AppliedRecommendation>,
    mutation_in_flight: bool,
    evidence_commit_in_flight: bool,
    rollback_probe_generation: u64,
    rollback_probe_in_flight: bool,
    rollback_probe_pending: bool,
    rollback_state: PersistedRollbackState,
    rollback_detail: Option<String>,
}

impl SetupFeatures {
    #[must_use]
    pub fn open(store: SettingsStore, settings: &Settings, active: bool) -> Self {
        let mailbox = Arc::new(EventMailbox::default());
        let language = settings_language(settings);
        let calibration = Arc::new(TransientCalibrationAudio::pinned_v1(language));
        let capture_states = empty_capture_states(&calibration);
        let mut features = Self {
            mailbox,
            store,
            ollama: OllamaFeature {
                generation: 0,
                cancel: None,
                inspect_in_flight: false,
                inspect_pending: false,
                state: None,
                operation: OllamaOperationState::Idle,
                operation_detail: None,
                progress_percent: None,
                selected: settings.formatting.ollama_model.clone(),
                selected_identity: settings.formatting.ollama_model_identity.clone(),
            },
            performance: PerformanceFeature {
                discovery_generation: 0,
                capture_generation: 0,
                benchmark_generation: 0,
                mutation_generation: 0,
                discovery_in_flight: false,
                discovery_pending: false,
                language,
                preference: PerformancePreference::Balanced,
                inventory: None,
                inventory_error: false,
                selected_candidate: None,
                calibration,
                capture_states,
                capture_details: BTreeMap::new(),
                active_capture: None,
                capture_starting: false,
                benchmark: None,
                benchmark_starting: false,
                benchmark_cancel_pending: false,
                run_state: PerformanceRunState::Discovering,
                run_detail: "Discovering exact, verified recognition candidates.".to_owned(),
                progress: (0, 0),
                evidence: Vec::new(),
                recommendation: None,
                apply_consent: None,
                recommender: None,
                applied: None,
                mutation_in_flight: false,
                evidence_commit_in_flight: false,
                rollback_probe_generation: 0,
                rollback_probe_in_flight: false,
                rollback_probe_pending: false,
                rollback_state: PersistedRollbackState::None,
                rollback_detail: None,
            },
            active,
            settings_refresh_pending: false,
        };
        if active {
            features.inspect_ollama();
            features.discover_performance();
            features.inspect_persisted_rollback();
        }
        features
    }

    pub fn activate(&mut self, settings: &Settings) {
        if self.active {
            return;
        }
        self.active = true;
        self.reconfigure(settings);
        if self.performance.benchmark.is_none()
            && !self.performance.benchmark_starting
            && !self.performance.evidence_commit_in_flight
            && !self.performance.mutation_in_flight
        {
            self.reset_calibration();
            if self.performance.applied.is_some() {
                self.performance.run_state = PerformanceRunState::Complete;
                self.performance.run_detail = "Recommendation remains staged for the next launch. Revert remains available in this session.".into();
            }
        }
        self.inspect_ollama();
        self.discover_performance();
        self.inspect_persisted_rollback();
    }

    pub fn deactivate(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        if matches!(
            self.ollama.operation,
            OllamaOperationState::Pulling | OllamaOperationState::Cancelling
        ) {
            if let Some(cancel) = self.ollama.cancel.as_ref() {
                cancel.cancel();
            }
            self.ollama.operation = OllamaOperationState::Cancelling;
            self.ollama.operation_detail = Some(
                "Stopping acquisition; reconcile will remain available if Ollama retained data."
                    .into(),
            );
        } else if self.ollama.inspect_in_flight {
            if let Some(cancel) = self.ollama.cancel.as_ref() {
                cancel.cancel();
            }
            self.ollama.inspect_pending = false;
        } else if self.ollama.operation != OllamaOperationState::Activating {
            if let Some(cancel) = self.ollama.cancel.take() {
                cancel.cancel();
            }
            self.ollama.generation = self.ollama.generation.wrapping_add(1);
            self.ollama.operation = OllamaOperationState::Idle;
            self.ollama.operation_detail = None;
        }
        self.performance.capture_generation = self.performance.capture_generation.wrapping_add(1);
        self.performance.discovery_pending = false;
        self.performance.rollback_probe_pending = false;
        self.performance.capture_starting = false;
        if let Some(active) = self.performance.active_capture.take() {
            drop_capture_async(active);
        }
        if let Some((service, ticket)) = self.performance.benchmark.as_ref() {
            self.performance.benchmark_cancel_pending = true;
            let _ = service.cancel(*ticket);
        }
        if self.performance.benchmark_starting {
            self.performance.benchmark_cancel_pending = true;
        }
        if self.performance.benchmark.is_none() && !self.performance.benchmark_starting {
            self.performance.calibration = Arc::new(TransientCalibrationAudio::pinned_v1(
                self.performance.language,
            ));
            self.performance.capture_states = empty_capture_states(&self.performance.calibration);
            self.performance.capture_details.clear();
            if self.performance.applied.is_some() {
                self.performance.run_state = PerformanceRunState::Complete;
                self.performance.run_detail = "Recommendation remains staged for the next launch. Revert remains available in this session; transient audio was destroyed.".into();
            } else {
                self.performance.run_state = PerformanceRunState::NeedsCalibration;
                self.performance.run_detail =
                    "Calibration stopped and all transient audio was destroyed.".into();
            }
        } else {
            self.performance.run_state = PerformanceRunState::Cancelling;
            self.performance.run_detail = "Stopping the native benchmark. Calibration audio remains only in the worker and will be destroyed when it returns or Phorminx exits.".into();
        }
    }

    pub fn enrich(&self, setup: &mut SetupSnapshot) {
        // The dedicated controllers below replace the legacy setup actions for
        // Ollama acquisition and calibration. Keep an already-running legacy
        // action visible so the user never loses its cancellation control.
        setup.actions.retain(|action| {
            action.running
                || !matches!(
                    action.id.as_str(),
                    id if id.starts_with("action:external:install:ollama")
                        || id.starts_with("action:external:start:ollama")
                        || id.starts_with("action:ollama:pull:")
                        || id.starts_with("action:benchmark:")
                )
        });
        setup.recommendation = None;
        let setup_busy = matches!(
            setup.stage,
            phorminx_ui::SetupStage::AwaitingConsent
                | phorminx_ui::SetupStage::Working
                | phorminx_ui::SetupStage::Benchmarking
        );
        let mut ollama = self.ollama_snapshot();
        let mut performance = self.performance_snapshot();
        if setup_busy {
            ollama.can_inspect = false;
            ollama.controls_enabled = false;
            performance.controls_enabled = false;
            performance.can_start_benchmark = false;
            performance.can_revert_after_restart = false;
            performance.can_discard_rollback = false;
            if let Some(recommendation) = performance.recommendation.as_mut() {
                recommendation.can_apply = false;
                recommendation.can_revert = false;
            }
        }
        setup.ollama = ollama;
        setup.performance = performance;
    }

    pub fn reconfigure(&mut self, settings: &Settings) {
        self.ollama.selected = settings.formatting.ollama_model.clone();
        self.ollama.selected_identity = settings.formatting.ollama_model_identity.clone();
        let language = settings_language(settings);
        if language != self.performance.language
            && !self.performance.benchmark_starting
            && self.performance.benchmark.is_none()
            && self.performance.active_capture.is_none()
            && !self.performance.capture_starting
            && !self.performance.mutation_in_flight
            && !self.performance.evidence_commit_in_flight
        {
            self.performance.language = language;
            self.reset_calibration();
            self.discover_performance();
        }
    }

    pub fn inspect_ollama(&mut self) {
        if !self.active {
            return;
        }
        if self.ollama.inspect_in_flight {
            self.ollama.inspect_pending = true;
            return;
        }
        if matches!(
            self.ollama.operation,
            OllamaOperationState::Pulling
                | OllamaOperationState::Cancelling
                | OllamaOperationState::Activating
        ) {
            return;
        }
        if let Some(cancel) = self.ollama.cancel.take() {
            cancel.cancel();
        }
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        let cancel = CancellationToken::new();
        self.ollama.cancel = Some(cancel.clone());
        self.ollama.inspect_in_flight = true;
        self.ollama.state = None;
        if self.ollama.operation != OllamaOperationState::ReconcileRequired {
            self.ollama.operation = OllamaOperationState::Idle;
            self.ollama.operation_detail =
                Some("Inspecting the fixed local installation boundary.".into());
        }
        let mailbox = Arc::clone(&self.mailbox);
        if thread::Builder::new()
            .name("phorminx-ollama-inspect".to_owned())
            .spawn(move || {
                let state = OllamaOnboardingHost::production().inspect(&cancel);
                mailbox.push_terminal(FeatureEvent::OllamaInspected { generation, state });
            })
            .is_err()
        {
            self.ollama.cancel = None;
            self.ollama.inspect_in_flight = false;
            self.ollama.operation_detail = Some("Local inspection could not start.".into());
        }
    }

    pub fn open_official_ollama_page(&mut self) {
        if !self.active || self.ollama.inspect_in_flight {
            return;
        }
        if !matches!(
            self.ollama.operation,
            OllamaOperationState::Idle
                | OllamaOperationState::Complete
                | OllamaOperationState::Failed
                | OllamaOperationState::ReconcileRequired
        ) {
            return;
        }
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        self.ollama.operation = OllamaOperationState::OpeningOfficialPage;
        self.ollama.operation_detail = Some(format!("Opening {OLLAMA_OFFICIAL_WINDOWS_DOWNLOAD}"));
        let mailbox = Arc::clone(&self.mailbox);
        if thread::Builder::new()
            .name("phorminx-ollama-official-page".to_owned())
            .spawn(move || {
                let review = OllamaInstallReview::manual();
                let confirmation = review.confirmation().to_owned();
                let succeeded = review
                    .authorize(&confirmation)
                    .and_then(|action| {
                        open_official_ollama_download(action)
                            .map_err(|_| phorminx_windows::OllamaInstallConsentError)
                    })
                    .is_ok();
                mailbox.push_terminal(FeatureEvent::OfficialPageOpened {
                    generation,
                    succeeded,
                });
            })
            .is_err()
        {
            self.ollama.operation = OllamaOperationState::Failed;
            self.ollama.operation_detail = Some("The official-page worker could not start.".into());
        }
    }

    pub fn pull_ollama_model(&mut self, id: &str) {
        if !self.active {
            return;
        }
        let Some(id) = curated_id(id) else {
            return;
        };
        if !matches!(
            self.ollama.state,
            Some(OllamaHostState::Daemon(DaemonState::Ready { .. }))
        ) || self.ollama.operation != OllamaOperationState::Idle
            || self.performance_busy()
        {
            return;
        }
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        let cancel = CancellationToken::new();
        self.ollama.cancel = Some(cancel.clone());
        self.ollama.operation = OllamaOperationState::Pulling;
        self.ollama.operation_detail = Some("Resolving the pinned model manifest.".into());
        self.ollama.progress_percent = Some(0);
        let mailbox = Arc::clone(&self.mailbox);
        if thread::Builder::new()
            .name("phorminx-ollama-pull".to_owned())
            .spawn(move || {
                let host = OllamaOnboardingHost::production();
                let review = host.review_pull(id);
                let confirmation = review.confirmation().to_owned();
                let authorization = match review.authorize(&confirmation) {
                    Ok(authorization) => authorization,
                    Err(_) => {
                        mailbox
                            .push_terminal(FeatureEvent::OllamaAuthorizationFailed { generation });
                        return;
                    }
                };
                let progress_mailbox = Arc::clone(&mailbox);
                let result = host
                    .pull(authorization, &cancel, |progress| {
                        progress_mailbox.push_progress(generation, progress);
                    })
                    .map(|outcome| outcome.model);
                mailbox.push_terminal(FeatureEvent::OllamaPullFinished { generation, result });
            })
            .is_err()
        {
            self.ollama.cancel = None;
            self.ollama.operation = OllamaOperationState::Failed;
            self.ollama.operation_detail =
                Some("The model acquisition worker could not start.".into());
        }
    }

    pub fn cancel_ollama_pull(&mut self) {
        if let Some(cancel) = self.ollama.cancel.as_ref()
            && self.ollama.operation == OllamaOperationState::Pulling
        {
            cancel.cancel();
            self.ollama.operation = OllamaOperationState::Cancelling;
            self.ollama.operation_detail = Some("Stopping the bounded local API request.".into());
        }
    }

    pub fn activate_ollama_model(&mut self, id: &str) {
        if !self.active || self.ollama.inspect_in_flight {
            return;
        }
        let Some(id) = curated_id(id) else {
            return;
        };
        if self.ollama.operation != OllamaOperationState::Idle || self.performance_busy() {
            return;
        }
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        self.ollama.operation = OllamaOperationState::Activating;
        self.ollama.operation_detail =
            Some("Re-verifying the exact model identity before activation.".into());
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-ollama-activate".to_owned())
            .spawn(move || {
                let cancel = CancellationToken::new();
                let host = OllamaOnboardingHost::production();
                let model = host.catalog().get(id);
                let result = match host.inspect(&cancel) {
                    OllamaHostState::Daemon(DaemonState::Ready { models, .. }) => {
                        let installed = models.iter().find(|installed| {
                            exact_installed(model, std::slice::from_ref(*installed))
                        });
                        let Some(installed) = installed else {
                            mailbox.push_terminal(FeatureEvent::OllamaActivated {
                                generation,
                                result: Err(()),
                            });
                            return;
                        };
                        let Ok(identity) = OllamaModelIdentity::new(
                            installed.manifest_sha256.clone(),
                            installed.bytes,
                        ) else {
                            mailbox.push_terminal(FeatureEvent::OllamaActivated {
                                generation,
                                result: Err(()),
                            });
                            return;
                        };
                        let before = store.load().map_err(|_| ());
                        before.and_then(|before| {
                            let mut after = before.clone();
                            after.formatting.ollama_model = Some(model.local_name().to_owned());
                            after.formatting.ollama_model_identity = Some(identity);
                            after.validate_and_normalize().map_err(|_| ())?;
                            match store.compare_and_save(&before, &after).map_err(|_| ())? {
                                true => Ok(model.local_name().to_owned()),
                                false => Err(()),
                            }
                        })
                    }
                    _ => Err(()),
                };
                mailbox.push_terminal(FeatureEvent::OllamaActivated { generation, result });
            })
            .is_err()
        {
            self.ollama.operation = OllamaOperationState::Failed;
            self.ollama.operation_detail =
                Some("The model activation worker could not start.".into());
        }
    }

    pub fn select_benchmark_candidate(&mut self, id: &str) {
        if self.performance_busy() || self.performance.applied.is_some() {
            return;
        }
        let Some(inventory) = self.performance.inventory.as_ref() else {
            return;
        };
        let selected = inventory
            .candidates_for(self.performance.language)
            .into_iter()
            .find(|candidate| candidate.candidate_id().as_str() == id)
            .map(|candidate| candidate.candidate_id().clone());
        if selected.is_some() {
            self.performance.selected_candidate = selected;
            self.performance.recommendation = None;
            self.performance.apply_consent = None;
            self.update_ready_state();
        }
    }

    pub fn set_preference(&mut self, preference: PerformancePreference) {
        if self.performance_busy() || self.performance.applied.is_some() {
            return;
        }
        self.performance.preference = preference;
        self.evaluate_recommendation();
    }

    pub fn start_capture(&mut self, id: &str, microphone: Option<String>) {
        if !self.active
            || self.performance.inventory.is_none()
            || self.performance_busy()
            || self.performance.applied.is_some()
        {
            return;
        }
        let Some(case_id) = self.capture_case_id(id) else {
            return;
        };
        if self.performance.calibration.discard(&case_id).is_err() {
            return;
        }
        self.performance.capture_generation = self.performance.capture_generation.wrapping_add(1);
        let generation = self.performance.capture_generation;
        self.performance.capture_starting = true;
        self.performance
            .capture_states
            .insert(case_id.clone(), CalibrationCaptureState::Starting);
        self.performance.capture_details.remove(&case_id);
        let failure_case_id = case_id.clone();
        let mailbox = Arc::clone(&self.mailbox);
        if thread::Builder::new()
            .name("phorminx-calibration-start".to_owned())
            .spawn(move || {
                let result = production_workload_coordinator()
                    .try_begin(RuntimeActivityKind::CalibrationCapture)
                    .map_err(|_| ())
                    .and_then(|activity| {
                        phorminx_audio::start_input(microphone.as_deref())
                            .map(|recording| (recording, activity))
                            .map_err(|_| ())
                    });
                mailbox.push_terminal(FeatureEvent::CaptureStarted {
                    generation,
                    case_id,
                    result,
                });
            })
            .is_err()
        {
            self.performance.capture_starting = false;
            self.performance
                .capture_states
                .insert(failure_case_id, CalibrationCaptureState::Failed);
        }
    }

    pub fn stop_capture(&mut self, id: &str) {
        let Some(case_id) = self.capture_case_id(id) else {
            return;
        };
        let Some(active) = self.performance.active_capture.take() else {
            return;
        };
        if active.case_id != case_id {
            self.performance.active_capture = Some(active);
            return;
        }
        self.performance
            .capture_states
            .insert(case_id.clone(), CalibrationCaptureState::Processing);
        let mailbox = Arc::clone(&self.mailbox);
        let calibration = Arc::clone(&self.performance.calibration);
        let generation = active.generation;
        let failure_case_id = case_id.clone();
        if thread::Builder::new()
            .name("phorminx-calibration-finish".to_owned())
            .spawn(move || {
                let result = active
                    .recording
                    .finish()
                    .map_err(|_| ())
                    .and_then(|mut clip| {
                        clip.samples.truncate(clip.sample_rate as usize * 30);
                        calibration.replace(&case_id, clip).map_err(|_| ())
                    });
                mailbox.push_terminal(FeatureEvent::CaptureFinished {
                    generation,
                    case_id,
                    result,
                });
            })
            .is_err()
        {
            self.performance
                .capture_states
                .insert(failure_case_id, CalibrationCaptureState::Failed);
        }
    }

    pub fn discard_capture(&mut self, id: &str) {
        if self.performance_busy() || self.performance.applied.is_some() {
            return;
        }
        let Some(case_id) = self.capture_case_id(id) else {
            return;
        };
        if self
            .performance
            .active_capture
            .as_ref()
            .is_some_and(|active| active.case_id == case_id)
        {
            return;
        }
        if self.performance.calibration.discard(&case_id).is_ok() {
            self.performance
                .capture_states
                .insert(case_id.clone(), CalibrationCaptureState::Empty);
            self.performance.capture_details.remove(&case_id);
            self.update_ready_state();
        }
    }

    pub fn reset_performance_calibration(&mut self) {
        if self.active && !self.performance_busy() && self.performance.applied.is_none() {
            self.reset_calibration();
        }
    }

    pub fn start_benchmark(&mut self) {
        if self.performance_busy()
            || self.performance.applied.is_some()
            || !self.performance.calibration.is_ready()
        {
            return;
        }
        let (Some(inventory), Some(selected)) = (
            self.performance.inventory.clone(),
            self.performance.selected_candidate.clone(),
        ) else {
            return;
        };
        let Some(candidate) = inventory
            .candidates_for(self.performance.language)
            .into_iter()
            .find(|candidate| candidate.candidate_id() == &selected)
        else {
            return;
        };
        let Some(context) = inventory.context().cloned() else {
            return;
        };
        self.performance.benchmark_generation =
            self.performance.benchmark_generation.wrapping_add(1);
        let generation = self.performance.benchmark_generation;
        self.performance.benchmark_starting = true;
        self.performance.benchmark_cancel_pending = false;
        self.performance.run_state = PerformanceRunState::Running;
        self.performance.run_detail =
            "Checking live workload, thermal, memory, and compute availability.".into();
        self.performance.progress = (0, 4);
        let language = self.performance.language;
        let calibration = Arc::clone(&self.performance.calibration);
        let mailbox = Arc::clone(&self.mailbox);
        if thread::Builder::new()
            .name("phorminx-benchmark-start".to_owned())
            .spawn(move || {
                let service = inventory.benchmark_service(calibration);
                let result = service
                    .start(candidate, context, language, BENCHMARK_DEADLINE)
                    .map(|ticket| (service, ticket))
                    .map_err(|error| error.to_string());
                mailbox.push_terminal(FeatureEvent::BenchmarkStarted { generation, result });
            })
            .is_err()
        {
            self.performance.benchmark_starting = false;
            self.performance.run_state = PerformanceRunState::Failed;
            self.performance.run_detail = "The benchmark worker could not start.".into();
        }
    }

    pub fn cancel_benchmark(&mut self) {
        if self.performance.benchmark.is_none() && !self.performance.benchmark_starting {
            return;
        }
        self.performance.benchmark_cancel_pending = true;
        if let Some((service, ticket)) = self.performance.benchmark.as_ref() {
            let _ = service.cancel(*ticket);
        }
        self.performance.run_state = PerformanceRunState::Cancelling;
        self.performance.run_detail =
            "Cancellation was requested. Audio remains only in memory until the native worker returns or Phorminx exits."
                .into();
    }

    pub fn apply_recommendation(&mut self, id: &str) {
        if self.performance_busy()
            || self.performance.rollback_state != PersistedRollbackState::None
        {
            return;
        }
        let valid_id = self.performance.recommendation.as_ref().is_some_and(|recommendation| {
            matches!(&recommendation.outcome, RecommendationOutcome::Recommended { candidate_id, .. } if candidate_id.as_str() == id)
        });
        if !valid_id {
            return;
        }
        let (Some(recommender), Some(consent)) = (
            self.performance.recommender.take(),
            self.performance.apply_consent.take(),
        ) else {
            return;
        };
        self.performance.mutation_generation = self.performance.mutation_generation.wrapping_add(1);
        let generation = self.performance.mutation_generation;
        self.performance.mutation_in_flight = true;
        self.performance.run_detail =
            "Re-verifying assets and committing settings atomically.".into();
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-recommendation-apply".to_owned())
            .spawn(move || {
                let mut recommender = recommender;
                let result = recommender
                    .apply(consent, &store)
                    .map(Box::new)
                    .map_err(|error| error.to_string());
                mailbox.push_terminal(FeatureEvent::RecommendationApplied {
                    generation,
                    recommender,
                    result,
                });
            })
            .is_err()
        {
            self.performance.mutation_in_flight = false;
            self.performance.run_detail = "The settings worker could not start.".into();
        }
    }

    pub fn revert_recommendation(&mut self) {
        if self.performance_busy()
            || self.performance.rollback_state != PersistedRollbackState::Ready
        {
            return;
        }
        self.performance.mutation_generation = self.performance.mutation_generation.wrapping_add(1);
        let generation = self.performance.mutation_generation;
        self.performance.mutation_in_flight = true;
        self.performance.rollback_state = PersistedRollbackState::Unavailable;
        self.performance.run_detail =
            "Checking that settings are unchanged before reverting.".into();
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-recommendation-revert".to_owned())
            .spawn(move || {
                let result = PerformanceRecommender::rollback_after_restart(&store)
                    .map_err(|error| error.to_string());
                mailbox.push_terminal(FeatureEvent::RecommendationReverted { generation, result });
            })
            .is_err()
        {
            self.performance.mutation_in_flight = false;
            self.performance.rollback_state = PersistedRollbackState::Ready;
            self.performance.run_detail = "The settings worker could not start.".into();
        }
    }

    pub fn discard_persisted_rollback(&mut self) {
        if self.performance_busy()
            || !matches!(
                self.performance.rollback_state,
                PersistedRollbackState::Ready | PersistedRollbackState::StaleOrCorrupt
            )
        {
            return;
        }
        self.performance.mutation_generation = self.performance.mutation_generation.wrapping_add(1);
        let generation = self.performance.mutation_generation;
        self.performance.mutation_in_flight = true;
        self.performance.rollback_state = PersistedRollbackState::Unavailable;
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-recommendation-discard".to_owned())
            .spawn(move || {
                let result = PerformanceRecommender::discard_persisted_rollback(&store)
                    .map_err(|error| error.to_string());
                mailbox.push_terminal(FeatureEvent::RollbackDiscarded { generation, result });
            })
            .is_err()
        {
            self.performance.mutation_in_flight = false;
            self.performance.run_detail = "The rollback discard worker could not start.".into();
            self.inspect_persisted_rollback();
        }
    }

    pub fn poll(&mut self) -> FeaturePoll {
        let mut outcome = FeaturePoll::default();
        for event in self.mailbox.drain() {
            outcome.changed = true;
            outcome.runtime_reload |= self.handle_event(event);
        }
        if self.poll_benchmark() {
            outcome.changed = true;
        }
        outcome.settings_refresh = self.settings_refresh_pending;
        self.settings_refresh_pending = false;
        let auto_stop = self
            .performance
            .active_capture
            .as_ref()
            .filter(|active| active.recording.captured_duration() >= Duration::from_secs(29))
            .map(|active| active.case_id.as_str().to_owned());
        if let Some(case_id) = auto_stop {
            self.stop_capture(&case_id);
            outcome.changed = true;
        }
        if self.active
            && self.performance.discovery_pending
            && !self.performance.discovery_in_flight
            && !self.performance_runtime_busy()
        {
            self.performance.discovery_pending = false;
            self.discover_performance();
            outcome.changed = true;
        }
        outcome
    }

    fn handle_event(&mut self, event: FeatureEvent) -> bool {
        match event {
            FeatureEvent::OllamaInspected { generation, state }
                if generation == self.ollama.generation =>
            {
                self.ollama.state = Some(state);
                self.ollama.cancel = None;
                self.ollama.inspect_in_flight = false;
                self.ollama.operation = OllamaOperationState::Idle;
                self.ollama.operation_detail = None;
                self.ollama.progress_percent = None;
                if self.ollama.inspect_pending && self.active {
                    self.ollama.inspect_pending = false;
                    self.inspect_ollama();
                }
            }
            FeatureEvent::OllamaProgress {
                generation,
                progress,
            } if generation == self.ollama.generation => {
                self.ollama.operation = OllamaOperationState::Pulling;
                self.ollama.operation_detail = Some(pull_phase_label(progress.phase).to_owned());
                self.ollama.progress_percent = pull_percent(&progress);
            }
            FeatureEvent::OllamaPullFinished { generation, result }
                if generation == self.ollama.generation =>
            {
                self.ollama.cancel = None;
                match result {
                    Ok(_) => {
                        self.ollama.operation = OllamaOperationState::Complete;
                        self.ollama.operation_detail = Some("The exact manifest identity is installed. Inspecting once more before selection.".into());
                        self.inspect_ollama();
                    }
                    Err(error) => {
                        self.ollama.operation = if error.residue == PullResidue::None {
                            OllamaOperationState::Failed
                        } else {
                            OllamaOperationState::ReconcileRequired
                        };
                        self.ollama.operation_detail = Some(pull_failure_label(&error).to_owned());
                    }
                }
            }
            FeatureEvent::OllamaAuthorizationFailed { generation }
                if generation == self.ollama.generation =>
            {
                self.ollama.cancel = None;
                self.ollama.operation = OllamaOperationState::Failed;
                self.ollama.operation_detail = Some(
                    "The exact compiled acquisition consent could not be authorized; no request was sent."
                        .into(),
                );
            }
            FeatureEvent::OfficialPageOpened {
                generation,
                succeeded,
            } if generation == self.ollama.generation => {
                self.ollama.operation = if succeeded {
                    OllamaOperationState::Complete
                } else {
                    OllamaOperationState::Failed
                };
                self.ollama.operation_detail = Some(if succeeded {
                    "The official page is open. Install manually, start Ollama, then inspect again."
                } else {
                    "Windows could not open the fixed official page."
                }.to_owned());
            }
            FeatureEvent::OllamaActivated { generation, result }
                if generation == self.ollama.generation =>
            {
                self.ollama.operation = if result.is_ok() {
                    OllamaOperationState::Complete
                } else {
                    OllamaOperationState::Failed
                };
                match result {
                    Ok(name) => {
                        self.ollama.selected = Some(name);
                        self.ollama.operation_detail = Some("The verified model is now the runtime selection.".into());
                        return true;
                    }
                    Err(()) => self.ollama.operation_detail = Some("The model changed or settings were edited concurrently; nothing was overwritten.".into()),
                }
            }
            FeatureEvent::CandidatesDiscovered { generation, result }
                if generation == self.performance.discovery_generation =>
            {
                self.performance.discovery_in_flight = false;
                match result {
                    Ok(inventory) => {
                        self.performance.inventory_error = false;
                        let candidates = inventory.candidates_for(self.performance.language);
                        self.performance.selected_candidate = candidates
                            .first()
                            .map(|candidate| candidate.candidate_id().clone());
                        self.performance.inventory = Some(inventory);
                        self.performance.run_detail = if candidates.is_empty() {
                            "No verified candidate supports the selected language.".into()
                        } else {
                            "Choose a candidate, capture the four transient samples, then measure it.".into()
                        };
                        self.update_ready_state();
                    }
                    Err(()) => {
                        self.performance.inventory_error = true;
                        self.performance.run_state = PerformanceRunState::Unavailable;
                        self.performance.run_detail =
                            "Verified candidate discovery could not complete.".into();
                    }
                }
                if self.performance.discovery_pending {
                    self.performance.discovery_pending = false;
                    self.discover_performance();
                }
            }
            FeatureEvent::RollbackInspected { generation, state }
                if generation == self.performance.rollback_probe_generation =>
            {
                self.performance.rollback_probe_in_flight = false;
                self.performance.rollback_state = state;
                self.performance.rollback_detail = Some(match state {
                    PersistedRollbackState::None => {
                        "No pending performance change can be reverted."
                    }
                    PersistedRollbackState::Ready => {
                        "The previous recognition settings can be restored, including after restart."
                    }
                    PersistedRollbackState::StaleOrCorrupt => {
                        "A rollback receipt exists but cannot safely overwrite current settings. Review and discard it before applying another recommendation."
                    }
                    PersistedRollbackState::Unavailable => {
                        "Rollback eligibility could not be inspected. Existing settings were not changed."
                    }
                }.into());
                if self.performance.rollback_probe_pending && self.active {
                    self.performance.rollback_probe_pending = false;
                    self.inspect_persisted_rollback();
                }
            }
            FeatureEvent::CaptureStarted {
                generation,
                case_id,
                result,
            } if generation == self.performance.capture_generation => {
                self.performance.capture_starting = false;
                match result {
                    Ok((recording, activity)) => {
                        self.performance
                            .capture_states
                            .insert(case_id.clone(), CalibrationCaptureState::Recording);
                        self.performance.active_capture = Some(ActiveCapture {
                            generation,
                            case_id,
                            recording,
                            _activity: activity,
                        });
                    }
                    Err(()) => {
                        self.performance
                            .capture_states
                            .insert(case_id.clone(), CalibrationCaptureState::Failed);
                        self.performance.capture_details.insert(
                            case_id,
                            "The microphone could not start. No audio was retained.".into(),
                        );
                    }
                }
            }
            FeatureEvent::CaptureStarted {
                result: Ok((recording, activity)),
                ..
            } => drop_recording_and_lease_async(recording, activity),
            FeatureEvent::CaptureFinished {
                generation,
                case_id,
                result,
            } if generation == self.performance.capture_generation => match result {
                Ok(()) => {
                    self.performance
                        .capture_states
                        .insert(case_id, CalibrationCaptureState::Ready);
                    self.update_ready_state();
                }
                Err(()) => {
                    self.performance
                        .capture_states
                        .insert(case_id.clone(), CalibrationCaptureState::Failed);
                    self.performance.capture_details.insert(case_id, "Capture must be 0.2–30 seconds and free of device faults. No audio was retained.".into());
                }
            },
            FeatureEvent::BenchmarkStarted { generation, result }
                if generation == self.performance.benchmark_generation =>
            {
                self.performance.benchmark_starting = false;
                match result {
                    Ok(pair) => {
                        if self.performance.benchmark_cancel_pending {
                            let _ = pair.0.cancel(pair.1);
                        }
                        self.performance.benchmark = Some(pair);
                        self.performance.run_detail =
                            "Recognition is running locally against the one-use calibration set."
                                .into();
                    }
                    Err(message) => {
                        self.performance.run_state = PerformanceRunState::Failed;
                        self.performance.run_detail = preflight_detail(&message).to_owned();
                    }
                }
            }
            FeatureEvent::EvidenceCommitted { generation, result }
                if generation == self.performance.benchmark_generation =>
            {
                self.performance.evidence_commit_in_flight = false;
                match result {
                    Ok(evidence) => {
                        self.performance.evidence = evidence;
                        self.performance.run_state = PerformanceRunState::Complete;
                        self.performance.run_detail = "Content-free measurements were committed locally. Calibration audio has been destroyed.".into();
                        self.evaluate_recommendation();
                    }
                    Err(()) => {
                        self.performance.run_state = PerformanceRunState::Failed;
                        self.performance.run_detail = "Measurements completed, but their content-free evidence could not be committed.".into();
                    }
                }
            }
            FeatureEvent::RecommendationApplied {
                generation,
                recommender,
                result,
            } if generation == self.performance.mutation_generation => {
                self.performance.mutation_in_flight = false;
                self.performance.recommender = Some(recommender);
                match result {
                    Ok(receipt) => {
                        self.performance.applied = Some(*receipt);
                        self.performance.rollback_state = PersistedRollbackState::Ready;
                        self.performance.rollback_detail = Some(
                            "The previous recognition settings remain safely revertible after restart."
                                .into(),
                        );
                        self.settings_refresh_pending = true;
                        self.performance.run_detail = "Recommendation applied and verified. Restarting the recognition runtime; Revert remains available afterward.".into();
                        return true;
                    }
                    Err(message) => {
                        self.performance.run_detail = apply_failure_detail(&message).to_owned()
                    }
                }
            }
            FeatureEvent::RecommendationReverted { generation, result }
                if generation == self.performance.mutation_generation =>
            {
                self.performance.mutation_in_flight = false;
                match result {
                    Ok(()) => {
                        self.performance.applied = None;
                        self.performance.rollback_state = PersistedRollbackState::None;
                        self.performance.rollback_detail = Some(
                            "Previous recognition settings were restored; the rollback was consumed."
                                .into(),
                        );
                        self.settings_refresh_pending = true;
                        self.performance.run_detail =
                            "Previous settings restored and verified. Restarting the recognition runtime."
                                .into();
                        return true;
                    }
                    Err(message) => {
                        self.performance.run_detail = apply_failure_detail(&message).to_owned();
                        self.inspect_persisted_rollback();
                    }
                }
            }
            FeatureEvent::RollbackDiscarded { generation, result }
                if generation == self.performance.mutation_generation =>
            {
                self.performance.mutation_in_flight = false;
                match result {
                    Ok(()) => {
                        self.performance.applied = None;
                        self.performance.rollback_state = PersistedRollbackState::None;
                        self.performance.rollback_detail = Some(
                            "Rollback receipt discarded. Current settings were not changed.".into(),
                        );
                    }
                    Err(message) => {
                        self.performance.run_detail = apply_failure_detail(&message).to_owned();
                        self.inspect_persisted_rollback();
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn discover_performance(&mut self) {
        if !self.active {
            return;
        }
        if self.performance.discovery_in_flight {
            self.performance.discovery_pending = true;
            return;
        }
        if self.performance_runtime_busy() {
            self.performance.discovery_pending = true;
            return;
        }
        self.performance.discovery_in_flight = true;
        self.performance.discovery_generation =
            self.performance.discovery_generation.wrapping_add(1);
        let generation = self.performance.discovery_generation;
        self.performance.inventory = None;
        self.performance.inventory_error = false;
        self.performance.run_state = PerformanceRunState::Discovering;
        self.performance.run_detail =
            "Hashing only pinned local candidates on a background worker.".into();
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-performance-discovery".to_owned())
            .spawn(move || {
                let result = discover_installed_candidates(&store).map_err(|_| ());
                mailbox.push_terminal(FeatureEvent::CandidatesDiscovered { generation, result });
            })
            .is_err()
        {
            self.performance.discovery_in_flight = false;
            self.performance.inventory_error = true;
            self.performance.run_state = PerformanceRunState::Unavailable;
            self.performance.run_detail = "Verified candidate discovery could not start.".into();
        }
    }

    fn inspect_persisted_rollback(&mut self) {
        if !self.active {
            return;
        }
        if self.performance.rollback_probe_in_flight {
            self.performance.rollback_probe_pending = true;
            return;
        }
        self.performance.rollback_probe_generation =
            self.performance.rollback_probe_generation.wrapping_add(1);
        let generation = self.performance.rollback_probe_generation;
        self.performance.rollback_probe_in_flight = true;
        self.performance.rollback_detail =
            Some("Inspecting the content-free rollback receipt.".into());
        let mailbox = Arc::clone(&self.mailbox);
        let store = self.store.clone();
        if thread::Builder::new()
            .name("phorminx-rollback-inspect".to_owned())
            .spawn(move || {
                let state = PerformanceRecommender::persisted_rollback_state(&store);
                mailbox.push_terminal(FeatureEvent::RollbackInspected { generation, state });
            })
            .is_err()
        {
            self.performance.rollback_probe_in_flight = false;
            self.performance.rollback_state = PersistedRollbackState::Unavailable;
            self.performance.rollback_detail =
                Some("The rollback inspection worker could not start.".into());
        }
    }

    fn poll_benchmark(&mut self) -> bool {
        let Some((service, _)) = self.performance.benchmark.as_ref() else {
            return false;
        };
        match service.snapshot() {
            BenchmarkRunState::Idle => false,
            BenchmarkRunState::Running { completed, total } => {
                let changed = self.performance.progress != (completed, total);
                self.performance.progress = (completed, total);
                changed
            }
            BenchmarkRunState::Cancelling { completed, total } => {
                self.performance.progress = (completed, total);
                self.performance.run_state = PerformanceRunState::Cancelling;
                true
            }
            BenchmarkRunState::Complete(evidence) => {
                let evidence = *evidence;
                self.performance.benchmark = None;
                self.consume_calibration_views();
                self.commit_evidence(evidence);
                true
            }
            BenchmarkRunState::Failed(failure) => {
                self.performance.benchmark = None;
                self.performance.run_state = PerformanceRunState::Failed;
                self.performance.run_detail =
                    format!("Benchmark stopped: {failure}. Capture a fresh set before retrying.");
                self.reset_calibration();
                true
            }
        }
    }

    fn commit_evidence(&mut self, evidence: BenchmarkEvidence) {
        let generation = self.performance.benchmark_generation;
        self.performance.run_detail = "Committing bounded, content-free evidence.".into();
        self.performance.evidence_commit_in_flight = true;
        let mailbox = Arc::clone(&self.mailbox);
        let context = self
            .performance
            .inventory
            .as_ref()
            .and_then(ProductionCandidateInventory::context)
            .cloned();
        if thread::Builder::new()
            .name("phorminx-evidence-commit".to_owned())
            .spawn(move || {
                let result = (|| {
                    let context = context.ok_or(())?;
                    let store = EvidenceStore::default_for_current_user().map_err(|_| ())?;
                    store.save(&[evidence]).map_err(|_| ())?;
                    store.load_matching(&context).map_err(|_| ())
                })();
                mailbox.push_terminal(FeatureEvent::EvidenceCommitted { generation, result });
            })
            .is_err()
        {
            self.performance.evidence_commit_in_flight = false;
            self.performance.run_state = PerformanceRunState::Failed;
            self.performance.run_detail = "The evidence worker could not start.".into();
        }
    }

    fn evaluate_recommendation(&mut self) {
        let Some(inventory) = self.performance.inventory.as_ref() else {
            return;
        };
        let Some(context) = inventory.context() else {
            return;
        };
        let candidates = inventory.candidates_for(self.performance.language);
        let Ok(policy) = RecommendationPolicy::interactive(
            context.protocol_id.clone(),
            context.build_id.clone(),
            context.device_id.clone(),
            context.driver_id.clone(),
            candidates
                .iter()
                .map(|candidate| candidate.trusted_identity()),
        ) else {
            return;
        };
        let Ok(mut recommender) =
            PerformanceRecommender::new(policy, inventory.profiles_for(self.performance.language))
        else {
            return;
        };
        let (recommendation, consent) = recommender.evaluate(
            self.performance.language,
            map_preference(self.performance.preference),
            self.performance.evidence.clone(),
        );
        self.performance.recommendation = Some(recommendation);
        self.performance.apply_consent = consent;
        self.performance.recommender = Some(recommender);
    }

    fn reset_calibration(&mut self) {
        self.performance.calibration = Arc::new(TransientCalibrationAudio::pinned_v1(
            self.performance.language,
        ));
        self.performance.capture_states = empty_capture_states(&self.performance.calibration);
        self.performance.capture_details.clear();
        self.performance.active_capture = None;
        self.performance.capture_starting = false;
        self.performance.benchmark = None;
        self.performance.benchmark_starting = false;
        self.performance.benchmark_cancel_pending = false;
        self.performance.evidence_commit_in_flight = false;
        self.update_ready_state();
    }

    #[must_use]
    pub fn mutation_conflict(&self) -> bool {
        self.performance_busy()
            || matches!(
                self.ollama.operation,
                OllamaOperationState::Pulling
                    | OllamaOperationState::Cancelling
                    | OllamaOperationState::Activating
            )
    }

    pub fn refresh_local_features(&mut self) {
        if self.active {
            self.inspect_ollama();
            self.discover_performance();
            self.inspect_persisted_rollback();
        }
    }

    fn performance_busy(&self) -> bool {
        self.performance.discovery_in_flight
            || self.performance.rollback_probe_in_flight
            || self.performance_runtime_busy()
    }

    fn performance_runtime_busy(&self) -> bool {
        self.performance.active_capture.is_some()
            || self.performance.capture_starting
            || self.performance.benchmark.is_some()
            || self.performance.benchmark_starting
            || self.performance.evidence_commit_in_flight
            || self.performance.mutation_in_flight
            || matches!(
                self.ollama.operation,
                OllamaOperationState::Pulling
                    | OllamaOperationState::Cancelling
                    | OllamaOperationState::Activating
            )
    }

    fn consume_calibration_views(&mut self) {
        self.performance.calibration = Arc::new(TransientCalibrationAudio::pinned_v1(
            self.performance.language,
        ));
        self.performance.capture_details.clear();
        for state in self.performance.capture_states.values_mut() {
            *state = CalibrationCaptureState::Consumed;
        }
    }

    fn update_ready_state(&mut self) {
        if self.performance.inventory_error
            || self.performance.inventory.as_ref().is_none_or(|inventory| {
                inventory
                    .candidates_for(self.performance.language)
                    .is_empty()
            })
        {
            self.performance.run_state = PerformanceRunState::Unavailable;
        } else if self.performance.calibration.is_ready() {
            self.performance.run_state = PerformanceRunState::Ready;
            self.performance.run_detail =
                "Calibration is complete and remains only in memory. Ready to measure.".into();
        } else {
            self.performance.run_state = PerformanceRunState::NeedsCalibration;
        }
    }

    fn capture_case_id(&self, id: &str) -> Option<ContentFreeId> {
        self.performance
            .calibration
            .prompts()
            .iter()
            .find(|prompt| prompt.case_id.as_str() == id)
            .map(|prompt| prompt.case_id.clone())
    }

    fn ollama_snapshot(&self) -> OllamaSetupSnapshot {
        let (state, detail, version, installed) = map_ollama_state(self.ollama.state.as_ref());
        let models = CuratedModelCatalog
            .all()
            .iter()
            .copied()
            .map(|model| {
                model_view(
                    model,
                    &installed,
                    self.ollama.selected.as_deref(),
                    self.ollama.selected_identity.as_ref(),
                )
            })
            .collect();
        OllamaSetupSnapshot {
            state,
            detail,
            version,
            operation: self.ollama.operation,
            operation_detail: self.ollama.operation_detail.clone(),
            progress_percent: self.ollama.progress_percent,
            can_inspect: self.active
                && !self.ollama.inspect_in_flight
                && !matches!(
                    self.ollama.operation,
                    OllamaOperationState::Pulling
                        | OllamaOperationState::Cancelling
                        | OllamaOperationState::Activating
                ),
            controls_enabled: self.active
                && self.ollama.operation == OllamaOperationState::Idle
                && !self.performance_busy(),
            models,
        }
    }

    fn performance_snapshot(&self) -> PerformanceSetupSnapshot {
        let candidates = self
            .performance
            .inventory
            .as_ref()
            .map_or_else(Vec::new, |inventory| {
                inventory
                    .candidates_for(self.performance.language)
                    .iter()
                    .map(|candidate| BenchmarkCandidateView {
                        id: candidate.candidate_id().as_str().to_owned(),
                        title: candidate_title(
                            candidate.engine(),
                            candidate.backend(),
                            candidate.model_class(),
                        ),
                        detail: format!(
                            "{} · {} · exact pinned identity",
                            engine_label(candidate.engine()),
                            backend_label(candidate.backend())
                        ),
                        selected: self.performance.selected_candidate.as_ref()
                            == Some(candidate.candidate_id()),
                    })
                    .collect()
            });
        let unavailable = self
            .performance
            .inventory
            .as_ref()
            .map_or_else(Vec::new, |inventory| {
                inventory
                    .unavailable
                    .iter()
                    .filter(|item| item.language == self.performance.language)
                    .map(|item| BenchmarkUnavailableView {
                        title: format!(
                            "{} · {}",
                            engine_label(item.engine),
                            backend_label(item.backend)
                        ),
                        reason: unavailable_reason(item.reason).to_owned(),
                    })
                    .collect()
            });
        let prompts = self
            .performance
            .calibration
            .prompts()
            .iter()
            .enumerate()
            .map(|(index, prompt)| CalibrationPromptView {
                id: prompt.case_id.as_str().to_owned(),
                ordinal: index + 1,
                kind: match prompt.kind {
                    CalibrationKind::Speech => "Speech",
                    CalibrationKind::Silence => "Silence",
                }
                .to_owned(),
                text: prompt.text().to_owned(),
                capture: self
                    .performance
                    .capture_states
                    .get(&prompt.case_id)
                    .copied()
                    .unwrap_or_default(),
                detail: self
                    .performance
                    .capture_details
                    .get(&prompt.case_id)
                    .cloned(),
            })
            .collect();
        let evidence = self
            .performance
            .evidence
            .iter()
            .filter(|item| item.measured_language == self.performance.language)
            .map(evidence_view)
            .collect();
        PerformanceSetupSnapshot {
            language: language_label(self.performance.language).to_owned(),
            preference: self.performance.preference,
            state: self.performance.run_state,
            detail: self.performance.run_detail.clone(),
            progress_completed: self.performance.progress.0,
            progress_total: self.performance.progress.1,
            controls_enabled: self.active
                && !self.performance_busy()
                && self.performance.applied.is_none(),
            can_start_benchmark: self.active
                && !self.performance_busy()
                && self.performance.applied.is_none()
                && self.performance.calibration.is_ready()
                && self.performance.selected_candidate.is_some(),
            can_cancel_benchmark: self.active
                && (self.performance.benchmark.is_some() || self.performance.benchmark_starting)
                && !self.performance.benchmark_cancel_pending,
            can_reset_calibration: self.active
                && !self.performance_busy()
                && self.performance.applied.is_none(),
            rollback_state: if self.performance.mutation_in_flight {
                PerformanceRollbackState::Working
            } else if self.performance.rollback_probe_in_flight {
                PerformanceRollbackState::Inspecting
            } else {
                match self.performance.rollback_state {
                    PersistedRollbackState::None => PerformanceRollbackState::None,
                    PersistedRollbackState::Ready => PerformanceRollbackState::Ready,
                    PersistedRollbackState::StaleOrCorrupt => {
                        PerformanceRollbackState::StaleOrCorrupt
                    }
                    PersistedRollbackState::Unavailable => PerformanceRollbackState::Unavailable,
                }
            },
            rollback_detail: self.performance.rollback_detail.clone(),
            can_revert_after_restart: self.active
                && !self.performance_busy()
                && self.performance.rollback_state == PersistedRollbackState::Ready,
            can_discard_rollback: self.active
                && !self.performance_busy()
                && matches!(
                    self.performance.rollback_state,
                    PersistedRollbackState::Ready | PersistedRollbackState::StaleOrCorrupt
                ),
            candidates,
            unavailable,
            prompts,
            evidence,
            recommendation: self
                .performance
                .recommendation
                .as_ref()
                .map(|recommendation| {
                    recommendation_view(
                        recommendation,
                        self.performance.apply_consent.is_some()
                            && !self.performance.mutation_in_flight
                            && self.performance.rollback_state == PersistedRollbackState::None,
                        self.performance.applied.is_some() && !self.performance.mutation_in_flight,
                    )
                }),
        }
    }
}

impl Drop for SetupFeatures {
    fn drop(&mut self) {
        self.deactivate();
    }
}

fn drop_recording_and_lease_async(recording: ActiveRecording, activity: RuntimeActivityLease) {
    let _ = thread::Builder::new()
        .name("phorminx-calibration-discard".to_owned())
        .spawn(move || drop((recording, activity)));
}

fn drop_capture_async(active: ActiveCapture) {
    drop_recording_and_lease_async(active.recording, active._activity);
}

fn settings_language(settings: &Settings) -> Language {
    if settings.recognition.language.eq_ignore_ascii_case("pt-br") {
        Language::PortugueseBrazil
    } else {
        Language::English
    }
}

fn empty_capture_states(
    audio: &TransientCalibrationAudio,
) -> BTreeMap<ContentFreeId, CalibrationCaptureState> {
    audio
        .prompts()
        .iter()
        .map(|prompt| (prompt.case_id.clone(), CalibrationCaptureState::Empty))
        .collect()
}

fn curated_id(value: &str) -> Option<CuratedModelId> {
    match value {
        "gemma3-1b" => Some(CuratedModelId::Gemma3OneB),
        "llama32-1b" => Some(CuratedModelId::Llama32OneB),
        "qwen25-3b-instruct" => Some(CuratedModelId::Qwen25ThreeBInstruct),
        _ => None,
    }
}

const fn curated_id_text(value: CuratedModelId) -> &'static str {
    match value {
        CuratedModelId::Gemma3OneB => "gemma3-1b",
        CuratedModelId::Llama32OneB => "llama32-1b",
        CuratedModelId::Qwen25ThreeBInstruct => "qwen25-3b-instruct",
    }
}

fn exact_installed(model: CuratedModel, installed: &[InstalledModelIdentity]) -> bool {
    installed.iter().any(|found| {
        (found.name == model.local_name() || found.name == model.fully_qualified_name())
            && found
                .manifest_sha256
                .eq_ignore_ascii_case(model.manifest_sha256())
            && found.bytes == model.manifest_bytes()
    })
}

fn identity_mismatch(model: CuratedModel, installed: &[InstalledModelIdentity]) -> bool {
    installed.iter().any(|found| {
        (found.name == model.local_name() || found.name == model.fully_qualified_name())
            && (found.bytes != model.manifest_bytes()
                || !found
                    .manifest_sha256
                    .eq_ignore_ascii_case(model.manifest_sha256()))
    })
}

fn model_view(
    model: CuratedModel,
    installed: &[InstalledModelIdentity],
    selected: Option<&str>,
    selected_identity: Option<&OllamaModelIdentity>,
) -> OllamaModelChoice {
    OllamaModelChoice {
        id: curated_id_text(model.id()).to_owned(),
        display_name: model.display_name().to_owned(),
        exact_name: model.local_name().to_owned(),
        summary: model.summary().to_owned(),
        languages: model
            .languages()
            .iter()
            .map(|language| match language {
                CuratedLanguage::English => "EN",
                CuratedLanguage::PortugueseBrazil => "PT-BR",
            })
            .collect::<Vec<_>>()
            .join(" · "),
        model_bytes: model.manifest_bytes(),
        minimum_free_disk_bytes: model.minimum_free_disk_bytes(),
        recommended_ram_bytes: model.recommended_system_ram_bytes(),
        installed: exact_installed(model, installed),
        identity_matches: !identity_mismatch(model, installed),
        selected: selected
            .is_some_and(|name| name == model.local_name() || name == model.fully_qualified_name())
            && selected_identity.is_some_and(|identity| {
                identity.matches(Some(model.manifest_sha256()), Some(model.manifest_bytes()))
            }),
    }
}

fn map_ollama_state(
    state: Option<&OllamaHostState>,
) -> (
    OllamaSetupState,
    String,
    Option<String>,
    Vec<InstalledModelIdentity>,
) {
    match state {
        None => (OllamaSetupState::Inspecting, "Inspecting the documented per-user installation and fixed loopback API.".into(), None, Vec::new()),
        Some(OllamaHostState::InstallationInspectionFailed) => (OllamaSetupState::Unhealthy, "Windows could not prove the installation boundary.".into(), None, Vec::new()),
        Some(OllamaHostState::UnsafeInstallation) => (OllamaSetupState::Unsafe, "The documented executable path contains an unsafe file or reparse boundary. Phorminx will not execute it.".into(), None, Vec::new()),
        Some(OllamaHostState::Daemon(DaemonState::NotInstalled)) => (OllamaSetupState::Missing, "Ollama is not installed at its documented per-user location.".into(), None, Vec::new()),
        Some(OllamaHostState::Daemon(DaemonState::InstalledButStopped)) => (OllamaSetupState::Stopped, "Ollama is installed but its fixed loopback API is not responding. Start it manually.".into(), None, Vec::new()),
        Some(OllamaHostState::Daemon(DaemonState::Incompatible { found, minimum })) => (OllamaSetupState::Incompatible, format!("Ollama {found} is too old; Phorminx requires {minimum} or newer."), Some(found.to_string()), Vec::new()),
        Some(OllamaHostState::Daemon(DaemonState::Unhealthy)) => (OllamaSetupState::Unhealthy, "The fixed loopback API answered with an invalid or unhealthy response.".into(), None, Vec::new()),
        Some(OllamaHostState::Daemon(DaemonState::Ready { version, models })) => (OllamaSetupState::Ready, format!("Ollama {version} is healthy on the fixed loopback API."), Some(version.to_string()), models.clone()),
    }
}

fn pull_percent(progress: &ModelPullProgress) -> Option<u8> {
    let (Some(done), Some(total)) = (progress.completed_bytes, progress.total_bytes) else {
        return None;
    };
    if total == 0 {
        return None;
    }
    Some(
        u8::try_from(
            done.saturating_mul(100)
                .checked_div(total)
                .unwrap_or(0)
                .min(100),
        )
        .unwrap_or(100),
    )
}

const fn pull_phase_label(phase: PullPhase) -> &'static str {
    match phase {
        PullPhase::ResolvingManifest => "Resolving the exact pinned manifest.",
        PullPhase::Downloading => "Downloading model layers through the local Ollama service.",
        PullPhase::Verifying => "Verifying byte count and manifest digest.",
        PullPhase::Activating => "Confirming the installed identity.",
    }
}

fn pull_failure_label(error: &PullFailure) -> &'static str {
    match error.kind {
        PullFailureKind::Busy => {
            return "Acquisition was refused because dictation, calibration, or a benchmark owns the local compute lane.";
        }
        PullFailureKind::CapacityUnavailable => {
            return "Windows could not prove free space on Ollama's model volume. Nothing was downloaded.";
        }
        PullFailureKind::InsufficientDisk => {
            return "The model volume does not have the disclosed minimum free space. Nothing was downloaded.";
        }
        PullFailureKind::InstallationUntrusted => {
            return "The documented Ollama installation boundary could not be re-verified. No request was sent.";
        }
        PullFailureKind::DaemonUnavailable => {
            return "The fixed loopback Ollama service became unavailable before acquisition completed.";
        }
        PullFailureKind::DaemonIncompatible => {
            return "The local Ollama version no longer satisfies this model's pinned minimum.";
        }
        PullFailureKind::ExistingIdentityMismatch => {
            return "A mutable tag with this name now has a different identity. It was preserved and cannot be selected.";
        }
        PullFailureKind::PostPullIdentityMismatch | PullFailureKind::RollbackFailed => {}
        PullFailureKind::Cancelled
        | PullFailureKind::Transport
        | PullFailureKind::ProtocolViolation => {}
    }
    match error.residue {
        PullResidue::None => {
            "Acquisition failed before any model residue was observed. Inspect and retry."
        }
        PullResidue::ResumableCacheMayRemain => {
            "Acquisition stopped; Ollama may retain a resumable cache. Inspect before retrying."
        }
        PullResidue::ModelMayRemain => {
            "The result is uncertain and a model may remain. Inspect to reconcile exact identity before selection."
        }
    }
}

const fn map_preference(value: PerformancePreference) -> RecommendationPreference {
    match value {
        PerformancePreference::Fastest => RecommendationPreference::Fastest,
        PerformancePreference::Balanced => RecommendationPreference::Balanced,
        PerformancePreference::Quality => RecommendationPreference::Fidelity,
    }
}

fn candidate_title(engine: EngineKind, backend: BackendKind, class: ModelClass) -> String {
    format!(
        "{} {} on {}",
        model_label(class),
        engine_label(engine),
        backend_label(backend)
    )
}
const fn engine_label(value: EngineKind) -> &'static str {
    match value {
        EngineKind::Accurate => "Accurate",
        EngineKind::Instant => "Instant",
    }
}
const fn backend_label(value: BackendKind) -> &'static str {
    match value {
        BackendKind::Cpu => "CPU",
        BackendKind::Vulkan => "Vulkan",
        BackendKind::VoskNative => "Vosk native",
    }
}
const fn model_label(value: ModelClass) -> &'static str {
    match value {
        ModelClass::Tiny => "Tiny",
        ModelClass::Base => "Base",
        ModelClass::Other => "Local",
    }
}
const fn language_label(value: Language) -> &'static str {
    match value {
        Language::English => "English",
        Language::PortugueseBrazil => "Português (Brasil)",
    }
}

const fn unavailable_reason(reason: CandidateUnavailableReason) -> &'static str {
    match reason {
        CandidateUnavailableReason::Catalog => "Pinned catalog unavailable",
        CandidateUnavailableReason::ManagedInventory => "Verified model is not installed",
        CandidateUnavailableReason::AssetIdentity => {
            "Installed bytes do not match the pinned identity"
        }
        CandidateUnavailableReason::RuntimeUnavailable => "Required native runtime is unavailable",
        CandidateUnavailableReason::UnsupportedLanguage => {
            "Candidate does not support this language"
        }
        CandidateUnavailableReason::BackendUnavailable => {
            "Backend is not available on this machine"
        }
        CandidateUnavailableReason::HostIdentity => "Device or driver identity could not be proven",
    }
}

fn evidence_view(value: &BenchmarkEvidence) -> BenchmarkEvidenceView {
    BenchmarkEvidenceView {
        candidate_id: value.candidate_id.as_str().to_owned(),
        title: candidate_title(value.engine, value.backend, value.model_class),
        release_p50_ms: value.measurements.release_p50_ms,
        release_p95_ms: value.measurements.release_p95_ms,
        realtime_factor_milli: value.measurements.realtime_factor_milli,
        word_error_per_mille: value.measurements.word_error_per_mille,
        hallucination_per_mille: value.measurements.hallucination_per_mille,
        protected_token_exact_per_mille: value.measurements.protected_token_exact_per_mille,
        peak_working_set_mib: value.measurements.peak_working_set_mib,
        available_memory_mib: value.measurements.available_memory_mib,
    }
}

fn recommendation_view(
    value: &Recommendation,
    can_apply: bool,
    can_revert: bool,
) -> PerformanceRecommendationView {
    let (id, title, rationale) = match &value.outcome {
        RecommendationOutcome::Recommended { candidate_id, engine, backend, model_class } => (
            candidate_id.as_str().to_owned(),
            candidate_title(*engine, *backend, *model_class),
            "This exact verified candidate passed the pinned quality, latency, memory, and confidence gates.".to_owned(),
        ),
        RecommendationOutcome::ConservativeFallback { language, .. } => (
            "conservative-fallback".into(),
            "Keep the conservative Accurate configuration".into(),
            format!("Evidence is not yet sufficient for a measured switch in {}.", language_label(*language)),
        ),
        RecommendationOutcome::Unavailable => (
            "unavailable".into(),
            "No safe recommendation yet".into(),
            "Every measured candidate was excluded by at least one pinned safety or quality gate.".into(),
        ),
    };
    let excluded = value
        .rejected_candidates
        .iter()
        .map(|candidate| {
            let reasons = candidate
                .reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} · {reasons}", candidate.candidate_id)
        })
        .collect();
    PerformanceRecommendationView {
        id,
        title,
        rationale,
        excluded,
        can_apply,
        can_revert,
    }
}

fn preflight_detail(message: &str) -> &'static str {
    if message.contains("dictation") {
        "Benchmark refused because dictation is active. Stop dictation and retry with the same transient calibration set."
    } else if message.contains("compute") {
        "Benchmark refused because Whisper, Ollama, or another benchmark owns the compute lane."
    } else if message.contains("thermal") {
        "Benchmark refused because the system is thermally unstable."
    } else if message.contains("load") {
        "Benchmark refused because system load or free memory would make the result unreliable."
    } else {
        "Benchmark preflight could not obtain trustworthy live machine state."
    }
}

fn apply_failure_detail(message: &str) -> &'static str {
    if message.contains("changed") || message.contains("stale") {
        "Settings changed concurrently, so Phorminx preserved the newer values."
    } else if message.contains("asset") {
        "The measured asset no longer matches. Nothing was changed."
    } else {
        "The recommendation could not be committed atomically. Existing settings were preserved."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inactive_features() -> (tempfile::TempDir, SetupFeatures) {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let features = SetupFeatures::open(store, &Settings::default(), false);
        (directory, features)
    }

    #[test]
    fn user_text_cannot_mint_model_authority() {
        assert_eq!(curated_id("gemma3-1b"), Some(CuratedModelId::Gemma3OneB));
        for hostile in ["gemma3:1b", "../../model", "qwen; calc", ""] {
            assert_eq!(curated_id(hostile), None);
        }
    }

    #[test]
    fn model_cards_disclose_resources_and_bilingual_support() {
        for model in CuratedModelCatalog.all() {
            let view = model_view(*model, &[], None, None);
            assert!(view.minimum_free_disk_bytes > view.model_bytes);
            assert!(view.recommended_ram_bytes > view.model_bytes);
            assert_eq!(view.languages, "EN · PT-BR");
            assert!(!view.installed);
        }
    }

    #[test]
    fn exact_identity_is_required_before_selection() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let mismatch = InstalledModelIdentity {
            name: model.local_name().to_owned(),
            manifest_sha256: "0".repeat(64),
            bytes: model.manifest_bytes(),
        };
        let stored = OllamaModelIdentity::new("0".repeat(64), model.manifest_bytes()).unwrap();
        let view = model_view(model, &[mismatch], Some(model.local_name()), Some(&stored));
        assert!(!view.installed);
        assert!(!view.identity_matches);
        assert!(!view.selected);

        let exact = InstalledModelIdentity {
            name: model.local_name().to_owned(),
            manifest_sha256: model.manifest_sha256().to_owned(),
            bytes: model.manifest_bytes(),
        };
        let stored =
            OllamaModelIdentity::new(model.manifest_sha256(), model.manifest_bytes()).unwrap();
        let view = model_view(model, &[exact], Some(model.local_name()), Some(&stored));
        assert!(view.selected);
    }

    #[test]
    fn progress_is_bounded_even_for_hostile_totals() {
        let progress = ModelPullProgress {
            phase: PullPhase::Downloading,
            completed_bytes: Some(u64::MAX),
            total_bytes: Some(1),
        };
        assert_eq!(pull_percent(&progress), Some(100));
    }

    #[test]
    fn mailbox_terminal_supersedes_progress_and_cannot_be_overwritten_by_late_progress() {
        let mailbox = EventMailbox::default();
        for completed in 0..1_000 {
            mailbox.push_progress(
                7,
                ModelPullProgress {
                    phase: PullPhase::Downloading,
                    completed_bytes: Some(completed),
                    total_bytes: Some(1_000),
                },
            );
        }
        mailbox.push_terminal(FeatureEvent::OfficialPageOpened {
            generation: 7,
            succeeded: true,
        });
        mailbox.push_progress(
            7,
            ModelPullProgress {
                phase: PullPhase::Downloading,
                completed_bytes: Some(1_000),
                total_bytes: Some(1_000),
            },
        );
        let events = mailbox.drain();
        assert_eq!(events.len(), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            FeatureEvent::OfficialPageOpened {
                generation: 7,
                succeeded: true
            }
        )));
    }

    #[test]
    fn mailbox_rejects_an_older_terminal_after_a_newer_one() {
        let mailbox = EventMailbox::default();
        mailbox.push_terminal(FeatureEvent::OfficialPageOpened {
            generation: 9,
            succeeded: true,
        });
        mailbox.push_terminal(FeatureEvent::OllamaAuthorizationFailed { generation: 8 });
        assert!(matches!(
            mailbox.drain().as_slice(),
            [FeatureEvent::OfficialPageOpened {
                generation: 9,
                succeeded: true
            }]
        ));
    }

    #[test]
    fn stale_completion_cannot_overwrite_a_newer_ollama_generation() {
        let (_directory, mut features) = inactive_features();
        features.ollama.generation = 4;
        features.ollama.operation = OllamaOperationState::Pulling;
        features.handle_event(FeatureEvent::OllamaAuthorizationFailed { generation: 3 });
        assert_eq!(features.ollama.operation, OllamaOperationState::Pulling);
    }

    #[test]
    fn authorization_failure_is_always_terminal_for_the_current_generation() {
        let (_directory, mut features) = inactive_features();
        features.ollama.generation = 4;
        features.ollama.operation = OllamaOperationState::Pulling;
        features.handle_event(FeatureEvent::OllamaAuthorizationFailed { generation: 4 });
        assert_eq!(features.ollama.operation, OllamaOperationState::Failed);
        assert!(features.ollama.operation_detail.is_some());
    }

    #[test]
    fn navigation_cancellation_preserves_pull_generation_until_reconciliation() {
        let (_directory, mut features) = inactive_features();
        features.active = true;
        features.ollama.generation = 11;
        features.ollama.operation = OllamaOperationState::Pulling;
        features.ollama.cancel = Some(CancellationToken::new());
        features.deactivate();
        assert_eq!(features.ollama.generation, 11);
        assert_eq!(features.ollama.operation, OllamaOperationState::Cancelling);
        assert!(features.ollama.cancel.as_ref().unwrap().is_cancelled());

        features.handle_event(FeatureEvent::OllamaPullFinished {
            generation: 11,
            result: Err(PullFailure {
                kind: PullFailureKind::Cancelled,
                residue: PullResidue::ModelMayRemain,
            }),
        });
        assert_eq!(
            features.ollama.operation,
            OllamaOperationState::ReconcileRequired
        );
    }

    #[test]
    fn successful_run_boundary_drops_the_calibration_bank_before_claiming_consumed() {
        let (_directory, mut features) = inactive_features();
        for prompt in features.performance.calibration.prompts() {
            features
                .performance
                .calibration
                .submit(
                    &prompt.case_id,
                    phorminx_core::AudioClip::new(vec![0.1; 3_200], 16_000).unwrap(),
                )
                .unwrap();
        }
        assert!(features.performance.calibration.is_ready());
        features.consume_calibration_views();
        assert!(!features.performance.calibration.is_ready());
        assert!(
            features
                .performance
                .capture_states
                .values()
                .all(|state| *state == CalibrationCaptureState::Consumed)
        );
    }

    #[test]
    fn navigation_never_releases_discovery_worker_ownership_early() {
        let (_directory, mut features) = inactive_features();
        features.active = true;
        features.performance.discovery_generation = 7;
        features.performance.discovery_in_flight = true;
        features.deactivate();
        assert!(features.performance.discovery_in_flight);
        assert_eq!(features.performance.discovery_generation, 7);

        features.active = true;
        features.discover_performance();
        assert!(features.performance.discovery_pending);
        assert_eq!(features.performance.discovery_generation, 7);
    }

    #[test]
    fn repeated_inspect_intent_queues_one_refresh_without_spawning_another_probe() {
        let (_directory, mut features) = inactive_features();
        features.active = true;
        features.ollama.generation = 5;
        features.ollama.inspect_in_flight = true;
        features.inspect_ollama();
        assert!(features.ollama.inspect_pending);
        assert_eq!(features.ollama.generation, 5);
    }

    #[test]
    fn legacy_setup_mutation_disables_restart_revert_and_discard() {
        let (_directory, mut features) = inactive_features();
        features.active = true;
        features.performance.rollback_state = PersistedRollbackState::Ready;
        let mut snapshot = SetupSnapshot {
            stage: phorminx_ui::SetupStage::Working,
            ..SetupSnapshot::default()
        };
        features.enrich(&mut snapshot);
        assert!(!snapshot.performance.can_revert_after_restart);
        assert!(!snapshot.performance.can_discard_rollback);
    }
}
