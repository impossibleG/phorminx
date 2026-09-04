//! Process-local orchestration for the Setup and Repair Center.
//!
//! The controller is the only bridge from UI action identifiers to opaque
//! `AuthorizedAction` values. UI strings, paths, URLs, digests, and commands
//! never cross this boundary as execution authority.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::Duration;

use phorminx_ollama::{CancellationToken, ClientTimeouts, OllamaClient, OllamaEndpoint};
use phorminx_setup::{
    ActionFailure, ActionKey, ActionPhase, ActionState, CapabilityId, CapabilityRecord,
    ConsentCategory, DesiredConfiguration, EngineKind, FormattingChoice, Language,
    RecognitionChoice, Sha256Digest,
};
use phorminx_ui::{Readiness, SetupAction, SetupCapability, SetupSnapshot, SetupStage};
use phorminx_windows::{
    LaunchAtLoginState, SetupOperationLock, launch_at_login_state, set_launch_at_login,
};

use crate::model::{identify_pinned_model, model_for_variant};
use crate::settings::{FormattingStrength, RecognitionMode, Settings, SettingsStore};
use crate::setup_host::{
    ActionAdapters, ActionEvent, AuthorizedAction, HostActionError, HttpsFetcher, ManagedInstall,
    ManagedRoot, NormalizedProbeFact, RecognitionProbeState, SetupAuthority, SetupExecutor,
};
use crate::ui_bridge::{UiReadinessSnapshot, UiReadinessState};

/// The performance feature injects an implementation after its own authority
/// has validated a versioned protocol. Setup does not invent benchmark work.
pub trait BenchmarkAdapter: Send + Sync + 'static {
    fn run(&self, protocol: &str, cancel: &AtomicBool) -> Result<(), HostActionError>;
}

#[derive(Default)]
pub struct BenchmarkUnavailable;

impl BenchmarkAdapter for BenchmarkUnavailable {
    fn run(&self, _protocol: &str, _cancel: &AtomicBool) -> Result<(), HostActionError> {
        Err(HostActionError::new(ActionFailure::BenchmarkFailed, false))
    }
}

/// Optional setup failure. The product shell remains usable when setup cannot
/// initialize, and exposes a content-free repair status instead.
pub struct SetupCenter {
    authority: SetupAuthority,
    root: ManagedRoot,
    executor: SetupExecutor,
    authorized: BTreeMap<String, AuthorizedAction>,
    presentations: Vec<crate::setup_host::ActionPresentation>,
    active: Option<ActionEvent>,
    initialization_notice: Option<&'static str>,
    planning_notice: Option<&'static str>,
    plan_generation: u64,
    plan_in_flight: bool,
    plan_worker_active: bool,
    pending_plan: Option<PlanRequest>,
    plan_tx: SyncSender<PlanResult>,
    plan_rx: Receiver<PlanResult>,
    storage_recovery_in_flight: bool,
    storage_recovery_tx: SyncSender<StorageRecoveryResult>,
    storage_recovery_rx: Receiver<StorageRecoveryResult>,
}

struct PlanRequest {
    generation: u64,
    settings: Settings,
    readiness: UiReadinessSnapshot,
}

struct PlanResult {
    generation: u64,
    result: Result<PreparedPlan, &'static str>,
}

struct PreparedPlan {
    authorized: BTreeMap<String, AuthorizedAction>,
    presentations: Vec<crate::setup_host::ActionPresentation>,
}

enum StorageRecoveryResult {
    Recovered,
    ManualReview,
    Failed,
}

