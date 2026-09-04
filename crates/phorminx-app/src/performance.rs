use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use phorminx_setup::{
    BackendKind, BenchmarkCandidate, BenchmarkContext, BenchmarkEvidence, BenchmarkObservation,
    CalibrationCase, CalibrationCorpus, ContentFreeId, ContentionCondition, EngineKind, Language,
    QualityCounts, Recommendation, RecommendationEngine, RecommendationOutcome,
    RecommendationPolicy, RecommendationPreference, ThermalCondition, aggregate_evidence,
};
use phorminx_windows::atomic_replace_file;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::{
    AccurateBackendPreference, AccurateModelVariant, RecognitionMode, Settings, SettingsStore,
};

const MAX_EVIDENCE_BYTES: u64 = 512 * 1024;
const MAX_EVIDENCE_RECORDS: usize = 64;
const MIN_BENCHMARK_DURATION: Duration = Duration::from_secs(5);
const MAX_BENCHMARK_DURATION: Duration = Duration::from_secs(15 * 60);
static EVIDENCE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static EVIDENCE_SAVE_LOCK: Mutex<()> = Mutex::new(());
static ROLLBACK_SAVE_LOCK: Mutex<()> = Mutex::new(());
static ROLLBACK_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static GLOBAL_BENCHMARK_PERMIT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static RECOMMENDER_AUTHORITY_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const ROLLBACK_SCHEMA_VERSION: u32 = 1;
const MAX_ROLLBACK_BYTES: u64 = 32 * 1024;

/// Content-free output of one production measurement. Raw audio, prompt text,
/// and recognized text stay inside the adapter call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasuredSample {
    pub load_latency: Duration,
    pub release_latency: Duration,
    pub inference_time: Duration,
    pub audio_duration: Duration,
    pub peak_working_set_mib: u32,
    pub available_memory_mib: u32,
    pub fallback_count: u32,
    pub thermal_condition: ThermalCondition,
    pub contention_condition: ContentionCondition,
    pub quality: QualityCounts,
}

#[derive(Clone)]
pub struct BenchmarkControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl BenchmarkControl {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    #[cfg(test)]
    pub(crate) fn for_host_test(duration: Duration) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + duration,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MeasurementFailure {
    #[error("recognition backend could not load")]
    LoadFailed,
    #[error("calibration capture could not complete")]
    CaptureFailed,
    #[error("recognition failed")]
    RecognitionFailed,
    #[error("resource measurement failed")]
    ResourceProbeFailed,
    #[error("measurement was cancelled")]
    Cancelled,
    #[error("measurement deadline elapsed")]
    DeadlineExceeded,
}

/// Production adapters must make every native/capture operation observe the
/// supplied cancellation flag and deadline. Errors are intentionally content-free.
pub trait PerformanceMeasurementAdapter: Send + Sync + 'static {
    fn measure(
        &self,
        candidate: &BenchmarkCandidate,
        case: &CalibrationCase,
        cold_load: bool,
        control: &BenchmarkControl,
    ) -> Result<MeasuredSample, MeasurementFailure>;
}

pub trait BenchmarkActivityLease: Send {}

impl<T: Send> BenchmarkActivityLease for T {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BenchmarkPreflightFailure {
    #[error("dictation is active")]
    DictationActive,
    #[error("Whisper or Ollama is using the required compute resources")]
    ComputeContention,
    #[error("the system is thermally unstable")]
    ThermalState,
    #[error("the system is under load")]
    SystemContention,
    #[error("workload state could not be measured safely")]
    Unavailable,
}

/// Must atomically reserve the recognition/compute lane while checking live
/// dictation, Whisper, Ollama, thermal, and contention state. The returned
/// lease is held until the worker reaches a terminal result.
pub trait PerformanceBenchmarkPreflight: Send + Sync + 'static {
    fn try_acquire(
        &self,
        candidate: &BenchmarkCandidate,
    ) -> Result<Box<dyn BenchmarkActivityLease>, BenchmarkPreflightFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BenchmarkStartError {
    #[error("a benchmark is already running")]
    AlreadyRunning,
    #[error("benchmark cannot start while dictation is active")]
    DictationActive,
    #[error("benchmark cannot start while a conflicting compute workload is active")]
    ComputeContention,
    #[error("benchmark cannot start while the system is thermally unstable")]
    ThermalState,
    #[error("benchmark cannot start while the system is under load")]
    SystemContention,
    #[error("benchmark preflight could not obtain trustworthy system state")]
    PreflightUnavailable,
    #[error("candidate does not support the requested language")]
    UnsupportedLanguage,
    #[error("benchmark duration must be between 5 seconds and 15 minutes")]
    InvalidDuration,
    #[error("benchmark worker could not start")]
    WorkerUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BenchmarkFailure {
    #[error("benchmark was cancelled")]
    Cancelled,
    #[error("benchmark deadline elapsed")]
    DeadlineExceeded,
    #[error("measurement adapter failed")]
    Measurement,
    #[error("benchmark evidence failed validation")]
    InvalidEvidence,
    #[error("benchmark worker failed")]
    WorkerFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BenchmarkRunState {
    Idle,
    Running { completed: u32, total: u32 },
    Cancelling { completed: u32, total: u32 },
    Complete(Box<BenchmarkEvidence>),
    Failed(BenchmarkFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BenchmarkTicket {
    generation: u64,
}

struct SharedRun {
    generation: AtomicU64,
    running: AtomicBool,
    cancelled: Arc<AtomicBool>,
    state: Mutex<BenchmarkRunState>,
}

/// A one-worker, snapshot-based coordinator. A hostile native call cannot block
/// the UI, shutdown, or create an unbounded series of replacement workers.
pub struct PerformanceBenchmarkService {
    shared: Arc<SharedRun>,
    adapter: Arc<dyn PerformanceMeasurementAdapter>,
    preflight: Arc<dyn PerformanceBenchmarkPreflight>,
    corpus: CalibrationCorpus,
    process_permit: Arc<AtomicBool>,
}

impl PerformanceBenchmarkService {
    #[must_use]
    pub fn new(
        adapter: Arc<dyn PerformanceMeasurementAdapter>,
        preflight: Arc<dyn PerformanceBenchmarkPreflight>,
    ) -> Self {
        let process_permit = GLOBAL_BENCHMARK_PERMIT
            .get_or_init(|| Arc::new(AtomicBool::new(false)))
            .clone();
        Self::with_permit(adapter, preflight, process_permit)
    }

    fn with_permit(
        adapter: Arc<dyn PerformanceMeasurementAdapter>,
        preflight: Arc<dyn PerformanceBenchmarkPreflight>,
        process_permit: Arc<AtomicBool>,
    ) -> Self {
        Self {
            shared: Arc::new(SharedRun {
                generation: AtomicU64::new(0),
                running: AtomicBool::new(false),
                cancelled: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(BenchmarkRunState::Idle),
            }),
            adapter,
            preflight,
            corpus: CalibrationCorpus::pinned_v1(),
            process_permit,
        }
    }

    pub fn start(
        &self,
        candidate: BenchmarkCandidate,
        context: BenchmarkContext,
        language: Language,
        maximum_duration: Duration,
    ) -> Result<BenchmarkTicket, BenchmarkStartError> {
        if !candidate.supports(language) {
            return Err(BenchmarkStartError::UnsupportedLanguage);
        }
        if !(MIN_BENCHMARK_DURATION..=MAX_BENCHMARK_DURATION).contains(&maximum_duration) {
            return Err(BenchmarkStartError::InvalidDuration);
        }
        self.process_permit
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BenchmarkStartError::AlreadyRunning)?;
        let workload_lease = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.preflight.try_acquire(&candidate)
        })) {
            Ok(Ok(lease)) => lease,
            Ok(Err(error)) => {
                self.process_permit.store(false, Ordering::Release);
                return Err(match error {
                    BenchmarkPreflightFailure::DictationActive => {
                        BenchmarkStartError::DictationActive
                    }
                    BenchmarkPreflightFailure::ComputeContention => {
                        BenchmarkStartError::ComputeContention
                    }
                    BenchmarkPreflightFailure::ThermalState => BenchmarkStartError::ThermalState,
                    BenchmarkPreflightFailure::SystemContention => {
                        BenchmarkStartError::SystemContention
                    }
                    BenchmarkPreflightFailure::Unavailable => {
                        BenchmarkStartError::PreflightUnavailable
                    }
                });
            }
            Err(_) => {
                self.process_permit.store(false, Ordering::Release);
                return Err(BenchmarkStartError::PreflightUnavailable);
            }
        };
        self.shared
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                self.process_permit.store(false, Ordering::Release);
                BenchmarkStartError::WorkerUnavailable
            })?;

