use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{ArtifactDescriptor, CapabilityId, ContentFreeId, Generation, Language, Sha256Digest};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ActionKey {
    Probe(CapabilityId),
    DownloadArtifact {
        artifact: ArtifactDescriptor,
    },
    ImportVerifiedAssets {
        artifacts: BTreeSet<ArtifactDescriptor>,
    },
    Validate {
        capability: CapabilityId,
        loads_native_code: bool,
    },
    ActivateRecognition {
        engine: crate::EngineKind,
        language: Language,
    },
    GrantMicrophoneAccess,
    SelectMicrophone,
    GuidedExternalInstall {
        tool: ContentFreeId,
    },
    StartExternalTool {
        tool: ContentFreeId,
    },
    PullOllamaModel {
        digest: Sha256Digest,
    },
    ApplyLaunchAtLogin {
        enabled: bool,
    },
    RunBenchmark {
        protocol: ContentFreeId,
    },
}

impl ActionKey {
    fn canonical(&self) -> String {
        match self {
            Self::Probe(capability) => format!("probe:{}", capability_key(capability)),
            Self::DownloadArtifact { artifact } => format!(
                "download:{}:{}",
                artifact.asset_id().as_str(),
                artifact.digest().as_str()
            ),
            Self::ImportVerifiedAssets { artifacts } => {
                format!("import:{}", canonical_artifacts(artifacts))
            }
            Self::Validate {
                capability,
                loads_native_code,
            } => format!(
                "validate:{}:{}",
                capability_key(capability),
                if *loads_native_code { "native" } else { "data" }
            ),
            Self::ActivateRecognition { engine, language } => {
                format!("activate:{}:{}", engine_key(*engine), language.code())
            }
            Self::GrantMicrophoneAccess => "microphone:grant".to_owned(),
            Self::SelectMicrophone => "microphone:select".to_owned(),
            Self::GuidedExternalInstall { tool } => format!("external:install:{tool}"),
            Self::StartExternalTool { tool } => format!("external:start:{tool}"),
            Self::PullOllamaModel { digest } => format!("ollama:pull:{}", digest.as_str()),
            Self::ApplyLaunchAtLogin { enabled } => {
                format!("startup:{}", if *enabled { "enable" } else { "disable" })
            }
            Self::RunBenchmark { protocol } => format!("benchmark:{protocol}"),
        }
    }

    #[must_use]
    pub fn required_consent(&self) -> BTreeSet<ConsentCategory> {
        match self {
            Self::DownloadArtifact { .. } | Self::PullOllamaModel { .. } => {
                [ConsentCategory::NetworkDownload].into_iter().collect()
            }
            Self::Validate {
                loads_native_code: true,
                ..
            }
            | Self::ActivateRecognition {
                engine: crate::EngineKind::Instant,
                ..
            } => [ConsentCategory::LoadNativeCode].into_iter().collect(),
            Self::GuidedExternalInstall { .. } => [
                ConsentCategory::NetworkDownload,
                ConsentCategory::ExecuteInstaller,
                ConsentCategory::Elevation,
            ]
            .into_iter()
            .collect(),
            Self::StartExternalTool { .. } => [ConsentCategory::StartBackgroundProcess]
                .into_iter()
                .collect(),
            Self::ApplyLaunchAtLogin { .. } => [ConsentCategory::PersistLaunchAtLogin]
                .into_iter()
                .collect(),
            Self::RunBenchmark { .. } => [ConsentCategory::RecordTransientCalibration]
                .into_iter()
                .collect(),
            Self::Probe(_)
            | Self::ImportVerifiedAssets { .. }
            | Self::Validate { .. }
            | Self::ActivateRecognition { .. }
            | Self::GrantMicrophoneAccess
            | Self::SelectMicrophone => BTreeSet::new(),
        }
    }