impl SetupCenter {
    pub fn open(
        store: SettingsStore,
        benchmark: Arc<dyn BenchmarkAdapter>,
    ) -> Result<Self, &'static str> {
        let authority = SetupAuthority::phorminx().map_err(|_| "Setup catalog unavailable.")?;
        let root = ManagedRoot::from_local_app_data().map_err(|_| "Setup storage unavailable.")?;
        let initialization_notice = match SetupOperationLock::try_acquire() {
            Ok(_lock) => match root.recover() {
                Ok(report) if report.rejected_journals > 0 => {
                    Some("A previous setup operation needs manual review.")
                }
                Ok(_) => None,
                Err(_) => Some(
                    "Setup recovery could not finish; existing runtime settings were preserved.",
                ),
            },
            Err(_) => {
                Some("Another Phorminx process is changing setup. Inspect again when it finishes.")
            }
        };
        let adapters = Arc::new(ProductionAdapters {
            store,
            root: root.clone(),
            benchmark,
            compensation: Mutex::new(BTreeMap::new()),
        });
        let (plan_tx, plan_rx) = mpsc::sync_channel(1);
        let (storage_recovery_tx, storage_recovery_rx) = mpsc::sync_channel(1);
        Ok(Self {
            authority,
            root: root.clone(),
            executor: SetupExecutor::new(root, adapters, Arc::new(HttpsFetcher)),
            authorized: BTreeMap::new(),
            presentations: Vec::new(),
            active: None,
            initialization_notice,
            planning_notice: None,
            plan_generation: 0,
            plan_in_flight: false,
            plan_worker_active: false,
            pending_plan: None,
            plan_tx,
            plan_rx,
            storage_recovery_in_flight: false,
            storage_recovery_tx,
            storage_recovery_rx,
        })
    }

    /// Retries only marker-verified recovery. It never deletes or adopts an
    /// unowned path. A successful retry re-enables fresh planning.
    pub fn retry_recovery(&mut self) -> Result<(), &'static str> {
        if self.initialization_notice.is_none() || self.storage_recovery_in_flight {
            return Ok(());
        }
        if self.plan_worker_active
            || self.executor.recovery_in_progress()
            || self
                .active
                .as_ref()
                .is_some_and(|event| !event.state.is_terminal())
        {
            return Err("Wait for the active setup operation before retrying recovery.");
        }
        let root = self.root.clone();
        let sender = self.storage_recovery_tx.clone();
        self.storage_recovery_in_flight = true;
        thread::Builder::new()
            .name("phorminx-setup-storage-recovery".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _lock = SetupOperationLock::try_acquire().map_err(|_| ())?;
                    root.recover().map_err(|_| ())
                }));
                let result = match result {
                    Ok(Ok(report)) if report.rejected_journals == 0 => {
                        StorageRecoveryResult::Recovered
                    }
                    Ok(Ok(_)) => StorageRecoveryResult::ManualReview,
                    Ok(Err(_)) | Err(_) => StorageRecoveryResult::Failed,
                };
                let _ = sender.send(result);
            })
            .map_err(|_| {
                self.storage_recovery_in_flight = false;
                "Setup recovery could not start."
            })?;
        Ok(())
    }

    pub fn replan(
        &mut self,
        settings: &Settings,
        readiness: &UiReadinessSnapshot,
    ) -> Result<(), &'static str> {
        if self
            .active
            .as_ref()
            .is_some_and(|event| !event.state.is_terminal())
        {
            return Ok(());
        }
        self.plan_generation = self.plan_generation.wrapping_add(1);
        let generation = self.plan_generation;
        self.plan_in_flight = true;
        self.planning_notice = None;
        // A plan is authority, so it becomes unusable before a newer probe is
        // dispatched rather than when that probe eventually returns.
        self.presentations.clear();
        self.authorized.clear();
        let request = PlanRequest {
            generation,
            settings: settings.clone(),
            readiness: readiness.clone(),
        };
        if self.plan_worker_active || self.storage_recovery_in_flight {
            // Only the newest observation matters. Keep one bounded pending
            // request instead of launching concurrent multi-gigabyte hashes.
            self.pending_plan = Some(request);
            return Ok(());
        }
        self.spawn_plan(request)
    }

    fn spawn_plan(&mut self, request: PlanRequest) -> Result<(), &'static str> {
        let authority = self.authority.clone();
        let root = self.root.clone();
        let sender = self.plan_tx.clone();
        self.plan_worker_active = true;
        thread::Builder::new()
            .name("phorminx-setup-plan".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepare_plan(&authority, &root, &request.settings, &request.readiness)
                }))
                .unwrap_or(Err("Local setup inspection failed safely."));
                let _ = sender.send(PlanResult {
                    generation: request.generation,
                    result,
                });
            })
            .map_err(|_| {
                self.plan_worker_active = false;
                self.plan_in_flight = false;
                self.planning_notice = Some("Local setup inspection could not start.");
                "Local setup inspection could not start."
            })?;
        Ok(())
    }

    pub fn start(&mut self, id: &str) -> Result<(), &'static str> {
        if self.initialization_notice.is_some() {
            return Err("Setup mutations are blocked until local recovery is repaired.");
        }
        if self
            .active
            .as_ref()
            .is_some_and(|event| event.action_id.as_str() == id && recovery_required(&event.state))
        {
            self.executor
                .retry_recovery(id)
                .map_err(|_| "The unfinished setup recovery could not restart.")?;
            return Ok(());
        }
        let action = self
            .authorized
            .get(id)
            .cloned()
            .ok_or("That setup action is no longer current. Inspect again.")?;
        if !self
            .executor
            .dependencies_satisfied(&action)
            .map_err(|_| "Setup prerequisite state is unavailable.")?
        {
            return Err("Complete the earlier setup steps first.");
        }
        let granted = action.required_consent().clone();
        self.executor
            .start(action, granted)
            .map_err(|_| "The setup action could not start.")?;
        self.authorized.remove(id);
        self.active = self
            .executor
            .current_state()
            .map_err(|_| "Setup status became unavailable.")?;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), &'static str> {
        if self.active.as_ref().map(|event| event.action_id.as_str()) != Some(id) {
            return Err("That setup action is no longer active.");
        }
        self.executor
            .cancel()
            .map_err(|_| "The setup action could not be cancelled.")?;
        self.active = self
            .executor
            .current_state()
            .map_err(|_| "Setup status became unavailable.")?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<bool, &'static str> {
        let mut changed = match self.executor.try_event() {
            Ok(Some(event)) => {
                let changed = self.active.as_ref() != Some(&event);
                self.active = Some(event);
                changed
            }
            Ok(None) => false,
            Err(_) => return Err("Setup status became unavailable."),
        };
        match self.storage_recovery_rx.try_recv() {
            Ok(StorageRecoveryResult::Recovered) => {
                self.storage_recovery_in_flight = false;
                self.initialization_notice = None;
                changed = true;
            }
            Ok(StorageRecoveryResult::ManualReview) => {
                self.storage_recovery_in_flight = false;
                self.initialization_notice =
                    Some("A previous setup operation needs manual review.");
                changed = true;
            }
            Ok(StorageRecoveryResult::Failed) => {
                self.storage_recovery_in_flight = false;
                self.initialization_notice = Some(
                    "Setup recovery could not finish; existing runtime settings were preserved.",
                );
                changed = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.storage_recovery_in_flight = false;
                self.initialization_notice = Some("Setup recovery became unavailable.");
                changed = true;
            }
        }
        if !self.storage_recovery_in_flight
            && !self.plan_worker_active
            && let Some(request) = self.pending_plan.take()
        {
            let _ = self.spawn_plan(request);
        }
        loop {
            match self.plan_rx.try_recv() {
                Ok(result) => {
                    self.plan_worker_active = false;
                    changed = true;
                    if let Some(request) = self.pending_plan.take() {
                        let _ = self.spawn_plan(request);
                        continue;
                    }
                    self.plan_in_flight = false;
                    if result.generation != self.plan_generation {
                        continue;
                    }
                    match result.result {
                        Ok(plan) => {
                            self.planning_notice = None;
                            self.authorized = plan.authorized;
                            self.presentations = plan.presentations;
                            if self.active.as_ref().is_some_and(|event| {
                                event.state.is_terminal()
                                    && !self
                                        .presentations
                                        .iter()
                                        .any(|item| item.id == event.action_id)
                            }) {
                                self.active = None;
                            }
                        }
                        Err(message) => {
                            self.authorized.clear();
                            self.presentations.clear();
                            self.planning_notice = Some(message);
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.plan_worker_active = false;
                    self.pending_plan = None;
                    self.plan_in_flight = false;
                    self.planning_notice = Some("Local setup inspection became unavailable.");
                    changed = true;
                    break;
                }
            }
        }
        Ok(changed)
    }

    #[must_use]
    pub fn terminal(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|event| event.state.is_terminal())
    }

    #[must_use]
    pub fn snapshot(&self, readiness: &UiReadinessSnapshot) -> SetupSnapshot {
        let discovering = [
            readiness.microphone.state,
            readiness.whisper.state,
            readiness.vosk.state,
            readiness.ollama.state,
        ]
        .contains(&UiReadinessState::Checking)
            || self.plan_in_flight
            || self.storage_recovery_in_flight;
        let core_ready = readiness.microphone.state == UiReadinessState::Ready
            && (readiness.whisper.state == UiReadinessState::Ready
                || readiness.vosk.state == UiReadinessState::Ready);
        let recovery_needed = self
            .active
            .as_ref()
            .is_some_and(|event| recovery_required(&event.state));
        let busy = self.executor.recovery_in_progress()
            || self.active.as_ref().is_some_and(|event| {
                !event.state.is_terminal() && !recovery_required(&event.state)
            });
        let failed = self.active.as_ref().is_some_and(|event| {
            matches!(
                event.state,
                ActionState::Failed { .. }
                    | ActionState::FailedExternalSideEffectsMayRemain { .. }
                    | ActionState::RollbackBlocked { .. }
            )
        });
        let stage = if discovering && !busy {
            SetupStage::Discovering
        } else if busy {
            match self.active.as_ref().map(|event| &event.state) {
                Some(ActionState::AwaitingConsent { .. }) => SetupStage::AwaitingConsent,
                Some(ActionState::Running { progress })
                    if progress.phase == ActionPhase::Benchmarking =>
                {
                    SetupStage::Benchmarking
                }
                _ => SetupStage::Working,
            }
        } else if failed
            || recovery_needed
            || self.initialization_notice.is_some()
            || self.planning_notice.is_some()
        {
            SetupStage::Blocked
        } else if !self.presentations.is_empty() {
            SetupStage::PlanReady
        } else if core_ready {
            SetupStage::Ready
        } else {
            SetupStage::Blocked
        };
        let summary = self
            .initialization_notice
            .or(self.planning_notice)
            .map_or_else(
                || {
                    match stage {
                SetupStage::PlanReady => {
                    "A verified local repair plan is ready. Each side effect requires review."
                        .to_owned()
                }
                SetupStage::Working | SetupStage::Benchmarking => {
                    "One local setup operation is in progress.".to_owned()
                }
                SetupStage::AwaitingConsent => {
                    "Review the exact side effects before continuing.".to_owned()
                }
                SetupStage::Ready => {
                    "Core local dictation is operational. Optional systems are reported separately."
                        .to_owned()
                }
                SetupStage::Blocked => {
                    "One or more required local capabilities need attention.".to_owned()
                }
                SetupStage::Discovering => {
                    "Reading local capabilities. No changes are being made.".to_owned()
                }
            }
                },
                str::to_owned,
            );
        let actions = if self.initialization_notice.is_some() {
            Vec::new()
        } else {
            self.presentations
                .iter()
                .map(|presentation| {
                    let dependencies_ready = self
                        .authorized
                        .get(presentation.id.as_str())
                        .and_then(|action| self.executor.dependencies_satisfied(action).ok())
                        .unwrap_or(false);
                    present_action(
                        presentation,
                        self.active.as_ref(),
                        dependencies_ready,
                        self.executor.recovery_in_progress(),
                    )
                })
                .collect()
        };
        SetupSnapshot {
            stage,
            summary,
            capabilities: capabilities(readiness),
            actions,
            recommendation: None,
        }
    }
}

