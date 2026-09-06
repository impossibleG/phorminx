use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{AudioSpan, AudioSpanError, CANONICAL_SAMPLE_RATE, SampleRange};

const FILE_MAGIC: &[u8; 8] = b"PHXSPOL1";
const RECORD_MAGIC: &[u8; 4] = b"RECD";
const FILE_VERSION: u16 = 1;
const KEY_LEN: usize = 32;
const NONCE_PREFIX_LEN: usize = 4;
const NONCE_LEN: usize = 12;
const FILE_ID_LEN: usize = 16;
const TAG_LEN: usize = 16;
const FILE_HEADER_LEN: usize = 8 + 2 + 4 + FILE_ID_LEN + NONCE_PREFIX_LEN;
const FILE_PREFIX_LEN: usize = FILE_HEADER_LEN + TAG_LEN;
const RECORD_HEADER_LEN: usize = 4 + 8 + 8 + 4 + 4 + 4;
const FILE_AAD_DOMAIN: &[u8] = b"phorminx/audio-spool/file/v1";
const RECORD_AAD_DOMAIN: &[u8] = b"phorminx/audio-spool/record/v1";
const FILE_NAME_PREFIX: &str = "phorminx-audio-";
const FILE_NAME_SUFFIX: &str = ".pxs";
const OWNER_MARKER_NAME: &str = ".phorminx-spool-root";
const OWNER_MARKER_CONTENT: &[u8] = b"PHORMINX_PRIVATE_AUDIO_SPOOL_ROOT_V1\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpoolQuota {
    pub max_file_bytes: u64,
    pub max_samples: u64,
    pub max_records: u64,
    pub max_record_samples: u32,
    /// Upper bound for one decryption/read allocation.
    pub max_read_samples: u32,
}

impl Default for SpoolQuota {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024 * 1024,
            max_samples: u64::from(CANONICAL_SAMPLE_RATE) * 60 * 60 * 8,
            max_records: 65_536,
            max_record_samples: CANONICAL_SAMPLE_RATE * 60,
            max_read_samples: CANONICAL_SAMPLE_RATE * 60,
        }
    }
}

/// Storage operations required by the encrypted spool.
///
/// The interface is public to allow deterministic fault injection and alternate local storage,
/// but implementations must provide append durability, truncation, and explicit cleanup.
pub trait SpoolStorage: Read + Write + Seek + Send {
    fn byte_len(&mut self) -> io::Result<u64>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn sync_all(&mut self) -> io::Result<()>;
    fn cleanup(&mut self) -> io::Result<()>;
}

/// Validated application-owned directory in which encrypted spool files may exist.
#[derive(Clone, Debug)]
pub struct SpoolRoot {
    path: PathBuf,
}

impl SpoolRoot {
    /// Opens an existing marked root, or claims a newly-created/empty directory atomically.
    /// A non-empty unmarked directory is never accepted as a scavenging target.
    pub fn open_or_create(directory: impl AsRef<Path>) -> Result<Self, SpoolError> {
        let directory = directory.as_ref();
        if directory.exists() {
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(SpoolError::UnownedRoot);
            }
        } else {
            fs::create_dir_all(directory)?;
        }

        let marker = directory.join(OWNER_MARKER_NAME);
        match fs::symlink_metadata(&marker) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(SpoolError::InvalidOwnerMarker);
                }
                if fs::read(&marker)? != OWNER_MARKER_CONTENT {
                    return Err(SpoolError::InvalidOwnerMarker);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::read_dir(directory)?.next().transpose()?.is_some() {
                    return Err(SpoolError::UnownedRoot);
                }
                let mut marker_file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker)?;
                marker_file.write_all(OWNER_MARKER_CONTENT)?;
                marker_file.sync_all()?;
            }
            Err(error) => return Err(SpoolError::Io(error)),
        }

        Ok(Self {
            path: fs::canonicalize(directory)?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn validate_owner_marker(&self) -> Result<(), SpoolError> {
        let marker = self.path.join(OWNER_MARKER_NAME);
        let metadata = fs::symlink_metadata(&marker)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || fs::read(marker)? != OWNER_MARKER_CONTENT
        {
            return Err(SpoolError::InvalidOwnerMarker);
        }
        Ok(())
    }
}

pub struct FileStorage {
    file: Option<File>,
    path: PathBuf,
}

impl FileStorage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "spool is closed"))
    }
}

impl Read for FileStorage {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file_mut()?.read(buffer)
    }
}

impl Write for FileStorage {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file_mut()?.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file_mut()?.flush()
    }
}

impl Seek for FileStorage {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file_mut()?.seek(position)
    }
}

impl SpoolStorage for FileStorage {
    fn byte_len(&mut self) -> io::Result<u64> {
        Ok(self.file_mut()?.metadata()?.len())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.file_mut()?.set_len(len)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.file_mut()?.sync_all()
    }

