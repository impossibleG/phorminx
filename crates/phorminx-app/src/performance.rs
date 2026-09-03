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

use crate::settings::{
    AccurateBackendPreference, AccurateModelVariant, RecognitionMode, Settings, SettingsError,
    SettingsStore,
};

const MAX_EVIDENCE_BYTES: u64 = 512 * 1024;
const MAX_EVIDENCE_RECORDS: usize = 64;
const MIN_BENCHMARK_DURATION: Duration = Duration::from_secs(5);
const MAX_BENCHMARK_DURATION: Duration = Duration::from_secs(15 * 60);
static EVIDENCE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static GLOBAL_BENCHMARK_PERMIT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static RECOMMENDER_AUTHORITY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

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
                if shared.generation.load(Ordering::Acquire) == generation {
                    set_state(
                        &shared,
                        match terminal {
                            Ok(evidence) => BenchmarkRunState::Complete(Box::new(evidence)),
                            Err(error) => BenchmarkRunState::Failed(error),
                        },
                    );
                }
                shared.running.store(false, Ordering::Release);
                process_permit.store(false, Ordering::Release);
            })
            .map_err(|_| {
                self.shared.running.store(false, Ordering::Release);
                self.process_permit.store(false, Ordering::Release);
                set_state(
                    &self.shared,
                    BenchmarkRunState::Failed(BenchmarkFailure::WorkerFailed),
                );
                BenchmarkStartError::AlreadyRunning
            })?;

        Ok(BenchmarkTicket { generation })
    }

    #[must_use]
    pub fn cancel(&self, ticket: BenchmarkTicket) -> bool {
        if self.shared.generation.load(Ordering::Acquire) != ticket.generation
            || !self.shared.running.load(Ordering::Acquire)
        {
            return false;
        }
        self.shared.cancelled.store(true, Ordering::Release);
        let current = self.snapshot();
        if let BenchmarkRunState::Running { completed, total } = current {
            set_state(
                &self.shared,
                BenchmarkRunState::Cancelling { completed, total },
            );
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
        if shared.generation.load(Ordering::Acquire) == generation {
            set_state(
                shared,
                BenchmarkRunState::Running {
                    completed: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    total,
                },
            );
        }
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

#[derive(Clone)]
pub struct EvidenceStore {
    path: PathBuf,
}

impl std::fmt::Debug for EvidenceStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EvidenceStore([REDACTED PATH])")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceStoreError {
    #[error("the evidence path is invalid")]
    InvalidPath,
    #[error("could not access benchmark evidence")]
    Access(#[source] std::io::Error),
    #[error("benchmark evidence exceeds its size limit")]
    TooLarge,
    #[error("benchmark evidence is corrupt or has an unsupported schema")]
    InvalidFormat,
    #[error("benchmark evidence contains invalid records")]
    InvalidEvidence,
    #[error("could not encode benchmark evidence")]
    Encode(#[source] serde_json::Error),
    #[error("could not atomically commit benchmark evidence")]
    Commit(#[source] phorminx_windows::AtomicReplaceError),
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
            Err(error) => return Err(EvidenceStoreError::Access(error)),
        };
        if file.metadata().map_err(EvidenceStoreError::Access)?.len() > MAX_EVIDENCE_BYTES {
            return Err(EvidenceStoreError::TooLarge);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_EVIDENCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(EvidenceStoreError::Access)?;
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err(EvidenceStoreError::TooLarge);
        }
        let envelope: EvidenceEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| EvidenceStoreError::InvalidFormat)?;
        validate_envelope(&envelope)?;
        Ok(envelope.evidence)
    }

    pub fn save(&self, evidence: &[BenchmarkEvidence]) -> Result<(), EvidenceStoreError> {
        let mut evidence = evidence.to_vec();
        evidence.sort();
        evidence.dedup();
        let envelope = EvidenceEnvelope {
            schema_version: 1,
            evidence,
        };
        validate_envelope(&envelope)?;
        let bytes = serde_json::to_vec(&envelope).map_err(EvidenceStoreError::Encode)?;
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err(EvidenceStoreError::TooLarge);
        }
        let directory = self.path.parent().ok_or(EvidenceStoreError::InvalidPath)?;
        fs::create_dir_all(directory).map_err(EvidenceStoreError::Access)?;
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
                .map_err(EvidenceStoreError::Access)?;
            file.write_all(&bytes)
                .and_then(|()| file.flush())
                .and_then(|()| file.sync_all())
                .map_err(EvidenceStoreError::Access)?;
            drop(file);
            atomic_replace_file(&temporary, &self.path).map_err(EvidenceStoreError::Commit)
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
    if envelope.evidence.iter().any(|evidence| {
        !identities.insert((
            evidence.protocol_id.clone(),
            evidence.build_id.clone(),
            evidence.device_id.clone(),
            evidence.driver_id.clone(),
            evidence.candidate_id.clone(),
            evidence.measured_language,
        ))
    }) {
        return Err(EvidenceStoreError::InvalidEvidence);
    }
    Ok(())
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
    previous: Settings,
    applied: Settings,
}

impl std::fmt::Debug for AppliedRecommendation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppliedRecommendation")
            .field("authority_id", &self.authority_id)
            .field("generation", &self.generation)
            .field("settings", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("application profile does not match its exact benchmark identity")]
    ProfileMismatch,
    #[error("recommendation is not eligible for application")]
    NotApplicable,
    #[error("application consent is stale or has already been consumed")]
    StaleConsent,
    #[error("settings changed after the recommendation was applied")]
    StaleRollback,
    #[error("settings changed before the recommendation could be committed")]
    ConcurrentSettingsChange,
    #[error("settings validation or atomic commit failed")]
    Settings(#[from] SettingsError),
}

pub struct PerformanceRecommender {
    policy: RecommendationPolicy,
    profiles: BTreeMap<ContentFreeId, ApplicationProfile>,
    authority_id: u64,
    next_generation: u64,
    pending_generation: Option<u64>,
}

impl PerformanceRecommender {
    pub fn new(
        policy: RecommendationPolicy,
        profiles: impl IntoIterator<Item = ApplicationProfile>,
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
        let previous = store.load()?;
        let mut applied = previous.clone();
        apply_profile(&mut applied, profile);
        applied.validate_and_normalize()?;
        if !store.compare_and_save(&previous, &applied)? {
            return Err(ApplyError::ConcurrentSettingsChange);
        }
        Ok(AppliedRecommendation {
            authority_id: self.authority_id,
            generation: consent.generation,
            previous,
            applied,
        })
    }

    pub fn rollback(
        &mut self,
        receipt: AppliedRecommendation,
        store: &SettingsStore,
    ) -> Result<(), ApplyError> {
        if receipt.authority_id != self.authority_id
            || receipt.generation > self.next_generation
            || store.load()? != receipt.applied
        {
            return Err(ApplyError::StaleRollback);
        }
        let mut previous = receipt.previous;
        previous.validate_and_normalize()?;
        if !store.compare_and_save(&receipt.applied, &previous)? {
            return Err(ApplyError::StaleRollback);
        }
        Ok(())
    }
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
    use std::sync::Condvar;

    use phorminx_setup::{
        BenchmarkSampleSummary, ModelClass, Sha256Digest, TrustedCandidateIdentity,
    };
    use tempfile::TempDir;

    use super::*;

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
        PerformanceRecommender::new(
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
        )
        .unwrap()
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