    #[must_use]
    pub const fn rollback_policy(&self) -> RollbackPolicy {
        match self {
            Self::DownloadArtifact { .. }
            | Self::ImportVerifiedAssets { .. }
            | Self::PullOllamaModel { .. } => RollbackPolicy::ManagedAssetsOnly,
            Self::ActivateRecognition { .. } | Self::ApplyLaunchAtLogin { .. } => {
                RollbackPolicy::CompensatingWrite
            }
            Self::Probe(_)
            | Self::Validate { .. }
            | Self::GrantMicrophoneAccess
            | Self::SelectMicrophone
            | Self::GuidedExternalInstall { .. }
            | Self::StartExternalTool { .. }
            | Self::RunBenchmark { .. } => RollbackPolicy::None,
        }
    }

    #[must_use]
    pub const fn permits_phase(&self, phase: ActionPhase) -> bool {
        match phase {
            ActionPhase::Preparing | ActionPhase::Finalizing => true,
            ActionPhase::Downloading => matches!(
                self,
                Self::DownloadArtifact { .. }
                    | Self::GuidedExternalInstall { .. }
                    | Self::PullOllamaModel { .. }
            ),
            ActionPhase::Importing => matches!(self, Self::ImportVerifiedAssets { .. }),
            ActionPhase::Verifying => matches!(
                self,
                Self::DownloadArtifact { .. }
                    | Self::ImportVerifiedAssets { .. }
                    | Self::Validate { .. }
                    | Self::PullOllamaModel { .. }
                    | Self::ApplyLaunchAtLogin { .. }
            ),
            ActionPhase::Loading => matches!(
                self,
                Self::Validate { .. }
                    | Self::ActivateRecognition { .. }
                    | Self::GuidedExternalInstall { .. }
                    | Self::StartExternalTool { .. }
            ),
            ActionPhase::Benchmarking => matches!(self, Self::RunBenchmark { .. }),
            ActionPhase::Committing => matches!(
                self,
                Self::DownloadArtifact { .. }
                    | Self::ImportVerifiedAssets { .. }
                    | Self::ActivateRecognition { .. }
                    | Self::GrantMicrophoneAccess
                    | Self::SelectMicrophone
                    | Self::GuidedExternalInstall { .. }
                    | Self::PullOllamaModel { .. }
                    | Self::ApplyLaunchAtLogin { .. }
            ),
        }
    }

    #[must_use]
    pub const fn may_leave_external_side_effects(&self) -> bool {
        matches!(
            self,
            Self::GuidedExternalInstall { .. }
                | Self::StartExternalTool { .. }
                | Self::GrantMicrophoneAccess
        )
    }
}