fn desired_configuration(
    settings: &Settings,
    readiness: &UiReadinessSnapshot,
) -> DesiredConfiguration {
    let language = if settings.recognition.language == "pt-br" {
        Language::PortugueseBrazil
    } else {
        Language::English
    };
    let recognition = match settings.recognition.mode {
        RecognitionMode::Accurate => RecognitionChoice::Accurate,
        RecognitionMode::Instant => RecognitionChoice::Instant,
    };
    let formatting = if matches!(
        settings.formatting.strength,
        FormattingStrength::Balanced | FormattingStrength::Strong | FormattingStrength::Custom
    ) {
        selected_ollama_digest(settings, readiness)
            .map_or(FormattingChoice::Deterministic, |model_digest| {
                FormattingChoice::Ollama { model_digest }
            })
    } else {
        FormattingChoice::Deterministic
    };
    DesiredConfiguration {
        language,
        recognition,
        formatting,
        launch_at_login: settings.startup.launch_at_login,
        benchmark_protocol: None,
    }
}

fn prepare_plan(
    authority: &SetupAuthority,
    root: &ManagedRoot,
    settings: &Settings,
    readiness: &UiReadinessSnapshot,
) -> Result<PreparedPlan, &'static str> {
    let desired = desired_configuration(settings, readiness);
    let facts = normalized_facts(settings, readiness, root)
        .map_err(|_| "Local setup inventory could not be verified.")?;
    let plan = authority
        .plan(&desired, facts)
        .map_err(|_| "A safe setup plan could not be created.")?;
    let presentations = plan.actions();
    let mut authorized = BTreeMap::new();
    for presentation in &presentations {
        let action = plan
            .authorize(&presentation.id)
            .map_err(|_| "A safe setup plan could not be authorized.")?;
        authorized.insert(presentation.id.as_str().to_owned(), action);
    }
    Ok(PreparedPlan {
        authorized,
        presentations,
    })
}

