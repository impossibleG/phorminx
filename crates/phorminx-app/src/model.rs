use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use phorminx_windows::atomic_replace_file;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::settings::AccurateModelVariant;

const EMBEDDED_MANIFEST: &str = include_str!("../../../config/model-manifest.json");
const DOWNLOAD_BUFFER_BYTES: usize = 128 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub file_name: String,
    pub language: String,
    pub quantization: String,
    pub bytes: u64,
    pub sha256: String,
    pub url: String,
    pub license: String,
}

pub fn recommended_model() -> Result<ModelSpec, ModelError> {
    model_for_variant(AccurateModelVariant::BaseEnglish)
}

/// Returns every pinned artifact in deterministic manifest order.
pub fn pinned_models() -> Result<Vec<ModelSpec>, ModelError> {
    let manifest: ModelManifest =
        serde_json::from_str(EMBEDDED_MANIFEST).map_err(ModelError::Manifest)?;
    if manifest.schema_version != 1 {
        return Err(ModelError::UnsupportedManifestVersion(
            manifest.schema_version,
        ));
    }
    let _ = manifest.source_revision;
    Ok(manifest.models.into_iter().map(Into::into).collect())
}

pub fn model_for_variant(variant: AccurateModelVariant) -> Result<ModelSpec, ModelError> {
    let id = variant
        .manifest_id()
        .ok_or(ModelError::CustomVariantHasNoPinnedModel)?;
    pinned_models()?
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| ModelError::PinnedModelMissing(id.to_owned()))
}

/// Infers a built-in variant only after filename, byte count, and SHA-256 match.
pub fn identify_pinned_model(path: &Path) -> Result<Option<AccurateModelVariant>, ModelError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ModelError::Inspect {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let candidates = pinned_models()?
        .into_iter()
        .filter(|model| model.file_name == file_name && model.bytes == metadata.len())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let actual = sha256_file(path)?;
    Ok(candidates.into_iter().find_map(|model| {
        actual
            .eq_ignore_ascii_case(&model.sha256)
            .then(|| AccurateModelVariant::from_manifest_id(&model.id))
            .flatten()
    }))
}

