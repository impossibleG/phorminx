use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{AssetId, CapabilityId, ContentFreeId, Generation, Language, Sha256Digest};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ActionKey {
    Probe(CapabilityId),
    DownloadArtifact {
        asset: AssetId,
    },
    ImportVerifiedAssets {
        assets: BTreeSet<AssetId>,
    },
    Validate(CapabilityId),
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
            Self::DownloadArtifact { asset } => format!("download:{}", asset.as_str()),
            Self::ImportVerifiedAssets { assets } => {
                format!("import:{}", canonical_assets(assets))
            }
            Self::Validate(capability) => format!("validate:{}", capability_key(capability)),
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
}

fn canonical_assets(assets: &BTreeSet<AssetId>) -> String {
    assets
        .iter()
        .map(|asset| format!("{}:{}", asset.as_str().len(), asset.as_str()))
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetupAction {
    pub id: ActionId,
    pub key: ActionKey,
    pub consent: BTreeSet<ConsentCategory>,
    pub rollback: RollbackPolicy,
}

impl SetupAction {
    #[must_use]
    pub fn new(
        key: ActionKey,
        consent: impl IntoIterator<Item = ConsentCategory>,
        rollback: RollbackPolicy,
    ) -> Self {
        let id = ActionId::for_key(&key);
        Self {
            id,
            key,
            consent: consent.into_iter().collect(),
            rollback,
        }
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
    RollbackPending,
    RolledBack,
}

impl ActionState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed { .. } | Self::RolledBack
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
            (ActionState::Running { .. }, ActionCommand::Succeed) => ActionState::Succeeded,
            (
                ActionState::Running { .. }
                | ActionState::Cancelling
                | ActionState::RollbackPending,
                ActionCommand::Fail { failure, retryable },
            ) => ActionState::Failed { failure, retryable },
            (ActionState::AwaitingConsent { .. } | ActionState::Queued, ActionCommand::Cancel) => {
                ActionState::RolledBack
            }
            (ActionState::Running { .. }, ActionCommand::Cancel) => ActionState::Cancelling,
            (ActionState::Cancelling, ActionCommand::CancellationCompleted) => {
                ActionState::RolledBack
            }
            (
                ActionState::Failed {
                    retryable: true, ..
                },
                ActionCommand::Retry,
            )
            | (ActionState::RolledBack, ActionCommand::Retry) => {
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
                ActionState::RollbackPending
            }
            (ActionState::RollbackPending, ActionCommand::RollbackCompleted) => {
                ActionState::RolledBack
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

    fn action(consent: bool, rollback: RollbackPolicy) -> SetupAction {
        SetupAction::new(
            ActionKey::Probe(CapabilityId::Microphone),
            consent.then_some(ConsentCategory::NetworkDownload),
            rollback,
        )
    }

    #[test]
    fn action_ids_are_stable_for_set_enumeration_order() {
        let a = AssetId::new("a").unwrap();
        let b = AssetId::new("b").unwrap();
        let left = [a.clone(), b.clone()].into_iter().collect();
        let right = [b, a].into_iter().collect();
        assert_eq!(
            ActionId::for_key(&ActionKey::ImportVerifiedAssets { assets: left }),
            ActionId::for_key(&ActionKey::ImportVerifiedAssets { assets: right })
        );
    }

    #[test]
    fn lifecycle_accepts_consent_run_cancel_retry() {
        let mut runtime = ActionRuntime::new(action(true, RollbackPolicy::ManagedAssetsOnly));
        runtime.apply(ActionCommand::GrantConsent).unwrap();
        runtime.apply(ActionCommand::Start).unwrap();
        runtime.apply(ActionCommand::Cancel).unwrap();
        assert_eq!(runtime.state, ActionState::Cancelling);
        runtime.apply(ActionCommand::CancellationCompleted).unwrap();
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
            let mut runtime = ActionRuntime::new(action(true, RollbackPolicy::ManagedAssetsOnly));
            assert_eq!(runtime.apply(command), Err(ActionError::IllegalTransition));
        }
    }

    #[test]
    fn progress_is_monotonic() {
        let mut runtime = ActionRuntime::new(action(false, RollbackPolicy::None));
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
            Err(ActionError::ProgressRegressed)
        );
    }

    #[test]
    fn non_retryable_failure_cannot_be_retried() {
        let mut runtime = ActionRuntime::new(action(false, RollbackPolicy::None));
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
        let mut non_owned = ActionRuntime::new(action(false, RollbackPolicy::None));
        non_owned.apply(ActionCommand::Start).unwrap();
        non_owned.apply(ActionCommand::Succeed).unwrap();
        assert_eq!(
            non_owned.apply(ActionCommand::RequestRollback),
            Err(ActionError::IllegalTransition)
        );

        let mut owned = ActionRuntime::new(action(false, RollbackPolicy::ManagedAssetsOnly));
        owned.apply(ActionCommand::Start).unwrap();
        owned.apply(ActionCommand::Succeed).unwrap();
        owned.apply(ActionCommand::RequestRollback).unwrap();
        owned.apply(ActionCommand::RollbackCompleted).unwrap();
        assert_eq!(owned.state, ActionState::RolledBack);
    }

    #[test]
    fn coordinator_is_single_operation_and_generation_guarded() {
        let mut coordinator = Coordinator::default();
        let first = action(false, RollbackPolicy::None);
        coordinator.start(first.clone()).unwrap();
        assert_eq!(
            coordinator.start(action(false, RollbackPolicy::None)),
            Err(CoordinatorError::Busy)
        );

        let stale = coordinator.begin_probe(CapabilityId::Microphone);
        let current = coordinator.begin_probe(CapabilityId::Microphone);
        assert!(!coordinator.accepts_probe(&stale));
        assert!(coordinator.accepts_probe(&current));

        coordinator
            .command(&first.id, ActionCommand::Start)
            .unwrap();
        coordinator
            .command(&first.id, ActionCommand::Succeed)
            .unwrap();
        coordinator
            .start(action(false, RollbackPolicy::None))
            .unwrap();
    }
}
