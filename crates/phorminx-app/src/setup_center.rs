//! Process-local orchestration for the Setup and Repair Center.
//!
//! The controller is the only bridge from UI action identifiers to opaque
//! `AuthorizedAction` values. UI strings, paths, URLs, digests, and commands
//! never cross this boundary as execution authority.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use phorminx_ollama::{CancellationToken, ClientTimeouts, OllamaClient, OllamaEndpoint};
use phorminx_setup::{
    ActionFailure, ActionKey, ActionPhase, ActionState, CapabilityId, CapabilityRecord,
    ConsentCategory, DesiredConfiguration, EngineKind, FormattingChoice, Language,
    RecognitionChoice, Sha256Digest,
};
use phorminx_ui::{Readiness, SetupAction, SetupCapability, SetupSnapshot, SetupStage};
use phorminx_windows::{LaunchAtLoginState, launch_at_login_state, set_launch_at_login};

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
}

impl SetupCenter {
    pub fn open(
        store: SettingsStore,
        benchmark: Arc<dyn BenchmarkAdapter>,
    ) -> Result<Self, &'static str> {
        let authority = SetupAuthority::phorminx().map_err(|_| "Setup catalog unavailable.")?;
        let root = ManagedRoot::from_local_app_data().map_err(|_| "Setup storage unavailable.")?;
        let initialization_notice = match root.recover() {
            Ok(report) if report.rejected_journals > 0 => {
                Some("A previous setup operation needs manual review.")
            }
            Ok(_) => None,
            Err(_) => {
                Some("Setup recovery could not finish; existing runtime settings were preserved.")
            }
        };
        let adapters = Arc::new(ProductionAdapters {
            store,
            root: root.clone(),
            benchmark,
            compensation: Mutex::new(BTreeMap::new()),
        });
        Ok(Self {
            authority,
            root: root.clone(),
            executor: SetupExecutor::new(root, adapters, Arc::new(HttpsFetcher)),
            authorized: BTreeMap::new(),
            presentations: Vec::new(),
            active: None,
            initialization_notice,
        })
    }

    /// Retries only marker-verified recovery. It never deletes or adopts an
    /// unowned path. A successful retry re-enables fresh planning.
    pub fn retry_recovery(&mut self) -> Result<(), &'static str> {
        match self.root.recover() {
            Ok(report) if report.rejected_journals == 0 => {
                self.initialization_notice = None;
                Ok(())
            }
            Ok(_) => {
                self.initialization_notice =
                    Some("A previous setup operation needs manual review.");
                Err("Setup recovery still needs manual review.")
            }
            Err(_) => {
                self.initialization_notice = Some(
                    "Setup recovery could not finish; existing runtime settings were preserved.",
                );
                Err("Setup recovery could not finish.")
            }
        }
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
        let desired = desired_configuration(settings, readiness);
        let facts = normalized_facts(settings, readiness, &self.executor)
            .map_err(|_| "Local setup inventory could not be verified.")?;
        let plan = self
            .authority
            .plan(&desired, facts)
            .map_err(|_| "A safe setup plan could not be created.")?;
        self.presentations = plan.actions();
        self.authorized.clear();
        for presentation in &self.presentations {
            if let Ok(action) = plan.authorize(&presentation.id) {
                self.authorized
                    .insert(presentation.id.as_str().to_owned(), action);
            }
        }
        Ok(())
    }

    pub fn start(&mut self, id: &str) -> Result<(), &'static str> {
        if self.initialization_notice.is_some() {
            return Err("Setup mutations are blocked until local recovery is repaired.");
        }
        let action = self
            .authorized
            .remove(id)
            .ok_or("That setup action is no longer current. Inspect again.")?;
        let granted = action.required_consent().clone();
        self.executor
            .start(action, granted)
            .map_err(|_| "The setup action could not start.")
    }

    pub fn cancel(&self, id: &str) -> Result<(), &'static str> {
        if self.active.as_ref().map(|event| event.action_id.as_str()) != Some(id) {
            return Err("That setup action is no longer active.");
        }
        self.executor
            .cancel()
            .map_err(|_| "The setup action could not be cancelled.")
    }

    pub fn poll(&mut self) -> Result<bool, &'static str> {
        match self.executor.try_event() {
            Ok(Some(event)) => {
                let changed = self.active.as_ref() != Some(&event);
                self.active = Some(event);
                Ok(changed)
            }
            Ok(None) => Ok(false),
            Err(_) => Err("Setup status became unavailable."),
        }
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
        .contains(&UiReadinessState::Checking);
        let core_ready = readiness.microphone.state == UiReadinessState::Ready
            && (readiness.whisper.state == UiReadinessState::Ready
                || readiness.vosk.state == UiReadinessState::Ready);
        let busy = self
            .active
            .as_ref()
            .is_some_and(|event| !event.state.is_terminal());
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
        } else if failed || self.initialization_notice.is_some() {
            SetupStage::Blocked
        } else if !self.presentations.is_empty() {
            SetupStage::PlanReady
        } else if core_ready {
            SetupStage::Ready
        } else {
            SetupStage::Blocked
        };
        let summary = self.initialization_notice.map_or_else(
            || match stage {
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
            },
            str::to_owned,
        );
        let actions = if self.initialization_notice.is_some() {
            Vec::new()
        } else {
            self.presentations
                .iter()
                .map(|presentation| present_action(presentation, self.active.as_ref()))
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

fn normalized_facts(
    settings: &Settings,
    readiness: &UiReadinessSnapshot,
    executor: &SetupExecutor,
) -> Result<Vec<NormalizedProbeFact>, ()> {
    let language = if settings.recognition.language == "pt-br" {
        Language::PortugueseBrazil
    } else {
        Language::English
    };
    let mut facts = vec![NormalizedProbeFact::Microphone {
        selected_is_available: readiness.microphone.state == UiReadinessState::Ready,
        permission_denied: false,
    }];
    let installed = executor.durable_installed().map_err(|_| ())?;
    let (engine, ui_ready, digest, all_managed) = match settings.recognition.mode {
        RecognitionMode::Accurate => {
            let expected = model_for_variant(settings.recognition.accurate_model)
                .ok()
                .and_then(|model| Sha256Digest::new(model.sha256).ok());
            let expected_id = settings.recognition.accurate_model.manifest_id();
            let managed = expected_id.is_some_and(|id| {
                installed.iter().any(|item| {
                    item.receipt.asset().asset_id().as_str() == id
                        && expected.as_ref() == Some(item.receipt.asset().digest())
                })
            });
            (
                EngineKind::Accurate,
                readiness.whisper.state,
                expected,
                managed,
            )
        }
        RecognitionMode::Instant => {
            let managed = ["vosk-runtime-win64-0-3-45", "vosk-model-small-en-us-0-15"]
                .iter()
                .all(|id| {
                    installed
                        .iter()
                        .any(|item| item.receipt.asset().asset_id().as_str() == *id)
                });
            (EngineKind::Instant, readiness.vosk.state, None, managed)
        }
    };
    let state = if ui_ready == UiReadinessState::Ready {
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
    readiness: &UiReadinessSnapshot,
) -> Option<Sha256Digest> {
    let selected = settings.formatting.ollama_model.as_ref()?;
    readiness
        .ollama
        .models
        .iter()
        .find(|model| &model.name == selected)
        .and_then(|model| model.digest.as_deref())
        .and_then(parse_ollama_digest)
}

fn parse_ollama_digest(value: &str) -> Option<Sha256Digest> {
    Sha256Digest::new(value.strip_prefix("sha256:").unwrap_or(value)).ok()
}

fn present_action(
    presentation: &crate::setup_host::ActionPresentation,
    active: Option<&ActionEvent>,
) -> SetupAction {
    let state = active
        .filter(|event| event.action_id == presentation.id)
        .map(|event| &event.state);
    let another_is_active = active
        .is_some_and(|event| event.action_id != presentation.id && !event.state.is_terminal());
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
        _ => (None, false, false, false),
    };
    let (title, detail) = action_copy(&presentation.key);
    SetupAction {
        id: presentation.id.as_str().to_owned(),
        title: title.to_owned(),
        detail: detail.to_owned(),
        progress_percent,
        consent: presentation
            .required_consent
            .iter()
            .map(|item| consent_copy(*item).to_owned())
            .collect(),
        running,
        can_retry,
        complete,
        available: !another_is_active,
    }
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
            "Start Ollama",
            "Start the local Ollama service as a background process.",
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
        let executable = trusted_ollama_executable(cancel)?;
        let mut child = Command::new(executable)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| platform_error())?;
        let timeouts = ClientTimeouts {
            connect: Duration::from_millis(300),
            response_headers: Duration::from_millis(500),
            response_body: Duration::from_millis(500),
            overall: Duration::from_millis(750),
        };
        let client =
            OllamaClient::new(OllamaEndpoint::default(), timeouts).map_err(|_| platform_error())?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if cancel.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostActionError::new(ActionFailure::Cancelled, true));
            }
            if client.discover(&CancellationToken::new()).is_ok() {
                return Ok(());
            }
            if child.try_wait().map_err(|_| platform_error())?.is_some() {
                return Err(platform_error());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(HostActionError::new(
            ActionFailure::PlatformOperationFailed,
            true,
        ))
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
        let verified = launch_at_login_state(&executable).map_err(|_| platform_error())?;
        if matches!(
            (enabled, verified),
            (true, LaunchAtLoginState::Enabled) | (false, LaunchAtLoginState::Disabled)
        ) {
            let saved = self.save_settings(ActionKey::ApplyLaunchAtLogin { enabled }, |settings| {
                settings.startup.launch_at_login = enabled;
                Ok(())
            });
            if saved.is_err() {
                let restore_enabled = matches!(before, LaunchAtLoginState::Enabled);
                let _ = set_launch_at_login(&executable, restore_enabled);
            }
            saved
        } else {
            Err(verification_error())
        }
    }

    fn run_benchmark(
        &self,
        protocol: &phorminx_setup::ContentFreeId,
        cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
        self.benchmark.run(protocol.as_str(), cancel)
    }

    fn rollback(&self, action: &ActionKey, _cancel: &AtomicBool) -> Result<(), HostActionError> {
        let previous = self
            .compensation
            .lock()
            .map_err(|_| platform_error())?
            .remove(action);
        let Some(compensation) = previous else {
            // No completed compensating write exists for this action.
            return Ok(());
        };
        if !self
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
        Ok(())
    }
    fn reconcile_external(
        &self,
        action: &ActionKey,
        _cancel: &AtomicBool,
    ) -> Result<(), HostActionError> {
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
                client
                    .discover(&CancellationToken::new())
                    .map(|_| ())
                    .map_err(|_| platform_error())
            }
            _ => Err(HostActionError::new(
                ActionFailure::ConsistencyFailure,
                false,
            )),
        }
    }
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

