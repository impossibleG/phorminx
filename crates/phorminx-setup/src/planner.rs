use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActionError, ActionId, ActionKey, ArtifactDescriptor, CapabilityId, CapabilityRecord,
    CapabilityRecordError, CapabilityValue, ContentFreeId, EngineKind, Language, Remedy,
    SetupAction, Sha256Digest, Usability,
};

/// Process-local planning authority supplied by the host's compiled/pinned
/// catalog. This type deliberately cannot be deserialized: persisted inventory
/// may describe a problem, but it cannot mint a download or executable target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanningPolicy {
    trusted_artifacts: BTreeSet<ArtifactDescriptor>,
    ollama_tool: ContentFreeId,
}

impl PlanningPolicy {
    pub const OLLAMA_TOOL_ID: &'static str = "ollama";

    pub fn new(
        trusted_artifacts: impl IntoIterator<Item = ArtifactDescriptor>,
        ollama_tool: ContentFreeId,
    ) -> Result<Self, PlanError> {
        if ollama_tool.as_str() != Self::OLLAMA_TOOL_ID {
            return Err(PlanError::UntrustedExternalTool);
        }
        Ok(Self {
            trusted_artifacts: trusted_artifacts.into_iter().collect(),
            ollama_tool,
        })
    }

    #[must_use]
    pub fn phorminx(trusted_artifacts: impl IntoIterator<Item = ArtifactDescriptor>) -> Self {
        Self::new(
            trusted_artifacts,
            ContentFreeId::new(Self::OLLAMA_TOOL_ID).expect("static Ollama tool identity is valid"),
        )
        .expect("static Phorminx planning policy is valid")
    }