fn normalized_facts(
    settings: &Settings,
    readiness: &UiReadinessSnapshot,
    root: &ManagedRoot,
) -> Result<Vec<NormalizedProbeFact>, ()> {
    let language = if settings.recognition.language == "pt-br" {
        Language::PortugueseBrazil
    } else {
        Language::English
    };
    let mut facts = vec![NormalizedProbeFact::Microphone {
        selected_is_available: readiness.microphone.state == UiReadinessState::Ready,
        permission_denied: false,
        selection_possible: !readiness.microphone.devices.is_empty(),
    }];
    let installed = root.installed().map_err(|_| ())?;
    let (engine, ui_ready, digest, all_managed) = match settings.recognition.mode {
        RecognitionMode::Accurate => {
            let expected_variant = settings.recognition.accurate_model;
            let expected = model_for_variant(settings.recognition.accurate_model)
                .ok()
                .and_then(|model| Sha256Digest::new(model.sha256).ok());
            let expected_id = settings.recognition.accurate_model.manifest_id();
            let managed_item = expected_id.and_then(|id| {
                installed.iter().find(|item| {
                    item.receipt.asset().asset_id().as_str() == id
                        && expected.as_ref() == Some(item.receipt.asset().digest())
                })
            });
            let managed = managed_item.is_some();
            let exact_ready = managed_item.is_some_and(|item| {
                let Ok(spec) = model_for_variant(expected_variant) else {
                    return false;
                };
                let managed_file = item.target.join(spec.file_name);
                same_existing_path(&readiness.whisper.configured_path, &managed_file)
                    && identify_pinned_model(&managed_file).ok().flatten() == Some(expected_variant)
            });
            (
                EngineKind::Accurate,
                readiness.whisper.state == UiReadinessState::Ready && exact_ready,
                expected,
                managed,
            )
        }
        RecognitionMode::Instant => {
            let managed_paths = vosk_paths(&installed);
            let managed = managed_paths.is_some() && language == Language::English;
            let exact_ready = managed_paths.is_some_and(|(runtime, model)| {
                same_existing_path(&readiness.vosk.runtime_path, &runtime)
                    && same_existing_path(&readiness.vosk.model_path, &model)
            });
            (
                EngineKind::Instant,
                readiness.vosk.state == UiReadinessState::Ready && exact_ready,
                None,
                managed,
            )
        }
    };
    let state = if ui_ready {
        RecognitionProbeState::Ready { resident: false }
    } else if all_managed {
        RecognitionProbeState::InstalledUnvalidated
    } else {
        RecognitionProbeState::Missing
    };
    facts.push(NormalizedProbeFact::Recognition {
        engine,
        language,
        model_digest: digest,
        state,
    });
    if let Some(digest) = selected_ollama_digest(settings, readiness) {
        facts.push(NormalizedProbeFact::OllamaDaemon {
            version: readiness
                .ollama
                .daemon_reachable
                .then(|| phorminx_setup::ContentFreeId::new("local").expect("static identity")),
            reachable: readiness.ollama.daemon_reachable,
            installed: readiness.ollama.installed,
        });
        let present_digest = readiness
            .ollama
            .models
            .iter()
            .find(|model| {
                model
                    .digest
                    .as_deref()
                    .and_then(parse_ollama_digest)
                    .as_ref()
                    == Some(&digest)
            })
            .and_then(|model| model.digest.as_deref())
            .and_then(parse_ollama_digest);
        facts.push(NormalizedProbeFact::OllamaModel {
            expected_digest: digest,
            present_digest,
        });
    }
    if let Ok(executable) = std::env::current_exe() {
        let state = launch_at_login_state(&executable).ok();
        facts.push(NormalizedProbeFact::LaunchAtLogin {
            enabled: matches!(state, Some(LaunchAtLoginState::Enabled)),
            exact_command: matches!(
                (settings.startup.launch_at_login, state),
                (true, Some(LaunchAtLoginState::Enabled))
                    | (false, Some(LaunchAtLoginState::Disabled))
            ),
        });
    }
    Ok(facts)
}

fn selected_ollama_digest(
    settings: &Settings,
    _readiness: &UiReadinessSnapshot,
) -> Option<Sha256Digest> {
    let _selected = settings.formatting.ollama_model.as_ref()?;
    settings
        .formatting
        .ollama_model_identity
        .as_ref()
        .and_then(|identity| Sha256Digest::new(&identity.manifest_sha256).ok())
}

fn parse_ollama_digest(value: &str) -> Option<Sha256Digest> {
    Sha256Digest::new(value.strip_prefix("sha256:").unwrap_or(value)).ok()
}