        let generation = self
            .shared
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.shared.cancelled.store(false, Ordering::Release);
        let cases = self.corpus.cases_for(language).cloned().collect::<Vec<_>>();
        let total = u32::try_from(cases.len()).unwrap_or(u32::MAX);
        set_state(
            &self.shared,
            BenchmarkRunState::Running {
                completed: 0,
                total,
            },
        );

        let shared = Arc::clone(&self.shared);
        let adapter = Arc::clone(&self.adapter);
        let corpus = self.corpus.clone();
        let process_permit = Arc::clone(&self.process_permit);
        let deadline = Instant::now() + maximum_duration;
        std::thread::Builder::new()
            .name("phorminx-performance-benchmark".to_owned())
            .spawn(move || {
                let _workload_lease = workload_lease;
                let terminal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_benchmark(
                        &shared,
                        adapter.as_ref(),
                        &corpus,
                        candidate,
                        context,
                        language,
                        cases,
                        generation,
                        deadline,
                    )
                }))
                .unwrap_or(Err(BenchmarkFailure::WorkerFailed));
                finalize_run(&shared, generation, deadline, terminal);
                process_permit.store(false, Ordering::Release);
            })
            .map_err(|_| {
                self.shared.running.store(false, Ordering::Release);
                self.process_permit.store(false, Ordering::Release);
                set_state(
                    &self.shared,
                    BenchmarkRunState::Failed(BenchmarkFailure::WorkerFailed),
                );
                BenchmarkStartError::WorkerUnavailable
            })?;

        Ok(BenchmarkTicket { generation })
    }

    #[must_use]
    pub fn cancel(&self, ticket: BenchmarkTicket) -> bool {
        if self.shared.generation.load(Ordering::Acquire) != ticket.generation {
            return false;
        }
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.shared.generation.load(Ordering::Acquire) != ticket.generation
            || !self.shared.running.load(Ordering::Acquire)
        {
            return false;
        }
        self.shared.cancelled.store(true, Ordering::Release);
        if let &BenchmarkRunState::Running { completed, total } = &*state {
            *state = BenchmarkRunState::Cancelling { completed, total };
        }
        true
    }

    #[must_use]
    pub fn snapshot(&self) -> BenchmarkRunState {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for PerformanceBenchmarkService {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_benchmark(
    shared: &SharedRun,
    adapter: &dyn PerformanceMeasurementAdapter,
    corpus: &CalibrationCorpus,
    candidate: BenchmarkCandidate,
    context: BenchmarkContext,
    language: Language,
    cases: Vec<CalibrationCase>,
    generation: u64,
    deadline: Instant,
) -> Result<BenchmarkEvidence, BenchmarkFailure> {
    let control = BenchmarkControl {
        cancelled: Arc::clone(&shared.cancelled),
        deadline,
    };
    let total = u32::try_from(cases.len()).unwrap_or(u32::MAX);
    let mut observations = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        ensure_live(&control)?;
        let sample = adapter
            .measure(&candidate, case, index == 0, &control)
            .map_err(map_measurement_failure)?;
        ensure_live(&control)?;
        observations.push(BenchmarkObservation {
            case_id: case.case_id().clone(),
            cold_load: index == 0,
            load_latency: sample.load_latency,
            release_latency: sample.release_latency,
            inference_time: sample.inference_time,
            audio_duration: sample.audio_duration,
            peak_working_set_mib: sample.peak_working_set_mib,
            available_memory_mib: sample.available_memory_mib,
            fallback_count: sample.fallback_count,
            thermal_condition: sample.thermal_condition,
            contention_condition: sample.contention_condition,
            quality: sample.quality,
        });
        set_progress(
            shared,
            generation,
            u32::try_from(index + 1).unwrap_or(u32::MAX),
            total,
        );
    }
    aggregate_evidence(&candidate, &context, corpus, language, &observations)
        .map_err(|_| BenchmarkFailure::InvalidEvidence)
}

fn ensure_live(control: &BenchmarkControl) -> Result<(), BenchmarkFailure> {
    if control.cancelled.load(Ordering::Acquire) {
        Err(BenchmarkFailure::Cancelled)
    } else if Instant::now() >= control.deadline {
        Err(BenchmarkFailure::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn finalize_run(
    shared: &SharedRun,
    generation: u64,
    deadline: Instant,
    terminal: Result<BenchmarkEvidence, BenchmarkFailure>,
) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shared.running.store(false, Ordering::Release);
    if shared.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let terminal = if shared.cancelled.load(Ordering::Acquire) {
        Err(BenchmarkFailure::Cancelled)
    } else if Instant::now() >= deadline {
        Err(BenchmarkFailure::DeadlineExceeded)
    } else {
        terminal
    };
    *state = match terminal {
        Ok(evidence) => BenchmarkRunState::Complete(Box::new(evidence)),
        Err(error) => BenchmarkRunState::Failed(error),
    };
}

const fn map_measurement_failure(error: MeasurementFailure) -> BenchmarkFailure {
    match error {
        MeasurementFailure::Cancelled => BenchmarkFailure::Cancelled,
        MeasurementFailure::DeadlineExceeded => BenchmarkFailure::DeadlineExceeded,
        MeasurementFailure::LoadFailed
        | MeasurementFailure::CaptureFailed
        | MeasurementFailure::RecognitionFailed
        | MeasurementFailure::ResourceProbeFailed => BenchmarkFailure::Measurement,
    }
}

fn set_state(shared: &SharedRun, state: BenchmarkRunState) {
    *shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
}

fn set_progress(shared: &SharedRun, generation: u64, completed: u32, total: u32) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if shared.generation.load(Ordering::Acquire) == generation
        && !shared.cancelled.load(Ordering::Acquire)
        && matches!(&*state, BenchmarkRunState::Running { .. })
    {
        *state = BenchmarkRunState::Running { completed, total };
    }
}

#[derive(Clone)]
pub struct EvidenceStore {
    path: PathBuf,
}

impl std::fmt::Debug for EvidenceStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EvidenceStore([REDACTED PATH])")
    }
}

#[derive(thiserror::Error)]
pub enum EvidenceStoreError {
    #[error("the evidence path is invalid")]
    InvalidPath,
    #[error("could not access benchmark evidence")]
    Access(std::io::ErrorKind),
    #[error("benchmark evidence exceeds its size limit")]
    TooLarge,
    #[error("benchmark evidence is corrupt or has an unsupported schema")]
    InvalidFormat,
    #[error("benchmark evidence contains invalid records")]
    InvalidEvidence,
    #[error("could not encode benchmark evidence")]
    Encode,
    #[error("could not atomically commit benchmark evidence")]
    Commit,
}

impl std::fmt::Debug for EvidenceStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPath => "EvidenceStoreError::InvalidPath",
            Self::Access(_) => "EvidenceStoreError::Access([REDACTED])",
            Self::TooLarge => "EvidenceStoreError::TooLarge",
            Self::InvalidFormat => "EvidenceStoreError::InvalidFormat",
            Self::InvalidEvidence => "EvidenceStoreError::InvalidEvidence",
            Self::Encode => "EvidenceStoreError::Encode",
            Self::Commit => "EvidenceStoreError::Commit",
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceEnvelope {
    schema_version: u32,
    evidence: Vec<BenchmarkEvidence>,
}