    fn cleanup(&mut self) -> io::Result<()> {
        self.file.take();
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for FileStorage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[derive(Clone, Debug)]
struct RecordIndex {
    range: SampleRange,
    offset: u64,
    counter: u64,
    plaintext_len: u32,
    sealed_len: u32,
}

/// An append-only encrypted audio spool. The encryption key exists only in this object.
///
/// There is intentionally no reopen API: after process death, a spool file's key is gone and the
/// remaining bytes are cryptographically unreadable. `scavenge_orphans` only deletes such files.
pub struct EncryptedAudioSpool<S: SpoolStorage = FileStorage> {
    storage: S,
    key_bytes: Zeroizing<[u8; KEY_LEN]>,
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    file_header: [u8; FILE_HEADER_LEN],
    quota: SpoolQuota,
    file_len: u64,
    origin_sample: Option<u64>,
    next_sample: Option<u64>,
    next_counter: u64,
    index: Vec<RecordIndex>,
    sample_count: u64,
    poisoned: bool,
    cleaned: bool,
}

impl EncryptedAudioSpool<FileStorage> {
    pub fn create(root: &SpoolRoot, quota: SpoolQuota) -> Result<Self, SpoolError> {
        validate_quota(quota)?;
        root.validate_owner_marker()?;
        let random = SystemRandom::new();
        let mut file_id = [0_u8; FILE_ID_LEN];
        for _ in 0..32 {
            random
                .fill(&mut file_id)
                .map_err(|_| SpoolError::RandomnessUnavailable)?;
            let path = root.path.join(spool_file_name(file_id));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    file.try_lock().map_err(|error| match error {
                        fs::TryLockError::WouldBlock => SpoolError::ActiveSpool,
                        fs::TryLockError::Error(error) => SpoolError::Io(error),
                    })?;
                    let storage = FileStorage {
                        file: Some(file),
                        path,
                    };
                    return Self::from_empty_storage_with_id(storage, quota, file_id);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(SpoolError::Io(error)),
            }
        }
        Err(SpoolError::NameCollision)
    }

    pub fn path(&self) -> &Path {
        self.storage.path()
    }
}

impl<S: SpoolStorage> EncryptedAudioSpool<S> {
    /// Initializes an empty injectable storage using a fresh random key and file identifier.
    pub fn from_empty_storage(storage: S, quota: SpoolQuota) -> Result<Self, SpoolError> {
        let random = SystemRandom::new();
        let mut file_id = [0_u8; FILE_ID_LEN];
        random
            .fill(&mut file_id)
            .map_err(|_| SpoolError::RandomnessUnavailable)?;
        Self::from_empty_storage_with_id(storage, quota, file_id)
    }

    fn from_empty_storage_with_id(
        mut storage: S,
        quota: SpoolQuota,
        file_id: [u8; FILE_ID_LEN],
    ) -> Result<Self, SpoolError> {
        validate_quota(quota)?;
        if storage.byte_len()? != 0 {
            return Err(SpoolError::StorageNotEmpty);
        }

        let random = SystemRandom::new();
        let mut key_bytes = Zeroizing::new([0_u8; KEY_LEN]);
        let mut nonce_prefix = [0_u8; NONCE_PREFIX_LEN];
        random
            .fill(key_bytes.as_mut())
            .map_err(|_| SpoolError::RandomnessUnavailable)?;
        random
            .fill(&mut nonce_prefix)
            .map_err(|_| SpoolError::RandomnessUnavailable)?;
        let key = less_safe_key(&key_bytes)?;
        let file_header = encode_file_header(file_id, nonce_prefix);
        let mut header_tag = Zeroizing::new(Vec::new());
        key.seal_in_place_append_tag(
            nonce_for(nonce_prefix, 0),
            Aad::from(file_aad(&file_header)),
            &mut *header_tag,
        )
        .map_err(|_| SpoolError::CryptoInitialization)?;
        debug_assert_eq!(header_tag.len(), TAG_LEN);

        let write_result = (|| -> io::Result<()> {
            storage.seek(SeekFrom::Start(0))?;
            storage.write_all(&file_header)?;
            storage.write_all(&header_tag)?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = storage.cleanup();
            return Err(SpoolError::Io(error));
        }

        Ok(Self {
            storage,
            key_bytes,
            nonce_prefix,
            file_header,
            quota,
            file_len: FILE_PREFIX_LEN as u64,
            origin_sample: None,
            next_sample: None,
            next_counter: 1,
            index: Vec::new(),
            sample_count: 0,
            poisoned: false,
            cleaned: false,
        })
    }

    pub const fn origin_sample(&self) -> Option<u64> {
        self.origin_sample
    }

    pub const fn next_sample(&self) -> Option<u64> {
        self.next_sample
    }

    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn indexed_ranges(&self) -> impl ExactSizeIterator<Item = SampleRange> + '_ {
        self.index.iter().map(|record| record.range)
    }

    /// Physically removes every complete encrypted record ending at or before
    /// `frontier`. The record intersecting the frontier is retained so callers
    /// never discard uncommitted audio; with the default record size this keeps
    /// at most one extra second.
    ///
    /// Returns the earliest sample still readable from the spool, or the
    /// append frontier when no records remain.
    pub fn discard_complete_before(&mut self, frontier: u64) -> Result<u64, SpoolError> {
        self.ensure_healthy()?;
        let Some(end) = self.next_sample else {
            return Ok(frontier);
        };
        let origin = self.origin_sample.unwrap_or(end);
        if frontier <= origin {
            return Ok(origin);
        }
        if frontier > end {
            return Err(SpoolError::ReadOutOfBounds {
                available: SampleRange::new(origin, end)
                    .expect("spool origin cannot exceed its frontier"),
                requested: SampleRange::new(origin, frontier)
                    .expect("discard frontier follows the spool origin"),
            });
        }

        let remove_count = self
            .index
            .iter()
            .take_while(|record| record.range.end() <= frontier)
            .count();
        if remove_count == 0 {
            return Ok(origin);
        }

        // Copy retained ciphertext toward the file prefix in ascending order.
        // Source ranges can overlap the destination, but every destination is
        // strictly before its source, so an ascending copy cannot overwrite a
        // later unread record. Record headers/AAD remain byte-for-byte intact.
        let mut destination = FILE_PREFIX_LEN as u64;
        for record in &mut self.index[remove_count..] {
            let record_len = (RECORD_HEADER_LEN as u64)
                .checked_add(u64::from(record.sealed_len))
                .ok_or(SpoolError::QuotaExceeded)?;
            let allocation = usize::try_from(record_len).map_err(|_| SpoolError::RecordTooLarge)?;
            let mut bytes = Zeroizing::new(Vec::new());
            bytes
                .try_reserve_exact(allocation)
                .map_err(|_| SpoolError::AllocationFailed)?;
            bytes.resize(allocation, 0);
            self.storage.seek(SeekFrom::Start(record.offset))?;
            read_exact_classified(&mut self.storage, &mut bytes)?;
            self.storage.seek(SeekFrom::Start(destination))?;
            if let Err(error) = self.storage.write_all(&bytes) {
                self.poisoned = true;
                return Err(SpoolError::Io(error));
            }
            record.offset = destination;
            destination = destination
                .checked_add(record_len)
                .ok_or(SpoolError::QuotaExceeded)?;
        }
        if let Err(error) = self.storage.set_len(destination) {
            self.poisoned = true;
            return Err(SpoolError::Io(error));
        }
        if let Err(error) = self.storage.sync_all() {
            self.poisoned = true;
            return Err(SpoolError::Io(error));
        }

        self.index.drain(..remove_count);
        self.file_len = destination;
        self.origin_sample = self.index.first().map(|record| record.range.start());
        self.sample_count = self.index.iter().map(|record| record.range.len()).sum();
        Ok(self.origin_sample.unwrap_or(end))
    }

