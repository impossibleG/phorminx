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
        let value = value.into().replace('\\', "/");
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.ends_with('/')
            && !value.contains(':')
            && value.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
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
    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    asset_id: AssetId,
    digest: Sha256Digest,
    slot: ManagedSlot,
}

impl ManagedAsset {
    #[must_use]
    pub const fn new(asset_id: AssetId, digest: Sha256Digest, slot: ManagedSlot) -> Self {
        Self {
            asset_id,
            digest,
            slot,
        }
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
    pub const fn slot(&self) -> &ManagedSlot {
        &self.slot
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "ownership")]
pub enum AssetLocation {
    Managed(ManagedAsset),
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
        let id = receipt.asset.asset_id.clone();
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
    pub schema_version: u32,
    pub action_id: ActionId,
    pub action_key: ActionKey,
    pub generation: Generation,
    pub phase: ActionPhase,
    pub staging_slot: Option<ManagedSlot>,
    pub started_at_epoch_ms: u64,
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
        Self::new(
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
        action_id: ActionId,
        action_key: ActionKey,
        generation: Generation,
        phase: ActionPhase,
        staging_slot: Option<ManagedSlot>,
        started_at_epoch_ms: u64,
    ) -> Result<Self, JournalError> {
        if staging_slot
            .as_ref()
            .is_some_and(|slot| !slot.as_str().starts_with("staging/"))
        {
            return Err(JournalError::NotAStagingSlot);
        }
        Ok(Self {
            schema_version: Self::SCHEMA_VERSION,
            action_id,
            action_key,
            generation,
            phase,
            staging_slot,
            started_at_epoch_ms,
        })
    }

    pub fn validate(&self) -> Result<(), JournalError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(JournalError::UnsupportedVersion);
        }
        if self
            .staging_slot
            .as_ref()
            .is_some_and(|slot| !slot.as_str().starts_with("staging/"))
        {
            return Err(JournalError::NotAStagingSlot);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OwnershipError {
    #[error("asset IDs must be bounded content-free identifiers")]
    InvalidAssetId,
    #[error("SHA-256 digest must contain exactly 64 hexadecimal characters")]
    InvalidDigest,
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
    #[error("journal cleanup target is not in the staging namespace")]
    NotAStagingSlot,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest::new(byte.to_string().repeat(64)).unwrap()
    }

    fn managed(id: &str, slot: &str) -> ManagedAsset {
        ManagedAsset::new(
            AssetId::new(id).unwrap(),
            digest('a'),
            ManagedSlot::new(slot).unwrap(),
        )
    }

    #[test]
    fn managed_slots_reject_escape_and_absolute_forms() {
        for path in ["", "../x", "assets/../x", "/assets/x", "C:/x", "assets//x"] {
            assert_eq!(
                ManagedSlot::new(path).unwrap_err(),
                OwnershipError::InvalidManagedSlot
            );
        }
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

        let impostor = ManagedAsset::new(
            recorded.asset_id().clone(),
            digest('b'),
            recorded.slot().clone(),
        );
        assert_eq!(registry.cleanup_target(&impostor), None);
        assert_eq!(registry.cleanup_target(&recorded), Some(recorded.slot()));
    }

    #[test]
    fn receipt_slots_cannot_be_shared() {
        let mut registry = AssetRegistry::default();
        registry
            .insert(AssetReceipt::new(managed("one", "assets/shared"), 1))
            .unwrap();
        let other = ManagedAsset::new(
            AssetId::new("two").unwrap(),
            digest('b'),
            ManagedSlot::new("assets/shared").unwrap(),
        );
        assert_eq!(
            registry.insert(AssetReceipt::new(other, 2)).unwrap_err(),
            OwnershipError::SlotAlreadyOwned
        );
    }

    #[test]
    fn journal_can_only_name_owned_staging_namespace() {
        let result = CrashJournal::new(
            ActionId::for_key(&ActionKey::Probe(CapabilityId::Microphone)),
            ActionKey::Probe(CapabilityId::Microphone),
            Generation(1),
            ActionPhase::Downloading,
            Some(ManagedSlot::new("assets/not-staging").unwrap()),
            1,
        );
        assert_eq!(result.unwrap_err(), JournalError::NotAStagingSlot);
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
        let action_key = ActionKey::Probe(CapabilityId::Microphone);
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

    use crate::CapabilityId;
}
