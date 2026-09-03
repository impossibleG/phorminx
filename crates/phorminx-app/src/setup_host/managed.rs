use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use phorminx_setup::{
    AcquiredArtifact, ActionId, ActionPhase, AssetReceipt, ContentFreeId, ManagedAsset,
    ManagedSlot, OwnerMarker, SetupAction, Sha256Digest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Packaging, PinnedArtifact};

const JOURNAL_FILE_LIMIT: u64 = 32 * 1024;
const IO_BUFFER_BYTES: usize = 128 * 1024;
static TRANSACTION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionLimits {
    pub maximum_entries: usize,
    pub maximum_depth: usize,
    pub maximum_expanded_bytes: u64,
    pub maximum_expansion_ratio: u64,
}

impl Default for AcquisitionLimits {
    fn default() -> Self {
        Self {
            maximum_entries: 10_000,
            maximum_depth: 24,
            maximum_expanded_bytes: 1024 * 1024 * 1024,
            maximum_expansion_ratio: 100,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationFault {
    Rename,
    ActivatedJournal,
    Receipt,
    JournalDeletion,
}

/// Fetches one exact HTTPS URL into a host-provided bounded writer.
pub trait ArtifactFetcher: Send + Sync + 'static {
    fn fetch(
        &self,
        url: &str,
        output: &mut dyn Write,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), FetchError>;
}

#[derive(Clone, Debug)]
pub struct ManagedRoot {
    root: PathBuf,
    limits: AcquisitionLimits,
    #[cfg(test)]
    activation_fault: Option<ActivationFault>,
}

impl ManagedRoot {
    /// Opens the only production managed root. Callers cannot supply a user
    /// path, so setup cleanup authority cannot escape LocalAppData/Phorminx.
    pub fn from_local_app_data() -> Result<Self, ManagedRootError> {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or(ManagedRootError::LocalAppDataUnavailable)?;
        Self::open(
            base.join("Phorminx").join("managed-setup-v1"),
            AcquisitionLimits::default(),
        )
    }

    fn open(root: PathBuf, limits: AcquisitionLimits) -> Result<Self, ManagedRootError> {
        if !root.is_absolute() {
            return Err(ManagedRootError::RootNotAbsolute);
        }
        fs::create_dir_all(&root).map_err(ManagedRootError::Io)?;
        reject_symlink(&root)?;
        for child in ["staging", "active", "journal", "receipts"] {
            let path = root.join(child);
            fs::create_dir_all(&path).map_err(ManagedRootError::Io)?;
            reject_symlink(&path)?;
        }
        Ok(Self {
            root,
            limits,
            #[cfg(test)]
            activation_fault: None,
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(
        root: &Path,
        limits: AcquisitionLimits,
    ) -> Result<Self, ManagedRootError> {
        Self::open(root.to_path_buf(), limits)
    }

    #[cfg(test)]
    fn for_test_with_fault(root: &Path, fault: ActivationFault) -> Result<Self, ManagedRootError> {
        let mut managed = Self::open(root.to_path_buf(), AcquisitionLimits::default())?;
        managed.activation_fault = Some(fault);
        Ok(managed)
    }

    pub(crate) fn install(
        &self,
        action: &SetupAction,
        pinned: &PinnedArtifact,
        fetcher: &dyn ArtifactFetcher,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(ActionPhase, u64, Option<u64>),
    ) -> Result<ManagedInstall, ManagedRootError> {
        let transaction = transaction_id()?;
        let staging = self.root.join("staging").join(transaction.as_str());
        fs::create_dir(&staging).map_err(ManagedRootError::Io)?;
        let transaction_marker = TransactionMarker::new(&transaction, action.id());
        write_json_atomic(&staging.join(".transaction.json"), &transaction_marker)?;

        let slot = ManagedSlot::new(pinned.activation_slot().to_owned())?;
        let mut journal = DiskJournal::new(
            &transaction,
            action.id(),
            &slot,
            pinned.descriptor().asset_id(),
            pinned.descriptor().digest(),
        );
        self.write_journal(&journal)?;
        let result = (|| {
            check_cancel(cancel)?;
            let archive = staging.join("artifact.download");
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&archive)
                .map_err(ManagedRootError::Io)?;
            let expected = pinned.descriptor().size_bytes();
            let mut verified = VerifyingWriter::new(file, expected, cancel, progress);
            fetcher
                .fetch(
                    pinned.descriptor().source_url(),
                    &mut verified,
                    cancel,
                    &mut |_| {},
                )
                .map_err(ManagedRootError::Fetch)?;
            let acquired = verified.finish(pinned.descriptor().digest())?;
            pinned.descriptor().verify_acquired(&acquired)?;
            journal.phase = DiskPhase::Downloaded;
            self.write_journal(&journal)?;
            progress(ActionPhase::Verifying, expected, Some(expected));
            check_cancel(cancel)?;

            let payload = staging.join("payload");
            match pinned.packaging() {
                Packaging::RawFile { file_name } => {
                    validate_leaf_name(file_name)?;
                    fs::create_dir(&payload).map_err(ManagedRootError::Io)?;
                    fs::rename(&archive, payload.join(file_name)).map_err(ManagedRootError::Io)?;
                }
                Packaging::Zip { expected_root } => {
                    extract_zip(
                        &archive,
                        &staging.join("expanded"),
                        expected_root,
                        self.limits,
                        cancel,
                    )?;
                    fs::rename(staging.join("expanded").join(expected_root), &payload)
                        .map_err(ManagedRootError::Io)?;
                }
            }
            let managed =
                ManagedAsset::from_verified(pinned.descriptor().clone(), &acquired, slot.clone())?;
            let receipt = AssetReceipt::new(transaction.clone(), managed, now_epoch_ms());
            let marker = OwnerMarker::for_receipt(&receipt);
            write_json_atomic(&payload.join(".phorminx-owner.json"), &marker)?;
            write_json_atomic(&payload.join(".phorminx-receipt.json"), &receipt)?;
            journal.phase = DiskPhase::Prepared;
            self.write_journal(&journal)?;
            check_cancel(cancel)?;

            let target = self.target(&slot)?;
            ensure_parents_without_links(&self.root.join("active"), target.parent())?;
            if target.exists() {
                return Err(ManagedRootError::TargetExists);
            }
            progress(ActionPhase::Committing, 0, Some(1));
            phorminx_windows::atomic_activate_directory(&payload, &target)
                .map_err(|error| ManagedRootError::AtomicReplace(error.to_string()))?;
            progress(ActionPhase::Committing, 1, Some(1));
            let install = ManagedInstall {
                receipt,
                target,
                transaction,
                action_id: action.id().clone(),
            };

            // Activation is the commit point. Owner and receipt witnesses are
            // already durable inside the atomically moved directory, and the
            // Prepared journal still identifies it. Bookkeeping failures after
            // this point must remain recoverable successes, never authority-
            // losing errors.
            if self.fault_at(ActivationFault::Rename) {
                return Ok(install);
            }
            journal.phase = DiskPhase::Activated;
            if self.write_journal(&journal).is_err()
                || self.fault_at(ActivationFault::ActivatedJournal)
            {
                return Ok(install);
            }
            if self.persist_receipt(&install.receipt).is_err()
                || self.fault_at(ActivationFault::Receipt)
            {
                return Ok(install);
            }
            let _ = fs::remove_file(archive);
            let _ = fs::remove_file(staging.join(".transaction.json"));
            let _ = fs::remove_dir(staging.join("expanded"));
            let _ = fs::remove_dir(&staging);
            let _ = self.remove_journal(&install.transaction);
            if self.fault_at(ActivationFault::JournalDeletion) {
                return Ok(install);
            }
            Ok(install)
        })();
        if result.is_err() {
            // Only transaction-owned staging is automatically removed here.
            // An activated target is retained for explicit marker reconciliation.
            let _ = self.remove_owned_staging(&journal);
        }
        result
    }

    pub fn recover(&self) -> Result<RecoveryReport, ManagedRootError> {
        let mut report = RecoveryReport::default();
        let journal_root = self.root.join("journal");
        for entry in fs::read_dir(&journal_root).map_err(ManagedRootError::Io)? {
            let entry = entry.map_err(ManagedRootError::Io)?;
            if !entry.file_type().map_err(ManagedRootError::Io)?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                report.rejected_journals += 1;
                continue;
            };
            if !name.ends_with(".json") {
                continue;
            }
            let journal = match read_bounded_json::<DiskJournal>(&entry.path(), JOURNAL_FILE_LIMIT)
            {
                Ok(journal) if journal.validate_file_name(&name) => journal,
                _ => {
                    report.rejected_journals += 1;
                    continue;
                }
            };
            let target = self.target(&journal.slot)?;
            // Atomic directory activation can complete immediately before the
            // journal's phase update. The prewritten owner marker is the
            // durable commit witness in either phase.
            if matches!(owner_marker_matches(&target, &journal), Ok(true)) {
                let install = self.install_from_target(&journal, target)?;
                self.persist_receipt(&install.receipt)?;
                report.activated_targets.push(install.target.clone());
                report.recovered_installs.push(install);
                let _ = self.remove_owned_staging(&journal)?;
            } else if journal.phase == DiskPhase::Activated {
                report.rejected_journals += 1;
                continue;
            } else if self.remove_owned_staging(&journal)? {
                report.removed_staging += 1;
            } else {
                report.rejected_journals += 1;
                continue;
            }
            self.remove_journal(&journal.transaction_id)?;
        }
        self.recover_orphaned_staging(&mut report)?;
        Ok(report)
    }

    pub fn rollback(&self, install: &ManagedInstall) -> Result<(), ManagedRootError> {
        let journal = DiskJournal::new(
            &install.transaction,
            &install.action_id,
            install.receipt.asset().slot(),
            install.receipt.asset().asset_id(),
            install.receipt.asset().digest(),
        );
        let active = self.root.join("active");
        reject_symlink(&self.root)?;
        reject_symlink(&active)?;
        ensure_existing_path_without_links(&active, &install.target)?;
        if install.target != self.target(install.receipt.asset().slot())?
            || !owner_marker_matches(&install.target, &journal)?
        {
            return Err(ManagedRootError::NotOwned);
        }
        ensure_tree_without_links(&install.target)?;
        fs::remove_dir_all(&install.target).map_err(ManagedRootError::Io)?;
        self.remove_receipt(install.receipt.asset().asset_id())?;
        Ok(())
    }

    /// Reloads durable receipts only when the fixed target and owner marker
    /// still match every receipt identity field.
    pub fn installed(&self) -> Result<Vec<ManagedInstall>, ManagedRootError> {
        let mut installs = Vec::new();
        for entry in fs::read_dir(self.root.join("receipts")).map_err(ManagedRootError::Io)? {
            let entry = entry.map_err(ManagedRootError::Io)?;
            if !entry.file_type().map_err(ManagedRootError::Io)?.is_file() {
                continue;
            }
            let receipt = match read_bounded_json::<AssetReceipt>(&entry.path(), JOURNAL_FILE_LIMIT)
            {
                Ok(receipt) => receipt,
                Err(_) => continue,
            };
            if entry.file_name().to_string_lossy()
                != format!("{}.json", receipt.asset().asset_id().as_str())
            {
                continue;
            }
            let target = self.target(receipt.asset().slot())?;
            if owner_matches_receipt(&target, &receipt)? {
                let action_id = ActionId::for_key(&phorminx_setup::ActionKey::DownloadArtifact {
                    artifact: Box::new(receipt.asset().descriptor().clone()),
                });
                installs.push(ManagedInstall {
                    transaction: receipt.transaction_id().clone(),
                    action_id,
                    receipt,
                    target,
                });
            }
        }
        Ok(installs)
    }

    fn target(&self, slot: &ManagedSlot) -> Result<PathBuf, ManagedRootError> {
        let active = self.root.join("active");
        let target = active.join(slot.as_str());
        if !target.starts_with(&active) {
            return Err(ManagedRootError::PathEscaped);
        }
        Ok(target)
    }

    fn fault_at(&self, point: ActivationFault) -> bool {
        #[cfg(test)]
        {
            self.activation_fault == Some(point)
        }
        #[cfg(not(test))]
        {
            let _ = point;
            false
        }
    }

    fn journal_path(&self, transaction: &ContentFreeId) -> PathBuf {
        self.root
            .join("journal")
            .join(format!("{}.json", transaction.as_str()))
    }

    fn write_journal(&self, journal: &DiskJournal) -> Result<(), ManagedRootError> {
        write_json_atomic(&self.journal_path(&journal.transaction_id), journal)
    }

    fn remove_journal(&self, transaction: &ContentFreeId) -> Result<(), ManagedRootError> {
        match fs::remove_file(self.journal_path(transaction)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ManagedRootError::Io(error)),
        }
    }

    fn receipt_path(&self, asset: &phorminx_setup::AssetId) -> PathBuf {
        self.root
            .join("receipts")
            .join(format!("{}.json", asset.as_str()))
    }

    fn persist_receipt(&self, receipt: &AssetReceipt) -> Result<(), ManagedRootError> {
        write_json_atomic(&self.receipt_path(receipt.asset().asset_id()), receipt)
    }

    fn remove_receipt(&self, asset: &phorminx_setup::AssetId) -> Result<(), ManagedRootError> {
        match fs::remove_file(self.receipt_path(asset)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ManagedRootError::Io(error)),
        }
    }

    fn install_from_target(
        &self,
        journal: &DiskJournal,
        target: PathBuf,
    ) -> Result<ManagedInstall, ManagedRootError> {
        let receipt = read_bounded_json::<AssetReceipt>(
            &target.join(".phorminx-receipt.json"),
            JOURNAL_FILE_LIMIT,
        )?;
        if receipt.transaction_id() != &journal.transaction_id
            || receipt.asset().asset_id() != &journal.asset_id
            || receipt.asset().digest() != &journal.digest
            || receipt.asset().slot() != &journal.slot
            || !owner_matches_receipt(&target, &receipt)?
        {
            return Err(ManagedRootError::NotOwned);
        }
        Ok(ManagedInstall {
            receipt,
            target,
            transaction: journal.transaction_id.clone(),
            action_id: journal.action_id.clone(),
        })
    }

    fn remove_owned_staging(&self, journal: &DiskJournal) -> Result<bool, ManagedRootError> {
        let staging = self
            .root
            .join("staging")
            .join(journal.transaction_id.as_str());
        if !staging.exists() {
            return Ok(true);
        }
        let marker = match read_bounded_json::<TransactionMarker>(
            &staging.join(".transaction.json"),
            JOURNAL_FILE_LIMIT,
        ) {
            Ok(marker) => marker,
            Err(ManagedRootError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return Err(ManagedRootError::Io(error));
            }
            Err(_) => return Ok(false),
        };
        if marker.schema_version != 1
            || marker.transaction_id != journal.transaction_id
            || marker.action_id != journal.action_id
        {
            return Ok(false);
        }
        ensure_tree_without_links(&staging)?;
        fs::remove_dir_all(staging).map_err(ManagedRootError::Io)?;
        Ok(true)
    }

    fn recover_orphaned_staging(
        &self,
        report: &mut RecoveryReport,
    ) -> Result<(), ManagedRootError> {
        let staging_root = self.root.join("staging");
        reject_symlink(&staging_root)?;
        for entry in fs::read_dir(&staging_root).map_err(ManagedRootError::Io)? {
            let entry = entry.map_err(ManagedRootError::Io)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(ManagedRootError::Io)?;
            if !metadata.is_dir() || is_link_or_reparse(&metadata) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(transaction) = ContentFreeId::new(name) else {
                continue;
            };
            if self.journal_path(&transaction).exists() {
                continue;
            }
            let marker_is_valid = match read_bounded_json::<TransactionMarker>(
                &entry.path().join(".transaction.json"),
                JOURNAL_FILE_LIMIT,
            ) {
                Ok(marker)
                    if marker.schema_version == 1 && marker.transaction_id == transaction =>
                {
                    true
                }
                _ => {
                    report.rejected_journals += 1;
                    false
                }
            };
            if !marker_is_valid {
                continue;
            }
            ensure_existing_path_without_links(&staging_root, &entry.path())?;
            ensure_tree_without_links(&entry.path())?;
            fs::remove_dir_all(entry.path()).map_err(ManagedRootError::Io)?;
            report.removed_staging += 1;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedInstall {
    pub receipt: AssetReceipt,
    pub target: PathBuf,
    transaction: ContentFreeId,
    action_id: ActionId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub removed_staging: usize,
    pub activated_targets: Vec<PathBuf>,
    pub recovered_installs: Vec<ManagedInstall>,
    pub rejected_journals: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiskPhase {
    Acquiring,
    Downloaded,
    Prepared,
    Activated,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskJournal {
    schema_version: u32,
    transaction_id: ContentFreeId,
    action_id: ActionId,
    slot: ManagedSlot,
    asset_id: phorminx_setup::AssetId,
    digest: Sha256Digest,
    phase: DiskPhase,
}

impl DiskJournal {
    fn new(
        transaction_id: &ContentFreeId,
        action_id: &ActionId,
        slot: &ManagedSlot,
        asset_id: &phorminx_setup::AssetId,
        digest: &Sha256Digest,
    ) -> Self {
        Self {
            schema_version: 1,
            transaction_id: transaction_id.clone(),
            action_id: action_id.clone(),
            slot: slot.clone(),
            asset_id: asset_id.clone(),
            digest: digest.clone(),
            phase: DiskPhase::Acquiring,
        }
    }

    fn validate_file_name(&self, name: &str) -> bool {
        self.schema_version == 1 && name == format!("{}.json", self.transaction_id.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionMarker {
    schema_version: u32,
    transaction_id: ContentFreeId,
    action_id: ActionId,
}

impl TransactionMarker {
    fn new(transaction_id: &ContentFreeId, action_id: &ActionId) -> Self {
        Self {
            schema_version: 1,
            transaction_id: transaction_id.clone(),
            action_id: action_id.clone(),
        }
    }
}

fn owner_marker_matches(target: &Path, journal: &DiskJournal) -> Result<bool, ManagedRootError> {
    let marker =
        read_bounded_json::<OwnerMarker>(&target.join(".phorminx-owner.json"), JOURNAL_FILE_LIMIT)?;
    let encoded = serde_json::to_value(marker).map_err(ManagedRootError::Json)?;
    Ok(encoded
        .get("transaction_id")
        .and_then(|value| value.as_str())
        == Some(journal.transaction_id.as_str())
        && encoded.get("asset_id")
            == Some(&serde_json::to_value(&journal.asset_id).map_err(ManagedRootError::Json)?)
        && encoded.get("digest")
            == Some(&serde_json::to_value(&journal.digest).map_err(ManagedRootError::Json)?)
        && encoded.get("slot")
            == Some(&serde_json::to_value(&journal.slot).map_err(ManagedRootError::Json)?))
}

fn owner_matches_receipt(target: &Path, receipt: &AssetReceipt) -> Result<bool, ManagedRootError> {
    let marker =
        read_bounded_json::<OwnerMarker>(&target.join(".phorminx-owner.json"), JOURNAL_FILE_LIMIT)?;
    let marker = serde_json::to_value(marker).map_err(ManagedRootError::Json)?;
    let expected =
        serde_json::to_value(OwnerMarker::for_receipt(receipt)).map_err(ManagedRootError::Json)?;
    Ok(marker == expected)
}

struct VerifyingWriter<'a> {
    file: File,
    expected: u64,
    written: u64,
    hasher: Sha256,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(ActionPhase, u64, Option<u64>),
}

impl<'a> VerifyingWriter<'a> {
    fn new(
        file: File,
        expected: u64,
        cancel: &'a AtomicBool,
        progress: &'a mut dyn FnMut(ActionPhase, u64, Option<u64>),
    ) -> Self {
        Self {
            file,
            expected,
            written: 0,
            hasher: Sha256::new(),
            cancel,
            progress,
        }
    }

    fn finish(
        mut self,
        expected_digest: &Sha256Digest,
    ) -> Result<AcquiredArtifact, ManagedRootError> {
        self.file.flush().map_err(ManagedRootError::Io)?;
        self.file.sync_all().map_err(ManagedRootError::Io)?;
        if self.written != self.expected {
            return Err(ManagedRootError::WrongSize {
                expected: self.expected,
                actual: self.written,
            });
        }
        let actual = self
            .hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != expected_digest.as_str() {
            return Err(ManagedRootError::WrongDigest);
        }
        Ok(AcquiredArtifact {
            digest: Sha256Digest::new(actual)?,
            size_bytes: self.written,
            signer: None,
        })
    }
}

impl Write for VerifyingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "setup operation cancelled",
            ));
        }
        let next = self
            .written
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| std::io::Error::other("download size overflow"))?;
        if next > self.expected {
            return Err(std::io::Error::other("download exceeds pinned size"));
        }
        self.file.write_all(buffer)?;
        self.hasher.update(buffer);
        self.written = next;
        (self.progress)(ActionPhase::Downloading, self.written, Some(self.expected));
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn extract_zip(
    archive_path: &Path,
    destination: &Path,
    expected_root: &str,
    limits: AcquisitionLimits,
    cancel: &AtomicBool,
) -> Result<(), ManagedRootError> {
    validate_leaf_name(expected_root)?;
    fs::create_dir(destination).map_err(ManagedRootError::Io)?;
    let archive_bytes = fs::metadata(archive_path)
        .map_err(ManagedRootError::Io)?
        .len();
    let file = File::open(archive_path).map_err(ManagedRootError::Io)?;
    let mut archive = zip::ZipArchive::new(file).map_err(ManagedRootError::Zip)?;
    if archive.len() > limits.maximum_entries {
        return Err(ManagedRootError::TooManyEntries);
    }
    let maximum_by_ratio = archive_bytes.saturating_mul(limits.maximum_expansion_ratio);
    let maximum = limits.maximum_expanded_bytes.min(maximum_by_ratio);
    let mut expanded = 0_u64;
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        check_cancel(cancel)?;
        let mut entry = archive.by_index(index).map_err(ManagedRootError::Zip)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(ManagedRootError::LinkEntry);
        }
        let normalized = entry.name().replace('\\', "/");
        let path = safe_archive_path(&normalized, expected_root, limits.maximum_depth)?;
        let collision_key = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if !seen.insert(collision_key) {
            return Err(ManagedRootError::CaseCollision);
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or(ManagedRootError::ExpansionLimit)?;
        if expanded > maximum {
            return Err(ManagedRootError::ExpansionLimit);
        }
        let target = destination.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(ManagedRootError::Io)?;
            continue;
        }
        fs::create_dir_all(target.parent().ok_or(ManagedRootError::PathEscaped)?)
            .map_err(ManagedRootError::Io)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .map_err(ManagedRootError::Io)?;
        let expected_size = entry.size();
        let actual = copy_bounded(&mut entry, &mut output, expected_size, cancel)?;
        if actual != expected_size {
            return Err(ManagedRootError::CorruptArchive);
        }
        output.sync_all().map_err(ManagedRootError::Io)?;
    }
    if !destination.join(expected_root).is_dir() {
        return Err(ManagedRootError::WrongArchiveRoot);
    }
    Ok(())
}

fn safe_archive_path(
    name: &str,
    expected_root: &str,
    maximum_depth: usize,
) -> Result<PathBuf, ManagedRootError> {
    if name.is_empty() || name.starts_with('/') || name.contains(':') || name.contains('\0') {
        return Err(ManagedRootError::UnsafeArchivePath);
    }
    let path = Path::new(name);
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or(ManagedRootError::UnsafeArchivePath)?;
                validate_leaf_name(part)?;
                parts.push(part);
            }
            _ => return Err(ManagedRootError::UnsafeArchivePath),
        }
    }
    if parts.is_empty() || parts[0] != expected_root || parts.len() > maximum_depth {
        return Err(ManagedRootError::WrongArchiveRoot);
    }
    Ok(parts.iter().collect())
}