    pub fn append(&mut self, span: &AudioSpan) -> Result<(), SpoolError> {
        self.ensure_healthy()?;
        if let Some(expected) = self.next_sample
            && span.range().start() != expected
        {
            return Err(SpoolError::NonContiguousAppend {
                expected_start: expected,
                actual_start: span.range().start(),
            });
        }

        let sample_len =
            u32::try_from(span.samples().len()).map_err(|_| SpoolError::RecordTooLarge)?;
        if sample_len > self.quota.max_record_samples {
            return Err(SpoolError::RecordTooLarge);
        }
        let plaintext_len = sample_len
            .checked_mul(4)
            .ok_or(SpoolError::RecordTooLarge)?;
        let sealed_len = plaintext_len
            .checked_add(TAG_LEN as u32)
            .ok_or(SpoolError::RecordTooLarge)?;
        let record_bytes = (RECORD_HEADER_LEN as u64)
            .checked_add(u64::from(sealed_len))
            .ok_or(SpoolError::QuotaExceeded)?;
        let future_file_len = self
            .file_len
            .checked_add(record_bytes)
            .ok_or(SpoolError::QuotaExceeded)?;
        let future_samples = self
            .sample_count
            .checked_add(u64::from(sample_len))
            .ok_or(SpoolError::QuotaExceeded)?;
        let future_records = u64::try_from(self.index.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or(SpoolError::QuotaExceeded)?;
        if future_file_len > self.quota.max_file_bytes
            || future_samples > self.quota.max_samples
            || future_records > self.quota.max_records
        {
            return Err(SpoolError::QuotaExceeded);
        }

        let counter = self.next_counter;
        if counter == u64::MAX {
            return Err(SpoolError::NonceExhausted);
        }
        // A nonce is burned as soon as it is selected. Storage rollback may make the sample range
        // retryable, but it must never make an AES-GCM nonce retryable under the same key.
        self.next_counter += 1;
        let record_header = encode_record_header(counter, span.range(), plaintext_len, sealed_len);
        let mut encrypted = Zeroizing::new(Vec::new());
        encrypted
            .try_reserve_exact(usize::try_from(sealed_len).map_err(|_| SpoolError::RecordTooLarge)?)
            .map_err(|_| SpoolError::AllocationFailed)?;
        for sample in span.samples() {
            encrypted.extend_from_slice(&sample.to_bits().to_le_bytes());
        }
        less_safe_key(&self.key_bytes)?
            .seal_in_place_append_tag(
                nonce_for(self.nonce_prefix, counter),
                Aad::from(record_aad(&self.file_header, &record_header)),
                &mut *encrypted,
            )
            .map_err(|_| SpoolError::EncryptionFailed)?;

        let committed_len = self.file_len;
        let write_result = (|| -> io::Result<()> {
            self.storage.seek(SeekFrom::Start(committed_len))?;
            self.storage.write_all(&record_header)?;
            self.storage.write_all(&encrypted)?;
            Ok(())
        })();
        if let Err(write_error) = write_result {
            if let Err(rollback_error) = self.storage.set_len(committed_len) {
                self.poisoned = true;
                return Err(SpoolError::RollbackFailed {
                    write_error,
                    rollback_error,
                });
            }
            let _ = self.storage.seek(SeekFrom::Start(committed_len));
            return Err(SpoolError::Io(write_error));
        }

        let range = span.range();
        self.index.push(RecordIndex {
            range,
            offset: committed_len,
            counter,
            plaintext_len,
            sealed_len,
        });
        self.origin_sample.get_or_insert(range.start());
        self.next_sample = Some(range.end());
        self.file_len = future_file_len;
        self.sample_count = future_samples;
        Ok(())
    }

    pub fn read_range(&mut self, requested: SampleRange) -> Result<AudioSpan, SpoolError> {
        self.ensure_healthy()?;
        if requested.is_empty() {
            return Err(SpoolError::EmptyRead);
        }
        let (origin, end) = self
            .origin_sample
            .zip(self.next_sample)
            .ok_or(SpoolError::EmptySpool)?;
        if requested.start() < origin || requested.end() > end {
            return Err(SpoolError::ReadOutOfBounds {
                available: SampleRange::new(origin, end)
                    .expect("spool origin cannot exceed its frontier"),
                requested,
            });
        }
        if requested.len() > u64::from(self.quota.max_read_samples) {
            return Err(SpoolError::ReadTooLarge);
        }
        self.authenticate_file_header()?;

        let output_len = usize::try_from(requested.len()).map_err(|_| SpoolError::ReadTooLarge)?;
        let mut samples = Zeroizing::new(Vec::new());
        samples
            .try_reserve_exact(output_len)
            .map_err(|_| SpoolError::AllocationFailed)?;

        for record in self
            .index
            .iter()
            .filter(|record| record.range.intersects(requested))
        {
            let expected_header = encode_record_header(
                record.counter,
                record.range,
                record.plaintext_len,
                record.sealed_len,
            );
            self.storage.seek(SeekFrom::Start(record.offset))?;
            let mut actual_header = [0_u8; RECORD_HEADER_LEN];
            read_exact_classified(&mut self.storage, &mut actual_header)?;
            if actual_header != expected_header {
                return Err(SpoolError::CorruptHeader);
            }

            let mut encrypted = Zeroizing::new(vec![
                0_u8;
                usize::try_from(record.sealed_len)
                    .map_err(|_| SpoolError::CorruptHeader)?
            ]);
            read_exact_classified(&mut self.storage, &mut encrypted)?;
            let plaintext = less_safe_key(&self.key_bytes)?
                .open_in_place(
                    nonce_for(self.nonce_prefix, record.counter),
                    Aad::from(record_aad(&self.file_header, &actual_header)),
                    &mut encrypted,
                )
                .map_err(|_| SpoolError::AuthenticationFailed)?;
            if plaintext.len()
                != usize::try_from(record.plaintext_len).map_err(|_| SpoolError::CorruptHeader)?
            {
                return Err(SpoolError::CorruptRecord);
            }

            let overlap_start = requested.start().max(record.range.start());
            let overlap_end = requested.end().min(record.range.end());
            let first = usize::try_from(overlap_start - record.range.start())
                .map_err(|_| SpoolError::ReadTooLarge)?;
            let last = usize::try_from(overlap_end - record.range.start())
                .map_err(|_| SpoolError::ReadTooLarge)?;
            let byte_start = first.checked_mul(4).ok_or(SpoolError::CorruptRecord)?;
            let byte_end = last.checked_mul(4).ok_or(SpoolError::CorruptRecord)?;
            let selected = plaintext
                .get(byte_start..byte_end)
                .ok_or(SpoolError::CorruptRecord)?;
            let (sample_bytes, remainder) = selected.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err(SpoolError::CorruptRecord);
            }
            for bytes in sample_bytes {
                samples.push(f32::from_bits(u32::from_le_bytes(*bytes)));
            }
        }

        if samples.len() != output_len {
            return Err(SpoolError::IndexCoverageMismatch);
        }
        let samples = std::mem::take(&mut *samples);
        AudioSpan::new(requested.start(), samples).map_err(SpoolError::InvalidAudio)
    }