    fn authorize_record(&self, record: &CapabilityRecord) -> Result<(), PlanError> {
        for remedy in record.remedies() {
            match remedy {
                Remedy::AcquireManagedAssets { artifacts }
                | Remedy::ImportVerifiedAssets { artifacts } => {
                    if !artifacts
                        .iter()
                        .all(|artifact| self.trusted_artifacts.contains(artifact))
                    {
                        return Err(PlanError::UntrustedArtifact);
                    }
                }
                Remedy::GuidedExternalInstall { tool } | Remedy::StartExternalTool { tool } => {
                    if tool != &self.ollama_tool {
                        return Err(PlanError::UntrustedExternalTool);
                    }
                }
                Remedy::Probe
                | Remedy::ValidateInstalled
                | Remedy::ActivateRecognition
                | Remedy::GrantMicrophoneAccess
                | Remedy::SelectMicrophone
                | Remedy::PullOllamaModel { .. }
                | Remedy::ApplyLaunchAtLogin { .. }
                | Remedy::ChooseAlternative { .. }
                | Remedy::UseDeterministicFormatting => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionChoice {
    Accurate,
    Instant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FormattingChoice {
    Deterministic,
    Ollama { model_digest: Sha256Digest },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesiredConfiguration {
    pub language: Language,
    pub recognition: RecognitionChoice,
    pub formatting: FormattingChoice,
    pub launch_at_login: bool,
    pub benchmark_protocol: Option<ContentFreeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedAction {
    action: SetupAction,
    dependencies: BTreeSet<ActionId>,
}

impl PlannedAction {
    #[must_use]
    pub const fn action(&self) -> &SetupAction {
        &self.action
    }

    #[must_use]
    pub const fn dependencies(&self) -> &BTreeSet<ActionId> {
        &self.dependencies
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SetupPlan {
    schema_version: u32,
    policy_version: ContentFreeId,
    /// Stable topological order. Actions with no dependency relationship are
    /// ordered by their stable action IDs.
    actions: Vec<PlannedAction>,
}

impl SetupPlan {
    pub const SCHEMA_VERSION: u32 = 1;
    pub const POLICY_VERSION: &'static str = "setup-policy-v1";

    #[must_use]
    pub fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    pub fn validate(&self) -> Result<(), PlanError> {
        if self.schema_version != Self::SCHEMA_VERSION
            || self.policy_version.as_str() != Self::POLICY_VERSION
        {
            return Err(PlanError::UnsupportedPlanVersion);
        }
        let mut seen = BTreeSet::new();
        for planned in &self.actions {
            planned
                .action
                .validate()
                .map_err(PlanError::InvalidAction)?;
            if !seen.insert(planned.action.id().clone()) {
                return Err(PlanError::DuplicateAction);
            }
            if !planned
                .dependencies
                .iter()
                .all(|dependency| seen.contains(dependency))
            {
                return Err(PlanError::NotTopologicallySorted);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PersistedSetupPlan {
    schema_version: u32,
    policy_version: ContentFreeId,
    actions: Vec<PlannedAction>,
}

#[derive(Deserialize)]
struct PersistedSetupPlanWire {
    schema_version: u32,
    policy_version: ContentFreeId,
    actions: Vec<PlannedAction>,
}

impl PersistedSetupPlan {
    pub fn authorize(
        self,
        desired: &DesiredConfiguration,
        capabilities: impl IntoIterator<Item = CapabilityRecord>,
        policy: &PlanningPolicy,
    ) -> Result<SetupPlan, PlanError> {
        let candidate = SetupPlan {
            schema_version: self.schema_version,
            policy_version: self.policy_version,
            actions: self.actions,
        };
        candidate.validate()?;
        let recomputed = Planner::plan(desired, capabilities, policy)?;
        if candidate != recomputed {
            return Err(PlanError::PersistedPlanMismatch);
        }
        Ok(recomputed)
    }
}

impl From<&SetupPlan> for PersistedSetupPlan {
    fn from(plan: &SetupPlan) -> Self {
        Self {
            schema_version: plan.schema_version,
            policy_version: plan.policy_version.clone(),
            actions: plan.actions.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for PersistedSetupPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = PersistedSetupPlanWire::deserialize(deserializer)?;
        let plan = Self {
            schema_version: wire.schema_version,
            policy_version: wire.policy_version,
            actions: wire.actions,
        };
        SetupPlan {
            schema_version: plan.schema_version,
            policy_version: plan.policy_version.clone(),
            actions: plan.actions.clone(),
        }
        .validate()
        .map_err(serde::de::Error::custom)?;
        Ok(plan)
    }
}

#[derive(Default)]
pub struct Planner;

impl Planner {
    pub fn plan(
        desired: &DesiredConfiguration,
        capabilities: impl IntoIterator<Item = CapabilityRecord>,
        policy: &PlanningPolicy,
    ) -> Result<SetupPlan, PlanError> {
        let mut inventory = BTreeMap::new();
        for capability in capabilities {
            capability.validate()?;
            policy.authorize_record(&capability)?;
            match inventory.entry(capability.id().clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(capability);
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get() == &capability => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(PlanError::ConflictingCapability);
                }
            }
        }

        let microphone = CapabilityId::Microphone;
        let recognition = match desired.recognition {
            RecognitionChoice::Accurate => CapabilityId::AccurateRecognition {
                language: desired.language,
            },
            RecognitionChoice::Instant => CapabilityId::InstantRecognition {
                language: desired.language,
            },
        };

        let mut graph = ActionGraph::default();
        let microphone_tail =
            Self::plan_capability(&mut graph, inventory.get(&microphone), microphone.clone())?;
        let recognition_tail =
            Self::plan_capability(&mut graph, inventory.get(&recognition), recognition.clone())?;

        let mut benchmark_dependencies = BTreeSet::new();
        if let Some(tail) = microphone_tail {
            benchmark_dependencies.insert(tail);
        }
        if let Some(tail) = recognition_tail {
            benchmark_dependencies.insert(tail);
        }
        if let FormattingChoice::Ollama { model_digest } = &desired.formatting {
            let daemon = CapabilityId::OllamaDaemon;
            let daemon_tail =
                Self::plan_capability(&mut graph, inventory.get(&daemon), daemon.clone())?;
            let model = CapabilityId::OllamaModel {
                digest: model_digest.clone(),
            };
            let model_tail =
                Self::plan_capability(&mut graph, inventory.get(&model), model.clone())?;
            if let (Some(daemon_tail), Some(model_tail)) = (&daemon_tail, &model_tail) {
                graph.add_dependency(model_tail, daemon_tail.clone())?;
            }
            benchmark_dependencies.extend([daemon_tail, model_tail].into_iter().flatten());
        }

        let startup = CapabilityId::LaunchAtLogin;
        if launch_at_login_needs_action(desired.launch_at_login, inventory.get(&startup)) {
            if inventory.contains_key(&startup) {
                graph.insert(
                    SetupAction::for_key(ActionKey::ApplyLaunchAtLogin {
                        enabled: desired.launch_at_login,
                    })?,
                    BTreeSet::new(),
                )?;
            } else {
                Self::plan_capability(&mut graph, inventory.get(&startup), startup)?;
            }
        }

        if let Some(protocol) = &desired.benchmark_protocol {
            let benchmark = SetupAction::for_key(ActionKey::RunBenchmark {
                protocol: protocol.clone(),
            })?;
            graph.insert(benchmark, benchmark_dependencies)?;
        }

        graph.finish()
    }

    fn plan_capability(
        graph: &mut ActionGraph,
        record: Option<&CapabilityRecord>,
        capability: CapabilityId,
    ) -> Result<Option<ActionId>, PlanError> {
        let Some(record) = record else {
            let probe = SetupAction::for_key(ActionKey::Probe(capability))?;
            return graph.insert(probe, BTreeSet::new()).map(Some);
        };
        if record.usability().is_ready() {
            return Ok(None);
        }

        let remedies = if record.remedies().is_empty() {
            [default_remedy(record.usability())].into_iter().collect()
        } else {
            record.remedies().clone()
        };
        let Some(selected) = remedies
            .iter()
            .filter_map(|remedy| remedy_priority(&capability, remedy).map(|rank| (rank, remedy)))
            .min_by(|(left_rank, left), (right_rank, right)| {
                left_rank.cmp(right_rank).then_with(|| left.cmp(right))
            })
            .map(|(_, remedy)| remedy.clone())
        else {
            return Ok(None);
        };
        Self::expand_remedy(graph, &capability, selected)
    }

    fn expand_remedy(
        graph: &mut ActionGraph,
        capability: &CapabilityId,
        remedy: Remedy,
    ) -> Result<Option<ActionId>, PlanError> {
        match remedy {
            Remedy::Probe => graph
                .insert(
                    SetupAction::for_key(ActionKey::Probe(capability.clone()))?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::AcquireManagedAssets { artifacts } => {
                if artifacts.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let dependencies = artifacts
                    .into_iter()
                    .map(|artifact| {
                        graph.insert(
                            SetupAction::for_key(ActionKey::DownloadArtifact {
                                artifact: Box::new(artifact),
                            })?,
                            BTreeSet::new(),
                        )
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                Self::validate_and_activate(graph, capability, dependencies)
            }
            Remedy::ImportVerifiedAssets { artifacts } => {
                if artifacts.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let imported = graph.insert(
                    SetupAction::for_key(ActionKey::ImportVerifiedAssets { artifacts })?,
                    BTreeSet::new(),
                )?;
                Self::validate_and_activate(graph, capability, [imported].into_iter().collect())
            }
            Remedy::ValidateInstalled => {
                Self::validate_and_activate(graph, capability, BTreeSet::new())
            }
            Remedy::ActivateRecognition => Self::activate(graph, capability, BTreeSet::new()),
            Remedy::GrantMicrophoneAccess => graph
                .insert(
                    SetupAction::for_key(ActionKey::GrantMicrophoneAccess)?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::SelectMicrophone => graph
                .insert(
                    SetupAction::for_key(ActionKey::SelectMicrophone)?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::GuidedExternalInstall { tool } => {
                let install = graph.insert(
                    SetupAction::for_key(ActionKey::GuidedExternalInstall { tool: tool.clone() })?,
                    BTreeSet::new(),
                )?;
                let start = graph.insert(
                    SetupAction::for_key(ActionKey::StartExternalTool { tool })?,
                    [install].into_iter().collect(),
                )?;
                Ok(Some(start))
            }
            Remedy::StartExternalTool { tool } => graph
                .insert(
                    SetupAction::for_key(ActionKey::StartExternalTool { tool })?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::PullOllamaModel { digest } => graph
                .insert(
                    SetupAction::for_key(ActionKey::PullOllamaModel { digest })?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::ApplyLaunchAtLogin { enabled } => graph
                .insert(
                    SetupAction::for_key(ActionKey::ApplyLaunchAtLogin { enabled })?,
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::ChooseAlternative { .. } | Remedy::UseDeterministicFormatting => Ok(None),
        }
    }

    fn validate_and_activate(
        graph: &mut ActionGraph,
        capability: &CapabilityId,
        dependencies: BTreeSet<ActionId>,
    ) -> Result<Option<ActionId>, PlanError> {
        let validation = graph.insert(
            SetupAction::for_key(ActionKey::Validate(capability.clone()))?,
            dependencies,
        )?;
        if matches!(
            capability,
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. }
        ) {
            Self::activate(graph, capability, [validation].into_iter().collect())
        } else {
            Ok(Some(validation))
        }
    }

    fn activate(
        graph: &mut ActionGraph,
        capability: &CapabilityId,
        dependencies: BTreeSet<ActionId>,
    ) -> Result<Option<ActionId>, PlanError> {
        let (engine, language) = match capability {
            CapabilityId::AccurateRecognition { language } => (EngineKind::Accurate, *language),
            CapabilityId::InstantRecognition { language } => (EngineKind::Instant, *language),
            _ => return Ok(dependencies.last().cloned()),
        };
        graph
            .insert(
                SetupAction::for_key(ActionKey::ActivateRecognition { engine, language })?,
                dependencies,
            )
            .map(Some)
    }
}

fn default_remedy(usability: &Usability) -> Remedy {
    match usability {
        Usability::InstalledUnvalidated => Remedy::ValidateInstalled,
        Usability::Unavailable { .. } | Usability::Degraded { .. } | Usability::Ready { .. } => {
            Remedy::Probe
        }
    }
}

fn remedy_priority(capability: &CapabilityId, remedy: &Remedy) -> Option<u8> {
    match (capability, remedy) {
        (_, Remedy::Probe) => Some(90),
        (
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. },
            Remedy::ImportVerifiedAssets { .. },
        ) => Some(10),
        (
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. },
            Remedy::AcquireManagedAssets { .. },
        ) => Some(20),
        (
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. },
            Remedy::ValidateInstalled,
        ) => Some(30),
        (
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. },
            Remedy::ActivateRecognition,
        ) => Some(40),
        (CapabilityId::Microphone, Remedy::GrantMicrophoneAccess) => Some(10),
        (CapabilityId::Microphone, Remedy::SelectMicrophone) => Some(20),
        (CapabilityId::OllamaDaemon, Remedy::StartExternalTool { .. }) => Some(10),
        (CapabilityId::OllamaDaemon, Remedy::GuidedExternalInstall { .. }) => Some(20),
        (CapabilityId::OllamaModel { .. }, Remedy::PullOllamaModel { .. }) => Some(10),
        (CapabilityId::LaunchAtLogin, Remedy::ApplyLaunchAtLogin { .. }) => Some(10),
        (_, Remedy::ChooseAlternative { .. } | Remedy::UseDeterministicFormatting)
        | (_, Remedy::ImportVerifiedAssets { .. })
        | (_, Remedy::AcquireManagedAssets { .. })
        | (_, Remedy::ValidateInstalled)
        | (_, Remedy::ActivateRecognition)
        | (_, Remedy::GrantMicrophoneAccess)
        | (_, Remedy::SelectMicrophone)
        | (_, Remedy::GuidedExternalInstall { .. })
        | (_, Remedy::StartExternalTool { .. })
        | (_, Remedy::PullOllamaModel { .. })
        | (_, Remedy::ApplyLaunchAtLogin { .. }) => None,
    }
}

fn launch_at_login_needs_action(desired: bool, record: Option<&CapabilityRecord>) -> bool {
    match record {
        Some(record) => match record.value() {
            Some(CapabilityValue::LaunchAtLogin {
                enabled,
                exact_command,
            }) => !record.usability().is_ready() || *enabled != desired || !*exact_command,
            Some(_) => true,
            None => true,
        },
        None => true,
    }
}

#[derive(Default)]
struct ActionGraph {
    nodes: BTreeMap<ActionId, PlannedAction>,
}

impl ActionGraph {
    fn insert(
        &mut self,
        action: SetupAction,
        dependencies: BTreeSet<ActionId>,
    ) -> Result<ActionId, PlanError> {
        let id = action.id().clone();
        match self.nodes.entry(id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(PlannedAction {
                    action,
                    dependencies,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().action != action {
                    return Err(PlanError::DuplicateAction);
                }
                entry.get_mut().dependencies.extend(dependencies);
            }
        }
        Ok(id)
    }

    fn add_dependency(&mut self, action: &ActionId, dependency: ActionId) -> Result<(), PlanError> {
        if !self.nodes.contains_key(&dependency) {
            return Err(PlanError::MissingDependency);
        }
        self.nodes
            .get_mut(action)
            .ok_or(PlanError::MissingDependency)?
            .dependencies
            .insert(dependency);
        Ok(())
    }

    fn finish(self) -> Result<SetupPlan, PlanError> {
        for node in self.nodes.values() {
            if !node
                .dependencies
                .iter()
                .all(|dependency| self.nodes.contains_key(dependency))
            {
                return Err(PlanError::MissingDependency);
            }
        }

        let mut remaining = self.nodes;
        let mut emitted = BTreeSet::new();
        let mut actions = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .find(|(_, node)| {
                    node.dependencies
                        .iter()
                        .all(|dependency| emitted.contains(dependency))
                })
                .map(|(id, _)| id.clone())
                .ok_or(PlanError::Cycle)?;
            let node = remaining.remove(&next).expect("selected existing node");
            emitted.insert(next);
            actions.push(node);
        }
        let plan = SetupPlan {
            schema_version: SetupPlan::SCHEMA_VERSION,
            policy_version: ContentFreeId::new(SetupPlan::POLICY_VERSION)
                .expect("static setup policy version is valid"),
            actions,
        };
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("plan schema or policy version is unsupported")]
    UnsupportedPlanVersion,
    #[error("plan contains an invalid action: {0}")]
    InvalidAction(#[from] ActionError),
    #[error("capability inventory contains an invalid record: {0}")]
    InvalidCapability(#[from] CapabilityRecordError),
    #[error("persisted plan does not exactly match a fresh plan from validated inputs")]
    PersistedPlanMismatch,
    #[error("capability inventory contains conflicting duplicate observations")]
    ConflictingCapability,
    #[error("plan contains conflicting duplicate actions")]
    DuplicateAction,
    #[error("plan contains a missing dependency")]
    MissingDependency,
    #[error("plan contains a dependency cycle")]
    Cycle,
    #[error("plan is not topologically sorted")]
    NotTopologicallySorted,
    #[error("managed asset acquisition requires at least one artifact")]
    EmptyAssetSet,
    #[error("setup inventory references an artifact outside the trusted host catalog")]
    UntrustedArtifact,
    #[error("setup inventory references an external tool outside the trusted host policy")]
    UntrustedExternalTool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactDescriptor, ArtifactKind, AssetId, ConsentCategory, ReadyAuthority,
        UnavailableReason,
    };

    fn artifact_for(
        id: &str,
        byte: char,
        engine: EngineKind,
        language: Language,
    ) -> ArtifactDescriptor {
        ArtifactDescriptor::new(
            AssetId::new(id).unwrap(),
            Sha256Digest::new(byte.to_string().repeat(64)).unwrap(),
            1024,
            ContentFreeId::new("vendor").unwrap(),
            ContentFreeId::new("v1").unwrap(),
            ContentFreeId::new("apache-2.0").unwrap(),
            format!("https://example.invalid/{id}.bin"),
            ArtifactKind::Data,
            None,
            engine,
            [language],
        )
        .unwrap()
    }

    fn artifact(id: &str, byte: char) -> ArtifactDescriptor {
        artifact_for(id, byte, EngineKind::Accurate, Language::English)
    }

    fn ready(id: CapabilityId) -> CapabilityRecord {
        let (value, authority) = match &id {
            CapabilityId::Microphone => (
                CapabilityValue::Microphone {
                    selected_is_available: true,
                },
                ReadyAuthority::FullProbe,
            ),
            CapabilityId::AccurateRecognition { language } => (
                CapabilityValue::Recognition {
                    engine: EngineKind::Accurate,
                    language: *language,
                    model_digest: Some(Sha256Digest::new("a".repeat(64)).unwrap()),
                },
                ReadyAuthority::FullProbe,
            ),
            CapabilityId::InstantRecognition { language } => (
                CapabilityValue::Recognition {
                    engine: EngineKind::Instant,
                    language: *language,
                    model_digest: Some(Sha256Digest::new("a".repeat(64)).unwrap()),
                },
                ReadyAuthority::ResidentWorker,
            ),
            CapabilityId::OllamaDaemon => (
                CapabilityValue::ExternalTool {
                    version: Some(ContentFreeId::new("v1").unwrap()),
                },
                ReadyAuthority::LoopbackHealth,
            ),
            CapabilityId::OllamaModel { digest } => (
                CapabilityValue::Model {
                    digest: digest.clone(),
                },
                ReadyAuthority::FullProbe,
            ),
            CapabilityId::LaunchAtLogin => (
                CapabilityValue::LaunchAtLogin {
                    enabled: false,
                    exact_command: true,
                },
                ReadyAuthority::OperatingSystemReadback,
            ),
        };
        CapabilityRecord::new(id, Some(value), Usability::Ready { authority }).unwrap()
    }

    fn missing(id: CapabilityId, remedy: Remedy) -> CapabilityRecord {
        CapabilityRecord::new(
            id,
            None,
            Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
        )
        .unwrap()
        .with_remedies([remedy])
        .unwrap()
    }

    fn desired() -> DesiredConfiguration {
        DesiredConfiguration {
            language: Language::English,
            recognition: RecognitionChoice::Accurate,
            formatting: FormattingChoice::Deterministic,
            launch_at_login: false,
            benchmark_protocol: None,
        }
    }

    fn policy(artifacts: impl IntoIterator<Item = ArtifactDescriptor>) -> PlanningPolicy {
        PlanningPolicy::phorminx(artifacts)
    }

    #[test]
    fn plan_is_invariant_to_inventory_enumeration_order() {
        let model = artifact("whisper-base-en-f16", 'a');
        let policy = policy([model.clone()]);
        let records = vec![
            ready(CapabilityId::Microphone),
            missing(
                CapabilityId::AccurateRecognition {
                    language: Language::English,
                },
                Remedy::AcquireManagedAssets {
                    artifacts: [model].into_iter().collect(),
                },
            ),
        ];
        let mut reversed = records.clone();
        reversed.reverse();
        assert_eq!(
            Planner::plan(&desired(), records, &policy).unwrap(),
            Planner::plan(&desired(), reversed, &policy).unwrap()
        );
    }

    #[test]
    fn planning_is_idempotent_and_topological() {
        let capability = CapabilityId::AccurateRecognition {
            language: Language::English,
        };
        let model = artifact("base-en", 'a');
        let policy = policy([model.clone()]);
        let records = vec![
            ready(CapabilityId::Microphone),
            missing(
                capability,
                Remedy::AcquireManagedAssets {
                    artifacts: [model].into_iter().collect(),
                },
            ),
        ];
        let first = Planner::plan(&desired(), records.clone(), &policy).unwrap();
        let second = Planner::plan(&desired(), records, &policy).unwrap();
        assert_eq!(first, second);
        first.validate().unwrap();
        assert!(matches!(
            first.actions[0].action.key(),
            ActionKey::DownloadArtifact { .. }
        ));
        assert!(matches!(
            first.actions.last().unwrap().action.key(),
            ActionKey::ActivateRecognition { .. }
        ));
    }

    #[test]
    fn optional_ollama_is_neutral() {
        let records = vec![
            ready(CapabilityId::Microphone),
            ready(CapabilityId::AccurateRecognition {
                language: Language::English,
            }),
            missing(
                CapabilityId::OllamaDaemon,
                Remedy::GuidedExternalInstall {
                    tool: ContentFreeId::new("ollama").unwrap(),
                },
            ),
        ];
        let plan = Planner::plan(&desired(), records, &policy([])).unwrap();
        assert!(plan.actions().iter().all(|planned| !matches!(
            planned.action().key(),
            ActionKey::GuidedExternalInstall { .. }
                | ActionKey::StartExternalTool { .. }
                | ActionKey::PullOllamaModel { .. }
        )));
    }

    #[test]
    fn requested_ollama_adds_daemon_before_model() {
        let digest = Sha256Digest::new("a".repeat(64)).unwrap();
        let mut configuration = desired();
        configuration.formatting = FormattingChoice::Ollama {
            model_digest: digest.clone(),
        };
        let records = vec![
            ready(CapabilityId::Microphone),
            ready(CapabilityId::AccurateRecognition {
                language: Language::English,
            }),
            missing(
                CapabilityId::OllamaDaemon,
                Remedy::StartExternalTool {
                    tool: ContentFreeId::new("ollama").unwrap(),
                },
            ),
            missing(
                CapabilityId::OllamaModel {
                    digest: digest.clone(),
                },
                Remedy::PullOllamaModel { digest },
            ),
        ];
        let plan = Planner::plan(&configuration, records, &policy([])).unwrap();
        plan.validate().unwrap();
        let start_index = plan
            .actions
            .iter()
            .position(|action| matches!(action.action.key(), ActionKey::StartExternalTool { .. }))
            .unwrap();
        let pull_index = plan
            .actions
            .iter()
            .position(|action| matches!(action.action.key(), ActionKey::PullOllamaModel { .. }))
            .unwrap();
        assert!(start_index < pull_index);
    }

    #[test]
    fn conflicting_duplicate_inventory_is_rejected() {
        let first = ready(CapabilityId::Microphone);
        let second = missing(CapabilityId::Microphone, Remedy::SelectMicrophone);
        assert_eq!(
            Planner::plan(&desired(), [first, second], &policy([])),
            Err(PlanError::ConflictingCapability)
        );
    }

    #[test]
    fn native_validation_and_activation_have_explicit_consent() {
        let desired = DesiredConfiguration {
            recognition: RecognitionChoice::Instant,
            ..desired()
        };
        let instant = CapabilityId::InstantRecognition {
            language: Language::English,
        };
        let artifacts = [
            artifact_for("vosk-runtime", 'a', EngineKind::Instant, Language::English),
            artifact_for("vosk-en-model", 'b', EngineKind::Instant, Language::English),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let policy = policy(artifacts.iter().cloned());
        let plan = Planner::plan(
            &desired,
            [
                ready(CapabilityId::Microphone),
                missing(instant, Remedy::AcquireManagedAssets { artifacts }),
            ],
            &policy,
        )
        .unwrap();
        let native_actions = plan
            .actions
            .iter()
            .filter(|action| {
                action
                    .action
                    .consent()
                    .contains(&ConsentCategory::LoadNativeCode)
            })
            .count();
        assert_eq!(native_actions, 2);
    }

    #[test]
    fn persisted_plan_revalidates_action_identity_and_policy_version() {
        let inventory = || {
            [
                missing(CapabilityId::Microphone, Remedy::SelectMicrophone),
                ready(CapabilityId::AccurateRecognition {
                    language: Language::English,
                }),
            ]
        };
        let policy = policy([]);
        let plan = Planner::plan(&desired(), inventory(), &policy).unwrap();
        let persisted: PersistedSetupPlan =
            serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(
            persisted
                .authorize(&desired(), inventory(), &policy)
                .unwrap(),
            plan
        );
        let mut relabelled = serde_json::to_value(&plan).unwrap();
        relabelled["actions"][0]["action"]["id"] = serde_json::json!(ActionId::for_key(
            &ActionKey::Probe(CapabilityId::Microphone)
        ));
        assert!(serde_json::from_value::<PersistedSetupPlan>(relabelled).is_err());

        let mut wrong_policy = serde_json::to_value(&plan).unwrap();
        wrong_policy["policy_version"] = serde_json::json!("setup-policy-v999");
        assert!(serde_json::from_value::<PersistedSetupPlan>(wrong_policy).is_err());

        let mut incomplete = serde_json::to_value(&plan).unwrap();
        incomplete["actions"].as_array_mut().unwrap().pop();
        let persisted: PersistedSetupPlan = serde_json::from_value(incomplete).unwrap();
        assert_eq!(
            persisted.authorize(
                &desired(),
                [
                    missing(CapabilityId::Microphone, Remedy::SelectMicrophone),
                    ready(CapabilityId::AccurateRecognition {
                        language: Language::English,
                    }),
                ],
                &policy,
            ),
            Err(PlanError::PersistedPlanMismatch)
        );
    }

    #[test]
    fn artifact_language_and_engine_must_match_recognition_capability() {
        let wrong_language = artifact_for(
            "base-pt",
            'a',
            EngineKind::Accurate,
            Language::PortugueseBrazil,
        );
        let result = CapabilityRecord::new(
            CapabilityId::AccurateRecognition {
                language: Language::English,
            },
            None,
            Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
        )
        .unwrap()
        .with_remedies([Remedy::AcquireManagedAssets {
            artifacts: [wrong_language].into_iter().collect(),
        }]);
        assert_eq!(result, Err(CapabilityRecordError::IncompatibleRemedy));
    }

    #[test]
    fn launch_at_login_compares_actual_value_not_only_ready_badge() {
        let mut configuration = desired();
        configuration.launch_at_login = false;
        let plan = Planner::plan(
            &configuration,
            [
                ready(CapabilityId::Microphone),
                ready(CapabilityId::AccurateRecognition {
                    language: Language::English,
                }),
                CapabilityRecord::new(
                    CapabilityId::LaunchAtLogin,
                    Some(CapabilityValue::LaunchAtLogin {
                        enabled: true,
                        exact_command: true,
                    }),
                    Usability::Ready {
                        authority: ReadyAuthority::OperatingSystemReadback,
                    },
                )
                .unwrap(),
            ],
            &policy([]),
        )
        .unwrap();
        assert!(plan.actions().iter().any(|planned| matches!(
            planned.action().key(),
            ActionKey::ApplyLaunchAtLogin { enabled: false }
        )));
    }

    #[test]
    fn unknown_launch_at_login_is_probed_even_when_desired_is_disabled() {
        let mut configuration = desired();
        configuration.launch_at_login = false;
        let plan = Planner::plan(
            &configuration,
            [
                ready(CapabilityId::Microphone),
                ready(CapabilityId::AccurateRecognition {
                    language: Language::English,
                }),
            ],
            &policy([]),
        )
        .unwrap();
        assert!(plan.actions().iter().any(|planned| matches!(
            planned.action().key(),
            ActionKey::Probe(CapabilityId::LaunchAtLogin)
        )));
    }

    #[test]
    fn explicit_policy_prefers_verified_import_over_download_regardless_of_enum_order() {
        let capability = CapabilityId::AccurateRecognition {
            language: Language::English,
        };
        let model = artifact("base-en", 'a');
        let policy = policy([model.clone()]);
        let record = CapabilityRecord::new(
            capability.clone(),
            None,
            Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
        )
        .unwrap()
        .with_remedies([
            Remedy::AcquireManagedAssets {
                artifacts: [model.clone()].into_iter().collect(),
            },
            Remedy::ImportVerifiedAssets {
                artifacts: [model].into_iter().collect(),
            },
        ])
        .unwrap();
        let plan = Planner::plan(
            &desired(),
            [ready(CapabilityId::Microphone), record],
            &policy,
        )
        .unwrap();
        assert!(matches!(
            plan.actions()[0].action().key(),
            ActionKey::ImportVerifiedAssets { .. }
        ));
    }

    #[test]
    fn benchmark_waits_for_recognition_and_every_requested_ollama_tail() {
        let digest = Sha256Digest::new("c".repeat(64)).unwrap();
        let mut configuration = desired();
        configuration.benchmark_protocol = Some(ContentFreeId::new("setup-v1").unwrap());
        configuration.formatting = FormattingChoice::Ollama {
            model_digest: digest.clone(),
        };
        let model = artifact("base-en", 'a');
        let policy = policy([model.clone()]);
        let plan = Planner::plan(
            &configuration,
            [
                ready(CapabilityId::Microphone),
                missing(
                    CapabilityId::AccurateRecognition {
                        language: Language::English,
                    },
                    Remedy::AcquireManagedAssets {
                        artifacts: [model].into_iter().collect(),
                    },
                ),
                missing(
                    CapabilityId::OllamaDaemon,
                    Remedy::StartExternalTool {
                        tool: ContentFreeId::new("ollama").unwrap(),
                    },
                ),
                missing(
                    CapabilityId::OllamaModel {
                        digest: digest.clone(),
                    },
                    Remedy::PullOllamaModel { digest },
                ),
            ],
            &policy,
        )
        .unwrap();
        let benchmark = plan
            .actions()
            .iter()
            .find(|planned| matches!(planned.action().key(), ActionKey::RunBenchmark { .. }))
            .unwrap();
        let dependency_keys = plan
            .actions()
            .iter()
            .filter(|planned| benchmark.dependencies().contains(planned.action().id()))
            .map(|planned| planned.action().key())
            .collect::<Vec<_>>();
        assert!(
            dependency_keys
                .iter()
                .any(|key| matches!(key, ActionKey::ActivateRecognition { .. }))
        );
        assert!(
            dependency_keys
                .iter()
                .any(|key| matches!(key, ActionKey::StartExternalTool { .. }))
        );
        assert!(
            dependency_keys
                .iter()
                .any(|key| matches!(key, ActionKey::PullOllamaModel { .. }))
        );
    }

    #[test]
    fn benchmark_waits_for_microphone_remediation() {
        let mut configuration = desired();
        configuration.benchmark_protocol = Some(ContentFreeId::new("setup-v1").unwrap());
        let plan = Planner::plan(
            &configuration,
            [
                missing(CapabilityId::Microphone, Remedy::GrantMicrophoneAccess),
                ready(CapabilityId::AccurateRecognition {
                    language: Language::English,
                }),
            ],
            &policy([]),
        )
        .unwrap();
        let microphone = plan
            .actions()
            .iter()
            .find(|planned| matches!(planned.action().key(), ActionKey::GrantMicrophoneAccess))
            .unwrap();
        let benchmark = plan
            .actions()
            .iter()
            .find(|planned| matches!(planned.action().key(), ActionKey::RunBenchmark { .. }))
            .unwrap();
        assert!(benchmark.dependencies().contains(microphone.action().id()));
        let microphone_index = plan
            .actions()
            .iter()
            .position(|planned| planned.action().id() == microphone.action().id())
            .unwrap();
        let benchmark_index = plan
            .actions()
            .iter()
            .position(|planned| planned.action().id() == benchmark.action().id())
            .unwrap();
        assert!(microphone_index < benchmark_index);
    }

    #[test]
    fn inventory_cannot_mint_artifacts_outside_the_host_policy() {
        let trusted = artifact("base-en", 'a');
        let injected = ArtifactDescriptor::new(
            trusted.asset_id().clone(),
            trusted.digest().clone(),
            trusted.size_bytes(),
            trusted.vendor().clone(),
            trusted.version().clone(),
            trusted.license().clone(),
            "https://attacker.invalid/base-en.bin",
            trusted.kind(),
            trusted.signer().cloned(),
            trusted.engine(),
            trusted.supported_languages().iter().copied(),
        )
        .unwrap();
        let record = missing(
            CapabilityId::AccurateRecognition {
                language: Language::English,
            },
            Remedy::AcquireManagedAssets {
                artifacts: [injected].into_iter().collect(),
            },
        );
        assert_eq!(
            Planner::plan(
                &desired(),
                [ready(CapabilityId::Microphone), record],
                &policy([trusted]),
            ),
            Err(PlanError::UntrustedArtifact)
        );
    }

    #[test]
    fn inventory_cannot_mint_an_external_tool_identity() {
        let record = missing(
            CapabilityId::OllamaDaemon,
            Remedy::GuidedExternalInstall {
                tool: ContentFreeId::new("lookalike-installer").unwrap(),
            },
        );
        assert_eq!(
            Planner::plan(&desired(), [record], &policy([])),
            Err(PlanError::UntrustedExternalTool)
        );
        assert_eq!(
            PlanningPolicy::new([], ContentFreeId::new("lookalike-installer").unwrap(),),
            Err(PlanError::UntrustedExternalTool)
        );
    }

    #[test]
    fn persisted_plan_reauthorization_uses_current_host_catalog() {
        let model = artifact("base-en", 'a');
        let inventory = || {
            [
                ready(CapabilityId::Microphone),
                missing(
                    CapabilityId::AccurateRecognition {
                        language: Language::English,
                    },
                    Remedy::AcquireManagedAssets {
                        artifacts: [model.clone()].into_iter().collect(),
                    },
                ),
            ]
        };
        let plan = Planner::plan(&desired(), inventory(), &policy([model.clone()])).unwrap();
        let persisted: PersistedSetupPlan =
            serde_json::from_value(serde_json::to_value(plan).unwrap()).unwrap();
        assert_eq!(
            persisted.authorize(&desired(), inventory(), &policy([])),
            Err(PlanError::UntrustedArtifact)
        );
    }
}