fn canonical_artifacts(artifacts: &BTreeSet<ArtifactDescriptor>) -> String {
    artifacts
        .iter()
        .map(|artifact| {
            format!(
                "{}:{}:{}",
                artifact.asset_id().as_str().len(),
                artifact.asset_id().as_str(),
                artifact.digest().as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn engine_key(engine: crate::EngineKind) -> &'static str {
    match engine {
        crate::EngineKind::Accurate => "accurate",
        crate::EngineKind::Instant => "instant",
    }
}

fn capability_key(capability: &CapabilityId) -> String {
    match capability {
        CapabilityId::Microphone => "microphone".to_owned(),
        CapabilityId::AccurateRecognition { language } => {
            format!("accurate:{}", language.code())
        }
        CapabilityId::InstantRecognition { language } => {
            format!("instant:{}", language.code())
        }
        CapabilityId::OllamaDaemon => "ollama:daemon".to_owned(),
        CapabilityId::OllamaModel { digest } => format!("ollama:model:{}", digest.as_str()),
        CapabilityId::LaunchAtLogin => "startup".to_owned(),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActionId(String);

impl ActionId {
    #[must_use]
    pub fn for_key(key: &ActionKey) -> Self {
        Self(format!("action:{}", key.canonical()))
    }

    /// Intended for restoring an already validated stable ID from a host.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ActionId {
    type Error = ActionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !value.starts_with("action:")
            || value.len() > 4_096
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(ActionError::InvalidActionId);
        }
        Ok(Self(value))
    }
}

impl From<ActionId> for String {
    fn from(value: ActionId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentCategory {
    NetworkDownload,
    LoadNativeCode,
    ExecuteInstaller,
    Elevation,
    StartBackgroundProcess,
    PersistLaunchAtLogin,
    RecordTransientCalibration,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPolicy {
    None,
    ManagedAssetsOnly,
    CompensatingWrite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SetupAction {
    id: ActionId,
    key: ActionKey,
    consent: BTreeSet<ConsentCategory>,
    rollback: RollbackPolicy,
}

impl SetupAction {
    pub fn for_key(key: ActionKey) -> Result<Self, ActionError> {
        let id = ActionId::for_key(&key);
        ActionId::try_from(id.as_str().to_owned())?;
        let consent = key.required_consent();
        let rollback = key.rollback_policy();
        Ok(Self {
            id,
            key,
            consent: consent.into_iter().collect(),
            rollback,
        })
    }

    pub fn validate(&self) -> Result<(), ActionError> {
        if self.id != ActionId::for_key(&self.key) {
            return Err(ActionError::ActionIdentityMismatch);
        }
        if self.consent != self.key.required_consent() {
            return Err(ActionError::ConsentMismatch);
        }
        if self.rollback != self.key.rollback_policy() {
            return Err(ActionError::RollbackPolicyMismatch);
        }
        Ok(())
    }

    #[must_use]
    pub const fn id(&self) -> &ActionId {
        &self.id
    }

    #[must_use]
    pub const fn key(&self) -> &ActionKey {
        &self.key
    }

    #[must_use]
    pub const fn consent(&self) -> &BTreeSet<ConsentCategory> {
        &self.consent
    }

    #[must_use]
    pub const fn rollback(&self) -> RollbackPolicy {
        self.rollback
    }
}

#[derive(Deserialize)]
struct SetupActionWire {
    id: ActionId,
    key: ActionKey,
    consent: BTreeSet<ConsentCategory>,
    rollback: RollbackPolicy,
}

impl<'de> Deserialize<'de> for SetupAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SetupActionWire::deserialize(deserializer)?;
        let action = Self {
            id: wire.id,
            key: wire.key,
            consent: wire.consent,
            rollback: wire.rollback,
        };
        action.validate().map_err(serde::de::Error::custom)?;
        Ok(action)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Preparing,
    Downloading,
    Importing,
    Verifying,
    Loading,
    Benchmarking,
    Committing,
    Finalizing,
}

impl ActionPhase {
    const fn rank(self) -> u8 {
        match self {
            Self::Preparing => 0,
            Self::Downloading | Self::Importing => 1,
            Self::Verifying => 2,
            Self::Loading | Self::Benchmarking => 3,
            Self::Committing => 4,
            Self::Finalizing => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActionProgress {
    pub phase: ActionPhase,
    pub completed_units: u64,
    pub total_units: Option<u64>,
}

impl ActionProgress {
    pub fn new(
        phase: ActionPhase,
        completed_units: u64,
        total_units: Option<u64>,
    ) -> Result<Self, ActionError> {
        if total_units.is_some_and(|total| completed_units > total) {
            return Err(ActionError::InvalidProgress);
        }
        Ok(Self {
            phase,
            completed_units,
            total_units,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionFailure {
    Cancelled,
    NetworkUnavailable,
    VerificationFailed,
    InsufficientDisk,
    PermissionDenied,
    PlatformOperationFailed,
    NativeLoadFailed,
    BenchmarkFailed,
    ConsistencyFailure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ActionState {
    AwaitingConsent {
        required: BTreeSet<ConsentCategory>,
    },
    Queued,
    Running {
        progress: ActionProgress,
    },
    Cancelling,
    Succeeded,
    Failed {
        failure: ActionFailure,
        retryable: bool,
    },
    FailedExternalSideEffectsMayRemain {
        failure: ActionFailure,
        retryable: bool,
    },
    FailedPendingRollback {
        failure: ActionFailure,
        retryable: bool,
        cause: RollbackCause,
    },
    RollbackPending {
        cause: RollbackCause,
    },
    Cancelled {
        outcome: CancellationOutcome,
    },
    RolledBack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationOutcome {
    NoLocalRollbackNeeded,
    RollbackCompleted,
    ExternalSideEffectsMayRemain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackCause {
    Cancellation,
    Failure,
    UserRequested,
}

impl ActionState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed { .. }
                | Self::FailedExternalSideEffectsMayRemain { .. }
                | Self::Cancelled { .. }
                | Self::RolledBack
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionCommand {
    GrantConsent,
    Start,
    Progress(ActionProgress),
    Succeed,
    Fail {
        failure: ActionFailure,
        retryable: bool,
    },
    Cancel,
    CancellationCompleted,
    Retry,
    RequestRollback,
    RollbackCompleted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionRuntime {
    pub action: SetupAction,
    pub state: ActionState,
    pub attempt: u32,
    consent_granted: bool,
}

impl ActionRuntime {
    #[must_use]
    pub fn new(action: SetupAction) -> Self {
        let state = if action.consent.is_empty() {
            ActionState::Queued
        } else {
            ActionState::AwaitingConsent {
                required: action.consent.clone(),
            }
        };
        Self {
            action,
            state,
            attempt: 1,
            consent_granted: false,
        }
    }

    pub fn apply(&mut self, command: ActionCommand) -> Result<(), ActionError> {
        let next = match (&self.state, command) {
            (ActionState::AwaitingConsent { .. }, ActionCommand::GrantConsent) => {
                self.consent_granted = true;
                ActionState::Queued
            }
            (ActionState::Queued, ActionCommand::Start) => ActionState::Running {
                progress: ActionProgress::new(ActionPhase::Preparing, 0, None)?,
            },
            (ActionState::Running { progress }, ActionCommand::Progress(next)) => {
                if !self.action.key.permits_phase(next.phase) {
                    return Err(ActionError::PhaseIncompatible);
                }
                if next.phase.rank() < progress.phase.rank()
                    || (next.phase.rank() == progress.phase.rank() && next.phase != progress.phase)
                    || (next.phase == progress.phase
                        && next.completed_units < progress.completed_units)
                    || (next.phase == progress.phase
                        && matches!(
                            (progress.total_units, next.total_units),
                            (Some(previous), Some(current)) if previous != current
                        ))
                    || (next.phase == progress.phase
                        && progress.total_units.is_some()
                        && next.total_units.is_none())
                {
                    return Err(ActionError::ProgressRegressed);
                }
                ActionState::Running { progress: next }
            }
            (
                ActionState::Running {
                    progress:
                        ActionProgress {
                            phase: ActionPhase::Finalizing,
                            ..
                        },
                },
                ActionCommand::Succeed,
            ) => ActionState::Succeeded,
            (
                ActionState::Running { .. } | ActionState::Cancelling,
                ActionCommand::Fail { failure, retryable },
            ) if self.action.rollback != RollbackPolicy::None => {
                ActionState::FailedPendingRollback {
                    failure,
                    retryable,
                    cause: if matches!(self.state, ActionState::Cancelling) {
                        RollbackCause::Cancellation
                    } else {
                        RollbackCause::Failure
                    },
                }
            }
            (
                ActionState::RollbackPending { cause },
                ActionCommand::Fail { failure, retryable },
            ) => ActionState::FailedPendingRollback {
                failure,
                retryable,
                cause: *cause,
            },
            (
                ActionState::Running { .. } | ActionState::Cancelling,
                ActionCommand::Fail { failure, retryable },
            ) if self.action.key.may_leave_external_side_effects() => {
                ActionState::FailedExternalSideEffectsMayRemain { failure, retryable }
            }
            (
                ActionState::Running { .. } | ActionState::Cancelling,
                ActionCommand::Fail { failure, retryable },
            ) => ActionState::Failed { failure, retryable },
            (ActionState::AwaitingConsent { .. } | ActionState::Queued, ActionCommand::Cancel) => {
                ActionState::Cancelled {
                    outcome: CancellationOutcome::NoLocalRollbackNeeded,
                }
            }
            (ActionState::Running { .. }, ActionCommand::Cancel) => ActionState::Cancelling,
            (ActionState::Cancelling, ActionCommand::CancellationCompleted)
                if self.action.rollback != RollbackPolicy::None =>
            {
                ActionState::RollbackPending {
                    cause: RollbackCause::Cancellation,
                }
            }
            (ActionState::Cancelling, ActionCommand::CancellationCompleted) => {
                ActionState::Cancelled {
                    outcome: if self.action.key.may_leave_external_side_effects() {
                        CancellationOutcome::ExternalSideEffectsMayRemain
                    } else {
                        CancellationOutcome::NoLocalRollbackNeeded
                    },
                }
            }
            (
                ActionState::Failed {
                    retryable: true, ..
                }
                | ActionState::FailedExternalSideEffectsMayRemain {
                    retryable: true, ..
                },
                ActionCommand::Retry,
            )
            | (ActionState::RolledBack | ActionState::Cancelled { .. }, ActionCommand::Retry) => {
                self.attempt = self.attempt.saturating_add(1);
                if self.action.consent.is_empty() || self.consent_granted {
                    ActionState::Queued
                } else {
                    ActionState::AwaitingConsent {
                        required: self.action.consent.clone(),
                    }
                }
            }
            (ActionState::Succeeded, ActionCommand::RequestRollback)
                if self.action.rollback != RollbackPolicy::None =>
            {
                ActionState::RollbackPending {
                    cause: RollbackCause::UserRequested,
                }
            }
            (ActionState::FailedPendingRollback { cause, .. }, ActionCommand::RequestRollback)
                if self.action.rollback != RollbackPolicy::None =>
            {
                ActionState::RollbackPending { cause: *cause }
            }
            (ActionState::RollbackPending { cause }, ActionCommand::RollbackCompleted) => {
                if *cause == RollbackCause::Cancellation {
                    ActionState::Cancelled {
                        outcome: CancellationOutcome::RollbackCompleted,
                    }
                } else {
                    ActionState::RolledBack
                }
            }
            _ => return Err(ActionError::IllegalTransition),
        };
        self.state = next;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeTicket {
    pub capability: CapabilityId,
    pub generation: Generation,
}

#[derive(Default)]
pub struct Coordinator {
    active: Option<ActionRuntime>,
    generations: BTreeMap<CapabilityId, Generation>,
}

impl Coordinator {
    pub fn start(&mut self, action: SetupAction) -> Result<&ActionRuntime, CoordinatorError> {
        if self
            .active
            .as_ref()
            .is_some_and(|runtime| !runtime.state.is_terminal())
        {
            return Err(CoordinatorError::Busy);
        }
        self.active = Some(ActionRuntime::new(action));
        Ok(self.active.as_ref().expect("just inserted"))
    }

    pub fn command(
        &mut self,
        id: &ActionId,
        command: ActionCommand,
    ) -> Result<&ActionRuntime, CoordinatorError> {
        let active = self
            .active
            .as_mut()
            .ok_or(CoordinatorError::NoActiveAction)?;
        if active.action.id != *id {
            return Err(CoordinatorError::WrongAction);
        }
        active.apply(command).map_err(CoordinatorError::Action)?;
        Ok(active)
    }

    #[must_use]
    pub fn active(&self) -> Option<&ActionRuntime> {
        self.active.as_ref()
    }

    #[must_use]
    pub fn begin_probe(&mut self, capability: CapabilityId) -> ProbeTicket {
        let generation = self
            .generations
            .get(&capability)
            .copied()
            .unwrap_or_default()
            .next();
        self.generations.insert(capability.clone(), generation);
        ProbeTicket {
            capability,
            generation,
        }
    }

    #[must_use]
    pub fn accepts_probe(&self, ticket: &ProbeTicket) -> bool {
        self.generations.get(&ticket.capability) == Some(&ticket.generation)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ActionError {
    #[error("action ID is invalid")]
    InvalidActionId,
    #[error("action ID does not match its key")]
    ActionIdentityMismatch,
    #[error("action consent does not exactly match its key")]
    ConsentMismatch,
    #[error("action rollback policy does not match its key")]
    RollbackPolicyMismatch,
    #[error("action phase is incompatible with its key")]
    PhaseIncompatible,
    #[error("action progress exceeds its total")]
    InvalidProgress,
    #[error("action progress cannot move backward")]
    ProgressRegressed,
    #[error("action lifecycle transition is illegal")]
    IllegalTransition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoordinatorError {
    #[error("another setup operation is active")]
    Busy,
    #[error("there is no active setup operation")]
    NoActiveAction,
    #[error("command does not target the active setup operation")]
    WrongAction,
    #[error(transparent)]
    Action(#[from] ActionError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactKind, AssetId};

    fn artifact(id: &str, digest_byte: char) -> ArtifactDescriptor {
        ArtifactDescriptor::new(
            AssetId::new(id).unwrap(),
            Sha256Digest::new(digest_byte.to_string().repeat(64)).unwrap(),
            1024,
            ContentFreeId::new("alphacephei").unwrap(),
            ContentFreeId::new("0.3.45").unwrap(),
            ContentFreeId::new("apache-2.0").unwrap(),
            format!("https://example.invalid/{id}.zip"),
            ArtifactKind::Data,
            None,
        )
        .unwrap()
    }

    fn probe() -> SetupAction {
        SetupAction::for_key(ActionKey::Probe(CapabilityId::Microphone)).unwrap()
    }

    fn download() -> SetupAction {
        SetupAction::for_key(ActionKey::DownloadArtifact {
            artifact: artifact("vosk-runtime", 'a'),
        })
        .unwrap()
    }

    fn external_install() -> SetupAction {
        SetupAction::for_key(ActionKey::GuidedExternalInstall {
            tool: ContentFreeId::new("ollama").unwrap(),
        })
        .unwrap()
    }

    fn finish(runtime: &mut ActionRuntime) {
        runtime
            .apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Finalizing, 1, Some(1)).unwrap(),
            ))
            .unwrap();
        runtime.apply(ActionCommand::Succeed).unwrap();
    }

    #[test]
    fn action_ids_are_stable_for_set_enumeration_order() {
        let a = artifact("a", 'a');
        let b = artifact("b", 'b');
        let left = [a.clone(), b.clone()].into_iter().collect();
        let right = [b, a].into_iter().collect();
        assert_eq!(
            ActionId::for_key(&ActionKey::ImportVerifiedAssets { artifacts: left }),
            ActionId::for_key(&ActionKey::ImportVerifiedAssets { artifacts: right })
        );
    }

    #[test]
    fn public_action_constructor_rejects_oversized_canonical_identity() {
        let artifacts = (0..80)
            .map(|index| artifact(&format!("artifact-{index:03}"), 'a'))
            .collect();
        assert_eq!(
            SetupAction::for_key(ActionKey::ImportVerifiedAssets { artifacts }).unwrap_err(),
            ActionError::InvalidActionId
        );
    }

    #[test]
    fn high_risk_consent_is_derived_completely_from_key() {
        let install = external_install();
        assert_eq!(
            install.consent(),
            &[
                ConsentCategory::NetworkDownload,
                ConsentCategory::ExecuteInstaller,
                ConsentCategory::Elevation,
            ]
            .into_iter()
            .collect()
        );
        let native = SetupAction::for_key(ActionKey::Validate {
            capability: CapabilityId::AccurateRecognition {
                language: Language::English,
            },
            loads_native_code: true,
        })
        .unwrap();
        assert_eq!(
            native.consent(),
            &[ConsentCategory::LoadNativeCode].into_iter().collect()
        );
    }

    #[test]
    fn lifecycle_accepts_consent_run_cancel_retry() {
        let mut runtime = ActionRuntime::new(download());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime.apply(ActionCommand::Cancel).unwrap();
        assert_eq!(runtime.state, ActionState::Cancelling);
        runtime.apply(ActionCommand::CancellationCompleted).unwrap();
        assert_eq!(
            runtime.state,
            ActionState::RollbackPending {
                cause: RollbackCause::Cancellation
            }
        );
        runtime.apply(ActionCommand::RollbackCompleted).unwrap();
        assert_eq!(
            runtime.state,
            ActionState::Cancelled {
                outcome: CancellationOutcome::RollbackCompleted
            }
        );
        runtime.apply(ActionCommand::Retry).unwrap();
        assert_eq!(runtime.state, ActionState::Queued);
        assert_eq!(runtime.attempt, 2);
    }

    #[test]
    fn lifecycle_rejects_every_obvious_illegal_shortcut() {
        let commands = [
            ActionCommand::Start,
            ActionCommand::Succeed,
            ActionCommand::Retry,
            ActionCommand::RequestRollback,
            ActionCommand::RollbackCompleted,
        ];
        for command in commands {
            let mut runtime = ActionRuntime::new(download());
            assert_eq!(runtime.apply(command), Err(ActionError::IllegalTransition));
        }
    }

    #[test]
    fn success_requires_explicit_finalization_boundary() {
        let mut runtime = ActionRuntime::new(probe());
        runtime.apply(ActionCommand::Start).unwrap();
        assert_eq!(
            runtime.apply(ActionCommand::Succeed),
            Err(ActionError::IllegalTransition)
        );
        finish(&mut runtime);
        assert_eq!(runtime.state, ActionState::Succeeded);
    }

    #[test]
    fn progress_is_monotonic() {
        let mut runtime = ActionRuntime::new(download());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime
            .apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Downloading, 50, Some(100)).unwrap(),
            ))
            .unwrap();
        assert_eq!(
            runtime.apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Downloading, 49, Some(100)).unwrap()
            )),
            Err(ActionError::ProgressRegressed)
        );
        assert_eq!(
            runtime.apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Preparing, 99, Some(100)).unwrap()
            )),
            Err(ActionError::ProgressRegressed)
        );
        assert_eq!(
            runtime.apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Importing, 60, Some(100)).unwrap()
            )),
            Err(ActionError::PhaseIncompatible)
        );
    }

    #[test]
    fn non_retryable_failure_cannot_be_retried() {
        let mut runtime = ActionRuntime::new(probe());
        runtime.apply(ActionCommand::Start).unwrap();
        runtime
            .apply(ActionCommand::Fail {
                failure: ActionFailure::VerificationFailed,
                retryable: false,
            })
            .unwrap();
        assert_eq!(
            runtime.apply(ActionCommand::Retry),
            Err(ActionError::IllegalTransition)
        );
    }

    #[test]
    fn rollback_is_available_only_for_owned_or_compensated_work() {
        let mut non_owned = ActionRuntime::new(probe());
        non_owned.apply(ActionCommand::Start).unwrap();
        finish(&mut non_owned);
        assert_eq!(
            non_owned.apply(ActionCommand::RequestRollback),
            Err(ActionError::IllegalTransition)
        );

        let mut owned = ActionRuntime::new(download());
        owned.apply(ActionCommand::GrantConsent).unwrap();
        owned.apply(ActionCommand::Start).unwrap();
        finish(&mut owned);
        owned.apply(ActionCommand::RequestRollback).unwrap();
        owned.apply(ActionCommand::RollbackCompleted).unwrap();
        assert_eq!(owned.state, ActionState::RolledBack);
    }

    #[test]
    fn untrusted_action_cannot_relabel_a_probe_or_drop_consent() {
        let probe_id = ActionId::for_key(&ActionKey::Probe(CapabilityId::Microphone));
        let relabelled = serde_json::json!({
            "id": probe_id,
            "key": {"kind": "guided_external_install", "tool": "ollama"},
            "consent": [],
            "rollback": "none"
        });
        assert!(serde_json::from_value::<SetupAction>(relabelled).is_err());

        let mut missing_consent = serde_json::to_value(external_install()).unwrap();
        missing_consent["consent"] = serde_json::json!([]);
        assert!(serde_json::from_value::<SetupAction>(missing_consent).is_err());
    }

    #[test]
    fn mutating_failure_cannot_be_abandoned_before_rollback() {
        let mut runtime = ActionRuntime::new(download());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime
            .apply(ActionCommand::Fail {
                failure: ActionFailure::NetworkUnavailable,
                retryable: true,
            })
            .unwrap();
        assert!(matches!(
            runtime.state,
            ActionState::FailedPendingRollback {
                cause: RollbackCause::Failure,
                ..
            }
        ));
        assert_eq!(
            runtime.apply(ActionCommand::Retry),
            Err(ActionError::IllegalTransition)
        );
        runtime.apply(ActionCommand::RequestRollback).unwrap();
        runtime.apply(ActionCommand::RollbackCompleted).unwrap();
        runtime.apply(ActionCommand::Retry).unwrap();
    }

    #[test]
    fn rollback_failure_remains_nonterminal_and_can_be_requested_again() {
        let mut runtime = ActionRuntime::new(download());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime
            .apply(ActionCommand::Fail {
                failure: ActionFailure::VerificationFailed,
                retryable: true,
            })
            .unwrap();
        runtime.apply(ActionCommand::RequestRollback).unwrap();
        runtime
            .apply(ActionCommand::Fail {
                failure: ActionFailure::PlatformOperationFailed,
                retryable: true,
            })
            .unwrap();
        assert!(!runtime.state.is_terminal());
        runtime.apply(ActionCommand::RequestRollback).unwrap();
        runtime.apply(ActionCommand::RollbackCompleted).unwrap();
        assert_eq!(runtime.state, ActionState::RolledBack);
    }

    #[test]
    fn external_cancellation_does_not_claim_rollback() {
        let mut runtime = ActionRuntime::new(external_install());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime.apply(ActionCommand::Cancel).unwrap();
        runtime.apply(ActionCommand::CancellationCompleted).unwrap();
        assert_eq!(
            runtime.state,
            ActionState::Cancelled {
                outcome: CancellationOutcome::ExternalSideEffectsMayRemain
            }
        );
    }

    #[test]
    fn external_install_failure_discloses_uncompensated_side_effects() {
        let mut runtime = ActionRuntime::new(external_install());
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime
            .apply(ActionCommand::Fail {
                failure: ActionFailure::PlatformOperationFailed,
                retryable: true,
            })
            .unwrap();
        assert!(matches!(
            runtime.state,
            ActionState::FailedExternalSideEffectsMayRemain { .. }
        ));
    }

    #[test]
    fn phase_must_match_action_key() {
        let mut runtime = ActionRuntime::new(probe());
        runtime.apply(ActionCommand::Start).unwrap();
        assert_eq!(
            runtime.apply(ActionCommand::Progress(
                ActionProgress::new(ActionPhase::Downloading, 1, None).unwrap()
            )),
            Err(ActionError::PhaseIncompatible)
        );
    }

    #[test]
    fn coordinator_is_single_operation_and_generation_guarded() {
        let mut coordinator = Coordinator::default();
        let first = probe();
        coordinator.start(first.clone()).unwrap();
        assert_eq!(coordinator.start(probe()), Err(CoordinatorError::Busy));

        let stale = coordinator.begin_probe(CapabilityId::Microphone);
        let current = coordinator.begin_probe(CapabilityId::Microphone);
        assert!(!coordinator.accepts_probe(&stale));
        assert!(coordinator.accepts_probe(&current));

        coordinator
            .command(first.id(), ActionCommand::Start)
            .unwrap();
        coordinator
            .command(
                first.id(),
                ActionCommand::Progress(
                    ActionProgress::new(ActionPhase::Finalizing, 1, Some(1)).unwrap(),
                ),
            )
            .unwrap();
        coordinator
            .command(first.id(), ActionCommand::Succeed)
            .unwrap();
        coordinator.start(probe()).unwrap();
    }
}
