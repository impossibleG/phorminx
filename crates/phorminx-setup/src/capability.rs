use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

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
    },
    ImportVerifiedAssets {
        artifacts: BTreeSet<ArtifactDescriptor>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityRecord {
    id: CapabilityId,
    #[serde(default)]
    value: Option<CapabilityValue>,
    usability: Usability,
    #[serde(default)]
    remedies: BTreeSet<Remedy>,
}

impl CapabilityRecord {
    pub fn new(
        id: CapabilityId,
        value: Option<CapabilityValue>,
        usability: Usability,
    ) -> Result<Self, CapabilityRecordError> {
        let record = Self {
            id,
            value,
            usability,
            remedies: BTreeSet::new(),
        };
        record.validate()?;
        Ok(record)
    }

    pub fn with_remedies(
        mut self,
        remedies: impl IntoIterator<Item = Remedy>,
    ) -> Result<Self, CapabilityRecordError> {
        self.remedies.extend(remedies);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), CapabilityRecordError> {
        if let Some(value) = &self.value {
            validate_value(&self.id, value)?;
        }
        if let Some(authority) = ready_authority(&self.usability) {
            let value = self
                .value
                .as_ref()
                .ok_or(CapabilityRecordError::ReadyWithoutValue)?;
            validate_ready(&self.id, value, authority)?;
        }
        for remedy in &self.remedies {
            validate_remedy(&self.id, remedy)?;
        }
        Ok(())
    }

    #[must_use]
    pub const fn id(&self) -> &CapabilityId {
        &self.id
    }

    #[must_use]
    pub const fn value(&self) -> Option<&CapabilityValue> {
        self.value.as_ref()
    }

    #[must_use]
    pub const fn usability(&self) -> &Usability {
        &self.usability
    }

    #[must_use]
    pub const fn remedies(&self) -> &BTreeSet<Remedy> {
        &self.remedies
    }
}

#[derive(Deserialize)]
struct CapabilityRecordWire {
    id: CapabilityId,
    #[serde(default)]
    value: Option<CapabilityValue>,
    usability: Usability,
    #[serde(default)]
    remedies: BTreeSet<Remedy>,
}

impl<'de> Deserialize<'de> for CapabilityRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CapabilityRecordWire::deserialize(deserializer)?;
        let record = Self {
            id: wire.id,
            value: wire.value,
            usability: wire.usability,
            remedies: wire.remedies,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

fn ready_authority(usability: &Usability) -> Option<ReadyAuthority> {
    match usability {
        Usability::Ready { authority } | Usability::Degraded { authority, .. } => Some(*authority),
        Usability::Unavailable { .. } | Usability::InstalledUnvalidated => None,
    }
}

fn validate_value(id: &CapabilityId, value: &CapabilityValue) -> Result<(), CapabilityRecordError> {
    let matches = match (id, value) {
        (CapabilityId::Microphone, CapabilityValue::Microphone { .. })
        | (CapabilityId::OllamaDaemon, CapabilityValue::ExternalTool { .. })
        | (CapabilityId::LaunchAtLogin, CapabilityValue::LaunchAtLogin { .. }) => true,
        (
            CapabilityId::AccurateRecognition { language },
            CapabilityValue::Recognition {
                engine: crate::EngineKind::Accurate,
                language: actual,
                ..
            },
        )
        | (
            CapabilityId::InstantRecognition { language },
            CapabilityValue::Recognition {
                engine: crate::EngineKind::Instant,
                language: actual,
                ..
            },
        ) => language == actual,
        (
            CapabilityId::OllamaModel { digest },
            CapabilityValue::Model {
                digest: actual_digest,
            },
        ) => digest == actual_digest,
        _ => false,
    };
    matches
        .then_some(())
        .ok_or(CapabilityRecordError::ValueIdentityMismatch)
}

fn validate_ready(
    id: &CapabilityId,
    value: &CapabilityValue,
    authority: ReadyAuthority,
) -> Result<(), CapabilityRecordError> {
    let authority_valid = match id {
        CapabilityId::Microphone => {
            matches!(
                authority,
                ReadyAuthority::FullProbe | ReadyAuthority::OperatingSystemReadback
            )
        }
        CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. } => {
            matches!(
                authority,
                ReadyAuthority::ResidentWorker | ReadyAuthority::FullProbe
            )
        }
        CapabilityId::OllamaDaemon | CapabilityId::OllamaModel { .. } => {
            matches!(
                authority,
                ReadyAuthority::FullProbe | ReadyAuthority::LoopbackHealth
            )
        }
        CapabilityId::LaunchAtLogin => authority == ReadyAuthority::OperatingSystemReadback,
    };
    if !authority_valid {
        return Err(CapabilityRecordError::InvalidReadyAuthority);
    }
    let usable_value = match value {
        CapabilityValue::Microphone {
            selected_is_available,
        } => *selected_is_available,
        CapabilityValue::Recognition { model_digest, .. } => model_digest.is_some(),
        CapabilityValue::ExternalTool { version } => version.is_some(),
        CapabilityValue::Model { .. } => true,
        CapabilityValue::LaunchAtLogin { exact_command, .. } => *exact_command,
    };
    usable_value
        .then_some(())
        .ok_or(CapabilityRecordError::ReadyValueNotUsable)
}

fn validate_remedy(id: &CapabilityId, remedy: &Remedy) -> Result<(), CapabilityRecordError> {
    let valid = match (id, remedy) {
        (_, Remedy::Probe) => true,
        (
            CapabilityId::AccurateRecognition { language }
            | CapabilityId::InstantRecognition { language },
            Remedy::AcquireManagedAssets { artifacts } | Remedy::ImportVerifiedAssets { artifacts },
        ) => {
            let engine = if matches!(id, CapabilityId::InstantRecognition { .. }) {
                crate::EngineKind::Instant
            } else {
                crate::EngineKind::Accurate
            };
            !artifacts.is_empty()
                && artifacts
                    .iter()
                    .all(|artifact| artifact.supports(engine, *language))
        }
        (
            CapabilityId::AccurateRecognition { .. } | CapabilityId::InstantRecognition { .. },
            Remedy::ValidateInstalled | Remedy::ActivateRecognition,
        )
        | (CapabilityId::Microphone, Remedy::GrantMicrophoneAccess | Remedy::SelectMicrophone)
        | (CapabilityId::OllamaDaemon, Remedy::GuidedExternalInstall { .. })
        | (CapabilityId::OllamaDaemon, Remedy::StartExternalTool { .. })
        | (CapabilityId::LaunchAtLogin, Remedy::ApplyLaunchAtLogin { .. }) => true,
        (
            CapabilityId::OllamaModel { digest },
            Remedy::PullOllamaModel {
                digest: remedy_digest,
            },
        ) => digest == remedy_digest,
        (_, Remedy::ChooseAlternative { capability }) => id == capability,
        (
            CapabilityId::OllamaDaemon | CapabilityId::OllamaModel { .. },
            Remedy::UseDeterministicFormatting,
        ) => true,
        _ => false,
    };
    valid
        .then_some(())
        .ok_or(CapabilityRecordError::IncompatibleRemedy)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CapabilityRecordError {
    #[error("ready capability requires a matching observed value")]
    ReadyWithoutValue,
    #[error("capability value does not match its identity")]
    ValueIdentityMismatch,
    #[error("ready authority is not valid for this capability")]
    InvalidReadyAuthority,
    #[error("ready capability value does not prove usability")]
    ReadyValueNotUsable,
    #[error("remedy is incompatible with the capability identity")]
    IncompatibleRemedy,
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

    #[test]
    fn hostile_ready_record_cannot_forge_recognition_authority_or_value() {
        let digest = "a".repeat(64);
        let wrong_authority = serde_json::json!({
            "id": {"kind": "instant_recognition", "language": "english"},
            "value": {
                "value": "recognition",
                "engine": "instant",
                "language": "english",
                "model_digest": digest
            },
            "usability": {"state": "ready", "authority": "operating_system_readback"},
            "remedies": []
        });
        assert!(serde_json::from_value::<CapabilityRecord>(wrong_authority).is_err());

        let wrong_engine = serde_json::json!({
            "id": {"kind": "instant_recognition", "language": "english"},
            "value": {
                "value": "recognition",
                "engine": "accurate",
                "language": "english",
                "model_digest": "b".repeat(64)
            },
            "usability": {"state": "ready", "authority": "full_probe"},
            "remedies": []
        });
        assert!(serde_json::from_value::<CapabilityRecord>(wrong_engine).is_err());
    }

    #[test]
    fn model_remedy_digest_must_match_capability_identity() {
        let expected = Sha256Digest::new("a".repeat(64)).unwrap();
        let wrong = Sha256Digest::new("b".repeat(64)).unwrap();
        assert_eq!(
            CapabilityRecord::new(
                CapabilityId::OllamaModel { digest: expected },
                None,
                Usability::Unavailable {
                    reason: UnavailableReason::Missing,
                },
            )
            .unwrap()
            .with_remedies([Remedy::PullOllamaModel { digest: wrong }]),
            Err(CapabilityRecordError::IncompatibleRemedy)
        );
    }
}
