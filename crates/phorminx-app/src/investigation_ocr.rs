//! Verified, user-initiated download of the local OCR models and the
//! background worker that extracts screenshot text for investigations.
//!
//! The models are PaddleOCR PP-OCR ONNX exports pinned to exact repository
//! revisions, byte counts and SHA-256 digests (see ADR-018). Nothing is
//! downloaded until the user asks; recognition never uses an operating-system
//! OCR service or the network.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use phorminx_ocr::OcrEngine;
use phorminx_persistence::{OcrLineRecord, Persistence};
use phorminx_windows::atomic_replace_file;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::model::{ModelError, create_temporary_file, stream_and_verify};

pub const OCR_MODEL_DIRECTORY: &str = "ocr/paddleocr-latin-v1";

pub struct OcrAsset {
    pub file_name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// PP-OCRv4 mobile text detection (multilingual) and PP-OCRv5 Latin mobile
/// recognition (English, Spanish and 30 other Latin-script languages), both
/// Apache-2.0. Digests were measured from the pinned revisions.
pub const OCR_ASSETS: [OcrAsset; 3] = [
    OcrAsset {
        file_name: "det.onnx",
        url: "https://huggingface.co/SWHL/RapidOCR/resolve/1cfba2e90fc938db55889873735088de210cc173/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx",
        bytes: 4_745_517,
        sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
    },
    OcrAsset {
        file_name: "rec-latin.onnx",
        url: "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65/languages/latin/rec.onnx",
        bytes: 7_862_832,
        sha256: "614ffc2d6d3902d360fad7f1b0dd455ee45e877069d14c4e51a99dc4ef144409",
    },
    OcrAsset {
        file_name: "latin-dict.txt",
        url: "https://huggingface.co/monkt/paddleocr-onnx/resolve/7b02d0a30a07ba2b92ad1ff5a8941ae2c633de65/languages/latin/dict.txt",
        bytes: 1_634,
        sha256: "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
    },
];

pub fn total_download_bytes() -> u64 {
    OCR_ASSETS.iter().map(|asset| asset.bytes).sum()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrModelView {
    /// `missing`, `downloading`, `ready` or `failed`.
    pub state: String,
    pub message: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelState {
    Missing,
    Downloading { downloaded: u64 },
    Ready,
    Failed(String),
}

/// Owns the OCR model folder: status, a single download at a time, removal.
pub struct OcrModelManager {
    root: PathBuf,
    state: Arc<Mutex<ModelState>>,
    cancel: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl OcrModelManager {
    pub fn new(root: PathBuf, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        let state = if assets_present(&root) {
            ModelState::Ready
        } else {
            ModelState::Missing
        };
        Self {
            root,
            state: Arc::new(Mutex::new(state)),
            cancel: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            on_change,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ready(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| *state == ModelState::Ready)
    }

    pub fn view(&self) -> OcrModelView {
        let state = self
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or(ModelState::Failed("Model status is unavailable.".into()));
        let total = total_download_bytes();
        let (name, message, downloaded) = match state {
            ModelState::Missing => (
                "missing",
                format!(
                    "Download the verified screenshot text models ({:.1} MB) to read text from images on this computer.",
                    total as f64 / 1_000_000.0
                ),
                0,
            ),
            ModelState::Downloading { downloaded } => (
                "downloading",
                format!(
                    "Downloading and verifying… {}%",
                    downloaded.saturating_mul(100) / total.max(1)
                ),
                downloaded,
            ),
            ModelState::Ready => (
                "ready",
                "Installed and verified. Screenshot text is read locally.".into(),
                total,
            ),
            ModelState::Failed(message) => ("failed", message, 0),
        };
        OcrModelView {
            state: name.into(),
            message,
            downloaded_bytes: downloaded,
            total_bytes: total,
        }
    }

    /// Reports installed files that failed verification or could not load, so
    /// screenshots do not wait silently. Downloading again repairs them.
    pub fn mark_unusable(&self) {
        if let Ok(mut state) = self.state.lock()
            && *state == ModelState::Ready
        {
            *state = ModelState::Failed(
                "The installed text models failed verification. Download them again to repair them.".into(),
            );
        }
        (self.on_change)();
    }

    /// Starts the verified download unless it is installed or already running.
    pub fn install(&self) -> Result<(), String> {
        let mut slot = self
            .thread
            .lock()
            .map_err(|_| "Model download is unavailable.")?;
        if slot.as_ref().is_some_and(|thread| !thread.is_finished()) {
            return Ok(());
        }
        if let Some(thread) = slot.take() {
            let _ = thread.join();
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Model download is unavailable.")?;
            if *state == ModelState::Ready {
                return Ok(());
            }
            *state = ModelState::Downloading { downloaded: 0 };
        }
        self.cancel.store(false, Ordering::Release);
        let root = self.root.clone();
        let state = self.state.clone();
        let cancel = self.cancel.clone();
        let on_change = self.on_change.clone();
        *slot = Some(
            thread::Builder::new()
                .name("phorminx-ocr-download".into())
                .spawn(move || {
                    let progress_state = state.clone();
                    let progress_change = on_change.clone();
                    let result = download_assets(&root, &cancel, move |downloaded| {
                        if let Ok(mut state) = progress_state.lock() {
                            *state = ModelState::Downloading { downloaded };
                        }
                        progress_change();
                    });
                    if let Ok(mut state) = state.lock() {
                        *state = match result {
                            Ok(()) => ModelState::Ready,
                            Err(ModelError::Cancelled) => ModelState::Missing,
                            Err(_) => ModelState::Failed(
                                "The download could not be completed or verified. Nothing was activated; retry when you are online.".into(),
                            ),
                        };
                    }
                    on_change();
                })
                .map_err(|_| "Model download could not start.")?,
        );
        Ok(())
    }

    /// Deletes the installed models. Existing extracted text is kept.
    pub fn remove(&self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self
            .thread
            .lock()
            .map_err(|_| "Model removal is unavailable.")?
            .take()
        {
            let _ = thread.join();
        }
        if self.root.exists() {
            fs::remove_dir_all(&self.root).map_err(|_| {
                "The model folder could not be removed. Close other programs using it and retry."
            })?;
        }
        if let Ok(mut state) = self.state.lock() {
            *state = ModelState::Missing;
        }
        (self.on_change)();
        Ok(())
    }
}

impl Drop for OcrModelManager {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Ok(mut slot) = self.thread.lock()
            && let Some(thread) = slot.take()
        {
            let _ = thread.join();
        }
    }
}

fn assets_present(root: &Path) -> bool {
    OCR_ASSETS.iter().all(|asset| {
        fs::metadata(root.join(asset.file_name))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == asset.bytes)
    })
}

/// Re-hashes installed files before they are loaded, so a corrupted or
/// replaced model is never executed.
pub fn verify_installed(root: &Path) -> bool {
    OCR_ASSETS.iter().all(|asset| {
        fs::read(root.join(asset.file_name)).is_ok_and(|bytes| {
            bytes.len() as u64 == asset.bytes && hex_sha256(&bytes) == asset.sha256
        })
    })
}

fn download_assets(
    root: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u64),
) -> Result<(), ModelError> {
    fs::create_dir_all(root).map_err(|source| ModelError::CreateDirectory {
        path: root.to_path_buf(),
        source,
    })?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut completed = 0u64;
    for asset in &OCR_ASSETS {
        let destination = root.join(asset.file_name);
        if fs::metadata(&destination).is_ok_and(|m| m.len() == asset.bytes)
            && fs::read(&destination).is_ok_and(|bytes| hex_sha256(&bytes) == asset.sha256)
        {
            completed += asset.bytes;
            progress(completed);
            continue;
        }
        let (temporary_path, mut temporary) = create_temporary_file(root)?;
        let result = (|| {
            let response = agent
                .get(asset.url)
                .call()
                .map_err(|error| ModelError::Http(error.to_string()))?;
            let mut reader = response.into_parts().1.into_reader();
            let base = completed;
            stream_and_verify(
                &mut reader,
                &mut temporary,
                asset.bytes,
                asset.sha256,
                cancel,
                |downloaded| progress(base + downloaded),
            )?;
            temporary
                .flush()
                .and_then(|_| temporary.sync_all())
                .map_err(ModelError::WriteStream)?;
            drop(temporary);
            atomic_replace_file(&temporary_path, &destination).map_err(ModelError::AtomicReplace)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result?;
        completed += asset.bytes;
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Shared wake-up and pause signal for background investigation workers.
#[derive(Default)]
pub struct WorkerSignal {
    generation: Mutex<u64>,
    wake: Condvar,
    stopping: AtomicBool,
    paused: AtomicU8,
}

/// Independent reasons to pause CPU-heavy background work.
pub const PAUSE_DICTATION: u8 = 1;
pub const PAUSE_VOICE_NOTE: u8 = 2;

impl WorkerSignal {
    pub fn notify(&self) {
        if let Ok(mut generation) = self.generation.lock() {
            *generation = generation.wrapping_add(1);
        }
        self.wake.notify_all();
    }

    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.notify();
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Pauses CPU-heavy work while speech is being captured. Each reason is
    /// tracked separately so one ending cannot resume work another needs paused.
    pub fn set_paused(&self, reason: u8, paused: bool) {
        let previous = if paused {
            self.paused.fetch_or(reason, Ordering::AcqRel)
        } else {
            self.paused.fetch_and(!reason, Ordering::AcqRel)
        };
        let now = if paused {
            previous | reason
        } else {
            previous & !reason
        };
        if previous != 0 && now == 0 {
            self.notify();
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire) != 0
    }

    /// Waits for a notification or the timeout, whichever comes first.
    pub fn wait(&self, timeout: Duration) {
        let Ok(generation) = self.generation.lock() else {
            return;
        };
        let start = *generation;
        let _ = self
            .wake
            .wait_timeout_while(generation, timeout, |current| {
                *current == start && !self.stopping()
            });
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Extracts text for pending screenshots, oldest first. The engine is loaded
/// lazily and released after a few idle minutes to return its memory.
pub struct OcrWorker {
    signal: Arc<WorkerSignal>,
    thread: Option<JoinHandle<()>>,
}

impl OcrWorker {
    pub fn spawn(
        database: PathBuf,
        images: PathBuf,
        models: Arc<OcrModelManager>,
        signal: Arc<WorkerSignal>,
        revision: Arc<AtomicU64>,
    ) -> Result<Self, String> {
        let thread_signal = signal.clone();
        let thread = thread::Builder::new()
            .name("phorminx-investigation-ocr".into())
            .spawn(move || ocr_loop(database, images, models, thread_signal, revision))
            .map_err(|_| "Screenshot text recognition could not start.")?;
        Ok(Self {
            signal,
            thread: Some(thread),
        })
    }
}

impl Drop for OcrWorker {
    fn drop(&mut self) {
        self.signal.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn ocr_loop(
    database: PathBuf,
    images: PathBuf,
    models: Arc<OcrModelManager>,
    signal: Arc<WorkerSignal>,
    revision: Arc<AtomicU64>,
) {
    let Ok(db) = Persistence::open_with_busy_timeout(&database, Duration::from_secs(2)) else {
        return;
    };
    let mut engine: Option<OcrEngine> = None;
    let mut last_used = Instant::now();
    let mut load_failed_for: Option<Instant> = None;
    while !signal.stopping() {
        if engine.is_some() && last_used.elapsed() > Duration::from_secs(300) {
            engine = None;
        }
        if signal.paused() || !models.ready() {
            signal.wait(Duration::from_secs(2));
            continue;
        }
        let Ok(Some(job)) = db.investigations().next_pending_ocr() else {
            signal.wait(Duration::from_secs(3));
            continue;
        };
        if engine.is_none() {
            if load_failed_for.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
                signal.wait(Duration::from_secs(5));
                continue;
            }
            let root = models.root().to_path_buf();
            engine = verify_installed(&root)
                .then(|| {
                    OcrEngine::load(
                        &root.join(OCR_ASSETS[0].file_name),
                        &root.join(OCR_ASSETS[1].file_name),
                        &root.join(OCR_ASSETS[2].file_name),
                    )
                    .ok()
                })
                .flatten();
            if engine.is_none() {
                load_failed_for = Some(Instant::now());
                models.mark_unusable();
                revision.fetch_add(1, Ordering::AcqRel);
                continue;
            }
            load_failed_for = None;
        }
        let Some(ocr) = engine.as_ref() else {
            continue;
        };
        last_used = Instant::now();
        let path = images
            .join(job.investigation_id.to_string())
            .join(format!("{}.png", job.target));
        let result = fs::read(&path)
            .map_err(|_| ())
            .and_then(|bytes| ocr.recognize_encoded(&bytes).map_err(|_| ()));
        let repo = db.investigations();
        let saved = match result {
            Ok(lines) => repo.save_ocr(
                job.item_id,
                &lines
                    .into_iter()
                    .map(|line| OcrLineRecord {
                        text: line.text,
                        confidence: line.confidence,
                        left: line.left,
                        top: line.top,
                        width: line.width,
                        height: line.height,
                    })
                    .collect::<Vec<_>>(),
                now_ms(),
            ),
            Err(()) => repo.fail_ocr(job.item_id, now_ms()),
        };
        if saved.is_err() {
            // Avoid a hot loop on a locked or failing database.
            signal.wait(Duration::from_secs(2));
        }
        revision.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_assets_are_https_revision_pinned_and_well_formed() {
        for asset in &OCR_ASSETS {
            assert!(asset.url.starts_with("https://huggingface.co/"));
            assert!(asset.url.contains("/resolve/") && !asset.url.contains("/resolve/main/"));
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(asset.bytes > 0);
        }
        assert_eq!(total_download_bytes(), 12_609_983);
    }

    #[test]
    fn installed_files_are_detected_by_size_and_verified_by_digest() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!assets_present(dir.path()));
        for asset in &OCR_ASSETS {
            fs::write(
                dir.path().join(asset.file_name),
                vec![0u8; asset.bytes as usize],
            )
            .unwrap();
        }
        assert!(assets_present(dir.path()));
        // Right size, wrong contents: never loaded.
        assert!(!verify_installed(dir.path()));
        let manager = OcrModelManager::new(dir.path().to_path_buf(), Arc::new(|| {}));
        assert_eq!(manager.view().state, "ready");
        manager.mark_unusable();
        assert_eq!(manager.view().state, "failed");
        manager.remove().unwrap();
        assert_eq!(manager.view().state, "missing");
        assert!(!dir.path().exists());
    }

    #[test]
    fn worker_signal_pauses_and_wakes() {
        let signal = Arc::new(WorkerSignal::default());
        signal.set_paused(PAUSE_DICTATION, true);
        signal.set_paused(PAUSE_VOICE_NOTE, true);
        signal.set_paused(PAUSE_DICTATION, false);
        assert!(signal.paused(), "a voice note still holds the pause");
        assert!(signal.paused());
        let waiter = signal.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || waiter.wait(Duration::from_secs(10)));
        thread::sleep(Duration::from_millis(50));
        signal.set_paused(PAUSE_VOICE_NOTE, false);
        handle.join().unwrap();
        assert!(!signal.paused());
        assert!(started.elapsed() < Duration::from_secs(5));
        signal.stop();
        signal.wait(Duration::from_secs(10));
        assert!(signal.stopping());
    }
}