impl EvidenceStore {
    pub fn default_for_current_user() -> Result<Self, EvidenceStoreError> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .ok_or(EvidenceStoreError::InvalidPath)?;
        Self::new(PathBuf::from(local_app_data).join("Phorminx/performance-evidence.json"))
    }

    pub fn new(path: PathBuf) -> Result<Self, EvidenceStoreError> {
        if !path.is_absolute() || path.file_name().is_none() || path.parent().is_none() {
            return Err(EvidenceStoreError::InvalidPath);
        }
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<Vec<BenchmarkEvidence>, EvidenceStoreError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(EvidenceStoreError::Access(error.kind())),
        };
        if file
            .metadata()
            .map_err(|error| EvidenceStoreError::Access(error.kind()))?
            .len()
            > MAX_EVIDENCE_BYTES
        {
            return Err(EvidenceStoreError::TooLarge);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_EVIDENCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| EvidenceStoreError::Access(error.kind()))?;
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err(EvidenceStoreError::TooLarge);
        }
        let envelope: EvidenceEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| EvidenceStoreError::InvalidFormat)?;
        validate_envelope(&envelope)?;
        Ok(envelope.evidence)
    }

    pub fn save(&self, evidence: &[BenchmarkEvidence]) -> Result<(), EvidenceStoreError> {
        let _save_guard = EVIDENCE_SAVE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut incoming = evidence.to_vec();
        incoming.sort();
        incoming.dedup();
        let incoming_envelope = EvidenceEnvelope {
            schema_version: 1,
            evidence: incoming.clone(),
        };
        validate_envelope(&incoming_envelope)?;

        // Saving benchmark output is an upsert, not a snapshot replacement:
        // one completed candidate must not erase comparable evidence already
        // collected for the same host. The process lock covers the read and
        // atomic replace as one local transaction.
        let mut merged = self.load()?;
        for replacement in incoming {
            let identity = evidence_identity(&replacement);
            merged.retain(|existing| evidence_identity(existing) != identity);
            merged.push(replacement);
        }
        merged.sort();
        let envelope = EvidenceEnvelope {
            schema_version: 1,
            evidence: merged,
        };
        validate_envelope(&envelope)?;
        let bytes = serde_json::to_vec(&envelope).map_err(|_| EvidenceStoreError::Encode)?;
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err(EvidenceStoreError::TooLarge);
        }
        let directory = self.path.parent().ok_or(EvidenceStoreError::InvalidPath)?;
        fs::create_dir_all(directory).map_err(|error| EvidenceStoreError::Access(error.kind()))?;
        let temporary = directory.join(format!(
            ".performance-evidence.{}.{}.tmp",
            std::process::id(),
            EVIDENCE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| EvidenceStoreError::Access(error.kind()))?;
            file.write_all(&bytes)
                .and_then(|()| file.flush())
                .and_then(|()| file.sync_all())
                .map_err(|error| EvidenceStoreError::Access(error.kind()))?;
            drop(file);
            atomic_replace_file(&temporary, &self.path).map_err(|_| EvidenceStoreError::Commit)
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }

    pub fn load_matching(
        &self,
        context: &BenchmarkContext,
    ) -> Result<Vec<BenchmarkEvidence>, EvidenceStoreError> {
        Ok(self
            .load()?
            .into_iter()
            .filter(|evidence| {
                evidence.protocol_id == context.protocol_id
                    && evidence.build_id == context.build_id
                    && evidence.device_id == context.device_id
                    && evidence.driver_id == context.driver_id
            })
            .collect())
    }
}

fn validate_envelope(envelope: &EvidenceEnvelope) -> Result<(), EvidenceStoreError> {
    if envelope.schema_version != 1
        || envelope.evidence.len() > MAX_EVIDENCE_RECORDS
        || envelope
            .evidence
            .iter()
            .any(|evidence| !evidence.is_structurally_valid())
    {
        return Err(EvidenceStoreError::InvalidEvidence);
    }
    let mut identities = BTreeSet::new();
    if envelope
        .evidence
        .iter()
        .any(|evidence| !identities.insert(evidence_identity(evidence)))
    {
        return Err(EvidenceStoreError::InvalidEvidence);
    }
    Ok(())
}

fn evidence_identity(
    evidence: &BenchmarkEvidence,
) -> (
    ContentFreeId,
    ContentFreeId,
    ContentFreeId,
    ContentFreeId,
    ContentFreeId,
    Language,
) {
    (
        evidence.protocol_id.clone(),
        evidence.build_id.clone(),
        evidence.device_id.clone(),
        evidence.driver_id.clone(),
        evidence.candidate_id.clone(),
        evidence.measured_language,
    )
}

#[derive(Clone)]
pub enum RecognitionApplication {
    Accurate {
        model: AccurateModelVariant,
        backend: AccurateBackendPreference,
        model_path: PathBuf,
    },
    Instant {
        model_path: PathBuf,
        runtime_path: PathBuf,
    },
}

impl std::fmt::Debug for RecognitionApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accurate { model, backend, .. } => formatter
                .debug_struct("Accurate")
                .field("model", model)
                .field("backend", backend)
                .field("model_path", &"[REDACTED PATH]")
                .finish(),
            Self::Instant { .. } => formatter
                .debug_struct("Instant")
                .field("model_path", &"[REDACTED PATH]")
                .field("runtime_path", &"[REDACTED PATH]")
                .finish(),
        }
    }
}

/// Host-trusted exact mapping from benchmark identity to runtime settings.
#[derive(Clone, Debug)]
pub struct ApplicationProfile {
    candidate: BenchmarkCandidate,
    language: Language,
    recognition: RecognitionApplication,
}

impl ApplicationProfile {
    pub fn new(
        candidate: BenchmarkCandidate,
        language: Language,
        recognition: RecognitionApplication,
    ) -> Result<Self, ApplyError> {
        if !candidate.supports(language)
            || (language == Language::PortugueseBrazil
                && candidate.engine() == EngineKind::Instant
                && !candidate.pt_brazil_instant_certified())
            || !application_matches(&candidate, language, &recognition)
        {
            return Err(ApplyError::ProfileMismatch);
        }
        Ok(Self {
            candidate,
            language,
            recognition,
        })
    }
}

#[derive(Debug)]
pub struct ApplyConsent {
    authority_id: u64,
    generation: u64,
    candidate_id: ContentFreeId,
}

#[derive(Clone)]
pub struct AppliedRecommendation {
    authority_id: u64,
    generation: u64,
    rollback_nonce: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecognitionRollbackFields {
    language: String,
    mode: RecognitionMode,
    accurate_model: AccurateModelVariant,
    accurate_backend: AccurateBackendPreference,
    model_path: PathBuf,
    instant_model_path: PathBuf,
    instant_runtime_path: PathBuf,
}

impl RecognitionRollbackFields {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            language: settings.recognition.language.clone(),
            mode: settings.recognition.mode,
            accurate_model: settings.recognition.accurate_model,
            accurate_backend: settings.recognition.accurate_backend,
            model_path: settings.recognition.model_path.clone(),
            instant_model_path: settings.recognition.instant_model_path.clone(),
            instant_runtime_path: settings.recognition.instant_runtime_path.clone(),
        }
    }

    fn apply_to(&self, settings: &mut Settings) {
        settings.recognition.language.clone_from(&self.language);
        settings.recognition.mode = self.mode;
        settings.recognition.accurate_model = self.accurate_model;
        settings.recognition.accurate_backend = self.accurate_backend;
        settings.recognition.model_path.clone_from(&self.model_path);
        settings
            .recognition
            .instant_model_path
            .clone_from(&self.instant_model_path);
        settings
            .recognition
            .instant_runtime_path
            .clone_from(&self.instant_runtime_path);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRollbackReceipt {
    schema_version: u32,
    generation: u64,
    nonce: u64,
    applied_settings_sha256: String,
    previous: RecognitionRollbackFields,
    applied: RecognitionRollbackFields,
}

impl std::fmt::Debug for AppliedRecommendation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppliedRecommendation")
            .field("authority_id", &self.authority_id)
            .field("generation", &self.generation)
            .field("rollback_nonce", &"[REDACTED]")
            .finish()
    }
}

#[derive(thiserror::Error)]
pub enum ApplyError {
    #[error("application profile does not match its exact benchmark identity")]
    ProfileMismatch,
    #[error("recommendation is not eligible for application")]
    NotApplicable,
    #[error("the recommended local recognition assets are unavailable or no longer match")]
    AssetUnavailable,
    #[error("application consent is stale or has already been consumed")]
    StaleConsent,
    #[error("settings changed after the recommendation was applied")]
    StaleRollback,
    #[error("settings changed before the recommendation could be committed")]
    ConcurrentSettingsChange,
    #[error("a previous applied recommendation still has a rollback available")]
    RollbackPending,
    #[error("settings validation or atomic commit failed")]
    Settings,
}

impl std::fmt::Debug for ApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ProfileMismatch => "ApplyError::ProfileMismatch",
            Self::NotApplicable => "ApplyError::NotApplicable",
            Self::AssetUnavailable => "ApplyError::AssetUnavailable",
            Self::StaleConsent => "ApplyError::StaleConsent",
            Self::StaleRollback => "ApplyError::StaleRollback",
            Self::ConcurrentSettingsChange => "ApplyError::ConcurrentSettingsChange",
            Self::RollbackPending => "ApplyError::RollbackPending",
            Self::Settings => "ApplyError::Settings",
        })
    }
}