    pub fn flush(&mut self) -> Result<(), SpoolError> {
        self.ensure_healthy()?;
        self.storage.flush()?;
        self.storage.sync_all()?;
        Ok(())
    }

    /// Flushes, closes, and removes the spool. Drop also performs best-effort cleanup.
    pub fn cleanup(mut self) -> Result<(), SpoolError> {
        self.flush()?;
        self.storage.cleanup()?;
        self.cleaned = true;
        Ok(())
    }

    fn authenticate_file_header(&mut self) -> Result<(), SpoolError> {
        self.storage.seek(SeekFrom::Start(0))?;
        let mut prefix = [0_u8; FILE_PREFIX_LEN];
        read_exact_classified(&mut self.storage, &mut prefix)?;
        if prefix[..FILE_HEADER_LEN] != self.file_header {
            return Err(SpoolError::CorruptHeader);
        }
        let mut tag = Zeroizing::new(prefix[FILE_HEADER_LEN..].to_vec());
        let plaintext = less_safe_key(&self.key_bytes)?
            .open_in_place(
                nonce_for(self.nonce_prefix, 0),
                Aad::from(file_aad(&self.file_header)),
                &mut tag,
            )
            .map_err(|_| SpoolError::AuthenticationFailed)?;
        if !plaintext.is_empty() {
            return Err(SpoolError::CorruptHeader);
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<(), SpoolError> {
        if self.cleaned {
            Err(SpoolError::AlreadyCleaned)
        } else if self.poisoned {
            Err(SpoolError::Poisoned)
        } else {
            Ok(())
        }
    }
}

impl<S: SpoolStorage> Drop for EncryptedAudioSpool<S> {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.storage.cleanup();
            self.cleaned = true;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScavengeReport {
    pub removed_files: u64,
    pub removed_bytes: u64,
    pub skipped_live_files: u64,
}

/// Deletes prior-process spool files. No decryption is attempted because keys are never persisted.
pub fn scavenge_orphans(root: &SpoolRoot) -> Result<ScavengeReport, SpoolError> {
    root.validate_owner_marker()?;
    let mut report = ScavengeReport::default();
    for entry in fs::read_dir(root.path())? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_spool_file_name(&name) {
            continue;
        }
        let file = match OpenOptions::new().read(true).write(true).open(entry.path()) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
                ) =>
            {
                report.skipped_live_files += 1;
                continue;
            }
            Err(error) => return Err(SpoolError::Io(error)),
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                report.skipped_live_files += 1;
                continue;
            }
            Err(fs::TryLockError::Error(error)) => return Err(SpoolError::Io(error)),
        }
        let bytes = file.metadata()?.len();
        // Keep the exclusive lock held through removal so another process cannot make the
        // candidate live in the gap between the lock probe and deletion.
        fs::remove_file(entry.path())?;
        drop(file);
        report.removed_files = report
            .removed_files
            .checked_add(1)
            .ok_or(SpoolError::ScavengeCountOverflow)?;
        report.removed_bytes = report
            .removed_bytes
            .checked_add(bytes)
            .ok_or(SpoolError::ScavengeCountOverflow)?;
    }
    Ok(report)
}