fn present_action(
    presentation: &crate::setup_host::ActionPresentation,
    active: Option<&ActionEvent>,
    dependencies_ready: bool,
    recovery_in_progress: bool,
) -> SetupAction {
    let state = active
        .filter(|event| event.action_id == presentation.id)
        .map(|event| &event.state);
    let another_is_active = active.is_some_and(|event| {
        event.action_id != presentation.id && (!event.state.is_terminal() || recovery_in_progress)
    });
    let needs_recovery = state.is_some_and(recovery_required);
    let host_supported = !matches!(
        presentation.key,
        ActionKey::GuidedExternalInstall { .. }
            | ActionKey::StartExternalTool { .. }
            | ActionKey::PullOllamaModel { .. }
            | ActionKey::GrantMicrophoneAccess
            | ActionKey::Probe(_)
            | ActionKey::ImportVerifiedAssets { .. }
    );
    let currently_executing = state.is_some_and(|state| {
        matches!(
            state,
            ActionState::Running { .. }
                | ActionState::AwaitingConsent { .. }
                | ActionState::Queued
                | ActionState::Cancelling
        )
    });
    let (progress_percent, running, can_retry, complete) = match state {
        Some(ActionState::Running { progress }) => (
            progress.total_units.and_then(|total| {
                (total > 0).then(|| {
                    ((progress.completed_units.saturating_mul(100) / total).min(100)) as u8
                })
            }),
            true,
            false,
            false,
        ),
        Some(
            ActionState::AwaitingConsent { .. } | ActionState::Queued | ActionState::Cancelling,
        ) => (None, true, false, false),
        Some(ActionState::Failed { retryable, .. }) => (None, false, *retryable, false),
        Some(ActionState::Succeeded) => (Some(100), false, false, true),
        Some(state) if recovery_required(state) => {
            (None, recovery_in_progress, !recovery_in_progress, false)
        }
        _ => (None, false, false, false),
    };
    let (title, detail) = action_copy(&presentation.key);
    SetupAction {
        id: presentation.id.as_str().to_owned(),
        title: title.to_owned(),
        detail: detail.to_owned(),
        progress_percent,
        consent: if needs_recovery {
            Vec::new()
        } else {
            presentation
                .required_consent
                .iter()
                .map(|item| consent_copy(*item).to_owned())
                .collect()
        },
        running,
        can_retry,
        complete,
        available: !another_is_active
            && (needs_recovery || currently_executing || (host_supported && dependencies_ready)),
    }
}

fn same_existing_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    std::fs::canonicalize(left)
        .and_then(|left| std::fs::canonicalize(right).map(|right| left == right))
        .unwrap_or(false)
}

fn recovery_required(state: &ActionState) -> bool {
    matches!(
        state,
        ActionState::RollbackRetryPending { .. }
            | ActionState::FailedExternalSideEffectsMayRemain { .. }
            | ActionState::Cancelled {
                outcome: phorminx_setup::CancellationOutcome::ExternalSideEffectsMayRemain
            }
    )
}

fn action_copy(key: &ActionKey) -> (&'static str, &'static str) {
    match key {
        ActionKey::DownloadArtifact { .. } => (
            "Acquire verified local asset",
            "Download one pinned artifact, verify its exact size and SHA-256, then activate it inside Phorminx-managed storage.",
        ),
        ActionKey::Validate(_) => (
            "Validate installed recognition",
            "Verify the installed asset identity and compatibility before it can become active.",
        ),
        ActionKey::ActivateRecognition { .. } => (
            "Activate recognition",
            "Commit only the already verified managed paths, preserving the previous working settings if the commit fails.",
        ),
        ActionKey::GrantMicrophoneAccess => (
            "Review microphone access",
            "Windows privacy access must be granted by you; Phorminx will not bypass the operating system prompt.",
        ),
        ActionKey::SelectMicrophone => (
            "Use the Windows default microphone",
            "Select a currently available input without retaining an unverified device name.",
        ),
        ActionKey::GuidedExternalInstall { .. } => (
            "Install Ollama",
            "Run the signed package-manager flow for Ollama. This is optional; Light formatting remains available.",
        ),
        ActionKey::StartExternalTool { .. } => (
            "Start Ollama manually",
            "Phorminx will not execute an unverified user-writable binary. Start Ollama yourself, then inspect again.",
        ),
        ActionKey::PullOllamaModel { .. } => (
            "Acquire the selected Ollama model",
            "Ask local Ollama to pull the exact model identity selected from trusted local inventory.",
        ),
        ActionKey::ApplyLaunchAtLogin { enabled: true } => (
            "Enable launch at login",
            "Write and verify the exact current Phorminx command for this Windows user.",
        ),
        ActionKey::ApplyLaunchAtLogin { enabled: false } => (
            "Disable launch at login",
            "Remove only Phorminx's current-user startup value and verify the result.",
        ),
        ActionKey::RunBenchmark { .. } => (
            "Measure local performance",
            "Record a short transient calibration sample. Audio and transcript content are not retained.",
        ),
        ActionKey::Probe(_) => (
            "Inspect local capability",
            "Read local state without changing it.",
        ),
        ActionKey::ImportVerifiedAssets { .. } => (
            "Import verified assets",
            "Import only assets that match the compiled catalog.",
        ),
    }
}

const fn consent_copy(value: ConsentCategory) -> &'static str {
    match value {
        ConsentCategory::NetworkDownload => "Download from the disclosed HTTPS source",
        ConsentCategory::LoadNativeCode => {
            "Load verified native code in a bounded validation process"
        }
        ConsentCategory::ExecuteInstaller => "Run the disclosed Ollama installer",
        ConsentCategory::Elevation => "Allow Windows to request elevation if required",
        ConsentCategory::StartBackgroundProcess => "Start a local background process",
        ConsentCategory::PersistLaunchAtLogin => "Change this user's launch-at-login setting",
        ConsentCategory::RecordTransientCalibration => "Record transient calibration audio",
    }
}

fn capabilities(readiness: &UiReadinessSnapshot) -> Vec<SetupCapability> {
    vec![
        capability(
            "microphone",
            "Microphone",
            &readiness.microphone.message,
            readiness.microphone.state,
            false,
        ),
        capability(
            "accurate-recognition",
            "Accurate recognition",
            &readiness.whisper.message,
            readiness.whisper.state,
            false,
        ),
        capability(
            "instant-recognition",
            "Instant recognition",
            &readiness.vosk.message,
            readiness.vosk.state,
            false,
        ),
        capability(
            "ollama",
            "Local refinement · optional",
            &readiness.ollama.message,
            readiness.ollama.state,
            true,
        ),
    ]
}

