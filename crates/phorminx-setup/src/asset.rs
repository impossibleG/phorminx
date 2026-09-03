use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{ActionId, ActionKey, ActionPhase, ContentFreeId, Generation};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AssetId(String);

impl AssetId {
    pub fn new(value: impl Into<String>) -> Result<Self, OwnershipError> {
        let value = value.into();
        ContentFreeId::new(value.clone()).map_err(|_| OwnershipError::InvalidAssetId)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AssetId {
    type Error = OwnershipError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AssetId> for String {
    fn from(value: AssetId) -> Self {
        value.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Data,
    NativeLibrary,
    Executable,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SignerRequirement {
    pub publisher: ContentFreeId,
    pub certificate_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ArtifactDescriptor {
    asset_id: AssetId,
    digest: Sha256Digest,
    size_bytes: u64,
    vendor: ContentFreeId,
    version: ContentFreeId,
    license: ContentFreeId,
    source_url: String,
    kind: ArtifactKind,
    signer: Option<SignerRequirement>,
}

#[derive(Deserialize)]
struct ArtifactDescriptorWire {
    asset_id: AssetId,
    digest: Sha256Digest,
    size_bytes: u64,
    vendor: ContentFreeId,
    version: ContentFreeId,
    license: ContentFreeId,
    source_url: String,
    kind: ArtifactKind,
    signer: Option<SignerRequirement>,
}

impl ArtifactDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        asset_id: AssetId,
        digest: Sha256Digest,
        size_bytes: u64,
        vendor: ContentFreeId,
        version: ContentFreeId,
        license: ContentFreeId,
        source_url: impl Into<String>,
        kind: ArtifactKind,
        signer: Option<SignerRequirement>,
    ) -> Result<Self, OwnershipError> {
        let descriptor = Self {
            asset_id,
            digest,
            size_bytes,
            vendor,
            version,
            license,
            source_url: source_url.into(),
            kind,
            signer,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), OwnershipError> {
        if self.size_bytes == 0
            || self.source_url.len() > 2_048
            || !self.source_url.starts_with("https://")
            || !self.source_url.is_ascii()
            || self
                .source_url
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
            || (self.kind == ArtifactKind::Executable && self.signer.is_none())
            || (self.kind != ArtifactKind::Executable && self.signer.is_some())
        {
            return Err(OwnershipError::InvalidArtifactDescriptor);
        }
        Ok(())
    }

    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    #[must_use]
    pub const fn vendor(&self) -> &ContentFreeId {
        &self.vendor
    }

    #[must_use]
    pub const fn version(&self) -> &ContentFreeId {
        &self.version
    }

    #[must_use]
    pub const fn license(&self) -> &ContentFreeId {
        &self.license
    }

    #[must_use]
    pub fn source_url(&self) -> &str {
        &self.source_url
    }

    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        self.kind
    }

    #[must_use]
    pub const fn signer(&self) -> Option<&SignerRequirement> {
        self.signer.as_ref()
    }

    pub fn verify_acquired(&self, acquired: &AcquiredArtifact) -> Result<(), OwnershipError> {
        if self.digest != acquired.digest || self.size_bytes != acquired.size_bytes {
            return Err(OwnershipError::ArtifactIdentityMismatch);
        }
        match (&self.signer, &acquired.signer) {
            (Some(expected), Some(actual)) if expected == actual => Ok(()),
            (None, None) => Ok(()),
            _ => Err(OwnershipError::SignerMismatch),
        }
    }
}

impl<'de> Deserialize<'de> for ArtifactDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ArtifactDescriptorWire::deserialize(deserializer)?;
        Self::new(
            wire.asset_id,
            wire.digest,
            wire.size_bytes,
            wire.vendor,
            wire.version,
            wire.license,
            wire.source_url,
            wire.kind,
            wire.signer,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquiredArtifact {
    pub digest: Sha256Digest,
    pub size_bytes: u64,
    pub signer: Option<SignerRequirement>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self, OwnershipError> {
        let value = value.into().to_ascii_lowercase();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(OwnershipError::InvalidDigest);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = OwnershipError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ManagedSlot(String);

impl ManagedSlot {
    pub fn new(value: impl Into<String>) -> Result<Self, OwnershipError> {
        let value = value.into().replace('\\', "/").to_ascii_lowercase();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.ends_with('/')
            && !value.contains(':')
            && value.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.ends_with('.')
                    && !is_windows_device_name(part)
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            });
        if !valid {
            return Err(OwnershipError::InvalidManagedSlot);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn staging_for(action: &ActionId) -> Self {
        let mut encoded = String::with_capacity(action.as_str().len() * 2);
        for byte in action.as_str().bytes() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(format!("staging/action-{encoded}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_windows_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or_default();
    matches!(stem, "con" | "prn" | "aux" | "nul")
        || stem.strip_prefix("com").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("lpt").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
}

impl TryFrom<String> for ManagedSlot {
    type Error = OwnershipError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ManagedSlot> for String {
    fn from(value: ManagedSlot) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedAsset {
    descriptor: ArtifactDescriptor,
    slot: ManagedSlot,
}

impl ManagedAsset {
    pub fn from_verified(
        descriptor: ArtifactDescriptor,
        acquired: &AcquiredArtifact,
        slot: ManagedSlot,
    ) -> Result<Self, OwnershipError> {
        descriptor.verify_acquired(acquired)?;
        Ok(Self { descriptor, slot })
    }

    #[must_use]
    pub const fn asset_id(&self) -> &AssetId {
        self.descriptor.asset_id()
    }

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        self.descriptor.digest()
    }

    #[must_use]
    pub const fn slot(&self) -> &ManagedSlot {
        &self.slot
    }

    #[must_use]
    pub const fn descriptor(&self) -> &ArtifactDescriptor {
        &self.descriptor
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "ownership")]
pub enum AssetLocation {
    Managed(Box<ManagedAsset>),
    /// A user-owned path. It is selectable, but never becomes cleanup authority.
    Custom {
        path: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetReceipt {
    pub schema_version: u32,
    pub asset: ManagedAsset,
    pub installed_at_epoch_ms: u64,
}

impl AssetReceipt {
    pub const SCHEMA_VERSION: u32 = 1;

    #[must_use]
    pub const fn new(asset: ManagedAsset, installed_at_epoch_ms: u64) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            asset,
            installed_at_epoch_ms,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AssetRegistry {
    receipts: BTreeMap<AssetId, AssetReceipt>,
}

impl AssetRegistry {
    pub fn insert(&mut self, receipt: AssetReceipt) -> Result<(), OwnershipError> {
        if receipt.schema_version != AssetReceipt::SCHEMA_VERSION {
            return Err(OwnershipError::UnsupportedReceiptVersion);
        }
        let id = receipt.asset.asset_id().clone();
        if self.receipts.contains_key(&id) {
            return Err(OwnershipError::DuplicateReceipt);
        }
        if self
            .receipts
            .values()
            .any(|existing| existing.asset.slot == receipt.asset.slot)
        {
            return Err(OwnershipError::SlotAlreadyOwned);
        }
        self.receipts.insert(id, receipt);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), OwnershipError> {
        let mut slots = std::collections::BTreeSet::new();
        for (id, receipt) in &self.receipts {
            if receipt.schema_version != AssetReceipt::SCHEMA_VERSION {
                return Err(OwnershipError::UnsupportedReceiptVersion);
            }
            if id != receipt.asset.asset_id() {
                return Err(OwnershipError::ReceiptIdentityMismatch);
            }
            if !slots.insert(receipt.asset.slot()) {
                return Err(OwnershipError::SlotAlreadyOwned);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn receipt(&self, id: &AssetId) -> Option<&AssetReceipt> {
        self.receipts.get(id)
    }

    /// Returns a cleanup target only when every identity component matches an
    /// installed managed receipt. Custom paths can never enter this API.
    #[must_use]
    pub fn cleanup_target(&self, asset: &ManagedAsset) -> Option<&ManagedSlot> {
        self.receipts
            .get(asset.asset_id())
            .filter(|receipt| receipt.asset == *asset)
            .map(|receipt| receipt.asset.slot())
    }

    pub fn forget(&mut self, asset: &ManagedAsset) -> Result<AssetReceipt, OwnershipError> {
        if self.cleanup_target(asset).is_none() {
            return Err(OwnershipError::NotOwned);
        }
        self.receipts
            .remove(asset.asset_id())
            .ok_or(OwnershipError::NotOwned)
    }
}

#[derive(Deserialize)]
struct AssetRegistryWire {
    receipts: BTreeMap<AssetId, AssetReceipt>,
}

impl<'de> Deserialize<'de> for AssetRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AssetRegistryWire::deserialize(deserializer)?;
        let registry = Self {
            receipts: wire.receipts,
        };
        registry.validate().map_err(serde::de::Error::custom)?;
        Ok(registry)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CrashJournal {
    schema_version: u32,
    action_id: ActionId,
    action_key: ActionKey,
    generation: Generation,
    phase: ActionPhase,
    staging_slot: Option<ManagedSlot>,
    started_at_epoch_ms: u64,
}

#[derive(Deserialize)]
struct CrashJournalWire {
    schema_version: u32,
    action_id: ActionId,
    action_key: ActionKey,
    generation: Generation,
    phase: ActionPhase,
    staging_slot: Option<ManagedSlot>,
    started_at_epoch_ms: u64,
}

impl<'de> Deserialize<'de> for CrashJournal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CrashJournalWire::deserialize(deserializer)?;
        if wire.schema_version != Self::SCHEMA_VERSION {
            return Err(serde::de::Error::custom(JournalError::UnsupportedVersion));
        }
        Self::restore(
            wire.action_id,
            wire.action_key,
            wire.generation,
            wire.phase,
            wire.staging_slot,
            wire.started_at_epoch_ms,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CrashJournal {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(
        action: &crate::SetupAction,
        generation: Generation,
        phase: ActionPhase,
        staging_slot: Option<ManagedSlot>,
        started_at_epoch_ms: u64,
    ) -> Result<Self, JournalError> {
        action.validate().map_err(|_| JournalError::InvalidAction)?;
        Self::restore(
            action.id().clone(),
            action.key().clone(),
            generation,
            phase,
            staging_slot,
            started_at_epoch_ms,
        )
    }

    fn restore(
        action_id: ActionId,
        action_key: ActionKey,
        generation: Generation,
        phase: ActionPhase,
        staging_slot: Option<ManagedSlot>,
        started_at_epoch_ms: u64,
    ) -> Result<Self, JournalError> {
        let journal = Self {
            schema_version: Self::SCHEMA_VERSION,
            action_id,
            action_key,
            generation,
            phase,
            staging_slot,
            started_at_epoch_ms,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn validate(&self) -> Result<(), JournalError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(JournalError::UnsupportedVersion);
        }
        if self.action_id != ActionId::for_key(&self.action_key) {
            return Err(JournalError::ActionIdentityMismatch);
        }
        if !self.action_key.permits_phase(self.phase) {
            return Err(JournalError::PhaseIncompatible);
        }
        match (&self.staging_slot, self.action_key.rollback_policy()) {
            (Some(slot), crate::RollbackPolicy::ManagedAssetsOnly)
                if *slot == ManagedSlot::staging_for(&self.action_id) => {}
            (Some(_), crate::RollbackPolicy::ManagedAssetsOnly) => {
                return Err(JournalError::StagingOwnershipMismatch);
            }
            (Some(_), _) => return Err(JournalError::StagingNotAllowed),
            (None, _) => {}
        }
        Ok(())
    }

    #[must_use]
    pub const fn action_id(&self) -> &ActionId {
        &self.action_id
    }

    #[must_use]
    pub const fn action_key(&self) -> &ActionKey {
        &self.action_key
    }

    #[must_use]
    pub const fn phase(&self) -> ActionPhase {
        self.phase
    }

    #[must_use]
    pub const fn staging_slot(&self) -> Option<&ManagedSlot> {
        self.staging_slot.as_ref()
    }

    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    #[must_use]
    pub const fn started_at_epoch_ms(&self) -> u64 {
        self.started_at_epoch_ms
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OwnershipError {
    #[error("asset IDs must be bounded content-free identifiers")]
    InvalidAssetId,
    #[error("SHA-256 digest must contain exactly 64 hexadecimal characters")]
    InvalidDigest,
    #[error("artifact descriptor must pin HTTPS source, nonzero size, and executable signer")]
    InvalidArtifactDescriptor,
    #[error("acquired artifact does not match the pinned digest and size")]
    ArtifactIdentityMismatch,
    #[error("acquired executable signer does not match the pinned signer")]
    SignerMismatch,
    #[error("managed slots must be normalized relative identifiers")]
    InvalidManagedSlot,
    #[error("receipt schema is unsupported")]
    UnsupportedReceiptVersion,
    #[error("asset already has a managed receipt")]
    DuplicateReceipt,
    #[error("managed slot already belongs to another receipt")]
    SlotAlreadyOwned,
    #[error("receipt map key does not match the managed asset identity")]
    ReceiptIdentityMismatch,
    #[error("asset is not owned by this registry")]
    NotOwned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum JournalError {
    #[error("journal schema is unsupported")]
    UnsupportedVersion,
    #[error("journal action is invalid")]
    InvalidAction,
    #[error("journal action ID does not match its key")]
    ActionIdentityMismatch,
    #[error("journal phase is incompatible with its action")]
    PhaseIncompatible,
    #[error("journal action is not allowed to own staging data")]
    StagingNotAllowed,
    #[error("journal staging slot is not bound to this action")]
    StagingOwnershipMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: &str) -> ArtifactDescriptor {
        ArtifactDescriptor::new(
            AssetId::new(id).unwrap(),
            digest('a'),
            1024,
            ContentFreeId::new("vendor").unwrap(),
            ContentFreeId::new("v1").unwrap(),
            ContentFreeId::new("apache-2.0").unwrap(),
            format!("https://example.invalid/{id}.zip"),
            ArtifactKind::Data,
            None,
        )
        .unwrap()
    }

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::new(byte.to_string().repeat(64)).unwrap()
    }

    fn managed(id: &str, slot: &str) -> ManagedAsset {
        let descriptor = descriptor(id);
        let acquired = AcquiredArtifact {
            digest: descriptor.digest().clone(),
            size_bytes: descriptor.size_bytes(),
            signer: descriptor.signer().cloned(),
        };
        ManagedAsset::from_verified(descriptor, &acquired, ManagedSlot::new(slot).unwrap()).unwrap()
    }

    #[test]
    fn managed_slots_reject_escape_and_absolute_forms() {
        for path in [
            "",
            "../x",
            "assets/../x",
            "/assets/x",
            "C:/x",
            "assets//x",
            "assets/name.",
            "assets/CON",
            "assets/com1.dll",
            "assets/LPT9",
        ] {
            assert_eq!(
                ManagedSlot::new(path).unwrap_err(),
                OwnershipError::InvalidManagedSlot
            );
        }
        assert_eq!(
            ManagedSlot::new(r"Assets\VOSK\Model").unwrap().as_str(),
            "assets/vosk/model"
        );
    }

    #[test]
    fn executable_descriptor_requires_exact_signer_and_acquired_identity() {
        let signer = SignerRequirement {
            publisher: ContentFreeId::new("ollama-inc").unwrap(),
            certificate_sha256: digest('b'),
        };
        let descriptor = ArtifactDescriptor::new(
            AssetId::new("ollama-installer").unwrap(),
            digest('a'),
            4096,
            ContentFreeId::new("ollama-inc").unwrap(),
            ContentFreeId::new("1.0.0").unwrap(),
            ContentFreeId::new("mit").unwrap(),
            "https://example.invalid/ollama.exe",
            ArtifactKind::Executable,
            Some(signer.clone()),
        )
        .unwrap();
        assert_eq!(
            descriptor.verify_acquired(&AcquiredArtifact {
                digest: digest('a'),
                size_bytes: 4095,
                signer: Some(signer.clone()),
            }),
            Err(OwnershipError::ArtifactIdentityMismatch)
        );
        assert_eq!(
            descriptor.verify_acquired(&AcquiredArtifact {
                digest: digest('a'),
                size_bytes: 4096,
                signer: None,
            }),
            Err(OwnershipError::SignerMismatch)
        );
        assert!(
            ManagedAsset::from_verified(
                descriptor,
                &AcquiredArtifact {
                    digest: digest('a'),
                    size_bytes: 4096,
                    signer: Some(signer),
                },
                ManagedSlot::new("assets/ollama-installer").unwrap(),
            )
            .is_ok()
        );
    }

    #[test]
    fn deserialization_revalidates_artifact_descriptor() {
        let invalid = serde_json::json!({
            "asset_id": "installer",
            "digest": "a".repeat(64),
            "size_bytes": 1,
            "vendor": "vendor",
            "version": "v1",
            "license": "mit",
            "source_url": "http://insecure.invalid/installer.exe",
            "kind": "executable",
            "signer": null
        });
        assert!(serde_json::from_value::<ArtifactDescriptor>(invalid).is_err());
    }

    #[test]
    fn registry_never_confers_ownership_from_a_custom_location() {
        let custom = AssetLocation::Custom {
            path: r"C:\user-models\model.bin".to_owned(),
        };
        assert!(matches!(custom, AssetLocation::Custom { .. }));

        let mut registry = AssetRegistry::default();
        let recorded = managed("whisper-base-en", "assets/whisper/base-en");
        registry
            .insert(AssetReceipt::new(recorded.clone(), 10))
            .unwrap();

        let impostor_descriptor = ArtifactDescriptor::new(
            recorded.asset_id().clone(),
            digest('b'),
            1024,
            ContentFreeId::new("vendor").unwrap(),
            ContentFreeId::new("v1").unwrap(),
            ContentFreeId::new("apache-2.0").unwrap(),
            "https://example.invalid/impostor.zip",
            ArtifactKind::Data,
            None,
        )
        .unwrap();
        let impostor_acquired = AcquiredArtifact {
            digest: impostor_descriptor.digest().clone(),
            size_bytes: impostor_descriptor.size_bytes(),
            signer: None,
        };
        let impostor = ManagedAsset::from_verified(
            impostor_descriptor,
            &impostor_acquired,
            recorded.slot().clone(),
        )
        .unwrap();
        assert_eq!(registry.cleanup_target(&impostor), None);
        assert_eq!(registry.cleanup_target(&recorded), Some(recorded.slot()));
    }

    #[test]
    fn receipt_slots_cannot_be_shared() {
        let mut registry = AssetRegistry::default();
        registry
            .insert(AssetReceipt::new(managed("one", "assets/shared"), 1))
            .unwrap();
        let descriptor = ArtifactDescriptor::new(
            AssetId::new("two").unwrap(),
            digest('b'),
            1024,
            ContentFreeId::new("vendor").unwrap(),
            ContentFreeId::new("v1").unwrap(),
            ContentFreeId::new("apache-2.0").unwrap(),
            "https://example.invalid/two.zip",
            ArtifactKind::Data,
            None,
        )
        .unwrap();
        let acquired = AcquiredArtifact {
            digest: descriptor.digest().clone(),
            size_bytes: descriptor.size_bytes(),
            signer: None,
        };
        let other = ManagedAsset::from_verified(
            descriptor,
            &acquired,
            ManagedSlot::new("assets/shared").unwrap(),
        )
        .unwrap();
        assert_eq!(
            registry.insert(AssetReceipt::new(other, 2)).unwrap_err(),
            OwnershipError::SlotAlreadyOwned
        );
    }

    #[test]
    fn journal_can_only_name_owned_staging_namespace() {
        let action = crate::SetupAction::for_key(ActionKey::DownloadArtifact {
            artifact: descriptor("runtime"),
        })
        .unwrap();
        let result = CrashJournal::new(
            &action,
            Generation(1),
            ActionPhase::Downloading,
            Some(ManagedSlot::new("assets/not-staging").unwrap()),
            1,
        );
        assert_eq!(result.unwrap_err(), JournalError::StagingOwnershipMismatch);
    }

    #[test]
    fn persisted_registry_revalidates_ownership_invariants() {
        let invalid = format!(
            r#"{{"receipts":{{"wrong":{{"schema_version":1,"asset":{{"asset_id":"right","digest":"{}","slot":"assets/right"}},"installed_at_epoch_ms":1}}}}}}"#,
            "a".repeat(64)
        );
        assert!(serde_json::from_str::<AssetRegistry>(&invalid).is_err());
    }

    #[test]
    fn persisted_journal_rejects_non_staging_cleanup_target() {
        let action_key = ActionKey::DownloadArtifact {
            artifact: descriptor("runtime"),
        };
        let journal = serde_json::json!({
            "schema_version": 1,
            "action_id": ActionId::for_key(&action_key),
            "action_key": action_key,
            "generation": 1,
            "phase": "downloading",
            "staging_slot": "assets/not-staging",
            "started_at_epoch_ms": 1
        });
        assert!(serde_json::from_value::<CrashJournal>(journal).is_err());
    }

    #[test]
    fn persisted_journal_rejects_relabelled_action_and_impossible_phase() {
        let probe = ActionKey::Probe(CapabilityId::Microphone);
        let relabelled = serde_json::json!({
            "schema_version": 1,
            "action_id": ActionId::for_key(&probe),
            "action_key": {"kind": "guided_external_install", "tool": "ollama"},
            "generation": 1,
            "phase": "loading",
            "staging_slot": null,
            "started_at_epoch_ms": 1
        });
        assert!(serde_json::from_value::<CrashJournal>(relabelled).is_err());

        let impossible = serde_json::json!({
            "schema_version": 1,
            "action_id": ActionId::for_key(&probe),
            "action_key": probe,
            "generation": 1,
            "phase": "downloading",
            "staging_slot": null,
            "started_at_epoch_ms": 1
        });
        assert!(serde_json::from_value::<CrashJournal>(impossible).is_err());
    }

    #[test]
    fn staging_slot_is_unambiguously_namespaced_by_action_identity() {
        let action = crate::SetupAction::for_key(ActionKey::DownloadArtifact {
            artifact: descriptor("runtime"),
        })
        .unwrap();
        let expected = ManagedSlot::staging_for(action.id());
        let journal = CrashJournal::new(
            &action,
            Generation(1),
            ActionPhase::Downloading,
            Some(expected.clone()),
            1,
        )
        .unwrap();
        assert_eq!(journal.staging_slot(), Some(&expected));

        let probe =
            crate::SetupAction::for_key(ActionKey::Probe(CapabilityId::Microphone)).unwrap();
        assert_eq!(
            CrashJournal::new(
                &probe,
                Generation(1),
                ActionPhase::Preparing,
                Some(ManagedSlot::staging_for(probe.id())),
                1,
            )
            .unwrap_err(),
            JournalError::StagingNotAllowed
        );
    }

    use crate::CapabilityId;
}