pub struct PerformanceRecommender {
    policy: RecommendationPolicy,
    profiles: BTreeMap<ContentFreeId, ApplicationProfile>,
    authority_id: u64,
    next_generation: u64,
    pending_generation: Option<u64>,
    latest_applied_generation: Option<u64>,
    asset_verifier: Arc<dyn ApplicationAssetVerifier>,
}

trait ApplicationAssetVerifier: Send + Sync {
    fn matches(&self, profile: &ApplicationProfile, store: &SettingsStore) -> bool;
}

struct LocalApplicationAssetVerifier;

impl ApplicationAssetVerifier for LocalApplicationAssetVerifier {
    fn matches(&self, profile: &ApplicationProfile, store: &SettingsStore) -> bool {
        match &profile.recognition {
            RecognitionApplication::Accurate {
                model,
                backend,
                model_path,
            } => {
                let model_matches =
                    crate::model::identify_pinned_model(&store.resolve_model_path(model_path))
                        .is_ok_and(|identified| identified == Some(*model));
                let backend_matches = match backend {
                    crate::settings::AccurateBackendPreference::Cpu => true,
                    crate::settings::AccurateBackendPreference::Vulkan => {
                        phorminx_whisper::probe_backend(
                            phorminx_whisper::WhisperBackendPreference::Vulkan,
                        )
                        .is_ok()
                    }
                    crate::settings::AccurateBackendPreference::Auto => false,
                };
                model_matches && backend_matches
            }
            RecognitionApplication::Instant {
                model_path,
                runtime_path,
            } => {
                let resolved_runtime = store.resolve_asset_path(runtime_path);
                let resolved_model = store.resolve_asset_path(model_path);
                let Ok(catalog) = crate::setup_host::PinnedCatalog::phorminx() else {
                    return false;
                };
                let expected =
                    catalog.preferred_recognition_artifacts(EngineKind::Instant, profile.language);
                if expected.len() != 2 {
                    return false;
                }
                let Ok(root) = crate::setup_host::ManagedRoot::from_local_app_data() else {
                    return false;
                };
                let Ok(installed) = root.installed() else {
                    return false;
                };
                let receipts_match = expected.iter().all(|descriptor| {
                    let expected_target = if descriptor.supported_languages().is_empty() {
                        &resolved_runtime
                    } else {
                        &resolved_model
                    };
                    installed.iter().any(|install| {
                        &install.target == expected_target
                            && install.receipt.asset().descriptor() == descriptor
                    })
                });
                let live_identity = crate::performance_runtime::vosk_live_identity(
                    &resolved_runtime,
                    &resolved_model,
                    profile.language,
                );
                receipts_match
                    && live_identity.is_ok_and(|(_, _, candidate_id, model_digest)| {
                        &candidate_id == profile.candidate.candidate_id()
                            && &model_digest == profile.candidate.model_digest()
                    })
                    && matches!(
                        phorminx_vosk::inspect(
                            &resolved_runtime,
                            &resolved_model,
                            profile.language.code(),
                        ),
                        phorminx_vosk::Readiness::Ready { .. }
                    )
            }
        }
    }
}

impl PerformanceRecommender {
    pub fn new(
        policy: RecommendationPolicy,
        profiles: impl IntoIterator<Item = ApplicationProfile>,
    ) -> Result<Self, ApplyError> {
        Self::new_with_verifier(policy, profiles, Arc::new(LocalApplicationAssetVerifier))
    }

    fn new_with_verifier(
        policy: RecommendationPolicy,
        profiles: impl IntoIterator<Item = ApplicationProfile>,
        asset_verifier: Arc<dyn ApplicationAssetVerifier>,
    ) -> Result<Self, ApplyError> {
        let mut exact = BTreeMap::new();
        for profile in profiles {
            match exact.entry(profile.candidate.candidate_id().clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(profile);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(ApplyError::ProfileMismatch);
                }
            }
        }
        Ok(Self {
            policy,
            profiles: exact,
            authority_id: RECOMMENDER_AUTHORITY_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            next_generation: 0,
            pending_generation: None,
            latest_applied_generation: None,
            asset_verifier,
        })
    }

    #[must_use]
    pub fn evaluate(
        &mut self,
        language: Language,
        preference: RecommendationPreference,
        evidence: impl IntoIterator<Item = BenchmarkEvidence>,
    ) -> (Recommendation, Option<ApplyConsent>) {
        let evidence = evidence
            .into_iter()
            .filter(|evidence| evidence.measured_language == language)
            .collect::<Vec<_>>();
        let recommendation = RecommendationEngine::recommend(
            language,
            preference,
            &self.policy,
            evidence
                .iter()
                .cloned()
                .map(|evidence| phorminx_setup::CandidateEvidence { evidence }),
        );
        self.pending_generation = None;
        let consent = match &recommendation.outcome {
            RecommendationOutcome::Recommended { candidate_id, .. }
                if self.profiles.get(candidate_id).is_some_and(|profile| {
                    profile.language == language
                        && evidence.iter().any(|evidence| {
                            profile.candidate.matches_evidence(evidence)
                                && evidence.supported_languages.contains(&language)
                        })
                }) =>
            {
                self.next_generation = self.next_generation.wrapping_add(1);
                self.pending_generation = Some(self.next_generation);
                Some(ApplyConsent {
                    authority_id: self.authority_id,
                    generation: self.next_generation,
                    candidate_id: candidate_id.clone(),
                })
            }
            _ => None,
        };
        (recommendation, consent)
    }

    pub fn cancel_apply(&mut self, consent: ApplyConsent) -> Result<(), ApplyError> {
        if consent.authority_id != self.authority_id
            || self.pending_generation != Some(consent.generation)
        {
            return Err(ApplyError::StaleConsent);
        }
        self.pending_generation = None;
        Ok(())
    }

    pub fn apply(
        &mut self,
        consent: ApplyConsent,
        store: &SettingsStore,
    ) -> Result<AppliedRecommendation, ApplyError> {
        if consent.authority_id != self.authority_id
            || self.pending_generation != Some(consent.generation)
        {
            return Err(ApplyError::StaleConsent);
        }
        self.pending_generation = None;
        let profile = self
            .profiles
            .get(&consent.candidate_id)
            .ok_or(ApplyError::NotApplicable)?;
        if !self.asset_verifier.matches(profile, store) {
            return Err(ApplyError::AssetUnavailable);
        }
        let previous = store.load().map_err(|_| ApplyError::Settings)?;
        let mut applied = previous.clone();
        apply_profile(&mut applied, profile);
        applied
            .validate_and_normalize()
            .map_err(|_| ApplyError::Settings)?;
        let rollback_nonce = ROLLBACK_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let persisted = PersistedRollbackReceipt {
            schema_version: ROLLBACK_SCHEMA_VERSION,
            generation: consent.generation,
            nonce: rollback_nonce,
            applied_settings_sha256: settings_digest(&applied)?,
            previous: RecognitionRollbackFields::from_settings(&previous),
            applied: RecognitionRollbackFields::from_settings(&applied),
        };
        let _rollback_guard = ROLLBACK_SAVE_LOCK
            .lock()
            .map_err(|_| ApplyError::Settings)?;
        if let Ok(existing) = load_rollback_receipt(store) {
            let current = store.load().map_err(|_| ApplyError::Settings)?;
            if settings_digest(&current)? == existing.applied_settings_sha256 {
                return Err(ApplyError::RollbackPending);
            }
            remove_rollback_receipt(store)?;
        } else if rollback_receipt_path(store)?.exists() {
            // Corrupt or future receipts require explicit discard. Silently
            // overwriting them would destroy the user's only possible undo.
            return Err(ApplyError::Settings);
        }
        write_rollback_receipt(store, &persisted)?;
        let committed = store.compare_and_save(&previous, &applied);
        match committed {
            Ok(true) => {}
            Ok(false) => {
                let _ = remove_rollback_receipt(store);
                return Err(ApplyError::ConcurrentSettingsChange);
            }
            Err(_) => {
                let _ = remove_rollback_receipt(store);
                return Err(ApplyError::Settings);
            }
        }
        self.latest_applied_generation = Some(consent.generation);
        Ok(AppliedRecommendation {
            authority_id: self.authority_id,
            generation: consent.generation,
            rollback_nonce,
        })
    }

    pub fn rollback(
        &mut self,
        receipt: AppliedRecommendation,
        store: &SettingsStore,
    ) -> Result<(), ApplyError> {
        if receipt.authority_id != self.authority_id
            || self.latest_applied_generation != Some(receipt.generation)
        {
            return Err(ApplyError::StaleRollback);
        }
        let _rollback_guard = ROLLBACK_SAVE_LOCK
            .lock()
            .map_err(|_| ApplyError::Settings)?;
        rollback_persisted_locked(store, Some(receipt.rollback_nonce))?;
        self.latest_applied_generation = None;
        Ok(())
    }

    /// Reverts the latest performance recommendation after a process restart.
    /// Only the recognition fields changed by the recommender are persisted,
    /// and rollback is rejected if any setting changed after application.
    pub fn rollback_after_restart(store: &SettingsStore) -> Result<(), ApplyError> {
        let _rollback_guard = ROLLBACK_SAVE_LOCK
            .lock()
            .map_err(|_| ApplyError::Settings)?;
        rollback_persisted_locked(store, None)
    }

    /// Permanently discards the pending rollback without changing settings.
    pub fn discard_persisted_rollback(store: &SettingsStore) -> Result<(), ApplyError> {
        let _rollback_guard = ROLLBACK_SAVE_LOCK
            .lock()
            .map_err(|_| ApplyError::Settings)?;
        remove_rollback_receipt(store)
    }
}

