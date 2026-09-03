use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActionError, ActionId, ActionKey, CapabilityId, CapabilityRecord, CapabilityValue,
    ContentFreeId, EngineKind, Language, Remedy, SetupAction, Sha256Digest, Usability,
};

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

#[derive(Deserialize)]
struct SetupPlanWire {
    schema_version: u32,
    policy_version: ContentFreeId,
    actions: Vec<PlannedAction>,
}

impl<'de> Deserialize<'de> for SetupPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SetupPlanWire::deserialize(deserializer)?;
        let plan = Self {
            schema_version: wire.schema_version,
            policy_version: wire.policy_version,
            actions: wire.actions,
        };
        plan.validate().map_err(serde::de::Error::custom)?;
        Ok(plan)
    }
}

#[derive(Default)]
pub struct Planner;

impl Planner {
    pub fn plan(
        desired: &DesiredConfiguration,
        capabilities: impl IntoIterator<Item = CapabilityRecord>,
    ) -> Result<SetupPlan, PlanError> {
        let mut inventory = BTreeMap::new();
        for capability in capabilities {
            match inventory.entry(capability.id.clone()) {
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
        Self::plan_capability(&mut graph, inventory.get(&microphone), microphone.clone())?;
        let recognition_tail =
            Self::plan_capability(&mut graph, inventory.get(&recognition), recognition.clone())?;

        let mut benchmark_dependencies = BTreeSet::new();
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
        if record.usability.is_ready() {
            return Ok(None);
        }

        let remedies = if record.remedies.is_empty() {
            [default_remedy(&record.usability)].into_iter().collect()
        } else {
            record.remedies.clone()
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
            Remedy::AcquireManagedAssets {
                artifacts,
                loads_native_code,
            } => {
                if artifacts.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let dependencies = artifacts
                    .into_iter()
                    .map(|artifact| {
                        graph.insert(
                            SetupAction::for_key(ActionKey::DownloadArtifact { artifact })?,
                            BTreeSet::new(),
                        )
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                Self::validate_and_activate(graph, capability, dependencies, loads_native_code)
            }
            Remedy::ImportVerifiedAssets {
                artifacts,
                loads_native_code,
            } => {
                if artifacts.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let imported = graph.insert(
                    SetupAction::for_key(ActionKey::ImportVerifiedAssets { artifacts })?,
                    BTreeSet::new(),
                )?;
                Self::validate_and_activate(
                    graph,
                    capability,
                    [imported].into_iter().collect(),
                    loads_native_code,
                )
            }
            Remedy::ValidateInstalled => Self::validate_and_activate(
                graph,
                capability,
                BTreeSet::new(),
                is_native(capability),
            ),
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
        loads_native_code: bool,
    ) -> Result<Option<ActionId>, PlanError> {
        let validation = graph.insert(
            SetupAction::for_key(ActionKey::Validate {
                capability: capability.clone(),
                loads_native_code,
            })?,
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
        Some(CapabilityRecord {
            value:
                Some(CapabilityValue::LaunchAtLogin {
                    enabled,
                    exact_command,
                }),
            usability,
            ..
        }) => !usability.is_ready() || *enabled != desired || !*exact_command,
        Some(record) => desired || !record.usability.is_ready(),
        None => desired,
    }
}

fn is_native(capability: &CapabilityId) -> bool {
    matches!(capability, CapabilityId::InstantRecognition { .. })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactDescriptor, ArtifactKind, AssetId, ConsentCategory, ReadyAuthority,
        UnavailableReason,
    };

    fn artifact(id: &str, byte: char) -> ArtifactDescriptor {
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
        )
        .unwrap()
    }

    fn ready(id: CapabilityId) -> CapabilityRecord {
        CapabilityRecord::new(
            id,
            Usability::Ready {
                authority: ReadyAuthority::FullProbe,
            },
        )
    }

    fn missing(id: CapabilityId, remedy: Remedy) -> CapabilityRecord {
        CapabilityRecord::new(
            id,
            Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
        )
        .with_remedies([remedy])
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

    #[test]
    fn plan_is_invariant_to_inventory_enumeration_order() {
        let model = artifact("whisper-base-en-f16", 'a');
        let records = vec![
            ready(CapabilityId::Microphone),
            missing(
                CapabilityId::AccurateRecognition {
                    language: Language::English,
                },
                Remedy::AcquireManagedAssets {
                    artifacts: [model].into_iter().collect(),
                    loads_native_code: false,
                },
            ),
        ];
        let mut reversed = records.clone();
        reversed.reverse();
        assert_eq!(
            Planner::plan(&desired(), records).unwrap(),
            Planner::plan(&desired(), reversed).unwrap()
        );
    }

    #[test]
    fn planning_is_idempotent_and_topological() {
        let capability = CapabilityId::AccurateRecognition {
            language: Language::English,
        };
        let records = vec![
            ready(CapabilityId::Microphone),
            missing(
                capability,
                Remedy::AcquireManagedAssets {
                    artifacts: [artifact("base-en", 'a')].into_iter().collect(),
                    loads_native_code: false,
                },
            ),
        ];
        let first = Planner::plan(&desired(), records.clone()).unwrap();
        let second = Planner::plan(&desired(), records).unwrap();
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
        let plan = Planner::plan(&desired(), records).unwrap();
        assert!(plan.actions.is_empty());
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
        let plan = Planner::plan(&configuration, records).unwrap();
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
            Planner::plan(&desired(), [first, second]),
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
            artifact("vosk-runtime", 'a'),
            artifact("vosk-en-model", 'b'),
        ]
        .into_iter()
        .collect();
        let plan = Planner::plan(
            &desired,
            [
                ready(CapabilityId::Microphone),
                missing(
                    instant,
                    Remedy::AcquireManagedAssets {
                        artifacts,
                        loads_native_code: true,
                    },
                ),
            ],
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
        let plan = Planner::plan(
            &desired(),
            [
                missing(CapabilityId::Microphone, Remedy::SelectMicrophone),
                ready(CapabilityId::AccurateRecognition {
                    language: Language::English,
                }),
            ],
        )
        .unwrap();
        let mut relabelled = serde_json::to_value(&plan).unwrap();
        relabelled["actions"][0]["action"]["id"] = serde_json::json!(ActionId::for_key(
            &ActionKey::Probe(CapabilityId::Microphone)
        ));
        assert!(serde_json::from_value::<SetupPlan>(relabelled).is_err());

        let mut wrong_policy = serde_json::to_value(&plan).unwrap();
        wrong_policy["policy_version"] = serde_json::json!("setup-policy-v999");
        assert!(serde_json::from_value::<SetupPlan>(wrong_policy).is_err());
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
                ready(CapabilityId::LaunchAtLogin).with_value(CapabilityValue::LaunchAtLogin {
                    enabled: true,
                    exact_command: true,
                }),
            ],
        )
        .unwrap();
        assert!(plan.actions().iter().any(|planned| matches!(
            planned.action().key(),
            ActionKey::ApplyLaunchAtLogin { enabled: false }
        )));
    }

    #[test]
    fn explicit_policy_prefers_verified_import_over_download_regardless_of_enum_order() {
        let capability = CapabilityId::AccurateRecognition {
            language: Language::English,
        };
        let model = artifact("base-en", 'a');
        let record = CapabilityRecord::new(
            capability.clone(),
            Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
        )
        .with_remedies([
            Remedy::AcquireManagedAssets {
                artifacts: [model.clone()].into_iter().collect(),
                loads_native_code: false,
            },
            Remedy::ImportVerifiedAssets {
                artifacts: [model].into_iter().collect(),
                loads_native_code: false,
            },
        ]);
        let plan = Planner::plan(&desired(), [ready(CapabilityId::Microphone), record]).unwrap();
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
        let plan = Planner::plan(
            &configuration,
            [
                ready(CapabilityId::Microphone),
                missing(
                    CapabilityId::AccurateRecognition {
                        language: Language::English,
                    },
                    Remedy::AcquireManagedAssets {
                        artifacts: [artifact("base-en", 'a')].into_iter().collect(),
                        loads_native_code: false,
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
}