fn capability(
    id: &str,
    name: &str,
    detail: &str,
    state: UiReadinessState,
    optional: bool,
) -> SetupCapability {
    SetupCapability {
        id: id.to_owned(),
        name: name.to_owned(),
        detail: detail.to_owned(),
        state: if optional && state == UiReadinessState::NeedsAttention {
            Readiness::Optional
        } else {
            map_readiness(state)
        },
        remedy: (state == UiReadinessState::NeedsAttention).then(|| {
            if optional {
                "Optional. Light formatting remains fully local and ready without it.".to_owned()
            } else {
                "A verified repair action is shown below when one is available.".to_owned()
            }
        }),
    }
}

const fn map_readiness(state: UiReadinessState) -> Readiness {
    match state {
        UiReadinessState::Ready => Readiness::Ready,
        UiReadinessState::Checking => Readiness::Working,
        UiReadinessState::NeedsAttention => Readiness::Unavailable,
    }
}

struct ProductionAdapters {
    store: SettingsStore,
    root: ManagedRoot,
    benchmark: Arc<dyn BenchmarkAdapter>,
    compensation: Mutex<BTreeMap<ActionKey, Compensation>>,
}

#[derive(Clone)]
struct Compensation {
    previous: Settings,
    written: Settings,
}

impl ProductionAdapters {
    fn check_cancel(cancel: &AtomicBool) -> Result<(), HostActionError> {
        if cancel.load(Ordering::Acquire) {
            Err(HostActionError::new(ActionFailure::Cancelled, true))
        } else {
            Ok(())
        }
    }

    fn installed(&self) -> Result<Vec<ManagedInstall>, HostActionError> {
        self.root.installed().map_err(|_| platform_error())
    }

    fn save_settings(
        &self,
        action: ActionKey,
        mutate: impl FnOnce(&mut Settings) -> Result<(), HostActionError>,
    ) -> Result<(), HostActionError> {
        let original = self.store.load().map_err(|_| platform_error())?;
        let mut candidate = original.clone();
        mutate(&mut candidate)?;
        let mut compensation = self.compensation.lock().map_err(|_| platform_error())?;
        if self
            .store
            .compare_and_save(&original, &candidate)
            .map_err(|_| platform_error())?
        {
            compensation.insert(
                action,
                Compensation {
                    previous: original,
                    written: candidate,
                },
            );
            Ok(())
        } else {
            Err(HostActionError::new(
                ActionFailure::ConsistencyFailure,
                true,
            ))
        }
    }
}

impl ActionAdapters for ProductionAdapters {
    fn probe(
        &self,
        _capability: &CapabilityId,
        cancel: &AtomicBool,
    ) -> Result<CapabilityRecord, HostActionError> {
        Self::check_cancel(cancel)?;
        Err(HostActionError::new(
            ActionFailure::ConsistencyFailure,
            false,
        ))
    }

    fn import_verified_assets(
        &self,
        _artifacts: &BTreeSet<phorminx_setup::ArtifactDescriptor>,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        Err(HostActionError::new(
            ActionFailure::ConsistencyFailure,
            false,
        ))
    }

    fn validate(
        &self,
        capability: &CapabilityId,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        let installed = self.installed()?;
        match capability {
            CapabilityId::AccurateRecognition { language } => {
                let expected_id = match language {
                    Language::English => "whisper-base-en-f16",
                    Language::PortugueseBrazil => "whisper-base-multilingual-f16",
                };
                let candidate = installed
                    .iter()
                    .find(|item| item.receipt.asset().asset_id().as_str() == expected_id)
                    .ok_or_else(verification_error)?;
                let file = candidate.target.join(
                    model_for_variant(
                        crate::settings::AccurateModelVariant::from_manifest_id(
                            candidate.receipt.asset().asset_id().as_str(),
                        )
                        .ok_or_else(verification_error)?,
                    )
                    .map_err(|_| verification_error())?
                    .file_name,
                );
                identify_pinned_model(&file)
                    .map_err(|_| verification_error())?
                    .filter(|variant| variant.manifest_id() == Some(expected_id))
                    .ok_or_else(verification_error)
                    .map(|_| ())
            }
            CapabilityId::InstantRecognition { language } => {
                let (runtime, model) = vosk_paths(&installed).ok_or_else(verification_error)?;
                let code = language.code();
                match phorminx_vosk::inspect(&runtime, &model, code) {
                    phorminx_vosk::Readiness::Ready { .. } => Ok(()),
                    _ => Err(HostActionError::new(ActionFailure::NativeLoadFailed, false)),
                }
            }
            _ => Ok(()),
        }
    }