fn validate_quota(quota: SpoolQuota) -> Result<(), SpoolError> {
    if quota.max_file_bytes < FILE_PREFIX_LEN as u64
        || quota.max_samples == 0
        || quota.max_records == 0
        || quota.max_record_samples == 0
        || quota.max_read_samples == 0
        || quota.max_record_samples > quota.max_read_samples
    {
        Err(SpoolError::InvalidQuota)
    } else {
        Ok(())
    }
}

fn encode_file_header(
    file_id: [u8; FILE_ID_LEN],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
) -> [u8; FILE_HEADER_LEN] {
    let mut output = [0_u8; FILE_HEADER_LEN];
    let mut cursor = 0;
    put(&mut output, &mut cursor, FILE_MAGIC);
    put(&mut output, &mut cursor, &FILE_VERSION.to_le_bytes());
    put(
        &mut output,
        &mut cursor,
        &CANONICAL_SAMPLE_RATE.to_le_bytes(),
    );
    put(&mut output, &mut cursor, &file_id);
    put(&mut output, &mut cursor, &nonce_prefix);
    output
}

fn encode_record_header(
    counter: u64,
    range: SampleRange,
    plaintext_len: u32,
    sealed_len: u32,
) -> [u8; RECORD_HEADER_LEN] {
    let mut output = [0_u8; RECORD_HEADER_LEN];
    let mut cursor = 0;
    put(&mut output, &mut cursor, RECORD_MAGIC);
    put(&mut output, &mut cursor, &counter.to_le_bytes());
    put(&mut output, &mut cursor, &range.start().to_le_bytes());
    put(
        &mut output,
        &mut cursor,
        &u32::try_from(range.len())
            .expect("record sample count was bounded before encoding")
            .to_le_bytes(),
    );
    put(&mut output, &mut cursor, &plaintext_len.to_le_bytes());
    put(&mut output, &mut cursor, &sealed_len.to_le_bytes());
    output
}

fn put<const N: usize>(output: &mut [u8; N], cursor: &mut usize, bytes: &[u8]) {
    let end = *cursor + bytes.len();
    output[*cursor..end].copy_from_slice(bytes);
    *cursor = end;
}

fn less_safe_key(key_bytes: &[u8; KEY_LEN]) -> Result<LessSafeKey, SpoolError> {
    let unbound = UnboundKey::new(&aead::AES_256_GCM, key_bytes)
        .map_err(|_| SpoolError::CryptoInitialization)?;
    Ok(LessSafeKey::new(unbound))
}

fn nonce_for(prefix: [u8; NONCE_PREFIX_LEN], counter: u64) -> Nonce {
    let mut bytes = [0_u8; NONCE_LEN];
    bytes[..NONCE_PREFIX_LEN].copy_from_slice(&prefix);
    bytes[NONCE_PREFIX_LEN..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn file_aad(header: &[u8; FILE_HEADER_LEN]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(FILE_AAD_DOMAIN.len() + header.len());
    aad.extend_from_slice(FILE_AAD_DOMAIN);
    aad.extend_from_slice(header);
    aad
}

fn record_aad(
    file_header: &[u8; FILE_HEADER_LEN],
    record_header: &[u8; RECORD_HEADER_LEN],
) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(RECORD_AAD_DOMAIN.len() + file_header.len() + record_header.len());
    aad.extend_from_slice(RECORD_AAD_DOMAIN);
    aad.extend_from_slice(file_header);
    aad.extend_from_slice(record_header);
    aad
}

fn read_exact_classified(reader: &mut impl Read, buffer: &mut [u8]) -> Result<(), SpoolError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Err(SpoolError::Truncated),
        Err(error) => Err(SpoolError::Io(error)),
    }
}