fn rollback_receipt_path(store: &SettingsStore) -> Result<PathBuf, ApplyError> {
    store
        .path()
        .parent()
        .map(|directory| directory.join("performance-recommendation-rollback.toml"))
        .ok_or(ApplyError::Settings)
}

fn write_rollback_receipt(
    store: &SettingsStore,
    receipt: &PersistedRollbackReceipt,
) -> Result<(), ApplyError> {
    let path = rollback_receipt_path(store)?;
    let directory = path.parent().ok_or(ApplyError::Settings)?;
    fs::create_dir_all(directory).map_err(|_| ApplyError::Settings)?;
    let serialized = toml::to_string(receipt).map_err(|_| ApplyError::Settings)?;
    if serialized.len() as u64 > MAX_ROLLBACK_BYTES {
        return Err(ApplyError::Settings);
    }
    let sequence = ROLLBACK_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(
        ".performance-recommendation-rollback.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| ApplyError::Settings)?;
        file.write_all(serialized.as_bytes())
            .and_then(|_| file.flush())
            .and_then(|_| file.sync_all())
            .map_err(|_| ApplyError::Settings)?;
        drop(file);
        atomic_replace_file(&temporary, &path).map_err(|_| ApplyError::Settings)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn load_rollback_receipt(store: &SettingsStore) -> Result<PersistedRollbackReceipt, ApplyError> {
    let path = rollback_receipt_path(store)?;
    let mut file = File::open(path).map_err(|_| ApplyError::StaleRollback)?;
    if file.metadata().map_err(|_| ApplyError::Settings)?.len() > MAX_ROLLBACK_BYTES {
        return Err(ApplyError::Settings);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ROLLBACK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ApplyError::Settings)?;
    if bytes.len() as u64 > MAX_ROLLBACK_BYTES {
        return Err(ApplyError::Settings);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| ApplyError::Settings)?;
    let receipt: PersistedRollbackReceipt =
        toml::from_str(text).map_err(|_| ApplyError::Settings)?;
    if receipt.schema_version != ROLLBACK_SCHEMA_VERSION || receipt.nonce == 0 {
        return Err(ApplyError::Settings);
    }
    if receipt.applied_settings_sha256.len() != 64
        || !receipt
            .applied_settings_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApplyError::Settings);
    }
    Ok(receipt)
}

fn remove_rollback_receipt(store: &SettingsStore) -> Result<(), ApplyError> {
    let path = rollback_receipt_path(store)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ApplyError::Settings),
    }
}

fn rollback_persisted_locked(
    store: &SettingsStore,
    expected_nonce: Option<u64>,
) -> Result<(), ApplyError> {
    let receipt = load_rollback_receipt(store)?;
    if expected_nonce.is_some_and(|nonce| nonce != receipt.nonce) {
        return Err(ApplyError::StaleRollback);
    }
    let current = store.load().map_err(|_| ApplyError::Settings)?;
    let current_recognition = RecognitionRollbackFields::from_settings(&current);
    if current_recognition != receipt.applied
        || settings_digest(&current)? != receipt.applied_settings_sha256
    {
        // A prepared receipt whose settings commit never happened, a replay,
        // or a newer recognition edit must never overwrite the current state.
        remove_rollback_receipt(store)?;
        return Err(ApplyError::StaleRollback);
    }
    let mut restored = current.clone();
    receipt.previous.apply_to(&mut restored);
    restored
        .validate_and_normalize()
        .map_err(|_| ApplyError::Settings)?;
    if !store
        .compare_and_save(&current, &restored)
        .map_err(|_| ApplyError::Settings)?
    {
        return Err(ApplyError::StaleRollback);
    }
    remove_rollback_receipt(store)?;
    Ok(())
}