fn validate_leaf_name(name: &str) -> Result<(), ManagedRootError> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or_default();
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || stem.strip_prefix("com").is_some_and(|tail| {
            tail.len() == 1 && tail.as_bytes()[0].is_ascii_digit() && tail != "0"
        })
        || stem.strip_prefix("lpt").is_some_and(|tail| {
            tail.len() == 1 && tail.as_bytes()[0].is_ascii_digit() && tail != "0"
        });
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.contains(['/', '\\', ':', '\0'])
        || !name.is_ascii()
        || name
            .bytes()
            .any(|byte| byte < 0x20 || b"<>\"|?*".contains(&byte))
        || device
    {
        return Err(ManagedRootError::UnsafeArchivePath);
    }
    Ok(())
}

fn copy_bounded(
    input: &mut impl Read,
    output: &mut impl Write,
    expected: u64,
    cancel: &AtomicBool,
) -> Result<u64, ManagedRootError> {
    let mut buffer = [0_u8; IO_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        check_cancel(cancel)?;
        let count = input.read(&mut buffer).map_err(ManagedRootError::Io)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ManagedRootError::ExpansionLimit)?;
        if total > expected {
            return Err(ManagedRootError::CorruptArchive);
        }
        output
            .write_all(&buffer[..count])
            .map_err(ManagedRootError::Io)?;
    }
    Ok(total)
}

