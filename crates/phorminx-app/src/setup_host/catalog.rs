use std::collections::BTreeMap;

use phorminx_setup::{
    ArtifactDescriptor, ArtifactKind, AssetId, ContentFreeId, EngineKind, Language, PlanningPolicy,
    Sha256Digest,
};

use crate::model::pinned_models;

const VOSK_RUNTIME_BYTES: u64 = 14_882_445;
const VOSK_RUNTIME_SHA256: &str =
    "f1dcc9cca460630f81ea8f71794f69c80bed6556d2a4e6237b5785e1d2dff34b";
const VOSK_RUNTIME_URL: &str =
    "https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-win64-0.3.45.zip";
const VOSK_MODEL_EN_BYTES: u64 = 41_205_931;
const VOSK_MODEL_EN_SHA256: &str =
    "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498";
const VOSK_MODEL_EN_URL: &str =
    "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Packaging {
    RawFile { file_name: String },
    Zip { expected_root: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedArtifact {
    pub(crate) descriptor: ArtifactDescriptor,
    pub(crate) activation_slot: String,
    pub(crate) packaging: Packaging,
}

impl PinnedArtifact {
    #[must_use]
    pub const fn descriptor(&self) -> &ArtifactDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub fn activation_slot(&self) -> &str {
        &self.activation_slot
    }

    #[must_use]
    pub const fn packaging(&self) -> &Packaging {
        &self.packaging
    }
}

/// Process-local catalog compiled from Phorminx's pinned manifests.
///
/// This type is intentionally not serializable. A downloaded or persisted
/// document cannot add a source URL, digest, extraction root, or target slot.
#[derive(Clone, Debug)]
pub struct PinnedCatalog {
    artifacts: BTreeMap<AssetId, PinnedArtifact>,
    policy: PlanningPolicy,
}

impl PinnedCatalog {
    pub fn phorminx() -> Result<Self, CatalogError> {
        let mut artifacts = BTreeMap::new();
        for model in pinned_models().map_err(|error| CatalogError::Manifest(error.to_string()))? {
            let languages = match model.language.as_str() {
                "en" => vec![Language::English],
                "multilingual" => vec![Language::English, Language::PortugueseBrazil],
                other => return Err(CatalogError::UnsupportedLanguage(other.to_owned())),
            };
            let id = AssetId::new(model.id.clone())?;
            let descriptor = ArtifactDescriptor::new(
                id.clone(),
                Sha256Digest::new(model.sha256)?,
                model.bytes,
                content_id("ggerganov")?,
                content_id("whisper-c521a4b")?,
                content_id(&model.license.to_ascii_lowercase())?,
                model.url,
                ArtifactKind::Data,
                None,
                EngineKind::Accurate,
                languages,
            )?;
            insert_unique(
                &mut artifacts,
                PinnedArtifact {
                    activation_slot: format!("models/whisper/{}", model.id),
                    packaging: Packaging::RawFile {
                        file_name: model.file_name,
                    },
                    descriptor,
                },
            )?;
        }

        insert_unique(
            &mut artifacts,
            archive(
                "vosk-runtime-win64-0-3-45",
                VOSK_RUNTIME_SHA256,
                VOSK_RUNTIME_BYTES,
                "0-3-45",
                VOSK_RUNTIME_URL,
                "runtime/vosk",
                "vosk-win64-0.3.45",
                std::iter::empty(),
            )?,
        )?;
        insert_unique(
            &mut artifacts,
            archive(
                "vosk-model-small-en-us-0-15",
                VOSK_MODEL_EN_SHA256,
                VOSK_MODEL_EN_BYTES,
                "0-15",
                VOSK_MODEL_EN_URL,
                "models/vosk-model-small-en-us-0.15",
                "vosk-model-small-en-us-0.15",
                [Language::English],
            )?,
        )?;

        let policy = PlanningPolicy::phorminx(
            artifacts
                .values()
                .map(|artifact| artifact.descriptor.clone()),
        );
        Ok(Self { artifacts, policy })
    }

    #[must_use]
    pub const fn policy(&self) -> &PlanningPolicy {
        &self.policy
    }

    #[must_use]
    pub fn artifact(&self, id: &AssetId) -> Option<&PinnedArtifact> {
        self.artifacts.get(id)
    }

    pub(crate) fn exact_artifact(
        &self,
        descriptor: &ArtifactDescriptor,
    ) -> Option<&PinnedArtifact> {
        self.artifact(descriptor.asset_id())
            .filter(|artifact| artifact.descriptor() == descriptor)
    }

    pub(crate) fn preferred_recognition_artifacts(
        &self,
        engine: EngineKind,
        language: Language,
    ) -> Vec<ArtifactDescriptor> {
        let ids: &[&str] = match (engine, language) {
            (EngineKind::Accurate, Language::English) => &["whisper-base-en-f16"],
            (EngineKind::Accurate, Language::PortugueseBrazil) => {
                &["whisper-base-multilingual-f16"]
            }
            (EngineKind::Instant, Language::English) => {
                &["vosk-runtime-win64-0-3-45", "vosk-model-small-en-us-0-15"]
            }
            // There is no locally pinned PT-BR Vosk archive yet. Never silently
            // substitute the English model or invent a download identity.
            (EngineKind::Instant, Language::PortugueseBrazil) => &[],
        };
        ids.iter()
            .filter_map(|id| AssetId::new(*id).ok())
            .filter_map(|id| self.artifact(&id))
            .map(|artifact| artifact.descriptor.clone())
            .collect()
    }

    #[cfg(test)]
    pub(super) fn for_test(
        artifacts: impl IntoIterator<Item = PinnedArtifact>,
    ) -> Result<Self, CatalogError> {
        let mut indexed = BTreeMap::new();
        for artifact in artifacts {
            insert_unique(&mut indexed, artifact)?;
        }
        let policy =
            PlanningPolicy::phorminx(indexed.values().map(|artifact| artifact.descriptor.clone()));
        Ok(Self {
            artifacts: indexed,
            policy,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn archive(
    id: &str,
    digest: &str,
    bytes: u64,
    version: &str,
    url: &str,
    activation_slot: &str,
    expected_root: &str,
    languages: impl IntoIterator<Item = Language>,
) -> Result<PinnedArtifact, CatalogError> {
    Ok(PinnedArtifact {
        descriptor: ArtifactDescriptor::new(
            AssetId::new(id)?,
            Sha256Digest::new(digest)?,
            bytes,
            content_id("alphacephei")?,
            content_id(version)?,
            content_id("apache-2-0")?,
            url,
            // The acquired object is a pinned archive, not a directly loaded
            // library. Native loading still requires separate explicit consent.
            ArtifactKind::Data,
            None,
            EngineKind::Instant,
            languages,
        )?,
        activation_slot: activation_slot.to_owned(),
        packaging: Packaging::Zip {
            expected_root: expected_root.to_owned(),
        },
    })
}

fn insert_unique(
    artifacts: &mut BTreeMap<AssetId, PinnedArtifact>,
    artifact: PinnedArtifact,
) -> Result<(), CatalogError> {
    let id = artifact.descriptor.asset_id().clone();
    if artifacts.insert(id, artifact).is_some() {
        return Err(CatalogError::DuplicateArtifact);
    }
    Ok(())
}

fn content_id(value: &str) -> Result<ContentFreeId, CatalogError> {
    ContentFreeId::new(value).map_err(|_| CatalogError::InvalidCatalog)
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the embedded Whisper manifest is invalid: {0}")]
    Manifest(String),
    #[error("the embedded catalog contains unsupported language {0}")]
    UnsupportedLanguage(String),
    #[error("the embedded catalog contains an invalid identity")]
    InvalidCatalog,
    #[error("the embedded catalog contains a duplicate artifact")]
    DuplicateArtifact,
    #[error(transparent)]
    Ownership(#[from] phorminx_setup::OwnershipError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_pinned_and_has_no_invented_portuguese_vosk_model() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        assert_eq!(
            catalog
                .preferred_recognition_artifacts(EngineKind::Instant, Language::English)
                .len(),
            2
        );
        assert!(
            catalog
                .preferred_recognition_artifacts(EngineKind::Instant, Language::PortugueseBrazil)
                .is_empty()
        );
        assert_eq!(
            catalog
                .preferred_recognition_artifacts(EngineKind::Accurate, Language::PortugueseBrazil)
                .len(),
            1
        );
    }

    #[test]
    fn runtime_recipe_has_a_bounded_expected_root() {
        let catalog = PinnedCatalog::phorminx().unwrap();
        let id = AssetId::new("vosk-runtime-win64-0-3-45").unwrap();
        assert_eq!(
            catalog.artifact(&id).unwrap().packaging(),
            &Packaging::Zip {
                expected_root: "vosk-win64-0.3.45".to_owned()
            }
        );
    }
}