    fn activate_recognition(
        &self,
        engine: EngineKind,
        language: Language,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        let installed = self.installed()?;
        match engine {
            EngineKind::Accurate => {
                let expected_id = match language {
                    Language::English => "whisper-base-en-f16",
                    Language::PortugueseBrazil => "whisper-base-multilingual-f16",
                };
                let item = installed
                    .iter()
                    .find(|item| item.receipt.asset().asset_id().as_str() == expected_id)
                    .ok_or_else(verification_error)?;
                let variant = crate::settings::AccurateModelVariant::from_manifest_id(
                    item.receipt.asset().asset_id().as_str(),
                )
                .ok_or_else(verification_error)?;
                let file_name = model_for_variant(variant)
                    .map_err(|_| verification_error())?
                    .file_name;
                let path = item.target.join(file_name);
                if identify_pinned_model(&path).map_err(|_| verification_error())? != Some(variant)
                {
                    return Err(verification_error());
                }
                self.save_settings(
                    ActionKey::ActivateRecognition { engine, language },
                    |settings| {
                        settings.recognition.model_path = path;
                        settings.recognition.accurate_model = variant;
                        settings.recognition.mode = RecognitionMode::Accurate;
                        settings.recognition.language = language.code().to_owned();
                        Ok(())
                    },
                )
            }
            EngineKind::Instant => {
                let (runtime, model) = vosk_paths(&installed).ok_or_else(verification_error)?;
                if !matches!(
                    phorminx_vosk::inspect(&runtime, &model, language.code()),
                    phorminx_vosk::Readiness::Ready { .. }
                ) {
                    return Err(verification_error());
                }
                self.save_settings(
                    ActionKey::ActivateRecognition { engine, language },
                    |settings| {
                        settings.recognition.instant_runtime_path = runtime;
                        settings.recognition.instant_model_path = model;
                        settings.recognition.mode = RecognitionMode::Instant;
                        settings.recognition.language = language.code().to_owned();
                        Ok(())
                    },
                )
            }
        }
    }

    fn grant_microphone_access(&self, cancel: &AtomicBool) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        Err(HostActionError::new(ActionFailure::PermissionDenied, false))
    }

    fn select_microphone(&self, cancel: &AtomicBool) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        self.save_settings(ActionKey::SelectMicrophone, |settings| {
            settings.recognition.microphone = None;
            Ok(())
        })
    }

    fn guided_ollama_install(&self, cancel: &AtomicBool) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        // Deliberately unavailable until a pinned installer or verified
        // publisher identity is part of the compiled host catalog. PATH lookup
        // is never treated as executable authority.
        Err(verification_error())
    }

    fn start_ollama(&self, cancel: &AtomicBool) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        // LocalAppData is user-writable, and `--version` output cannot prove a
        // publisher identity. Until a pinned Authenticode signer contract is
        // compiled into the host, Phorminx must not execute this binary.
        Err(verification_error())
    }

    fn pull_ollama_model(
        &self,
        digest: &Sha256Digest,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        let _ = digest;
        // A digest alone is not an acquisition contract. A future curated
        // catalog must bind model name, digest, and source before this runs.
        Err(verification_error())
    }

    fn apply_launch_at_login(
        &self,
        enabled: bool,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        let executable = std::env::current_exe().map_err(|_| platform_error())?;
        let before = launch_at_login_state(&executable).map_err(|_| platform_error())?;
        if matches!(&before, LaunchAtLoginState::DifferentCommand(_)) {
            return Err(HostActionError::new(
                ActionFailure::ConsistencyFailure,
                false,
            ));
        }
        set_launch_at_login(&executable, enabled).map_err(|_| platform_error())?;
        let applied = launch_at_login_state(&executable).is_ok_and(|state| {
            matches!(
                (enabled, state),
                (true, LaunchAtLoginState::Enabled) | (false, LaunchAtLoginState::Disabled)
            )
        });
        if !applied {
            restore_launch_at_login(&executable, &before)?;
            return Err(verification_error());
        }
        let saved = self.save_settings(ActionKey::ApplyLaunchAtLogin { enabled }, |settings| {
            settings.startup.launch_at_login = enabled;
            Ok(())
        });
        if saved.is_err() {
            restore_launch_at_login(&executable, &before)?;
        }
        saved
    }

    fn run_benchmark(
        &self,
        protocol: &phorminx_setup::ContentFreeId,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        self.benchmark.run(protocol.as_str(), cancel)
    }

    fn rollback(&self, action: &ActionKey, _cancel: &AtomicBool) -> Result<(), HostActionError> {
        let compensation = self
            .compensation
            .lock()
            .map_err(|_| platform_error())?
            .get(action)
            .cloned();
        let Some(compensation) = compensation else {
            // No completed compensating write exists for this action.
            return Ok(());
        };
        let current = self.store.load().map_err(|_| platform_error())?;
        if current != compensation.previous
            && !self
                .store
                .compare_and_save(&compensation.written, &compensation.previous)
                .map_err(|_| platform_error())?
        {
            return Err(HostActionError::new(
                ActionFailure::ConsistencyFailure,
                false,
            ));
        }
        if matches!(action, ActionKey::ApplyLaunchAtLogin { .. }) {
            let executable = std::env::current_exe().map_err(|_| platform_error())?;
            set_launch_at_login(&executable, compensation.previous.startup.launch_at_login)
                .map_err(|_| platform_error())?;
            let state = launch_at_login_state(&executable).map_err(|_| platform_error())?;
            if !matches!(
                (compensation.previous.startup.launch_at_login, state),
                (true, LaunchAtLoginState::Enabled) | (false, LaunchAtLoginState::Disabled)
            ) {
                return Err(verification_error());
            }
        }
        self.compensation
            .lock()
            .map_err(|_| platform_error())?
            .remove(action);
        Ok(())
    }
    fn reconcile_external(
        &self,
        action: &ActionKey,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        Self::check_cancel(cancel)?;
        match action {
            ActionKey::StartExternalTool { tool } if tool.as_str() == "ollama" => {
                let client = OllamaClient::new(
                    OllamaEndpoint::default(),
                    ClientTimeouts {
                        connect: Duration::from_millis(300),
                        response_headers: Duration::from_millis(500),
                        response_body: Duration::from_millis(500),
                        overall: Duration::from_millis(750),
                    },
                )
                .map_err(|_| platform_error())?;
                let result = client
                    .discover(&CancellationToken::new())
                    .map(|_| ())
                    .map_err(|_| platform_error());
                Self::check_cancel(cancel)?;
                result
            }
            _ => Err(HostActionError::new(
                ActionFailure::ConsistencyFailure,
                false,
            )),
        }
    }
}

