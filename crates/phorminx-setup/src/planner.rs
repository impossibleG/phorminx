use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActionId, ActionKey, CapabilityId, CapabilityRecord, ConsentCategory, ContentFreeId,
    EngineKind, Language, Remedy, RollbackPolicy, SetupAction, Sha256Digest, Usability,
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
    pub action: SetupAction,
    pub dependencies: BTreeSet<ActionId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetupPlan {
    /// Stable topological order. Actions with no dependency relationship are
    /// ordered by their stable action IDs.
    pub actions: Vec<PlannedAction>,
}

impl SetupPlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        let mut seen = BTreeSet::new();
        for planned in &self.actions {
            if !seen.insert(planned.action.id.clone()) {
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

        if let FormattingChoice::Ollama { model_digest } = &desired.formatting {
            let daemon = CapabilityId::OllamaDaemon;
            let daemon_tail =
                Self::plan_capability(&mut graph, inventory.get(&daemon), daemon.clone())?;
            let model = CapabilityId::OllamaModel {
                digest: model_digest.clone(),
            };
            let model_tail =
                Self::plan_capability(&mut graph, inventory.get(&model), model.clone())?;
            if let (Some(daemon_tail), Some(model_tail)) = (daemon_tail, model_tail) {
                graph.add_dependency(&model_tail, daemon_tail)?;
            }
        }

        let startup = CapabilityId::LaunchAtLogin;
        if desired.launch_at_login
            || inventory
                .get(&startup)
                .is_some_and(|record| !record.usability.is_ready())
        {
            Self::plan_capability(&mut graph, inventory.get(&startup), startup)?;
        }

        if let Some(protocol) = &desired.benchmark_protocol {
            let benchmark = SetupAction::new(
                ActionKey::RunBenchmark {
                    protocol: protocol.clone(),
                },
                [ConsentCategory::RecordTransientCalibration],
                RollbackPolicy::None,
            );
            let benchmark_id = graph.insert(benchmark, BTreeSet::new())?;
            if let Some(recognition_tail) = recognition_tail {
                graph.add_dependency(&benchmark_id, recognition_tail)?;
            }
        }

        graph.finish()
    }

    fn plan_capability(
        graph: &mut ActionGraph,
        record: Option<&CapabilityRecord>,
        capability: CapabilityId,
    ) -> Result<Option<ActionId>, PlanError> {
        let Some(record) = record else {
            let probe = SetupAction::new(ActionKey::Probe(capability), [], RollbackPolicy::None);
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
            .find(|remedy| is_actionable(remedy))
            .cloned()
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
                    SetupAction::new(
                        ActionKey::Probe(capability.clone()),
                        [],
                        RollbackPolicy::None,
                    ),
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::AcquireManagedAssets {
                assets,
                loads_native_code,
            } => {
                if assets.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let dependencies = assets
                    .into_iter()
                    .map(|asset| {
                        graph.insert(
                            SetupAction::new(
                                ActionKey::DownloadArtifact { asset },
                                [ConsentCategory::NetworkDownload],
                                RollbackPolicy::ManagedAssetsOnly,
                            ),
                            BTreeSet::new(),
                        )
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                Self::validate_and_activate(graph, capability, dependencies, loads_native_code)
            }
            Remedy::ImportVerifiedAssets {
                assets,
                loads_native_code,
            } => {
                if assets.is_empty() {
                    return Err(PlanError::EmptyAssetSet);
                }
                let imported = graph.insert(
                    SetupAction::new(
                        ActionKey::ImportVerifiedAssets { assets },
                        [],
                        RollbackPolicy::ManagedAssetsOnly,
                    ),
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
                    SetupAction::new(ActionKey::GrantMicrophoneAccess, [], RollbackPolicy::None),
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::SelectMicrophone => graph
                .insert(
                    SetupAction::new(ActionKey::SelectMicrophone, [], RollbackPolicy::None),
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::GuidedExternalInstall { tool } => {
                let install = graph.insert(
                    SetupAction::new(
                        ActionKey::GuidedExternalInstall { tool: tool.clone() },
                        [
                            ConsentCategory::NetworkDownload,
                            ConsentCategory::ExecuteInstaller,
                        ],
                        RollbackPolicy::None,
                    ),
                    BTreeSet::new(),
                )?;
                let start = graph.insert(
                    SetupAction::new(
                        ActionKey::StartExternalTool { tool },
                        [ConsentCategory::StartBackgroundProcess],
                        RollbackPolicy::None,
                    ),
                    [install].into_iter().collect(),
                )?;
                Ok(Some(start))
            }
            Remedy::StartExternalTool { tool } => graph
                .insert(
                    SetupAction::new(
                        ActionKey::StartExternalTool { tool },
                        [ConsentCategory::StartBackgroundProcess],
                        RollbackPolicy::None,
                    ),
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::PullOllamaModel { digest } => graph
                .insert(
                    SetupAction::new(
                        ActionKey::PullOllamaModel { digest },
                        [ConsentCategory::NetworkDownload],
                        RollbackPolicy::ManagedAssetsOnly,
                    ),
                    BTreeSet::new(),
                )
                .map(Some),
            Remedy::ApplyLaunchAtLogin { enabled } => graph
                .insert(
                    SetupAction::new(
                        ActionKey::ApplyLaunchAtLogin { enabled },
                        [ConsentCategory::PersistLaunchAtLogin],
                        RollbackPolicy::CompensatingWrite,
                    ),
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
            SetupAction::new(
                ActionKey::Validate(capability.clone()),
                loads_native_code.then_some(ConsentCategory::LoadNativeCode),
                RollbackPolicy::None,
            ),
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
                SetupAction::new(
                    ActionKey::ActivateRecognition { engine, language },
                    (engine == EngineKind::Instant).then_some(ConsentCategory::LoadNativeCode),
                    RollbackPolicy::CompensatingWrite,
                ),
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

fn is_actionable(remedy: &Remedy) -> bool {
    !matches!(
        remedy,
        Remedy::ChooseAlternative { .. } | Remedy::UseDeterministicFormatting
    )
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
        let id = action.id.clone();
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
        let plan = SetupPlan { actions };
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlanError {
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
    use crate::{AssetId, ReadyAuthority, UnavailableReason};

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
        let model = AssetId::new("whisper-base-en-f16").unwrap();
        let records = vec![
            ready(CapabilityId::Microphone),
            missing(
                CapabilityId::AccurateRecognition {
                    language: Language::English,
                },
                Remedy::AcquireManagedAssets {
                    assets: [model].into_iter().collect(),
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
                    assets: [AssetId::new("base-en").unwrap()].into_iter().collect(),
                    loads_native_code: false,
                },
            ),
        ];
        let first = Planner::plan(&desired(), records.clone()).unwrap();
        let second = Planner::plan(&desired(), records).unwrap();
        assert_eq!(first, second);
        first.validate().unwrap();
        assert!(matches!(
            first.actions[0].action.key,
            ActionKey::DownloadArtifact { .. }
        ));
        assert!(matches!(
            first.actions.last().unwrap().action.key,
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
            .position(|action| matches!(action.action.key, ActionKey::StartExternalTool { .. }))
            .unwrap();
        let pull_index = plan
            .actions
            .iter()
            .position(|action| matches!(action.action.key, ActionKey::PullOllamaModel { .. }))
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
        let assets = [
            AssetId::new("vosk-runtime").unwrap(),
            AssetId::new("vosk-en-model").unwrap(),
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
                        assets,
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
                    .consent
                    .contains(&ConsentCategory::LoadNativeCode)
            })
            .count();
        assert_eq!(native_actions, 2);
    }
}
