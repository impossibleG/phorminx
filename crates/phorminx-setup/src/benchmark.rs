use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Language, Sha256Digest};

/// A bounded identifier which cannot carry prose, paths, prompts, or transcript
/// content. It is suitable for persisted content-free evidence.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentFreeId(String);

impl ContentFreeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContentFreeIdError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(ContentFreeIdError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentFreeId {
    type Error = ContentFreeIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ContentFreeId> for String {
    fn from(value: ContentFreeId) -> Self {
        value.0
    }
}

impl fmt::Display for ContentFreeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "identifier must contain 1-128 ASCII letters, digits, dots, dashes, underscores, or colons"
)]
pub struct ContentFreeIdError;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Accurate,
    Instant,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Cpu,
    Vulkan,
    VoskNative,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelClass {
    Tiny,
    Base,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalCondition {
    Nominal,
    Elevated,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentionCondition {
    Idle,
    Contended,
    Unknown,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct BenchmarkProtocol {
    pub protocol_id: ContentFreeId,
    pub minimum_speech_samples: u32,
    pub minimum_silence_samples: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct BenchmarkSampleSummary {
    pub speech_samples: u32,
    pub silence_samples: u32,
    pub run_count: u32,
    pub cold_load_ms: u64,
    pub warm_load_ms: u64,
    pub release_p50_ms: u64,
    pub release_p95_ms: u64,
    pub release_dispersion_ms: u64,
    pub confidence_per_mille: u16,
    pub realtime_factor_milli: u32,
    pub word_error_per_mille: u16,
    pub character_error_per_mille: u16,
    pub protected_token_exact_per_mille: u16,
    pub hallucination_per_mille: u16,
    pub peak_working_set_mib: u32,
    pub available_memory_mib: u32,
    pub fallback_count: u32,
    pub thermal_condition: ThermalCondition,
    pub contention_condition: ContentionCondition,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct BenchmarkEvidence {
    pub schema_version: u32,
    pub protocol_id: ContentFreeId,
    pub build_id: ContentFreeId,
    pub candidate_id: ContentFreeId,
    pub device_id: ContentFreeId,
    pub driver_id: ContentFreeId,
    pub engine: EngineKind,
    pub backend: BackendKind,
    pub model_class: ModelClass,
    pub model_digest: Sha256Digest,
    pub supported_languages: BTreeSet<Language>,
    /// PT-BR Instant is eligible only after the exact artifact/protocol has a
    /// corpus qualification. Accurate multilingual evidence does not need it.
    pub pt_brazil_instant_certified: bool,
    pub loaded: bool,
    pub measurements: BenchmarkSampleSummary,
}

impl BenchmarkEvidence {
    pub const SCHEMA_VERSION: u32 = 2;

    #[must_use]
    pub fn is_structurally_valid(&self) -> bool {
        self.schema_version == Self::SCHEMA_VERSION
            && self.measurements.word_error_per_mille <= 1_000
            && self.measurements.character_error_per_mille <= 1_000
            && self.measurements.protected_token_exact_per_mille <= 1_000
            && self.measurements.hallucination_per_mille <= 1_000
            && self.measurements.confidence_per_mille <= 1_000
            && self.measurements.release_p50_ms <= self.measurements.release_p95_ms
            && self.measurements.run_count
                >= self
                    .measurements
                    .speech_samples
                    .saturating_add(self.measurements.silence_samples)
            && matches!(
                (self.engine, self.backend),
                (EngineKind::Instant, BackendKind::VoskNative)
                    | (EngineKind::Accurate, BackendKind::Cpu | BackendKind::Vulkan)
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_free_ids_reject_text_and_paths() {
        for value in [
            "",
            "contains spaces",
            r"C:\Users\Alice\model.bin",
            "a/user/path",
            "hello\nworld",
        ] {
            assert!(ContentFreeId::new(value).is_err());
        }
        assert!(ContentFreeId::new("protocol-v1:accurate").is_ok());
    }

    #[test]
    fn deserialization_cannot_bypass_identifier_validation() {
        let invalid = serde_json::from_str::<ContentFreeId>(r#""private transcript""#);
        assert!(invalid.is_err());
    }
}