/// Resolves only Ollama's documented per-user install location. The version
/// probe is bounded and the executable itself must not be a link. No PATH or
/// current-directory lookup is used.
fn trusted_ollama_executable(cancel: &AtomicBool) -> Result<PathBuf, HostActionError> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(verification_error)?;
    let expected_parent = PathBuf::from(local).join("Programs/Ollama");
    let expected = expected_parent.join("ollama.exe");
    let metadata = std::fs::symlink_metadata(&expected).map_err(|_| verification_error())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(verification_error());
    }
    let canonical_parent =
        std::fs::canonicalize(&expected_parent).map_err(|_| verification_error())?;
    let canonical = std::fs::canonicalize(&expected).map_err(|_| verification_error())?;
    if canonical.parent() != Some(canonical_parent.as_path())
        || canonical.file_name().and_then(|name| name.to_str()) != Some("ollama.exe")
    {
        return Err(verification_error());
    }
    let mut child = Command::new(&canonical)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| verification_error())?;
    wait_bounded(&mut child, cancel, Duration::from_secs(3))?;
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or_else(verification_error)?
        .take(4096)
        .read_to_string(&mut output)
        .map_err(|_| verification_error())?;
    if !output.to_ascii_lowercase().contains("ollama version") {
        return Err(verification_error());
    }
    Ok(canonical)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
const fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn wait_bounded(
    child: &mut Child,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<(), HostActionError> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(HostActionError::new(ActionFailure::Cancelled, true));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(verification_error()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(platform_error());
            }
            Err(_) => return Err(platform_error()),
        }
    }
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