fn settings_digest(settings: &Settings) -> Result<String, ApplyError> {
    let canonical = toml::to_string(settings).map_err(|_| ApplyError::Settings)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(canonical.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

fn application_matches(
    candidate: &BenchmarkCandidate,
    language: Language,
    application: &RecognitionApplication,
) -> bool {
    match (candidate.engine(), candidate.backend(), application) {
        (
            EngineKind::Accurate,
            BackendKind::Cpu | BackendKind::Vulkan,
            RecognitionApplication::Accurate { model, backend, .. },
        ) => {
            let backend_matches = matches!(
                (candidate.backend(), backend),
                (BackendKind::Cpu, AccurateBackendPreference::Cpu)
                    | (BackendKind::Vulkan, AccurateBackendPreference::Vulkan)
            );
            let class_matches = matches!(
                (candidate.model_class(), model),
                (
                    phorminx_setup::ModelClass::Tiny,
                    AccurateModelVariant::TinyEnglish | AccurateModelVariant::TinyMultilingual
                ) | (
                    phorminx_setup::ModelClass::Base,
                    AccurateModelVariant::BaseEnglish | AccurateModelVariant::BaseMultilingual
                )
            );
            let digest_matches = crate::model::model_for_variant(*model).is_ok_and(|spec| {
                spec.sha256
                    .eq_ignore_ascii_case(candidate.model_digest().as_str())
            });
            backend_matches
                && class_matches
                && digest_matches
                && model.supports_language(language.code())
        }
        (EngineKind::Instant, BackendKind::VoskNative, RecognitionApplication::Instant { .. }) => {
            candidate.model_class() == phorminx_setup::ModelClass::Other
        }
        _ => false,
    }
}

fn apply_profile(settings: &mut Settings, profile: &ApplicationProfile) {
    settings.recognition.language = profile.language.code().to_owned();
    match &profile.recognition {
        RecognitionApplication::Accurate {
            model,
            backend,
            model_path,
        } => {
            settings.recognition.mode = RecognitionMode::Accurate;
            settings.recognition.accurate_model = *model;
            settings.recognition.accurate_backend = *backend;
            settings.recognition.model_path.clone_from(model_path);
        }
        RecognitionApplication::Instant {
            model_path,
            runtime_path,
        } => {
            settings.recognition.mode = RecognitionMode::Instant;
            settings
                .recognition
                .instant_model_path
                .clone_from(model_path);
            settings
                .recognition
                .instant_runtime_path
                .clone_from(runtime_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Barrier, Condvar};

    use phorminx_setup::{
        BenchmarkSampleSummary, ModelClass, Sha256Digest, TrustedCandidateIdentity,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::settings::SettingsError;

    fn id(value: &str) -> ContentFreeId {
        ContentFreeId::new(value).unwrap()
    }

    fn candidate() -> BenchmarkCandidate {
        BenchmarkCandidate::new(
            id("accurate-base-vulkan"),
            EngineKind::Accurate,
            BackendKind::Vulkan,
            ModelClass::Base,
            Sha256Digest::new(
                crate::model::model_for_variant(AccurateModelVariant::BaseEnglish)
                    .unwrap()
                    .sha256,
            )
            .unwrap(),
            [Language::English],
            false,
        )
    }

    fn context() -> BenchmarkContext {
        BenchmarkContext {
            protocol_id: id(CalibrationCorpus::PROTOCOL_ID),
            build_id: id("build-1"),
            device_id: id("device-1"),
            driver_id: id("driver-1"),
        }
    }

    fn evidence() -> BenchmarkEvidence {
        let candidate = candidate();
        BenchmarkEvidence {
            schema_version: BenchmarkEvidence::SCHEMA_VERSION,
            protocol_id: context().protocol_id,
            build_id: context().build_id,
            candidate_id: candidate.candidate_id().clone(),
            device_id: context().device_id,
            driver_id: context().driver_id,
            engine: candidate.engine(),
            backend: candidate.backend(),
            model_class: candidate.model_class(),
            model_digest: candidate.model_digest().clone(),
            measured_language: Language::English,
            supported_languages: [Language::English].into_iter().collect(),
            pt_brazil_instant_certified: false,
            loaded: true,
            measurements: BenchmarkSampleSummary {
                speech_samples: 3,
                silence_samples: 1,
                run_count: 4,
                cold_load_ms: 400,
                warm_load_ms: 40,
                release_p50_ms: 100,
                release_p95_ms: 200,
                release_dispersion_ms: 20,
                confidence_per_mille: 900,
                realtime_factor_milli: 100,
                word_error_per_mille: 20,
                character_error_per_mille: 10,
                protected_token_exact_per_mille: 1_000,
                hallucination_per_mille: 0,
                peak_working_set_mib: 500,
                available_memory_mib: 4_000,
                fallback_count: 0,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Idle,
            },
        }
    }

    struct PerfectAdapter;

    impl PerformanceMeasurementAdapter for PerfectAdapter {
        fn measure(
            &self,
            _candidate: &BenchmarkCandidate,
            case: &CalibrationCase,
            cold_load: bool,
            _control: &BenchmarkControl,
        ) -> Result<MeasuredSample, MeasurementFailure> {
            Ok(MeasuredSample {
                load_latency: Duration::from_millis(if cold_load { 400 } else { 40 }),
                release_latency: Duration::from_millis(100),
                inference_time: Duration::from_millis(100),
                audio_duration: Duration::from_secs(2),
                peak_working_set_mib: 500,
                available_memory_mib: 4_000,
                fallback_count: 0,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Idle,
                quality: phorminx_setup::score_transcript(
                    &case.ephemeral_content(),
                    case.ephemeral_content().reference(),
                ),
            })
        }
    }

    struct AllowPreflight;

    impl PerformanceBenchmarkPreflight for AllowPreflight {
        fn try_acquire(
            &self,
            _candidate: &BenchmarkCandidate,
        ) -> Result<Box<dyn BenchmarkActivityLease>, BenchmarkPreflightFailure> {
            Ok(Box::new(()))
        }
    }

    struct RejectPreflight(BenchmarkPreflightFailure);

    impl PerformanceBenchmarkPreflight for RejectPreflight {
        fn try_acquire(
            &self,
            _candidate: &BenchmarkCandidate,
        ) -> Result<Box<dyn BenchmarkActivityLease>, BenchmarkPreflightFailure> {
            Err(self.0)
        }
    }

    struct PanicPreflight;

    impl PerformanceBenchmarkPreflight for PanicPreflight {
        fn try_acquire(
            &self,
            _candidate: &BenchmarkCandidate,
        ) -> Result<Box<dyn BenchmarkActivityLease>, BenchmarkPreflightFailure> {
            panic!("synthetic content-free preflight failure")
        }
    }

    struct PanicAdapter;

    impl PerformanceMeasurementAdapter for PanicAdapter {
        fn measure(
            &self,
            _candidate: &BenchmarkCandidate,
            _case: &CalibrationCase,
            _cold_load: bool,
            _control: &BenchmarkControl,
        ) -> Result<MeasuredSample, MeasurementFailure> {
            panic!("synthetic content-free adapter failure")
        }
    }

    struct AllowAssetVerifier;

    impl ApplicationAssetVerifier for AllowAssetVerifier {
        fn matches(&self, _profile: &ApplicationProfile, _store: &SettingsStore) -> bool {
            true
        }
    }

    fn service(adapter: Arc<dyn PerformanceMeasurementAdapter>) -> PerformanceBenchmarkService {
        PerformanceBenchmarkService::with_permit(
            adapter,
            Arc::new(AllowPreflight),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn wait_terminal(service: &PerformanceBenchmarkService) -> BenchmarkRunState {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = service.snapshot();
            if matches!(
                snapshot,
                BenchmarkRunState::Complete(_) | BenchmarkRunState::Failed(_)
            ) {
                return snapshot;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn service_produces_complete_content_free_evidence() {
        let service = service(Arc::new(PerfectAdapter));
        service
            .start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            )
            .unwrap();
        let BenchmarkRunState::Complete(evidence) = wait_terminal(&service) else {
            panic!("benchmark did not complete")
        };
        assert!(evidence.is_structurally_valid());
        assert_eq!(evidence.measurements.speech_samples, 3);
        assert_eq!(evidence.measurements.silence_samples, 1);
    }

    struct BlockingAdapter {
        entered: Arc<(Mutex<bool>, Condvar)>,
    }

    impl PerformanceMeasurementAdapter for BlockingAdapter {
        fn measure(
            &self,
            _candidate: &BenchmarkCandidate,
            _case: &CalibrationCase,
            _cold_load: bool,
            _control: &BenchmarkControl,
        ) -> Result<MeasuredSample, MeasurementFailure> {
            let (lock, ready) = &*self.entered;
            let mut entered = lock.lock().unwrap();
            *entered = true;
            ready.notify_all();
            drop(ready.wait(entered).unwrap());
            Err(MeasurementFailure::Cancelled)
        }
    }

    #[test]
    fn stalled_adapter_never_blocks_cancel_drop_or_spawns_replacements() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let process_permit = Arc::new(AtomicBool::new(false));
        let service = PerformanceBenchmarkService::with_permit(
            Arc::new(BlockingAdapter {
                entered: Arc::clone(&entered),
            }),
            Arc::new(AllowPreflight),
            Arc::clone(&process_permit),
        );
        let ticket = service
            .start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            )
            .unwrap();
        let (lock, ready) = &*entered;
        let guard = lock.lock().unwrap();
        let (guard, timeout) = ready
            .wait_timeout_while(guard, Duration::from_secs(1), |entered| !*entered)
            .unwrap();
        assert!(!timeout.timed_out());
        drop(guard);
        let started = Instant::now();
        assert!(service.cancel(ticket));
        let second_service = PerformanceBenchmarkService::with_permit(
            Arc::new(PerfectAdapter),
            Arc::new(AllowPreflight),
            process_permit,
        );
        assert!(matches!(
            second_service.start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            ),
            Err(BenchmarkStartError::AlreadyRunning)
        ));
        assert!(started.elapsed() < Duration::from_millis(100));
        ready.notify_all();
        assert_eq!(
            wait_terminal(&service),
            BenchmarkRunState::Failed(BenchmarkFailure::Cancelled)
        );
    }

    #[test]
    fn launch_gates_prevent_dictation_and_compute_disruption() {
        for (failure, expected) in [
            (
                BenchmarkPreflightFailure::DictationActive,
                BenchmarkStartError::DictationActive,
            ),
            (
                BenchmarkPreflightFailure::ComputeContention,
                BenchmarkStartError::ComputeContention,
            ),
            (
                BenchmarkPreflightFailure::ThermalState,
                BenchmarkStartError::ThermalState,
            ),
            (
                BenchmarkPreflightFailure::SystemContention,
                BenchmarkStartError::SystemContention,
            ),
        ] {
            let service = PerformanceBenchmarkService::with_permit(
                Arc::new(PerfectAdapter),
                Arc::new(RejectPreflight(failure)),
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(
                service.start(
                    candidate(),
                    context(),
                    Language::English,
                    Duration::from_secs(5),
                ),
                Err(expected)
            );
        }
    }

    #[test]
    fn panicking_preflight_fails_closed_without_leaking_the_worker_permit() {
        let permit = Arc::new(AtomicBool::new(false));
        let broken = PerformanceBenchmarkService::with_permit(
            Arc::new(PerfectAdapter),
            Arc::new(PanicPreflight),
            Arc::clone(&permit),
        );
        assert_eq!(
            broken.start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            ),
            Err(BenchmarkStartError::PreflightUnavailable)
        );
        let healthy = PerformanceBenchmarkService::with_permit(
            Arc::new(PerfectAdapter),
            Arc::new(AllowPreflight),
            permit,
        );
        assert!(
            healthy
                .start(
                    candidate(),
                    context(),
                    Language::English,
                    Duration::from_secs(5),
                )
                .is_ok()
        );
        assert!(matches!(
            wait_terminal(&healthy),
            BenchmarkRunState::Complete(_)
        ));
    }

    #[test]
    fn panicking_worker_fails_closed_and_releases_the_process_permit() {
        let permit = Arc::new(AtomicBool::new(false));
        let broken = PerformanceBenchmarkService::with_permit(
            Arc::new(PanicAdapter),
            Arc::new(AllowPreflight),
            Arc::clone(&permit),
        );
        broken
            .start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            wait_terminal(&broken),
            BenchmarkRunState::Failed(BenchmarkFailure::WorkerFailed)
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while permit.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }

        let healthy = PerformanceBenchmarkService::with_permit(
            Arc::new(PerfectAdapter),
            Arc::new(AllowPreflight),
            permit,
        );
        healthy
            .start(
                candidate(),
                context(),
                Language::English,
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(matches!(
            wait_terminal(&healthy),
            BenchmarkRunState::Complete(_)
        ));
    }

    #[test]
    fn acknowledged_cancel_always_wins_the_terminal_publication_race() {
        for _ in 0..100 {
            let service = service(Arc::new(PerfectAdapter));
            let ticket = service
                .start(
                    candidate(),
                    context(),
                    Language::English,
                    Duration::from_secs(5),
                )
                .unwrap();
            let acknowledged = service.cancel(ticket);
            let terminal = wait_terminal(&service);
            if acknowledged {
                assert_eq!(
                    terminal,
                    BenchmarkRunState::Failed(BenchmarkFailure::Cancelled)
                );
            } else {
                assert!(matches!(terminal, BenchmarkRunState::Complete(_)));
            }
        }
    }

    #[test]
    fn deadline_at_terminal_publication_overrides_a_late_success() {
        let shared = SharedRun {
            generation: AtomicU64::new(7),
            running: AtomicBool::new(true),
            cancelled: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(BenchmarkRunState::Running {
                completed: 4,
                total: 4,
            }),
        };
        finalize_run(
            &shared,
            7,
            Instant::now() - Duration::from_millis(1),
            Ok(evidence()),
        );
        assert_eq!(
            shared.state.into_inner().unwrap(),
            BenchmarkRunState::Failed(BenchmarkFailure::DeadlineExceeded)
        );
    }

    fn temporary_store() -> (TempDir, EvidenceStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = EvidenceStore::new(directory.path().join("evidence.json")).unwrap();
        (directory, store)
    }

    #[test]
    fn evidence_round_trip_is_content_free_and_stale_context_is_invalidated() {
        let (_directory, store) = temporary_store();
        store.save(&[evidence()]).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        let serialized = String::from_utf8(bytes).unwrap();
        assert!(!serialized.contains("release notes"));
        assert!(!serialized.contains("notas da versão"));
        assert!(!serialized.contains("daily dictation"));
        assert_eq!(store.load().unwrap(), vec![evidence()]);

        let mut stale = context();
        stale.driver_id = id("driver-2");
        assert!(store.load_matching(&stale).unwrap().is_empty());
    }

    #[test]
    fn corrupt_partial_oversize_and_duplicate_evidence_fail_closed() {
        let (_directory, store) = temporary_store();
        fs::write(&store.path, br#"{"schema_version":1,"evidence":["#).unwrap();
        assert!(matches!(
            store.load(),
            Err(EvidenceStoreError::InvalidFormat)
        ));
        fs::write(&store.path, vec![b'x'; MAX_EVIDENCE_BYTES as usize + 1]).unwrap();
        assert!(matches!(store.load(), Err(EvidenceStoreError::TooLarge)));

        let envelope = EvidenceEnvelope {
            schema_version: 1,
            evidence: vec![evidence(), evidence()],
        };
        fs::write(&store.path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(matches!(
            store.load(),
            Err(EvidenceStoreError::InvalidEvidence)
        ));

        let mut injected = serde_json::to_value(EvidenceEnvelope {
            schema_version: 1,
            evidence: vec![evidence()],
        })
        .unwrap();
        injected["evidence"][0]["transcript"] =
            serde_json::Value::String("private calibration output".to_owned());
        fs::write(&store.path, serde_json::to_vec(&injected).unwrap()).unwrap();
        assert!(matches!(
            store.load(),
            Err(EvidenceStoreError::InvalidFormat)
        ));
    }

    #[test]
    fn concurrent_evidence_writers_leave_one_complete_valid_envelope() {
        let (_directory, store) = temporary_store();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut record = evidence();
                    record.candidate_id = id(&format!("candidate-{index}"));
                    barrier.wait();
                    store.save(&[record.clone()]).unwrap();
                    record
                })
            })
            .collect::<Vec<_>>();
        let expected = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), expected.len());
        assert!(expected.iter().all(|record| loaded.contains(record)));
    }

    #[test]
    fn evidence_store_errors_never_disclose_the_storage_path_in_debug() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("private-evidence-destination");
        fs::create_dir(&destination).unwrap();
        let store = EvidenceStore::new(destination).unwrap();
        let error = store.save(&[evidence()]).unwrap_err();
        let rendered = format!("{error:?}");
        assert!(!rendered.contains(&directory.path().display().to_string()));
        assert!(matches!(
            error,
            EvidenceStoreError::Access(_) | EvidenceStoreError::Commit
        ));
    }

    #[test]
    fn evidence_store_keeps_distinct_measured_languages_for_one_artifact() {
        let (_directory, store) = temporary_store();
        let english = evidence();
        let mut portuguese = english.clone();
        portuguese.measured_language = Language::PortugueseBrazil;
        portuguese
            .supported_languages
            .insert(Language::PortugueseBrazil);
        store.save(&[english.clone(), portuguese.clone()]).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&english));
        assert!(loaded.contains(&portuguese));
    }

    #[test]
    fn forged_persisted_identity_cannot_cross_the_trusted_policy() {
        let trusted = candidate();
        let policy = policy(&trusted);
        let mut forged = evidence();
        forged.model_digest = Sha256Digest::new("b".repeat(64)).unwrap();
        let recommendation = RecommendationEngine::recommend(
            Language::English,
            RecommendationPreference::Fastest,
            &policy,
            [phorminx_setup::CandidateEvidence { evidence: forged }],
        );
        assert_eq!(recommendation.outcome, RecommendationOutcome::Unavailable);
    }

    fn policy(candidate: &BenchmarkCandidate) -> RecommendationPolicy {
        RecommendationPolicy::interactive(
            id(CalibrationCorpus::PROTOCOL_ID),
            id("build-1"),
            id("device-1"),
            id("driver-1"),
            [TrustedCandidateIdentity::new(
                candidate.candidate_id().clone(),
                candidate.engine(),
                candidate.backend(),
                candidate.model_class(),
                candidate.model_digest().clone(),
                [Language::English],
                false,
            )],
        )
        .unwrap()
    }

    fn recommender() -> PerformanceRecommender {
        let candidate = candidate();
        PerformanceRecommender::new_with_verifier(
            policy(&candidate),
            [ApplicationProfile::new(
                candidate,
                Language::English,
                RecognitionApplication::Accurate {
                    model: AccurateModelVariant::BaseEnglish,
                    backend: AccurateBackendPreference::Vulkan,
                    model_path: PathBuf::from("models/ggml-base.en.bin"),
                },
            )
            .unwrap()],
            Arc::new(AllowAssetVerifier),
        )
        .unwrap()
    }

    #[test]
    fn apply_revalidates_the_exact_local_asset_before_committing() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();
        let candidate = candidate();
        let mut recommender = PerformanceRecommender::new(
            policy(&candidate),
            [ApplicationProfile::new(
                candidate,
                Language::English,
                RecognitionApplication::Accurate {
                    model: AccurateModelVariant::BaseEnglish,
                    backend: AccurateBackendPreference::Vulkan,
                    model_path: PathBuf::from("models/missing-or-drifted.bin"),
                },
            )
            .unwrap()],
        )
        .unwrap();
        let (_, consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        assert!(matches!(
            recommender.apply(consent.unwrap(), &store),
            Err(ApplyError::AssetUnavailable)
        ));
        assert_eq!(store.load().unwrap(), original);
    }

    #[test]
    fn apply_requires_fresh_explicit_consent_and_rolls_back_exactly() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();
        let mut recommender = recommender();
        let (_, consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let receipt = recommender.apply(consent.unwrap(), &store).unwrap();
        let applied = store.load().unwrap();
        assert_eq!(
            applied.recognition.accurate_model,
            AccurateModelVariant::BaseEnglish
        );
        assert_eq!(
            applied.recognition.accurate_backend,
            AccurateBackendPreference::Vulkan
        );
        recommender.rollback(receipt, &store).unwrap();
        assert_eq!(store.load().unwrap(), original);
    }

    #[test]
    fn restart_rollback_is_narrow_content_free_and_one_use() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let mut original = Settings::default();
        original.recognition.microphone = Some("private microphone label".to_owned());
        original.formatting.custom_instructions = Some("private writing preference".to_owned());
        store.save(&original).unwrap();
        let mut recommender = recommender();
        let (_, consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let _receipt = recommender.apply(consent.unwrap(), &store).unwrap();

        let receipt_text = fs::read_to_string(rollback_receipt_path(&store).unwrap()).unwrap();
        assert!(!receipt_text.contains("private microphone label"));
        assert!(!receipt_text.contains("private writing preference"));
        PerformanceRecommender::rollback_after_restart(&store).unwrap();
        assert_eq!(store.load().unwrap(), original);
        assert!(matches!(
            PerformanceRecommender::rollback_after_restart(&store),
            Err(ApplyError::StaleRollback)
        ));
    }

    #[test]
    fn failed_second_apply_preserves_the_first_valid_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();
        let mut first = recommender();
        let (_, consent) = first.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let first_receipt = first.apply(consent.unwrap(), &store).unwrap();
        let receipt_before = fs::read(rollback_receipt_path(&store).unwrap()).unwrap();

        let mut second = recommender();
        let (_, consent) = second.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        assert!(matches!(
            second.apply(consent.unwrap(), &store),
            Err(ApplyError::RollbackPending)
        ));
        assert_eq!(
            fs::read(rollback_receipt_path(&store).unwrap()).unwrap(),
            receipt_before
        );
        first.rollback(first_receipt, &store).unwrap();
        assert_eq!(store.load().unwrap(), original);
    }

    #[test]
    fn cancelled_apply_consent_cannot_commit_settings() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();
        let mut recommender = recommender();
        let (_, consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        recommender.cancel_apply(consent.unwrap()).unwrap();
        assert_eq!(store.load().unwrap(), original);
    }

    #[test]
    fn newer_evaluation_invalidates_an_older_apply_consent() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        store.save(&Settings::default()).unwrap();
        let mut recommender = recommender();
        let (_, first) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let _ = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        assert!(matches!(
            recommender.apply(first.unwrap(), &store),
            Err(ApplyError::StaleConsent)
        ));
        assert_eq!(store.load().unwrap(), Settings::default());
    }

    #[test]
    fn consent_and_rollback_authority_cannot_cross_recommender_instances() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        store.save(&Settings::default()).unwrap();
        let mut first = recommender();
        let mut second = recommender();
        let (_, first_consent) = first.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let _ = second.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        assert!(matches!(
            second.apply(first_consent.unwrap(), &store),
            Err(ApplyError::StaleConsent)
        ));
        assert_eq!(store.load().unwrap(), Settings::default());
    }

    #[test]
    fn rollback_refuses_to_overwrite_later_user_changes() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        store.save(&Settings::default()).unwrap();
        let mut recommender = recommender();
        let (_, consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let receipt = recommender.apply(consent.unwrap(), &store).unwrap();
        let mut changed = store.load().unwrap();
        changed.recognition.minimum_rms = 0.2;
        store.save(&changed).unwrap();
        assert!(matches!(
            recommender.rollback(receipt, &store),
            Err(ApplyError::StaleRollback)
        ));
        assert_eq!(store.load().unwrap(), changed);
    }

    #[test]
    fn old_or_replayed_rollback_receipts_cannot_match_a_later_identical_apply() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let original = Settings::default();
        store.save(&original).unwrap();
        let mut recommender = recommender();

        let (_, first_consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let first_receipt = recommender.apply(first_consent.unwrap(), &store).unwrap();
        store.save(&original).unwrap();

        let (_, second_consent) = recommender.evaluate(
            Language::English,
            RecommendationPreference::Balanced,
            [evidence()],
        );
        let second_receipt = recommender.apply(second_consent.unwrap(), &store).unwrap();
        let expected_applied = store.load().unwrap();
        assert!(matches!(
            recommender.rollback(first_receipt, &store),
            Err(ApplyError::StaleRollback)
        ));
        assert_eq!(store.load().unwrap(), expected_applied);

        let replay = second_receipt.clone();
        recommender.rollback(second_receipt, &store).unwrap();
        store.save(&expected_applied).unwrap();
        assert!(matches!(
            recommender.rollback(replay, &store),
            Err(ApplyError::StaleRollback)
        ));
        assert_eq!(store.load().unwrap(), expected_applied);
    }

    #[test]
    fn apply_errors_redact_settings_paths_in_debug() {
        let private = PathBuf::from(r"C:\Users\Private\settings.toml");
        let underlying = SettingsError::InvalidSettingsPath(private.clone());
        let error = ApplyError::Settings;
        let rendered = format!("{error:?}");
        assert!(!rendered.contains(&private.display().to_string()));
        assert!(!rendered.contains(&underlying.to_string()));
        assert_eq!(rendered, "ApplyError::Settings");
    }

    #[test]
    fn uncertified_portuguese_instant_profile_is_impossible() {
        let instant = BenchmarkCandidate::new(
            id("instant-pt"),
            EngineKind::Instant,
            BackendKind::VoskNative,
            ModelClass::Other,
            Sha256Digest::new("c".repeat(64)).unwrap(),
            [Language::PortugueseBrazil],
            false,
        );
        assert!(matches!(
            ApplicationProfile::new(
                instant,
                Language::PortugueseBrazil,
                RecognitionApplication::Instant {
                    model_path: PathBuf::from("models/pt"),
                    runtime_path: PathBuf::from("runtime/vosk"),
                },
            ),
            Err(ApplyError::ProfileMismatch)
        ));
    }

    #[test]
    fn certification_flag_cannot_invent_a_missing_pinned_portuguese_asset() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let instant = BenchmarkCandidate::new(
            id("instant-pt-forged-certification"),
            EngineKind::Instant,
            BackendKind::VoskNative,
            ModelClass::Other,
            Sha256Digest::new("c".repeat(64)).unwrap(),
            [Language::PortugueseBrazil],
            true,
        );
        let profile = ApplicationProfile::new(
            instant,
            Language::PortugueseBrazil,
            RecognitionApplication::Instant {
                model_path: PathBuf::from("models/pt"),
                runtime_path: PathBuf::from("runtime/vosk"),
            },
        )
        .unwrap();
        assert!(!LocalApplicationAssetVerifier.matches(&profile, &store));
    }

    #[test]
    fn application_profile_cannot_recombine_model_class_or_language_variant() {
        assert!(matches!(
            ApplicationProfile::new(
                candidate(),
                Language::English,
                RecognitionApplication::Accurate {
                    model: AccurateModelVariant::TinyEnglish,
                    backend: AccurateBackendPreference::Vulkan,
                    model_path: PathBuf::from("models/tiny.bin"),
                },
            ),
            Err(ApplyError::ProfileMismatch)
        ));

        let portuguese = BenchmarkCandidate::new(
            id("accurate-base-pt"),
            EngineKind::Accurate,
            BackendKind::Cpu,
            ModelClass::Base,
            Sha256Digest::new(
                crate::model::model_for_variant(AccurateModelVariant::BaseMultilingual)
                    .unwrap()
                    .sha256,
            )
            .unwrap(),
            [Language::PortugueseBrazil],
            false,
        );
        assert!(matches!(
            ApplicationProfile::new(
                portuguese,
                Language::PortugueseBrazil,
                RecognitionApplication::Accurate {
                    model: AccurateModelVariant::BaseEnglish,
                    backend: AccurateBackendPreference::Cpu,
                    model_path: PathBuf::from("models/base.en.bin"),
                },
            ),
            Err(ApplyError::ProfileMismatch)
        ));
    }
}