fn restore_launch_at_login(
    executable: &std::path::Path,
    before: &LaunchAtLoginState,
) -> Result<(), HostActionError> {
    let restore_enabled = matches!(before, LaunchAtLoginState::Enabled);
    if set_launch_at_login(executable, restore_enabled).is_err()
        || !launch_at_login_state(executable).is_ok_and(|state| {
            matches!(
                (restore_enabled, state),
                (true, LaunchAtLoginState::Enabled) | (false, LaunchAtLoginState::Disabled)
            )
        })
    {
        return Err(HostActionError::rollback_blocked(
            ActionFailure::ConsistencyFailure,
            true,
        ));
    }
    Ok(())
}

fn vosk_paths(installed: &[ManagedInstall]) -> Option<(PathBuf, PathBuf)> {
    let runtime = installed
        .iter()
        .find(|item| item.receipt.asset().asset_id().as_str() == "vosk-runtime-win64-0-3-45")?
        .target
        .clone();
    let model = installed
        .iter()
        .find(|item| item.receipt.asset().asset_id().as_str() == "vosk-model-small-en-us-0-15")?
        .target
        .clone();
    Some((runtime, model))
}

const fn verification_error() -> HostActionError {
    HostActionError::new(ActionFailure::VerificationFailed, false)
}
const fn platform_error() -> HostActionError {
    HostActionError::new(ActionFailure::PlatformOperationFailed, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AccurateBackendPreference;
    use crate::ui_bridge::{
        UiMicrophone, UiMicrophoneReadiness, UiOllamaModel, UiOllamaReadiness, UiVoskReadiness,
        UiVoskReadinessKind, UiWhisperReadiness,
    };

    fn readiness_with_ollama(digest: Option<String>) -> UiReadinessSnapshot {
        UiReadinessSnapshot {
            microphone: UiMicrophoneReadiness {
                state: UiReadinessState::Ready,
                devices: vec![UiMicrophone {
                    name: "Device".to_owned(),
                    is_default: true,
                }],
                selected: None,
                message: "Microphone ready.".to_owned(),
            },
            whisper: UiWhisperReadiness {
                state: UiReadinessState::Ready,
                configured_path: PathBuf::from("model.bin"),
                size_bytes: Some(1),
                language: "en".to_owned(),
                selected_backend: Some("cpu".to_owned()),
                device_name: None,
                message: "Whisper ready.".to_owned(),
            },
            vosk: UiVoskReadiness {
                state: UiReadinessState::NeedsAttention,
                kind: UiVoskReadinessKind::MissingRuntime,
                runtime_path: PathBuf::from("runtime"),
                model_path: PathBuf::from("model"),
                message: "Vosk missing.".to_owned(),
            },
            ollama: UiOllamaReadiness {
                state: UiReadinessState::Ready,
                models: vec![UiOllamaModel {
                    name: "curated:local".to_owned(),
                    size_bytes: Some(1),
                    family: None,
                    digest,
                }],
                selected: Some("curated:local".to_owned()),
                message: "Ollama ready.".to_owned(),
                daemon_reachable: true,
                installed: true,
            },
        }
    }

    #[test]
    fn digest_parser_accepts_ollama_prefix_but_not_names() {
        let hex = "a".repeat(64);
        assert_eq!(
            parse_ollama_digest(&format!("sha256:{hex}"))
                .unwrap()
                .as_str(),
            hex
        );
        assert!(parse_ollama_digest("qwen:3b").is_none());
    }

    #[test]
    fn consent_copy_is_specific_and_content_free() {
        for category in [
            ConsentCategory::NetworkDownload,
            ConsentCategory::LoadNativeCode,
            ConsentCategory::ExecuteInstaller,
            ConsentCategory::Elevation,
            ConsentCategory::StartBackgroundProcess,
            ConsentCategory::PersistLaunchAtLogin,
            ConsentCategory::RecordTransientCalibration,
        ] {
            let copy = consent_copy(category);
            assert!(!copy.is_empty());
            assert!(!copy.contains('\\'));
        }
    }

    #[test]
    fn missing_or_invalid_ollama_digest_never_mints_pull_authority() {
        let mut settings = Settings::default();
        settings.formatting.strength = FormattingStrength::Strong;
        settings.formatting.ollama_model = Some("curated:local".to_owned());
        let desired = desired_configuration(&settings, &readiness_with_ollama(None));
        assert_eq!(desired.formatting, FormattingChoice::Deterministic);

        let desired = desired_configuration(
            &settings,
            &readiness_with_ollama(Some("not-a-digest".to_owned())),
        );
        assert_eq!(desired.formatting, FormattingChoice::Deterministic);
    }

    #[test]
    fn exact_loopback_digest_is_preserved_in_desired_configuration() {
        let digest = "b".repeat(64);
        let mut settings = Settings::default();
        settings.formatting.strength = FormattingStrength::Strong;
        settings.formatting.ollama_model = Some("curated:local".to_owned());
        settings.formatting.ollama_model_identity =
            Some(crate::settings::OllamaModelIdentity::new(digest.clone(), 1_000).unwrap());
        settings.recognition.accurate_backend = AccurateBackendPreference::Cpu;
        let desired = desired_configuration(
            &settings,
            &readiness_with_ollama(Some(format!("sha256:{digest}"))),
        );
        assert_eq!(
            desired.formatting,
            FormattingChoice::Ollama {
                model_digest: Sha256Digest::new(digest).unwrap()
            }
        );
    }

    #[test]
    fn optional_ollama_failure_never_blocks_core_readiness() {
        let mut readiness = readiness_with_ollama(None);
        readiness.ollama.state = UiReadinessState::NeedsAttention;
        let rows = capabilities(&readiness);
        assert_eq!(rows.last().unwrap().state, Readiness::Optional);
        assert_eq!(rows[0].state, Readiness::Ready);
    }
}