fn transaction_id() -> Result<ContentFreeId, ManagedRootError> {
    let sequence = TRANSACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    ContentFreeId::new(format!("tx-{}-{sequence}", std::process::id()))
        .map_err(|_| ManagedRootError::InvalidTransaction)
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), ManagedRootError> {
    if cancel.load(Ordering::Acquire) {
        Err(ManagedRootError::Cancelled)
    } else {
        Ok(())
    }
}

fn reject_symlink(path: &Path) -> Result<(), ManagedRootError> {
    let metadata = fs::symlink_metadata(path).map_err(ManagedRootError::Io)?;
    if is_link_or_reparse(&metadata) {
        return Err(ManagedRootError::ManagedPathIsLink);
    }
    Ok(())
}

fn ensure_parents_without_links(
    base: &Path,
    parent: Option<&Path>,
) -> Result<(), ManagedRootError> {
    let parent = parent.ok_or(ManagedRootError::PathEscaped)?;
    if !parent.starts_with(base) {
        return Err(ManagedRootError::PathEscaped);
    }
    let relative = parent
        .strip_prefix(base)
        .map_err(|_| ManagedRootError::PathEscaped)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ManagedRootError::PathEscaped);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
                return Err(ManagedRootError::ManagedPathIsLink);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(ManagedRootError::Io)?;
            }
            Err(error) => return Err(ManagedRootError::Io(error)),
        }
    }
    Ok(())
}

