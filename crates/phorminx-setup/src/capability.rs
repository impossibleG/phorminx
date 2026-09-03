use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{ArtifactDescriptor, ContentFreeId, Sha256Digest};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    English,
    PortugueseBrazil,
}

impl Language {
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::PortugueseBrazil => "pt-br",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CapabilityId {
    Microphone,
    AccurateRecognition { language: Language },
    InstantRecognition { language: Language },
    OllamaDaemon,
    OllamaModel { digest: Sha256Digest },
    LaunchAtLogin,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Required,
    Optional,
    NotRequested,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Generation(pub u64);

impl Generation {
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Observation<T> {
    #[default]
    Unknown,
    Checking {
        generation: Generation,
    },
    Observed {
        generation: Generation,
        value: T,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyAuthority {
    ResidentWorker,
    FullProbe,
    OperatingSystemReadback,
    LoopbackHealth,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum UnavailableReason {
    Missing,
    PermissionDenied,
    UnsupportedLanguage,
    IncompatibleArtifact,
    LoadFailed,
    Corrupt,
    InsufficientResources,
    ExternalToolStopped,
    ApiMismatch,
    DifferentConfiguration,
    NoPinnedArtifact,
    ProbeFailed,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum DegradedReason {
    CpuFallback,
    DeterministicFormattingFallback,
    SavedMicrophoneUnavailable,
    QualityWarning,
    RestartRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Usability {
    Unavailable {
        reason: UnavailableReason,
    },
    InstalledUnvalidated,
    Ready {
        authority: ReadyAuthority,
    },
    Degraded {
        authority: ReadyAuthority,
        reason: DegradedReason,
    },
}

impl Usability {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        match self {
            Self::Ready { .. } => true,
            Self::Degraded { reason, .. } => !matches!(reason, DegradedReason::RestartRequired),
            Self::Unavailable { .. } | Self::InstalledUnvalidated => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "value")]
pub enum CapabilityValue {
    Microphone {
        selected_is_available: bool,
    },
    Recognition {
        engine: crate::EngineKind,
        language: Language,
        model_digest: Option<Sha256Digest>,
    },
    ExternalTool {
        version: Option<ContentFreeId>,
    },
    Model {
        digest: Sha256Digest,
    },
    LaunchAtLogin {
        enabled: bool,
        exact_command: bool,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Remedy {
    Probe,
    AcquireManagedAssets {
        artifacts: BTreeSet<ArtifactDescriptor>,
        loads_native_code: bool,
    },
    ImportVerifiedAssets {
        artifacts: BTreeSet<ArtifactDescriptor>,
        loads_native_code: bool,
    },
    ValidateInstalled,
    ActivateRecognition,
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
    ChooseAlternative {
        capability: CapabilityId,
    },
    UseDeterministicFormatting,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRecord {
    pub id: CapabilityId,
    #[serde(default)]
    pub value: Option<CapabilityValue>,
    pub usability: Usability,
    #[serde(default)]
    pub remedies: BTreeSet<Remedy>,
}

impl CapabilityRecord {
    #[must_use]
    pub fn new(id: CapabilityId, usability: Usability) -> Self {
        Self {
            id,
            value: None,
            usability,
            remedies: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn with_value(mut self, value: CapabilityValue) -> Self {
        self.value = Some(value);
        self
    }

    #[must_use]
    pub fn with_remedies(mut self, remedies: impl IntoIterator<Item = Remedy>) -> Self {
        self.remedies.extend(remedies);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Capability<T> {
    pub id: CapabilityId,
    pub requirement: Requirement,
    pub observation: Observation<T>,
    pub usability: Usability,
    #[serde(default)]
    pub remedies: BTreeSet<Remedy>,
}

impl<T> Capability<T> {
    #[must_use]
    pub fn begin_check(&mut self) -> Generation {
        let generation = match self.observation {
            Observation::Unknown => Generation(1),
            Observation::Checking { generation } | Observation::Observed { generation, .. } => {
                generation.next()
            }
        };
        self.observation = Observation::Checking { generation };
        generation
    }

    /// Applies a probe result only if it belongs to the newest check.
    #[must_use]
    pub fn accept_observation(
        &mut self,
        generation: Generation,
        value: T,
        usability: Usability,
    ) -> bool {
        if !matches!(
            self.observation,
            Observation::Checking { generation: expected } if expected == generation
        ) {
            return false;
        }
        self.observation = Observation::Observed { generation, value };
        self.usability = usability;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_probe_result_is_rejected() {
        let mut capability = Capability {
            id: CapabilityId::Microphone,
            requirement: Requirement::Required,
            observation: Observation::Unknown,
            usability: Usability::Unavailable {
                reason: UnavailableReason::Missing,
            },
            remedies: BTreeSet::new(),
        };
        let stale = capability.begin_check();
        let current = capability.begin_check();
        assert!(!capability.accept_observation(
            stale,
            CapabilityValue::Microphone {
                selected_is_available: false
            },
            Usability::Unavailable {
                reason: UnavailableReason::Missing
            }
        ));
        assert!(capability.accept_observation(
            current,
            CapabilityValue::Microphone {
                selected_is_available: true
            },
            Usability::Ready {
                authority: ReadyAuthority::FullProbe
            }
        ));
    }

    #[test]
    fn layout_presence_is_not_ready() {
        assert!(!Usability::InstalledUnvalidated.is_ready());
    }
}