fn spool_file_name(file_id: [u8; FILE_ID_LEN]) -> String {
    let mut output =
        String::with_capacity(FILE_NAME_PREFIX.len() + FILE_ID_LEN * 2 + FILE_NAME_SUFFIX.len());
    output.push_str(FILE_NAME_PREFIX);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in file_id {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output.push_str(FILE_NAME_SUFFIX);
    output
}

fn is_spool_file_name(name: &str) -> bool {
    let Some(hex) = name
        .strip_prefix(FILE_NAME_PREFIX)
        .and_then(|name| name.strip_suffix(FILE_NAME_SUFFIX))
    else {
        return false;
    };
    hex.len() == FILE_ID_LEN * 2
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("spool root is not an empty or application-owned directory")]
    UnownedRoot,
    #[error("spool root owner marker is missing or invalid")]
    InvalidOwnerMarker,
    #[error("spool file is already active")]
    ActiveSpool,
    #[error("spool quota is invalid")]
    InvalidQuota,
    #[error("spool quota exceeded")]
    QuotaExceeded,
    #[error("audio record exceeds the configured record bound")]
    RecordTooLarge,
    #[error("append must start at {expected_start}, got {actual_start}")]
    NonContiguousAppend {
        expected_start: u64,
        actual_start: u64,
    },
    #[error("cannot read an empty range")]
    EmptyRead,
    #[error("cannot read from an empty spool")]
    EmptySpool,
    #[error("requested {requested:?} outside available {available:?}")]
    ReadOutOfBounds {
        available: SampleRange,
        requested: SampleRange,
    },
    #[error("requested range is too large for this platform")]
    ReadTooLarge,
    #[error("storage must be empty when a spool is initialized")]
    StorageNotEmpty,
    #[error("operating-system randomness is unavailable")]
    RandomnessUnavailable,
    #[error("AES-256-GCM initialization failed")]
    CryptoInitialization,
    #[error("audio encryption failed")]
    EncryptionFailed,
    #[error("spool authentication failed")]
    AuthenticationFailed,
    #[error("spool data is truncated")]
    Truncated,
    #[error("spool header is corrupt")]
    CorruptHeader,
    #[error("spool record is corrupt")]
    CorruptRecord,
    #[error("in-memory spool index does not cover the requested range")]
    IndexCoverageMismatch,
    #[error("nonce space exhausted")]
    NonceExhausted,
    #[error("spool is poisoned after an unrecoverable partial write")]
    Poisoned,
    #[error("spool has already been cleaned")]
    AlreadyCleaned,
    #[error("spool filename collision limit reached")]
    NameCollision,
    #[error("spool count overflow during scavenging")]
    ScavengeCountOverflow,
    #[error("allocation failed")]
    AllocationFailed,
    #[error("write failed and rollback also failed")]
    RollbackFailed {
        write_error: io::Error,
        rollback_error: io::Error,
    },
    #[error(transparent)]
    InvalidAudio(#[from] AudioSpanError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::cmp;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[derive(Default)]
    struct MemoryState {
        bytes: Vec<u8>,
        fail_write_at: Option<usize>,
        fail_truncate: bool,
        syncs: u64,
        cleaned: bool,
    }

    #[derive(Clone, Default)]
    struct MemoryHandle(Arc<Mutex<MemoryState>>);

    struct MemoryStorage {
        state: MemoryHandle,
        cursor: u64,
    }

    impl MemoryStorage {
        fn new() -> (Self, MemoryHandle) {
            let state = MemoryHandle::default();
            (
                Self {
                    state: state.clone(),
                    cursor: 0,
                },
                state,
            )
        }
    }

    impl Read for MemoryStorage {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let state = self.state.0.lock().unwrap();
            let cursor = usize::try_from(self.cursor).unwrap_or(usize::MAX);
            if cursor >= state.bytes.len() {
                return Ok(0);
            }
            let len = cmp::min(output.len(), state.bytes.len() - cursor);
            output[..len].copy_from_slice(&state.bytes[cursor..cursor + len]);
            drop(state);
            self.cursor += len as u64;
            Ok(len)
        }
    }

    impl Write for MemoryStorage {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let mut state = self.state.0.lock().unwrap();
            let cursor =
                usize::try_from(self.cursor).map_err(|_| io::Error::other("cursor overflow"))?;
            if state.fail_write_at.is_some_and(|limit| cursor >= limit) {
                return Err(io::Error::other("injected write failure"));
            }
            let allowed = state.fail_write_at.map_or(input.len(), |limit| {
                input.len().min(limit.saturating_sub(cursor))
            });
            if allowed == 0 {
                return Err(io::Error::other("injected write failure"));
            }
            let end = cursor + allowed;
            if state.bytes.len() < end {
                state.bytes.resize(end, 0);
            }
            state.bytes[cursor..end].copy_from_slice(&input[..allowed]);
            drop(state);
            self.cursor += allowed as u64;
            Ok(allowed)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for MemoryStorage {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let len = self.state.0.lock().unwrap().bytes.len() as i128;
            let next = match position {
                SeekFrom::Start(value) => i128::from(value),
                SeekFrom::End(value) => len + i128::from(value),
                SeekFrom::Current(value) => i128::from(self.cursor) + i128::from(value),
            };
            if !(0..=i128::from(u64::MAX)).contains(&next) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"));
            }
            self.cursor = next as u64;
            Ok(self.cursor)
        }
    }

    impl SpoolStorage for MemoryStorage {
        fn byte_len(&mut self) -> io::Result<u64> {
            Ok(self.state.0.lock().unwrap().bytes.len() as u64)
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            let mut state = self.state.0.lock().unwrap();
            if state.fail_truncate {
                return Err(io::Error::other("injected truncate failure"));
            }
            state.bytes.resize(
                usize::try_from(len).map_err(|_| io::Error::other("length overflow"))?,
                0,
            );
            Ok(())
        }

        fn sync_all(&mut self) -> io::Result<()> {
            self.state.0.lock().unwrap().syncs += 1;
            Ok(())
        }

        fn cleanup(&mut self) -> io::Result<()> {
            let mut state = self.state.0.lock().unwrap();
            state.bytes.clear();
            state.cleaned = true;
            Ok(())
        }
    }

    fn memory_spool() -> (EncryptedAudioSpool<MemoryStorage>, MemoryHandle) {
        let (storage, handle) = MemoryStorage::new();
        (
            EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default()).unwrap(),
            handle,
        )
    }

    fn span(start: u64, samples: &[f32]) -> AudioSpan {
        AudioSpan::new(start, samples.to_vec()).unwrap()
    }

    #[test]
    fn records_round_trip_across_exact_partial_ranges() {
        let (mut spool, _) = memory_spool();
        spool.append(&span(100, &[0.1, 0.2, 0.3])).unwrap();
        spool.append(&span(103, &[0.4, 0.5])).unwrap();
        assert_eq!(
            spool.indexed_ranges().collect::<Vec<_>>(),
            vec![
                SampleRange::new(100, 103).unwrap(),
                SampleRange::new(103, 105).unwrap()
            ]
        );
        let restored = spool
            .read_range(SampleRange::new(101, 105).unwrap())
            .unwrap();
        assert_eq!(restored.range(), SampleRange::new(101, 105).unwrap());
        assert_eq!(restored.samples(), &[0.2, 0.3, 0.4, 0.5]);
    }

    #[test]
    fn plaintext_audio_is_not_present_in_storage() {
        let (mut spool, handle) = memory_spool();
        let samples: Vec<f32> = (0..64).map(|value| value as f32 / 100.0).collect();
        let plaintext: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_bits().to_le_bytes())
            .collect();
        spool.append(&span(0, &samples)).unwrap();
        let state = handle.0.lock().unwrap();
        assert!(
            !state
                .bytes
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
    }

    #[test]
    fn noncontiguous_append_and_out_of_bounds_read_fail_closed() {
        let (mut spool, _) = memory_spool();
        spool.append(&span(10, &[0.0, 1.0])).unwrap();
        assert!(matches!(
            spool.append(&span(13, &[2.0])),
            Err(SpoolError::NonContiguousAppend { .. })
        ));
        assert!(matches!(
            spool.read_range(SampleRange::new(9, 11).unwrap()),
            Err(SpoolError::ReadOutOfBounds { .. })
        ));
        assert_eq!(spool.next_sample(), Some(12));
    }

    #[test]
    fn one_read_is_bounded_independently_from_total_spool_size() {
        let quota = SpoolQuota {
            max_record_samples: 2,
            max_read_samples: 2,
            ..SpoolQuota::default()
        };
        let (storage, _) = MemoryStorage::new();
        let mut spool = EncryptedAudioSpool::from_empty_storage(storage, quota).unwrap();
        spool.append(&span(0, &[0.1, 0.2])).unwrap();
        spool.append(&span(2, &[0.3])).unwrap();
        assert!(matches!(
            spool.read_range(SampleRange::new(0, 3).unwrap()),
            Err(SpoolError::ReadTooLarge)
        ));
        assert_eq!(
            spool
                .read_range(SampleRange::new(1, 3).unwrap())
                .unwrap()
                .samples(),
            &[0.2, 0.3]
        );
    }

    #[test]
    fn ciphertext_tamper_is_rejected() {
        let (mut spool, handle) = memory_spool();
        spool.append(&span(0, &[0.1, 0.2, 0.3])).unwrap();
        let ciphertext_offset = spool.index[0].offset as usize + RECORD_HEADER_LEN;
        handle.0.lock().unwrap().bytes[ciphertext_offset] ^= 0x80;
        assert!(matches!(
            spool.read_range(SampleRange::new(0, 3).unwrap()),
            Err(SpoolError::AuthenticationFailed)
        ));
    }

    #[test]
    fn authenticated_record_header_tamper_is_rejected() {
        let (mut spool, handle) = memory_spool();
        spool.append(&span(0, &[0.1])).unwrap();
        let offset = spool.index[0].offset as usize + 5;
        handle.0.lock().unwrap().bytes[offset] ^= 1;
        assert!(matches!(
            spool.read_range(SampleRange::new(0, 1).unwrap()),
            Err(SpoolError::CorruptHeader)
        ));
    }

    #[test]
    fn file_header_tamper_is_rejected() {
        let (mut spool, handle) = memory_spool();
        spool.append(&span(0, &[0.1])).unwrap();
        handle.0.lock().unwrap().bytes[3] ^= 1;
        assert!(matches!(
            spool.read_range(SampleRange::new(0, 1).unwrap()),
            Err(SpoolError::CorruptHeader)
        ));
    }

    #[test]
    fn truncation_is_distinguished_from_authentication_failure() {
        let (mut spool, handle) = memory_spool();
        spool.append(&span(0, &[0.1, 0.2])).unwrap();
        handle.0.lock().unwrap().bytes.pop();
        assert!(matches!(
            spool.read_range(SampleRange::new(0, 2).unwrap()),
            Err(SpoolError::Truncated)
        ));
    }

    #[test]
    fn quota_is_preflighted_without_partial_writes() {
        let quota = SpoolQuota {
            max_file_bytes: (FILE_PREFIX_LEN + RECORD_HEADER_LEN + TAG_LEN + 4) as u64,
            max_samples: 1,
            max_records: 1,
            max_record_samples: 1,
            max_read_samples: 1,
        };
        let (storage, handle) = MemoryStorage::new();
        let mut spool = EncryptedAudioSpool::from_empty_storage(storage, quota).unwrap();
        let initial_len = handle.0.lock().unwrap().bytes.len();
        assert!(matches!(
            spool.append(&span(0, &[0.0, 1.0])),
            Err(SpoolError::RecordTooLarge)
        ));
        assert_eq!(handle.0.lock().unwrap().bytes.len(), initial_len);
        spool.append(&span(0, &[0.0])).unwrap();
        let committed_len = handle.0.lock().unwrap().bytes.len();
        assert!(matches!(
            spool.append(&span(1, &[1.0])),
            Err(SpoolError::QuotaExceeded)
        ));
        assert_eq!(handle.0.lock().unwrap().bytes.len(), committed_len);
    }

    #[test]
    fn injected_partial_write_rolls_back_and_can_retry() {
        let (mut spool, handle) = memory_spool();
        let committed_len = spool.file_len as usize;
        handle.0.lock().unwrap().fail_write_at = Some(committed_len + RECORD_HEADER_LEN + 2);
        assert!(matches!(
            spool.append(&span(0, &[0.1, 0.2])),
            Err(SpoolError::Io(_))
        ));
        assert_eq!(handle.0.lock().unwrap().bytes.len(), committed_len);
        assert_eq!(spool.next_sample(), None);
        assert_eq!(spool.next_counter, 2, "failed write must burn nonce 1");
        handle.0.lock().unwrap().fail_write_at = None;
        spool.append(&span(0, &[0.7, 0.8])).unwrap();
        assert_eq!(spool.next_sample(), Some(2));
        assert_eq!(spool.index[0].counter, 2);
        assert_eq!(spool.next_counter, 3);
        assert_eq!(
            spool
                .read_range(SampleRange::new(0, 2).unwrap())
                .unwrap()
                .samples(),
            &[0.7, 0.8]
        );
    }

    #[test]
    fn failed_rollback_poison_spool() {
        let (mut spool, handle) = memory_spool();
        let committed_len = spool.file_len as usize;
        {
            let mut state = handle.0.lock().unwrap();
            state.fail_write_at = Some(committed_len + 1);
            state.fail_truncate = true;
        }
        assert!(matches!(
            spool.append(&span(0, &[0.1])),
            Err(SpoolError::RollbackFailed { .. })
        ));
        assert!(matches!(spool.flush(), Err(SpoolError::Poisoned)));
    }

    #[test]
    fn flush_and_explicit_cleanup_reach_storage() {
        let (mut spool, handle) = memory_spool();
        spool.append(&span(0, &[0.1])).unwrap();
        spool.flush().unwrap();
        assert_eq!(handle.0.lock().unwrap().syncs, 1);
        spool.cleanup().unwrap();
        let state = handle.0.lock().unwrap();
        assert!(state.cleaned);
        assert!(state.bytes.is_empty());
    }

    #[test]
    fn drop_performs_best_effort_cleanup() {
        let handle = {
            let (mut spool, handle) = memory_spool();
            spool.append(&span(0, &[0.1])).unwrap();
            handle
        };
        assert!(handle.0.lock().unwrap().cleaned);
    }

    #[test]
    fn many_bounded_records_round_trip_random_ranges() {
        let (mut spool, _) = memory_spool();
        let mut expected = Vec::new();
        for record in 0..512_u64 {
            let samples: Vec<f32> = (0..257)
                .map(|offset| ((record * 257 + offset) % 10_000) as f32 / 10_000.0)
                .collect();
            let start = expected.len() as u64;
            spool.append(&span(start, &samples)).unwrap();
            expected.extend(samples);
        }
        let mut state = 0xdecafbad_u64;
        for _ in 0..1_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let start = state % (expected.len() as u64 - 1);
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let max_width = (expected.len() as u64 - start).min(2_048);
            let width = 1 + state % max_width;
            let end = start + width;
            let restored = spool
                .read_range(SampleRange::new(start, end).unwrap())
                .unwrap();
            assert_eq!(restored.samples(), &expected[start as usize..end as usize]);
        }
    }

    fn unique_test_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("phorminx-session-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn file_spool_cleanup_removes_the_file() {
        let directory = unique_test_directory();
        let root = SpoolRoot::open_or_create(&directory).unwrap();
        let mut spool = EncryptedAudioSpool::create(&root, SpoolQuota::default()).unwrap();
        let path = spool.path().to_owned();
        spool.append(&span(0, &[0.1])).unwrap();
        assert!(path.exists());
        spool.cleanup().unwrap();
        assert!(!path.exists());
        fs::remove_file(directory.join(OWNER_MARKER_NAME)).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn startup_scavenger_only_removes_exact_spool_names() {
        let directory = unique_test_directory();
        let root = SpoolRoot::open_or_create(&directory).unwrap();
        let orphan = directory.join(format!(
            "{FILE_NAME_PREFIX}{}{FILE_NAME_SUFFIX}",
            "a".repeat(32)
        ));
        let unrelated = directory.join("keep-me.pxs");
        fs::write(&orphan, [1_u8, 2, 3]).unwrap();
        fs::write(&unrelated, [4_u8]).unwrap();
        let report = scavenge_orphans(&root).unwrap();
        assert_eq!(
            report,
            ScavengeReport {
                removed_files: 1,
                removed_bytes: 3,
                skipped_live_files: 0,
            }
        );
        assert!(!orphan.exists());
        assert!(unrelated.exists());
        fs::remove_file(unrelated).unwrap();
        fs::remove_file(directory.join(OWNER_MARKER_NAME)).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn unmarked_nonempty_directory_is_never_a_spool_root() {
        let directory = unique_test_directory();
        fs::create_dir(&directory).unwrap();
        let user_file = directory.join("user-data.txt");
        fs::write(&user_file, b"keep").unwrap();
        assert!(matches!(
            SpoolRoot::open_or_create(&directory),
            Err(SpoolError::UnownedRoot)
        ));
        assert_eq!(fs::read(&user_file).unwrap(), b"keep");
        fs::remove_file(user_file).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn scavenger_refuses_a_live_locked_spool() {
        let directory = unique_test_directory();
        let root = SpoolRoot::open_or_create(&directory).unwrap();
        let spool = EncryptedAudioSpool::create(&root, SpoolQuota::default()).unwrap();
        let path = spool.path().to_owned();
        let report = scavenge_orphans(&root).unwrap();
        assert_eq!(report.removed_files, 0);
        assert_eq!(report.skipped_live_files, 1);
        assert!(path.exists());
        drop(spool);
        assert!(!path.exists());
        fs::remove_file(directory.join(OWNER_MARKER_NAME)).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn record_decrypt_allocation_cannot_exceed_read_bound() {
        let invalid = SpoolQuota {
            max_record_samples: 10,
            max_read_samples: 9,
            ..SpoolQuota::default()
        };
        let (storage, _) = MemoryStorage::new();
        assert!(matches!(
            EncryptedAudioSpool::from_empty_storage(storage, invalid),
            Err(SpoolError::InvalidQuota)
        ));
    }
}