fn ensure_existing_path_without_links(base: &Path, target: &Path) -> Result<(), ManagedRootError> {
    if !target.starts_with(base) {
        return Err(ManagedRootError::PathEscaped);
    }
    let relative = target
        .strip_prefix(base)
        .map_err(|_| ManagedRootError::PathEscaped)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ManagedRootError::PathEscaped);
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(ManagedRootError::Io)?;
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(ManagedRootError::ManagedPathIsLink);
        }
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn ensure_tree_without_links(root: &Path) -> Result<(), ManagedRootError> {
    let metadata = fs::symlink_metadata(root).map_err(ManagedRootError::Io)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(ManagedRootError::ManagedPathIsLink);
    }
    for entry in fs::read_dir(root).map_err(ManagedRootError::Io)? {
        let entry = entry.map_err(ManagedRootError::Io)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(ManagedRootError::Io)?;
        if is_link_or_reparse(&metadata) {
            return Err(ManagedRootError::ManagedPathIsLink);
        }
        if metadata.is_dir() {
            ensure_tree_without_links(&entry.path())?;
        }
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), ManagedRootError> {
    let parent = path.parent().ok_or(ManagedRootError::PathEscaped)?;
    fs::create_dir_all(parent).map_err(ManagedRootError::Io)?;
    let temporary = parent.join(format!(
        ".journal-{}-{}.tmp",
        std::process::id(),
        TRANSACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(ManagedRootError::Io)?;
    serde_json::to_writer(&mut file, value).map_err(ManagedRootError::Json)?;
    file.flush().map_err(ManagedRootError::Io)?;
    file.sync_all().map_err(ManagedRootError::Io)?;
    drop(file);
    phorminx_windows::atomic_replace_file(&temporary, path)
        .map_err(|error| ManagedRootError::AtomicReplace(error.to_string()))
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    maximum: u64,
) -> Result<T, ManagedRootError> {
    let metadata = fs::metadata(path).map_err(ManagedRootError::Io)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(ManagedRootError::JournalTooLarge);
    }
    let file = File::open(path).map_err(ManagedRootError::Io)?;
    serde_json::from_reader(file).map_err(ManagedRootError::Json)
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("the HTTPS request failed")]
    Network,
    #[error("the response stream failed")]
    Response,
    #[error("the operation was cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedRootError {
    #[error("LocalAppData is unavailable")]
    LocalAppDataUnavailable,
    #[error("the managed root must be absolute")]
    RootNotAbsolute,
    #[error("a managed path is a link or reparse-like alias")]
    ManagedPathIsLink,
    #[error("the managed path escaped its fixed root")]
    PathEscaped,
    #[error("the activation target already exists")]
    TargetExists,
    #[error("the target is not owned by this transaction")]
    NotOwned,
    #[error("the transaction identity is invalid")]
    InvalidTransaction,
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("the response length was {actual}, expected {expected}")]
    WrongSize { expected: u64, actual: u64 },
    #[error("the response SHA-256 did not match the pinned digest")]
    WrongDigest,
    #[error("the archive path is unsafe")]
    UnsafeArchivePath,
    #[error("the archive root does not match the pinned root")]
    WrongArchiveRoot,
    #[error("the archive contains a link")]
    LinkEntry,
    #[error("the archive contains a case-insensitive path collision")]
    CaseCollision,
    #[error("the archive contains too many entries")]
    TooManyEntries,
    #[error("the archive exceeds its expansion limit")]
    ExpansionLimit,
    #[error("the archive is corrupt")]
    CorruptArchive,
    #[error("the journal is not a bounded regular file")]
    JournalTooLarge,
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Ownership(#[from] phorminx_setup::OwnershipError),
    #[error("ZIP processing failed: {0}")]
    Zip(zip::result::ZipError),
    #[error("managed setup I/O failed: {0}")]
    Io(std::io::Error),
    #[error("managed setup metadata is invalid: {0}")]
    Json(serde_json::Error),
    #[error("atomic metadata replacement failed: {0}")]
    AtomicReplace(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use phorminx_setup::{ArtifactDescriptor, ArtifactKind, AssetId, EngineKind, Language};
    use std::io::Cursor;

    struct BytesFetcher {
        bytes: Vec<u8>,
        cancel_first: bool,
    }

    impl ArtifactFetcher for BytesFetcher {
        fn fetch(
            &self,
            _url: &str,
            output: &mut dyn Write,
            cancel: &AtomicBool,
            progress: &mut dyn FnMut(u64),
        ) -> Result<(), FetchError> {
            if self.cancel_first {
                cancel.store(true, Ordering::Release);
                return Err(FetchError::Cancelled);
            }
            output
                .write_all(&self.bytes)
                .map_err(|_| FetchError::Response)?;
            progress(self.bytes.len() as u64);
            Ok(())
        }
    }

    fn pinned(bytes: Vec<u8>, packaging: Packaging, slot: &str) -> PinnedArtifact {
        let digest = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        PinnedArtifact {
            descriptor: ArtifactDescriptor::new(
                AssetId::new("test-asset").unwrap(),
                Sha256Digest::new(digest).unwrap(),
                bytes.len() as u64,
                ContentFreeId::new("test-vendor").unwrap(),
                ContentFreeId::new("v1").unwrap(),
                ContentFreeId::new("mit").unwrap(),
                "https://example.invalid/asset",
                ArtifactKind::Data,
                None,
                EngineKind::Accurate,
                [Language::English],
            )
            .unwrap(),
            activation_slot: slot.to_owned(),
            packaging,
        }
    }

    fn action(artifact: &PinnedArtifact) -> SetupAction {
        SetupAction::for_key(phorminx_setup::ActionKey::DownloadArtifact {
            artifact: Box::new(artifact.descriptor().clone()),
        })
        .unwrap()
    }

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            for (name, bytes) in entries {
                writer
                    .start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn verified_raw_asset_activates_and_rolls_back_only_its_owned_target() {
        let temporary = tempfile::tempdir().unwrap();
        let user_file = temporary.path().join("user-owned.txt");
        fs::write(&user_file, b"keep").unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            AcquisitionLimits::default(),
        )
        .unwrap();
        let bytes = b"verified model".to_vec();
        let artifact = pinned(
            bytes.clone(),
            Packaging::RawFile {
                file_name: "model.bin".to_owned(),
            },
            "models/test",
        );
        let install = root
            .install(
                &action(&artifact),
                &artifact,
                &BytesFetcher {
                    bytes,
                    cancel_first: false,
                },
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap();
        assert!(install.target.join("model.bin").is_file());
        root.rollback(&install).unwrap();
        assert!(!install.target.exists());
        assert_eq!(fs::read(user_file).unwrap(), b"keep");

        #[cfg(windows)]
        {
            let user_directory = temporary.path().join("user-directory-link-target");
            fs::create_dir(&user_directory).unwrap();
            fs::write(user_directory.join("keep"), b"keep").unwrap();
            if std::os::windows::fs::symlink_dir(&user_directory, &install.target).is_ok() {
                assert!(matches!(
                    root.rollback(&install),
                    Err(ManagedRootError::ManagedPathIsLink)
                ));
                assert_eq!(fs::read(user_directory.join("keep")).unwrap(), b"keep");
            }
        }
    }

    #[test]
    fn digest_size_cancellation_and_corrupt_zip_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            AcquisitionLimits::default(),
        )
        .unwrap();
        let good = b"good".to_vec();
        let raw = pinned(
            good,
            Packaging::RawFile {
                file_name: "model.bin".to_owned(),
            },
            "raw/one",
        );
        assert!(
            root.install(
                &action(&raw),
                &raw,
                &BytesFetcher {
                    bytes: b"evil".to_vec(),
                    cancel_first: false
                },
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .is_err()
        );
        assert!(matches!(
            root.install(
                &action(&raw),
                &raw,
                &BytesFetcher {
                    bytes: vec![],
                    cancel_first: true
                },
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            ),
            Err(ManagedRootError::Fetch(FetchError::Cancelled))
        ));

        let corrupt = b"not a zip".to_vec();
        let archive = pinned(
            corrupt.clone(),
            Packaging::Zip {
                expected_root: "expected".to_owned(),
            },
            "zip/corrupt",
        );
        assert!(matches!(
            root.install(
                &action(&archive),
                &archive,
                &BytesFetcher {
                    bytes: corrupt,
                    cancel_first: false
                },
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            ),
            Err(ManagedRootError::Zip(_))
        ));
    }

    #[test]
    fn verifier_rejects_short_oversize_and_wrong_digest_streams() {
        let temporary = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let expected = Sha256Digest::new("a".repeat(64)).unwrap();

        let file = File::create(temporary.path().join("short")).unwrap();
        let mut short_progress = |_, _, _| {};
        let mut writer = VerifyingWriter::new(file, 4, &cancel, &mut short_progress);
        writer.write_all(b"abc").unwrap();
        assert!(matches!(
            writer.finish(&expected),
            Err(ManagedRootError::WrongSize {
                expected: 4,
                actual: 3
            })
        ));

        let file = File::create(temporary.path().join("oversize")).unwrap();
        let mut oversize_progress = |_, _, _| {};
        let mut writer = VerifyingWriter::new(file, 3, &cancel, &mut oversize_progress);
        assert!(writer.write_all(b"four").is_err());

        let file = File::create(temporary.path().join("digest")).unwrap();
        let mut digest_progress = |_, _, _| {};
        let mut writer = VerifyingWriter::new(file, 3, &cancel, &mut digest_progress);
        writer.write_all(b"abc").unwrap();
        assert!(matches!(
            writer.finish(&expected),
            Err(ManagedRootError::WrongDigest)
        ));
    }

    #[test]
    fn archive_paths_reject_traversal_ads_devices_wrong_root_and_depth() {
        for path in [
            "root/../escape",
            "/root/file",
            "root/file:stream",
            "root/CON",
            "wrong/file",
            "root/file.",
        ] {
            assert!(safe_archive_path(path, "root", 8).is_err(), "{path}");
        }
        assert!(safe_archive_path("root/a/b/c", "root", 3).is_err());
        assert!(safe_archive_path("root/safe.bin", "root", 3).is_ok());
    }

    #[test]
    fn zip_case_collisions_and_expansion_limits_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let collision = zip(&[("root/A.txt", b"a"), ("root/a.txt", b"b")]);
        assert!(matches!(
            extract_zip(
                &write_archive(temporary.path(), "collision.zip", &collision),
                &temporary.path().join("out-one"),
                "root",
                AcquisitionLimits::default(),
                &AtomicBool::new(false)
            ),
            Err(ManagedRootError::CaseCollision)
        ));
        let expanded = zip(&[("root/large.bin", &[0_u8; 4096])]);
        assert!(matches!(
            extract_zip(
                &write_archive(temporary.path(), "large.zip", &expanded),
                &temporary.path().join("out-two"),
                "root",
                AcquisitionLimits {
                    maximum_expanded_bytes: 100,
                    ..AcquisitionLimits::default()
                },
                &AtomicBool::new(false)
            ),
            Err(ManagedRootError::ExpansionLimit)
        ));
    }

    #[test]
    fn restart_recovery_requires_matching_transaction_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            AcquisitionLimits::default(),
        )
        .unwrap();
        let artifact = pinned(
            b"x".to_vec(),
            Packaging::RawFile {
                file_name: "x".to_owned(),
            },
            "models/x",
        );
        let action = action(&artifact);
        let transaction = ContentFreeId::new("tx-recovery").unwrap();
        let journal = DiskJournal::new(
            &transaction,
            action.id(),
            &ManagedSlot::new("models/x").unwrap(),
            artifact.descriptor().asset_id(),
            artifact.descriptor().digest(),
        );
        let staging = root.root.join("staging/tx-recovery");
        fs::create_dir(&staging).unwrap();
        write_json_atomic(
            &staging.join(".transaction.json"),
            &TransactionMarker::new(&transaction, action.id()),
        )
        .unwrap();
        fs::write(staging.join("partial"), b"partial").unwrap();
        root.write_journal(&journal).unwrap();
        let report = root.recover().unwrap();
        assert_eq!(report.removed_staging, 1);
        assert!(!staging.exists());

        let hostile = temporary.path().join("user-directory");
        fs::create_dir(&hostile).unwrap();
        fs::write(hostile.join("keep"), b"keep").unwrap();
        let bad_transaction = ContentFreeId::new("tx-bad").unwrap();
        let bad = DiskJournal::new(
            &bad_transaction,
            action.id(),
            &ManagedSlot::new("models/x").unwrap(),
            artifact.descriptor().asset_id(),
            artifact.descriptor().digest(),
        );
        fs::create_dir(root.root.join("staging/tx-bad")).unwrap();
        root.write_journal(&bad).unwrap();
        let report = root.recover().unwrap();
        assert_eq!(report.rejected_journals, 1);
        assert!(hostile.join("keep").exists());
    }

    #[test]
    fn restart_recovery_removes_only_marker_owned_orphan_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            AcquisitionLimits::default(),
        )
        .unwrap();
        let artifact = pinned(
            b"x".to_vec(),
            Packaging::RawFile {
                file_name: "x".to_owned(),
            },
            "models/x",
        );
        let action = action(&artifact);

        let orphan_id = ContentFreeId::new("tx-orphan").unwrap();
        let orphan = root.root.join("staging/tx-orphan");
        fs::create_dir(&orphan).unwrap();
        write_json_atomic(
            &orphan.join(".transaction.json"),
            &TransactionMarker::new(&orphan_id, action.id()),
        )
        .unwrap();
        fs::write(orphan.join("partial"), b"partial").unwrap();

        let unowned = root.root.join("staging/tx-unowned");
        fs::create_dir(&unowned).unwrap();
        fs::write(unowned.join("keep"), b"keep").unwrap();

        let wrong_schema_id = ContentFreeId::new("tx-wrong-schema").unwrap();
        let wrong_schema = root.root.join("staging/tx-wrong-schema");
        fs::create_dir(&wrong_schema).unwrap();
        let mut marker = TransactionMarker::new(&wrong_schema_id, action.id());
        marker.schema_version = 2;
        write_json_atomic(&wrong_schema.join(".transaction.json"), &marker).unwrap();

        let report = root.recover().unwrap();
        assert_eq!(report.removed_staging, 1);
        assert_eq!(report.rejected_journals, 2);
        assert!(!orphan.exists());
        assert_eq!(fs::read(unowned.join("keep")).unwrap(), b"keep");
        assert!(wrong_schema.exists());
    }

    #[test]
    fn restart_between_atomic_activation_and_journal_update_uses_owner_witness() {
        let temporary = tempfile::tempdir().unwrap();
        let root = ManagedRoot::for_test(
            &temporary.path().join("managed"),
            AcquisitionLimits::default(),
        )
        .unwrap();
        let artifact = pinned(
            b"x".to_vec(),
            Packaging::RawFile {
                file_name: "x".to_owned(),
            },
            "models/x",
        );
        let action = action(&artifact);
        let transaction = ContentFreeId::new("tx-commit-window").unwrap();
        let slot = ManagedSlot::new("models/x").unwrap();
        let journal = DiskJournal::new(
            &transaction,
            action.id(),
            &slot,
            artifact.descriptor().asset_id(),
            artifact.descriptor().digest(),
        );
        let acquired = AcquiredArtifact {
            digest: artifact.descriptor().digest().clone(),
            size_bytes: artifact.descriptor().size_bytes(),
            signer: None,
        };
        let managed =
            ManagedAsset::from_verified(artifact.descriptor().clone(), &acquired, slot).unwrap();
        let receipt = AssetReceipt::new(transaction.clone(), managed, 1);
        let target = root.target(receipt.asset().slot()).unwrap();
        fs::create_dir_all(&target).unwrap();
        write_json_atomic(
            &target.join(".phorminx-owner.json"),
            &OwnerMarker::for_receipt(&receipt),
        )
        .unwrap();
        write_json_atomic(&target.join(".phorminx-receipt.json"), &receipt).unwrap();
        let staging = root.root.join("staging/tx-commit-window");
        fs::create_dir(&staging).unwrap();
        write_json_atomic(
            &staging.join(".transaction.json"),
            &TransactionMarker::new(&transaction, action.id()),
        )
        .unwrap();
        root.write_journal(&journal).unwrap();

        let report = root.recover().unwrap();
        assert_eq!(report.activated_targets, vec![target.clone()]);
        assert!(target.exists());
        assert!(!staging.exists());
        assert!(!root.journal_path(&transaction).exists());
    }

    #[test]
    fn every_post_activation_fault_retains_recoverable_ownership_authority() {
        for (index, fault) in [
            ActivationFault::Rename,
            ActivationFault::ActivatedJournal,
            ActivationFault::Receipt,
            ActivationFault::JournalDeletion,
        ]
        .into_iter()
        .enumerate()
        {
            let temporary = tempfile::tempdir().unwrap();
            let root = ManagedRoot::for_test_with_fault(
                &temporary.path().join(format!("managed-{index}")),
                fault,
            )
            .unwrap();
            let bytes = format!("verified-{index}").into_bytes();
            let mut artifact = pinned(
                bytes.clone(),
                Packaging::RawFile {
                    file_name: "model.bin".to_owned(),
                },
                &format!("models/fault-{index}"),
            );
            artifact.descriptor = ArtifactDescriptor::new(
                AssetId::new(format!("fault-asset-{index}")).unwrap(),
                artifact.descriptor().digest().clone(),
                artifact.descriptor().size_bytes(),
                ContentFreeId::new("test-vendor").unwrap(),
                ContentFreeId::new("v1").unwrap(),
                ContentFreeId::new("mit").unwrap(),
                "https://example.invalid/asset",
                ArtifactKind::Data,
                None,
                EngineKind::Accurate,
                [Language::English],
            )
            .unwrap();
            let install = root
                .install(
                    &action(&artifact),
                    &artifact,
                    &BytesFetcher {
                        bytes,
                        cancel_first: false,
                    },
                    &AtomicBool::new(false),
                    &mut |_, _, _| {},
                )
                .unwrap();
            assert!(install.target.exists());
            let _ = root.recover().unwrap();
            let installed = root.installed().unwrap();
            assert_eq!(installed.len(), 1, "fault point {fault:?}");
            root.rollback(&installed[0]).unwrap();
            assert!(!install.target.exists());
        }
    }

    fn write_archive(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}