fn sha256_file(path: &Path) -> Result<String, ModelError> {
    let mut file = File::open(path).map_err(|source| ModelError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; DOWNLOAD_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| ModelError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub struct ModelDownload {
    events: Receiver<ModelDownloadEvent>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ModelDownload {
    pub fn start(destination_directory: &Path) -> Result<Self, ModelError> {
        Self::start_variant(destination_directory, AccurateModelVariant::BaseEnglish)
    }

    pub fn start_variant(
        destination_directory: &Path,
        variant: AccurateModelVariant,
    ) -> Result<Self, ModelError> {
        let spec = model_for_variant(variant)?;
        let destination = destination_directory.join(&spec.file_name);
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("phorminx-model-download".to_owned())
            .spawn(move || {
                let result = download_model(&spec, &destination, &thread_cancel, |downloaded| {
                    let _ = event_tx.send(ModelDownloadEvent::Progress {
                        downloaded,
                        total: spec.bytes,
                    });
                });
                let event = match result {
                    Ok(()) => ModelDownloadEvent::Completed { path: destination },
                    Err(ModelError::Cancelled) => ModelDownloadEvent::Cancelled,
                    Err(error) => ModelDownloadEvent::Failed(error.to_string()),
                };
                let _ = event_tx.send(event);
            })
            .map_err(ModelError::Spawn)?;
        Ok(Self {
            events: event_rx,
            cancel,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<ModelDownloadEvent> {
        &self.events
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn shutdown(mut self) -> Result<(), ModelError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), ModelError> {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| ModelError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for ModelDownload {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelDownloadEvent {
    Progress { downloaded: u64, total: u64 },
    Completed { path: PathBuf },
    Cancelled,
    Failed(String),
}

fn download_model(
    spec: &ModelSpec,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<(), ModelError> {
    let directory = destination
        .parent()
        .ok_or_else(|| ModelError::InvalidDestination(destination.to_path_buf()))?;
    fs::create_dir_all(directory).map_err(|source| ModelError::CreateDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    let (temporary_path, mut temporary) = create_temporary_file(directory)?;
    let result = (|| {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build();
        let agent: ureq::Agent = config.into();
        let response = agent
            .get(&spec.url)
            .call()
            .map_err(|source| ModelError::Http(source.to_string()))?;
        let mut reader = response.into_parts().1.into_reader();
        stream_and_verify(
            &mut reader,
            &mut temporary,
            spec.bytes,
            &spec.sha256,
            cancel,
            progress,
        )?;
        temporary
            .flush()
            .and_then(|_| temporary.sync_all())
            .map_err(|source| ModelError::Write {
                path: temporary_path.clone(),
                source,
            })?;
        drop(temporary);
        atomic_replace_file(&temporary_path, destination).map_err(ModelError::AtomicReplace)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn stream_and_verify(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected_bytes: u64,
    expected_sha256: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), ModelError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; DOWNLOAD_BUFFER_BYTES];
    let mut downloaded = 0_u64;
    let mut last_percent = u64::MAX;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(ModelError::Cancelled);
        }
        let count = reader.read(&mut buffer).map_err(ModelError::ReadResponse)?;
        if count == 0 {
            break;
        }
        downloaded = downloaded
            .checked_add(count as u64)
            .ok_or(ModelError::DownloadTooLarge)?;
        if downloaded > expected_bytes {
            return Err(ModelError::ByteCount {
                expected: expected_bytes,
                actual: downloaded,
            });
        }
        writer
            .write_all(&buffer[..count])
            .map_err(ModelError::WriteStream)?;
        hasher.update(&buffer[..count]);
        let percent = downloaded.saturating_mul(100) / expected_bytes.max(1);
        if percent != last_percent {
            progress(downloaded);
            last_percent = percent;
        }
    }
    if downloaded != expected_bytes {
        return Err(ModelError::ByteCount {
            expected: expected_bytes,
            actual: downloaded,
        });
    }
    let actual_sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(ModelError::Checksum {
            expected: expected_sha256.to_owned(),
            actual: actual_sha256,
        });
    }
    Ok(())
}

fn create_temporary_file(directory: &Path) -> Result<(PathBuf, File), ModelError> {
    for _ in 0..100 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".phorminx-model.{}.{}.download",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(ModelError::CreateTemporary { path, source }),
        }
    }
    Err(ModelError::TemporaryNameExhausted(directory.to_path_buf()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelManifest {
    schema_version: u32,
    source_revision: String,
    models: Vec<ManifestModel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestModel {
    id: String,
    file_name: String,
    language: String,
    quantization: String,
    bytes: u64,
    sha256: String,
    url: String,
    license: String,
}

impl From<ManifestModel> for ModelSpec {
    fn from(model: ManifestModel) -> Self {
        Self {
            id: model.id,
            file_name: model.file_name,
            language: model.language,
            quantization: model.quantization,
            bytes: model.bytes,
            sha256: model.sha256,
            url: model.url,
            license: model.license,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("the embedded model manifest is invalid: {0}")]
    Manifest(serde_json::Error),
    #[error("model manifest version {0} is not supported")]
    UnsupportedManifestVersion(u32),
    #[error("the pinned model {0} is missing from the embedded manifest")]
    PinnedModelMissing(String),
    #[error("a custom model variant has no pinned artifact")]
    CustomVariantHasNoPinnedModel,
    #[error("failed to inspect model {path}: {source}")]
    Inspect {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid model destination: {0}")]
    InvalidDestination(PathBuf),
    #[error("failed to create model directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create temporary model file {path}: {source}")]
    CreateTemporary {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not allocate a unique temporary model file in {0}")]
    TemporaryNameExhausted(PathBuf),
    #[error("model download request failed: {0}")]
    Http(String),
    #[error("failed while reading the model response: {0}")]
    ReadResponse(std::io::Error),
    #[error("download size overflowed")]
    DownloadTooLarge,
    #[error("model size mismatch: expected {expected} bytes, received {actual}")]
    ByteCount { expected: u64, actual: u64 },
    #[error("failed while writing the model: {0}")]
    WriteStream(std::io::Error),
    #[error("model checksum mismatch: expected {expected}, received {actual}")]
    Checksum { expected: String, actual: String },
    #[error("failed to flush downloaded model {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    AtomicReplace(phorminx_windows::AtomicReplaceError),
    #[error("model download was cancelled")]
    Cancelled,
    #[error("failed to start the model download thread: {0}")]
    Spawn(std::io::Error),
    #[error("the model download thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn embedded_manifest_selects_the_pinned_english_model() {
        let model = recommended_model().unwrap();
        assert_eq!(model.id, "whisper-base-en-f16");
        assert_eq!(model.file_name, "ggml-base.en.bin");
        assert_eq!(model.bytes, 147_964_211);
        assert_eq!(model.sha256.len(), 64);
        assert!(model.url.starts_with("https://"));
    }

    #[test]
    fn manifest_pins_fast_accurate_and_multilingual_variants() {
        let models = pinned_models().unwrap();
        for variant in [
            AccurateModelVariant::TinyEnglish,
            AccurateModelVariant::BaseEnglish,
            AccurateModelVariant::TinyMultilingual,
            AccurateModelVariant::BaseMultilingual,
        ] {
            let model = model_for_variant(variant).unwrap();
            assert!(models.contains(&model));
            assert_eq!(model.sha256.len(), 64);
            assert!(model.bytes > 70_000_000);
            assert!(
                model
                    .url
                    .contains("c521a4b02f422512d734391fdf08bb08c0862f68")
            );
        }
        assert_eq!(
            model_for_variant(AccurateModelVariant::TinyEnglish)
                .unwrap()
                .bytes,
            77_704_715
        );
        assert_eq!(
            model_for_variant(AccurateModelVariant::BaseMultilingual)
                .unwrap()
                .sha256,
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"
        );
    }

    #[test]
    fn familiar_filename_without_verified_identity_is_not_pinned() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ggml-tiny.en.bin");
        fs::write(&path, b"not a model").unwrap();
        assert_eq!(identify_pinned_model(&path).unwrap(), None);
    }

    #[test]
    fn stream_verifies_size_and_sha256() {
        let mut output = Vec::new();
        let mut progress = Vec::new();
        stream_and_verify(
            &mut Cursor::new(b"abc"),
            &mut output,
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            &AtomicBool::new(false),
            |downloaded| progress.push(downloaded),
        )
        .unwrap();
        assert_eq!(output, b"abc");
        assert_eq!(progress, [3]);
    }

    #[test]
    fn stream_rejects_size_checksum_and_cancellation() {
        let mut output = Vec::new();
        assert!(matches!(
            stream_and_verify(
                &mut Cursor::new(b"abc"),
                &mut output,
                4,
                "irrelevant",
                &AtomicBool::new(false),
                |_| {}
            ),
            Err(ModelError::ByteCount { .. })
        ));
        output.clear();
        assert!(matches!(
            stream_and_verify(
                &mut Cursor::new(b"abc"),
                &mut output,
                3,
                "wrong",
                &AtomicBool::new(false),
                |_| {}
            ),
            Err(ModelError::Checksum { .. })
        ));
        assert!(matches!(
            stream_and_verify(
                &mut Cursor::new(b"abc"),
                &mut Vec::new(),
                3,
                "irrelevant",
                &AtomicBool::new(true),
                |_| {}
            ),
            Err(ModelError::Cancelled)
        ));
    }
}
