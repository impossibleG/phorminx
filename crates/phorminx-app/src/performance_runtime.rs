//! Production host bindings for the content-free performance recommender.
//!
//! Calibration audio and recognizer text exist only in this module's bounded,
//! one-use in-memory pipeline. Neither is serializable or included in errors.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use phorminx_core::{AudioClip, Transcript, TranscriptionOptions};
use phorminx_setup::{
    AssetId, BackendKind, BenchmarkCandidate, BenchmarkContext, CalibrationCase, CalibrationCorpus,
    CalibrationKind, ContentFreeId, ContentionCondition, EngineKind, Language, ModelClass,
    Sha256Digest, ThermalCondition, score_transcript,
};
use phorminx_vosk::VoskModel;
use phorminx_whisper::{WhisperBackend, WhisperBackendPreference, WhisperRecognizer};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{identify_pinned_model, model_for_variant};
use crate::performance::{
    ApplicationProfile, BenchmarkActivityLease, BenchmarkControl, BenchmarkPreflightFailure,
    MeasuredSample, MeasurementFailure, PerformanceBenchmarkPreflight, PerformanceBenchmarkService,
    PerformanceMeasurementAdapter, RecognitionApplication,
};
use crate::settings::{AccurateBackendPreference, AccurateModelVariant, SettingsStore};
use crate::setup_host::{ManagedInstall, ManagedRoot, Packaging, PinnedCatalog};

const CALIBRATION_SAMPLE_RATE: u32 = 16_000;
const MIN_CALIBRATION_DURATION: Duration = Duration::from_millis(200);
const MAX_CALIBRATION_DURATION: Duration = Duration::from_secs(30);
const MAX_HASHED_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_HASHED_TREE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_HASHED_TREE_ENTRIES: usize = 10_000;
const MAX_VULKAN_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const WORKER_POLL: Duration = Duration::from_millis(20);
const NATIVE_WORKER_HANDOFF_TIMEOUT: Duration = Duration::from_millis(250);
const NATIVE_WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const MIN_AVAILABLE_MEMORY_MIB: u32 = 512;
static GLOBAL_NATIVE_BENCHMARK_WORKER: AtomicBool = AtomicBool::new(false);
static GLOBAL_WORKLOAD: OnceLock<Arc<WorkloadCoordinator>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CandidateUnavailableReason {
    Catalog,
    ManagedInventory,
    AssetIdentity,
    RuntimeUnavailable,
    UnsupportedLanguage,
    BackendUnavailable,
    HostIdentity,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CandidateUnavailable {
    pub engine: EngineKind,
    pub backend: BackendKind,
    pub language: Language,
    pub reason: CandidateUnavailableReason,
}

/// An installed candidate plus its exact, host-trusted runtime mapping.
#[derive(Clone)]
pub struct InstalledBenchmarkCandidate {
    candidate: BenchmarkCandidate,
    context: BenchmarkContext,
    profile: ApplicationProfile,
    runtime: RuntimeCandidate,
}

impl std::fmt::Debug for InstalledBenchmarkCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstalledBenchmarkCandidate")
            .field("candidate", &self.candidate)
            .field("context", &self.context)
            .field("profile", &self.profile)
            .field("runtime", &"[REDACTED PATHS]")
            .finish()
    }
}

impl InstalledBenchmarkCandidate {
    #[must_use]
    pub const fn candidate(&self) -> &BenchmarkCandidate {
        &self.candidate
    }

    #[must_use]
    pub const fn context(&self) -> &BenchmarkContext {
        &self.context
    }

    #[must_use]
    pub const fn profile(&self) -> &ApplicationProfile {
        &self.profile
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProductionCandidateInventory {
    pub available: Vec<InstalledBenchmarkCandidate>,
    pub unavailable: Vec<CandidateUnavailable>,
}

impl ProductionCandidateInventory {
    #[must_use]
    pub fn candidates_for(&self, language: Language) -> Vec<BenchmarkCandidate> {
        self.available
            .iter()
            .filter(|entry| entry.runtime.language() == language)
            .map(|entry| entry.candidate.clone())
            .collect()
    }

    #[must_use]
    pub fn profiles_for(&self, language: Language) -> Vec<ApplicationProfile> {
        self.available
            .iter()
            .filter(|entry| entry.runtime.language() == language)
            .map(|entry| entry.profile.clone())
            .collect()
    }

    #[must_use]
    pub fn context(&self) -> Option<&BenchmarkContext> {
        let first = self.available.first()?.context();
        self.available
            .iter()
            .all(|entry| entry.context() == first)
            .then_some(first)
    }

    #[must_use]
    pub fn benchmark_service(
        &self,
        calibration: Arc<TransientCalibrationAudio>,
    ) -> PerformanceBenchmarkService {
        PerformanceBenchmarkService::new(
            Arc::new(ProductionPerformanceAdapter::new(self, calibration)),
            Arc::new(ProductionBenchmarkPreflight::for_current_process()),
        )
    }
}

#[derive(Clone, Copy, thiserror::Error)]
pub enum PerformanceRuntimeError {
    #[error("the pinned performance catalog is unavailable")]
    Catalog,
    #[error("managed recognition inventory is unavailable")]
    ManagedInventory,
    #[error("the host performance identity is unavailable")]
    HostIdentity,
    #[error("a benchmark identifier could not be constructed")]
    InvalidIdentity,
    #[error("calibration audio is invalid or incomplete")]
    Calibration,
}

impl std::fmt::Debug for PerformanceRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Catalog => "PerformanceRuntimeError::Catalog",
            Self::ManagedInventory => "PerformanceRuntimeError::ManagedInventory",
            Self::HostIdentity => "PerformanceRuntimeError::HostIdentity",
            Self::InvalidIdentity => "PerformanceRuntimeError::InvalidIdentity",
            Self::Calibration => "PerformanceRuntimeError::Calibration",
        })
    }
}

/// Discovers only pinned Whisper files whose full hash matches, and pinned
/// managed Vosk installations whose receipt, slot, and layout all match.
pub fn discover_installed_candidates(
    settings_store: &SettingsStore,
) -> Result<ProductionCandidateInventory, PerformanceRuntimeError> {
    let catalog = PinnedCatalog::phorminx().map_err(|_| PerformanceRuntimeError::Catalog)?;
    let managed = ManagedRoot::from_local_app_data()
        .and_then(|root| root.installed())
        .map_err(|_| PerformanceRuntimeError::ManagedInventory)?;
    discover_with(settings_store, &catalog, &managed, &ProductionIdentity)
}

trait IdentityProvider {
    fn context(&self, include_vulkan: bool) -> Result<BenchmarkContext, PerformanceRuntimeError>;
}

struct ProductionIdentity;

impl IdentityProvider for ProductionIdentity {
    fn context(&self, include_vulkan: bool) -> Result<BenchmarkContext, PerformanceRuntimeError> {
        production_context(include_vulkan)
    }
}

fn discover_with(
    settings_store: &SettingsStore,
    catalog: &PinnedCatalog,
    managed: &[ManagedInstall],
    identity: &dyn IdentityProvider,
) -> Result<ProductionCandidateInventory, PerformanceRuntimeError> {
    let mut inventory = ProductionCandidateInventory::default();
    let mut accurate_paths = BTreeSet::new();
    if let Ok(settings) = settings_store.load() {
        let configured = settings_store.resolve_model_path(&settings.recognition.model_path);
        if let Ok(Some(variant)) = identify_pinned_model(&configured) {
            accurate_paths.insert((variant_rank(variant), configured));
        }
    }

    for variant in pinned_variants() {
        let spec = model_for_variant(variant).map_err(|_| PerformanceRuntimeError::Catalog)?;
        let id = AssetId::new(spec.id.clone()).map_err(|_| PerformanceRuntimeError::Catalog)?;
        let Some(pinned) = catalog.artifact(&id) else {
            return Err(PerformanceRuntimeError::Catalog);
        };
        if let Some(install) = exact_install(managed, pinned) {
            let Packaging::RawFile { file_name } = pinned.packaging() else {
                return Err(PerformanceRuntimeError::Catalog);
            };
            let path = install.target.join(file_name);
            if identify_pinned_model(&path).ok().flatten() == Some(variant) {
                accurate_paths.insert((variant_rank(variant), path));
            } else {
                for language in languages_for_variant(variant) {
                    inventory.unavailable.push(CandidateUnavailable {
                        engine: EngineKind::Accurate,
                        backend: BackendKind::Cpu,
                        language,
                        reason: CandidateUnavailableReason::AssetIdentity,
                    });
                }
            }
        }
    }

    let present_variants = accurate_paths
        .iter()
        .map(|(rank, _)| *rank)
        .collect::<BTreeSet<_>>();
    for variant in pinned_variants() {
        if present_variants.contains(&variant_rank(variant)) {
            continue;
        }
        for language in languages_for_variant(variant) {
            if !inventory.unavailable.iter().any(|unavailable| {
                unavailable.engine == EngineKind::Accurate
                    && unavailable.backend == BackendKind::Cpu
                    && unavailable.language == language
                    && unavailable.reason == CandidateUnavailableReason::AssetIdentity
            }) {
                inventory.unavailable.push(CandidateUnavailable {
                    engine: EngineKind::Accurate,
                    backend: BackendKind::Cpu,
                    language,
                    reason: CandidateUnavailableReason::ManagedInventory,
                });
            }
        }
    }

    // A GPU probe alone is not enough: if its exact device/ICD identity cannot
    // be captured, retain CPU candidates and truthfully omit Vulkan.
    let (vulkan_available, context) =
        if phorminx_whisper::probe_backend(WhisperBackendPreference::Vulkan).is_ok() {
            match identity.context(true) {
                Ok(context) => (true, context),
                Err(_) => (false, identity.context(false)?),
            }
        } else {
            (false, identity.context(false)?)
        };
    for (_, path) in accurate_paths {
        let Some(variant) = identify_pinned_model(&path).ok().flatten() else {
            continue;
        };
        let spec = model_for_variant(variant).map_err(|_| PerformanceRuntimeError::Catalog)?;
        let digest =
            Sha256Digest::new(spec.sha256).map_err(|_| PerformanceRuntimeError::Catalog)?;
        for language in languages_for_variant(variant) {
            add_accurate_candidate(
                &mut inventory.available,
                &context,
                variant,
                path.clone(),
                language,
                BackendKind::Cpu,
                digest.clone(),
            )?;
            if vulkan_available {
                add_accurate_candidate(
                    &mut inventory.available,
                    &context,
                    variant,
                    path.clone(),
                    language,
                    BackendKind::Vulkan,
                    digest.clone(),
                )?;
            } else {
                inventory.unavailable.push(CandidateUnavailable {
                    engine: EngineKind::Accurate,
                    backend: BackendKind::Vulkan,
                    language,
                    reason: CandidateUnavailableReason::BackendUnavailable,
                });
            }
        }
    }

    add_vosk_candidates(&mut inventory, &context, catalog, managed)?;
    inventory.available.sort_by(|left, right| {
        left.candidate
            .candidate_id()
            .cmp(right.candidate.candidate_id())
    });
    inventory
        .available
        .dedup_by(|left, right| left.candidate.candidate_id() == right.candidate.candidate_id());
    inventory.unavailable.sort();
    inventory.unavailable.dedup();
    Ok(inventory)
}

fn add_accurate_candidate(
    output: &mut Vec<InstalledBenchmarkCandidate>,
    context: &BenchmarkContext,
    variant: AccurateModelVariant,
    path: PathBuf,
    language: Language,
    backend: BackendKind,
    digest: Sha256Digest,
) -> Result<(), PerformanceRuntimeError> {
    let spec = model_for_variant(variant).map_err(|_| PerformanceRuntimeError::Catalog)?;
    let candidate = BenchmarkCandidate::new(
        content_id(&format!(
            "{}-{}-{}",
            spec.id,
            backend_label(backend),
            language.code()
        ))?,
        EngineKind::Accurate,
        backend,
        model_class(variant),
        digest.clone(),
        [language],
        false,
    );
    let preference = match backend {
        BackendKind::Cpu => AccurateBackendPreference::Cpu,
        BackendKind::Vulkan => AccurateBackendPreference::Vulkan,
        BackendKind::VoskNative => return Err(PerformanceRuntimeError::InvalidIdentity),
    };
    let profile = ApplicationProfile::new(
        candidate.clone(),
        language,
        RecognitionApplication::Accurate {
            model: variant,
            backend: preference,
            model_path: path.clone(),
        },
    )
    .map_err(|_| PerformanceRuntimeError::InvalidIdentity)?;
    output.push(InstalledBenchmarkCandidate {
        candidate,
        context: context.clone(),
        profile,
        runtime: RuntimeCandidate::Accurate {
            model: path,
            expected_model_digest: digest.clone(),
            backend: match backend {
                BackendKind::Cpu => WhisperBackendPreference::Cpu,
                BackendKind::Vulkan => WhisperBackendPreference::Vulkan,
                BackendKind::VoskNative => unreachable!(),
            },
            language,
        },
    });
    Ok(())
}

fn add_vosk_candidates(
    inventory: &mut ProductionCandidateInventory,
    context: &BenchmarkContext,
    catalog: &PinnedCatalog,
    managed: &[ManagedInstall],
) -> Result<(), PerformanceRuntimeError> {
    let runtime_id =
        AssetId::new("vosk-runtime-win64-0-3-45").map_err(|_| PerformanceRuntimeError::Catalog)?;
    let model_id = AssetId::new("vosk-model-small-en-us-0-15")
        .map_err(|_| PerformanceRuntimeError::Catalog)?;
    let (Some(runtime_pin), Some(model_pin)) =
        (catalog.artifact(&runtime_id), catalog.artifact(&model_id))
    else {
        return Err(PerformanceRuntimeError::Catalog);
    };
    inventory.unavailable.push(CandidateUnavailable {
        engine: EngineKind::Instant,
        backend: BackendKind::VoskNative,
        language: Language::PortugueseBrazil,
        reason: CandidateUnavailableReason::UnsupportedLanguage,
    });
    let (Some(runtime), Some(model)) = (
        exact_install(managed, runtime_pin),
        exact_install(managed, model_pin),
    ) else {
        inventory.unavailable.push(CandidateUnavailable {
            engine: EngineKind::Instant,
            backend: BackendKind::VoskNative,
            language: Language::English,
            reason: CandidateUnavailableReason::RuntimeUnavailable,
        });
        return Ok(());
    };
    if !matches!(
        phorminx_vosk::validate_asset_layout(&runtime.target, &model.target, "en"),
        phorminx_vosk::AssetLayout::Present { .. }
    ) {
        inventory.unavailable.push(CandidateUnavailable {
            engine: EngineKind::Instant,
            backend: BackendKind::VoskNative,
            language: Language::English,
            reason: CandidateUnavailableReason::AssetIdentity,
        });
        return Ok(());
    }
    let (Ok(runtime_digest), Ok(model_digest)) =
        (hash_tree(&runtime.target), hash_tree(&model.target))
    else {
        inventory.unavailable.push(CandidateUnavailable {
            engine: EngineKind::Instant,
            backend: BackendKind::VoskNative,
            language: Language::English,
            reason: CandidateUnavailableReason::AssetIdentity,
        });
        return Ok(());
    };
    let mut identity = Sha256::new();
    identity.update(b"vosk-native\0");
    identity.update(&runtime_digest);
    identity.update(b"\0en\0");
    identity.update(&model_digest);
    let candidate_id = digest_id("vosk-en", &identity.finalize())?;
    let candidate = BenchmarkCandidate::new(
        candidate_id,
        EngineKind::Instant,
        BackendKind::VoskNative,
        ModelClass::Other,
        Sha256Digest::new(digest_hex(&model_digest))
            .map_err(|_| PerformanceRuntimeError::InvalidIdentity)?,
        [Language::English],
        false,
    );
    let profile = ApplicationProfile::new(
        candidate.clone(),
        Language::English,
        RecognitionApplication::Instant {
            model_path: model.target.clone(),
            runtime_path: runtime.target.clone(),
        },
    )
    .map_err(|_| PerformanceRuntimeError::InvalidIdentity)?;
    inventory.available.push(InstalledBenchmarkCandidate {
        candidate,
        context: context.clone(),
        profile,
        runtime: RuntimeCandidate::Instant {
            runtime: runtime.target.clone(),
            model: model.target.clone(),
            expected_runtime_digest: runtime_digest,
            expected_model_digest: model_digest,
            language: Language::English,
        },
    });
    Ok(())
}

fn exact_install<'a>(
    installs: &'a [ManagedInstall],
    pinned: &crate::setup_host::PinnedArtifact,
) -> Option<&'a ManagedInstall> {
    installs.iter().find(|install| {
        install.receipt.asset().descriptor() == pinned.descriptor()
            && install.receipt.asset().slot().as_str() == pinned.activation_slot()
    })
}

fn pinned_variants() -> [AccurateModelVariant; 4] {
    [
        AccurateModelVariant::TinyEnglish,
        AccurateModelVariant::BaseEnglish,
        AccurateModelVariant::TinyMultilingual,
        AccurateModelVariant::BaseMultilingual,
    ]
}

const fn variant_rank(variant: AccurateModelVariant) -> u8 {
    match variant {
        AccurateModelVariant::TinyEnglish => 0,
        AccurateModelVariant::BaseEnglish => 1,
        AccurateModelVariant::TinyMultilingual => 2,
        AccurateModelVariant::BaseMultilingual => 3,
        AccurateModelVariant::Custom => 4,
    }
}

fn languages_for_variant(variant: AccurateModelVariant) -> Vec<Language> {
    match variant {
        AccurateModelVariant::TinyEnglish | AccurateModelVariant::BaseEnglish => {
            vec![Language::English]
        }
        AccurateModelVariant::TinyMultilingual | AccurateModelVariant::BaseMultilingual => {
            vec![Language::English, Language::PortugueseBrazil]
        }
        AccurateModelVariant::Custom => Vec::new(),
    }
}

const fn model_class(variant: AccurateModelVariant) -> ModelClass {
    match variant {
        AccurateModelVariant::TinyEnglish | AccurateModelVariant::TinyMultilingual => {
            ModelClass::Tiny
        }
        AccurateModelVariant::BaseEnglish | AccurateModelVariant::BaseMultilingual => {
            ModelClass::Base
        }
        AccurateModelVariant::Custom => ModelClass::Other,
    }
}

const fn backend_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Cpu => "cpu",
        BackendKind::Vulkan => "vulkan",
        BackendKind::VoskNative => "vosk",
    }
}

fn content_id(value: &str) -> Result<ContentFreeId, PerformanceRuntimeError> {
    ContentFreeId::new(value).map_err(|_| PerformanceRuntimeError::InvalidIdentity)
}

fn production_context(include_vulkan: bool) -> Result<BenchmarkContext, PerformanceRuntimeError> {
    let executable = env::current_exe().map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    let build = hash_file(&executable)?;

    let mut device = Sha256::new();
    device.update(env::consts::ARCH.as_bytes());
    device.update(
        thread::available_parallelism()
            .map_err(|_| PerformanceRuntimeError::HostIdentity)?
            .get()
            .to_le_bytes(),
    );
    for key in [
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
    ] {
        if let Some(value) = env::var_os(key) {
            device.update(value.to_string_lossy().as_bytes());
        }
        device.update([0]);
    }
    if include_vulkan {
        let (_, name) = phorminx_whisper::probe_backend(WhisperBackendPreference::Vulkan)
            .map_err(|_| PerformanceRuntimeError::HostIdentity)?;
        device.update(
            name.ok_or(PerformanceRuntimeError::HostIdentity)?
                .as_bytes(),
        );
    }

    let windows_root = env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(PerformanceRuntimeError::HostIdentity)?;
    let mut driver = Sha256::new();
    driver.update(hash_file(&windows_root.join("System32/ntoskrnl.exe"))?);
    if include_vulkan {
        let manifests = phorminx_windows::vulkan_driver_manifests()
            .map_err(|_| PerformanceRuntimeError::HostIdentity)?;
        if manifests.is_empty() {
            return Err(PerformanceRuntimeError::HostIdentity);
        }
        for manifest in manifests {
            let (manifest_digest, library_digest) = hash_vulkan_driver(&manifest)?;
            driver.update(manifest_digest);
            driver.update(library_digest);
        }
    }

    Ok(BenchmarkContext {
        protocol_id: content_id(CalibrationCorpus::PROTOCOL_ID)?,
        build_id: digest_id("build", &build)?,
        device_id: digest_id("device", &device.finalize())?,
        driver_id: digest_id("driver", &driver.finalize())?,
    })
}

#[derive(Deserialize)]
struct VulkanManifest {
    #[serde(rename = "ICD")]
    icd: VulkanIcd,
}

#[derive(Deserialize)]
struct VulkanIcd {
    library_path: String,
}

fn hash_vulkan_driver(path: &Path) -> Result<(Vec<u8>, Vec<u8>), PerformanceRuntimeError> {
    let mut file = File::open(path).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    let length = file
        .metadata()
        .map_err(|_| PerformanceRuntimeError::HostIdentity)?
        .len();
    if length == 0 || length > MAX_VULKAN_MANIFEST_BYTES {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    Read::by_ref(&mut file)
        .take(MAX_VULKAN_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    if bytes.len() as u64 != length {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    let manifest_digest = Sha256::digest(&bytes).to_vec();
    let manifest: VulkanManifest =
        serde_json::from_slice(&bytes).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    let configured = PathBuf::from(manifest.icd.library_path);
    let library = if configured.is_absolute() {
        configured
    } else {
        path.parent()
            .ok_or(PerformanceRuntimeError::HostIdentity)?
            .join(configured)
    };
    let library_digest = hash_file(&library)?;
    // Detect an ICD manifest replacement between parsing it and hashing the
    // referenced driver. A racing configuration is not exact evidence.
    if hash_file(path)? != manifest_digest {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    Ok((manifest_digest, library_digest))
}

fn hash_file(path: &Path) -> Result<Vec<u8>, PerformanceRuntimeError> {
    let mut file = File::open(path).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    let length = file
        .metadata()
        .map_err(|_| PerformanceRuntimeError::HostIdentity)?
        .len();
    if length == 0 || length > MAX_HASHED_FILE_BYTES {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| PerformanceRuntimeError::HostIdentity)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_vec())
}

/// Creates a deterministic digest of every regular file in a managed tree.
/// This binds benchmark evidence to the bytes that will actually load, rather
/// than trusting an installation receipt after mutable files could change.
fn hash_tree(root: &Path) -> Result<Vec<u8>, PerformanceRuntimeError> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|_| PerformanceRuntimeError::HostIdentity)? {
            let entry = entry.map_err(|_| PerformanceRuntimeError::HostIdentity)?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
            if metadata.file_type().is_symlink() {
                return Err(PerformanceRuntimeError::HostIdentity);
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| PerformanceRuntimeError::HostIdentity)?
                .to_str()
                .ok_or(PerformanceRuntimeError::HostIdentity)?
                .replace('\\', "/");
            if matches!(
                relative.as_str(),
                ".phorminx-owner.json" | ".phorminx-receipt.json"
            ) {
                continue;
            }
            if relative.is_empty() || relative.as_bytes().contains(&0) {
                return Err(PerformanceRuntimeError::HostIdentity);
            }
            if metadata.is_dir() {
                pending.push(path.clone());
                entries.push((relative, path, None));
            } else if metadata.is_file() {
                entries.push((relative, path, Some(metadata.len())));
            } else {
                return Err(PerformanceRuntimeError::HostIdentity);
            }
            if entries.len() > MAX_HASHED_TREE_ENTRIES {
                return Err(PerformanceRuntimeError::HostIdentity);
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut total_bytes = 0_u64;
    let mut tree = Sha256::new();
    for (relative, path, length) in entries {
        tree.update(if length.is_some() {
            b"file\0".as_slice()
        } else {
            b"dir\0".as_slice()
        });
        tree.update(
            u64::try_from(relative.len())
                .map_err(|_| PerformanceRuntimeError::HostIdentity)?
                .to_le_bytes(),
        );
        tree.update(relative.as_bytes());
        let Some(length) = length else {
            continue;
        };
        total_bytes = total_bytes
            .checked_add(length)
            .filter(|total| *total <= MAX_HASHED_TREE_BYTES)
            .ok_or(PerformanceRuntimeError::HostIdentity)?;
        tree.update(length.to_le_bytes());
        tree.update(hash_file_bounded(&path, length)?);
    }
    if total_bytes == 0 {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    Ok(tree.finalize().to_vec())
}

fn hash_file_bounded(
    path: &Path,
    expected_length: u64,
) -> Result<Vec<u8>, PerformanceRuntimeError> {
    if expected_length > MAX_HASHED_TREE_BYTES {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    let mut file = File::open(path).map_err(|_| PerformanceRuntimeError::HostIdentity)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut read_bytes = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| PerformanceRuntimeError::HostIdentity)?;
        if count == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(count as u64)
            .filter(|total| *total <= expected_length)
            .ok_or(PerformanceRuntimeError::HostIdentity)?;
        hasher.update(&buffer[..count]);
    }
    if read_bytes != expected_length {
        return Err(PerformanceRuntimeError::HostIdentity);
    }
    Ok(hasher.finalize().to_vec())
}

fn digest_hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_id(prefix: &str, digest: &[u8]) -> Result<ContentFreeId, PerformanceRuntimeError> {
    let encoded = digest_hex(digest);
    content_id(&format!("{prefix}-{encoded}"))
}

/// Prompt text is intentionally displayable but redacted from Debug and is
/// never serializable. It is the exact reference later used for scoring.
#[derive(Clone, Eq, PartialEq)]
pub struct CalibrationPrompt {
    pub case_id: ContentFreeId,
    pub kind: CalibrationKind,
    text: &'static str,
}

impl CalibrationPrompt {
    #[must_use]
    pub const fn text(&self) -> &'static str {
        self.text
    }
}

impl std::fmt::Debug for CalibrationPrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CalibrationPrompt")
            .field("case_id", &self.case_id)
            .field("kind", &self.kind)
            .field("text", &"[REDACTED]")
            .finish()
    }
}

enum CalibrationSlot {
    Empty,
    Ready(AudioClip),
    Consumed,
}

struct CalibrationState {
    expected: BTreeMap<ContentFreeId, CalibrationSlot>,
}

/// Bounded, one-use calibration audio. Dropping this value drops every sample;
/// it has no persistence, export, logging, or cloning API for audio.
pub struct TransientCalibrationAudio {
    language: Language,
    prompts: Vec<CalibrationPrompt>,
    state: Mutex<CalibrationState>,
}

impl std::fmt::Debug for TransientCalibrationAudio {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransientCalibrationAudio")
            .field("language", &self.language)
            .field("sample_count", &self.prompts.len())
            .field("audio", &"[REDACTED]")
            .finish()
    }
}

impl TransientCalibrationAudio {
    #[must_use]
    pub fn pinned_v1(language: Language) -> Self {
        let corpus = CalibrationCorpus::pinned_v1();
        let prompts = corpus
            .cases_for(language)
            .map(|case| CalibrationPrompt {
                case_id: case.case_id().clone(),
                kind: case.kind(),
                text: case.ephemeral_content().reference(),
            })
            .collect::<Vec<_>>();
        let expected = prompts
            .iter()
            .map(|prompt| (prompt.case_id.clone(), CalibrationSlot::Empty))
            .collect();
        Self {
            language,
            prompts,
            state: Mutex::new(CalibrationState { expected }),
        }
    }

    #[must_use]
    pub fn prompts(&self) -> &[CalibrationPrompt] {
        &self.prompts
    }

    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    pub fn submit(
        &self,
        case_id: &ContentFreeId,
        clip: AudioClip,
    ) -> Result<(), PerformanceRuntimeError> {
        if clip.sample_rate != CALIBRATION_SAMPLE_RATE
            || !(MIN_CALIBRATION_DURATION..=MAX_CALIBRATION_DURATION).contains(&clip.duration())
            || clip.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(PerformanceRuntimeError::Calibration);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| PerformanceRuntimeError::Calibration)?;
        let slot = state
            .expected
            .get_mut(case_id)
            .ok_or(PerformanceRuntimeError::Calibration)?;
        if !matches!(slot, CalibrationSlot::Empty) {
            return Err(PerformanceRuntimeError::Calibration);
        }
        *slot = CalibrationSlot::Ready(clip);
        Ok(())
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .expected
                .values()
                .all(|slot| matches!(slot, CalibrationSlot::Ready(_)))
        })
    }

    fn take(&self, case: &CalibrationCase) -> Result<AudioClip, MeasurementFailure> {
        if case.language() != self.language {
            return Err(MeasurementFailure::CaptureFailed);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| MeasurementFailure::CaptureFailed)?;
        let slot = state
            .expected
            .get_mut(case.case_id())
            .ok_or(MeasurementFailure::CaptureFailed)?;
        match std::mem::replace(slot, CalibrationSlot::Consumed) {
            CalibrationSlot::Ready(clip) => Ok(clip),
            previous => {
                *slot = previous;
                Err(MeasurementFailure::CaptureFailed)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeActivityKind {
    Dictation,
    Whisper,
    Ollama,
}

#[derive(Default)]
struct WorkloadState {
    benchmark: bool,
    dictation: u32,
    whisper: u32,
    ollama: u32,
}

#[derive(Default)]
pub struct WorkloadCoordinator {
    state: Arc<Mutex<WorkloadState>>,
}

impl std::fmt::Debug for WorkloadCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorkloadCoordinator([REDACTED STATE])")
    }
}

impl WorkloadCoordinator {
    pub fn try_begin(
        &self,
        kind: RuntimeActivityKind,
    ) -> Result<RuntimeActivityLease, BenchmarkPreflightFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BenchmarkPreflightFailure::Unavailable)?;
        if state.benchmark {
            return Err(BenchmarkPreflightFailure::ComputeContention);
        }
        let counter = match kind {
            RuntimeActivityKind::Dictation => &mut state.dictation,
            RuntimeActivityKind::Whisper => &mut state.whisper,
            RuntimeActivityKind::Ollama => &mut state.ollama,
        };
        *counter = counter
            .checked_add(1)
            .ok_or(BenchmarkPreflightFailure::Unavailable)?;
        Ok(RuntimeActivityLease {
            state: Arc::clone(&self.state),
            kind,
            active: true,
        })
    }

    fn try_begin_benchmark(&self) -> Result<BenchmarkLease, BenchmarkPreflightFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BenchmarkPreflightFailure::Unavailable)?;
        if state.benchmark {
            return Err(BenchmarkPreflightFailure::ComputeContention);
        }
        if state.dictation != 0 {
            return Err(BenchmarkPreflightFailure::DictationActive);
        }
        if state.whisper != 0 || state.ollama != 0 {
            return Err(BenchmarkPreflightFailure::ComputeContention);
        }
        state.benchmark = true;
        Ok(BenchmarkLease {
            state: Arc::clone(&self.state),
            active: true,
        })
    }
}

#[must_use]
pub fn production_workload_coordinator() -> Arc<WorkloadCoordinator> {
    Arc::clone(GLOBAL_WORKLOAD.get_or_init(|| Arc::new(WorkloadCoordinator::default())))
}

pub struct RuntimeActivityLease {
    state: Arc<Mutex<WorkloadState>>,
    kind: RuntimeActivityKind,
    active: bool,
}

impl Drop for RuntimeActivityLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let counter = match self.kind {
                RuntimeActivityKind::Dictation => &mut state.dictation,
                RuntimeActivityKind::Whisper => &mut state.whisper,
                RuntimeActivityKind::Ollama => &mut state.ollama,
            };
            *counter = counter.saturating_sub(1);
        }
        self.active = false;
    }
}

struct BenchmarkLease {
    state: Arc<Mutex<WorkloadState>>,
    active: bool,
}

impl Drop for BenchmarkLease {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut state) = self.state.lock() {
                state.benchmark = false;
            }
            self.active = false;
        }
    }
}

pub trait HostResourceProbe: Send + Sync + 'static {
    fn snapshot(&self) -> Result<HostResources, MeasurementFailure>;

    fn contention_condition(&self) -> Result<ContentionCondition, MeasurementFailure> {
        self.snapshot()
            .map(|snapshot| snapshot.contention_condition)
    }

    fn preflight(&self) -> Result<HostResources, MeasurementFailure> {
        let mut snapshot = self.snapshot()?;
        snapshot.contention_condition = self.contention_condition()?;
        Ok(snapshot)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostResources {
    pub working_set_mib: u32,
    pub available_memory_mib: u32,
    pub thermal_condition: ThermalCondition,
    pub contention_condition: ContentionCondition,
}

#[derive(Default)]
pub struct WindowsHostResourceProbe;

impl HostResourceProbe for WindowsHostResourceProbe {
    fn snapshot(&self) -> Result<HostResources, MeasurementFailure> {
        let snapshot = phorminx_windows::host_resource_snapshot()
            .map_err(|_| MeasurementFailure::ResourceProbeFailed)?;
        Ok(HostResources {
            working_set_mib: snapshot.working_set_mib,
            available_memory_mib: snapshot.available_memory_mib,
            thermal_condition: if snapshot.execution_speed_throttled {
                ThermalCondition::Elevated
            } else {
                ThermalCondition::Nominal
            },
            contention_condition: ContentionCondition::Idle,
        })
    }

    fn contention_condition(&self) -> Result<ContentionCondition, MeasurementFailure> {
        let busy = phorminx_windows::host_cpu_busy_per_mille(Duration::from_millis(75))
            .map_err(|_| MeasurementFailure::ResourceProbeFailed)?;
        Ok(if busy > 850 {
            ContentionCondition::Contended
        } else {
            ContentionCondition::Idle
        })
    }
}

pub struct ProductionBenchmarkPreflight {
    workloads: Arc<WorkloadCoordinator>,
    resources: Arc<dyn HostResourceProbe>,
}

impl ProductionBenchmarkPreflight {
    #[must_use]
    pub fn new(workloads: Arc<WorkloadCoordinator>, resources: Arc<dyn HostResourceProbe>) -> Self {
        Self {
            workloads,
            resources,
        }
    }

    #[must_use]
    pub fn for_current_process() -> Self {
        Self::new(
            production_workload_coordinator(),
            Arc::new(WindowsHostResourceProbe),
        )
    }
}

impl PerformanceBenchmarkPreflight for ProductionBenchmarkPreflight {
    fn try_acquire(
        &self,
        _candidate: &BenchmarkCandidate,
    ) -> Result<Box<dyn BenchmarkActivityLease>, BenchmarkPreflightFailure> {
        let lease = self.workloads.try_begin_benchmark()?;
        let resources = self
            .resources
            .preflight()
            .map_err(|_| BenchmarkPreflightFailure::Unavailable)?;
        if resources.thermal_condition != ThermalCondition::Nominal {
            return Err(BenchmarkPreflightFailure::ThermalState);
        }
        if resources.available_memory_mib < MIN_AVAILABLE_MEMORY_MIB {
            return Err(BenchmarkPreflightFailure::SystemContention);
        }
        if resources.contention_condition != ContentionCondition::Idle {
            return Err(BenchmarkPreflightFailure::SystemContention);
        }
        Ok(Box::new(lease))
    }
}

#[derive(Clone)]
enum RuntimeCandidate {
    Accurate {
        model: PathBuf,
        expected_model_digest: Sha256Digest,
        backend: WhisperBackendPreference,
        language: Language,
    },
    Instant {
        runtime: PathBuf,
        model: PathBuf,
        expected_runtime_digest: Vec<u8>,
        expected_model_digest: Vec<u8>,
        language: Language,
    },
}

impl std::fmt::Debug for RuntimeCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accurate {
                backend, language, ..
            } => formatter
                .debug_struct("Accurate")
                .field("backend", backend)
                .field("language", language)
                .field("model", &"[REDACTED PATH]")
                .finish(),
            Self::Instant { language, .. } => formatter
                .debug_struct("Instant")
                .field("language", language)
                .field("runtime", &"[REDACTED PATH]")
                .field("model", &"[REDACTED PATH]")
                .finish(),
        }
    }
}

impl RuntimeCandidate {
    const fn language(&self) -> Language {
        match self {
            Self::Accurate { language, .. } | Self::Instant { language, .. } => *language,
        }
    }
}

struct EngineMeasurement {
    text: String,
    load_latency: Duration,
    release_latency: Duration,
    inference_time: Duration,
    audio_duration: Duration,
    peak_working_set_mib: u32,
    available_memory_mib: u32,
    fallback_count: u32,
    thermal_condition: ThermalCondition,
    contention_condition: ContentionCondition,
}

trait ResidentRecognitionEngine {
    fn transcribe(
        &mut self,
        clip: &AudioClip,
        abort: &Arc<AtomicBool>,
    ) -> Result<Transcript, MeasurementFailure>;
}

trait RecognitionEngineFactory: Send + Sync + 'static {
    fn load(
        &self,
        runtime: &RuntimeCandidate,
    ) -> Result<Box<dyn ResidentRecognitionEngine>, MeasurementFailure>;
}

struct ProductionRecognitionEngineFactory;

impl RecognitionEngineFactory for ProductionRecognitionEngineFactory {
    fn load(
        &self,
        runtime: &RuntimeCandidate,
    ) -> Result<Box<dyn ResidentRecognitionEngine>, MeasurementFailure> {
        match runtime {
            RuntimeCandidate::Accurate {
                model,
                expected_model_digest,
                backend,
                language,
            } => {
                if digest_hex(&hash_file(model).map_err(|_| MeasurementFailure::LoadFailed)?)
                    != expected_model_digest.as_str()
                {
                    return Err(MeasurementFailure::LoadFailed);
                }
                let recognizer = WhisperRecognizer::load_with_backend(model, *backend)
                    .map_err(|_| MeasurementFailure::LoadFailed)?;
                let expected = match backend {
                    WhisperBackendPreference::Cpu => WhisperBackend::Cpu,
                    WhisperBackendPreference::Vulkan => WhisperBackend::Vulkan,
                    WhisperBackendPreference::Auto => return Err(MeasurementFailure::LoadFailed),
                };
                let readiness = recognizer.readiness();
                if readiness.backend != expected || readiness.fallback_from.is_some() {
                    return Err(MeasurementFailure::LoadFailed);
                }
                Ok(Box::new(ProductionResidentEngine::Accurate {
                    recognizer,
                    language: *language,
                }))
            }
            RuntimeCandidate::Instant {
                runtime,
                model,
                expected_runtime_digest,
                expected_model_digest,
                language,
            } => {
                if &hash_tree(runtime).map_err(|_| MeasurementFailure::LoadFailed)?
                    != expected_runtime_digest
                    || &hash_tree(model).map_err(|_| MeasurementFailure::LoadFailed)?
                        != expected_model_digest
                {
                    return Err(MeasurementFailure::LoadFailed);
                }
                let recognizer = VoskModel::load(runtime, model, language.code())
                    .map_err(|_| MeasurementFailure::LoadFailed)?;
                Ok(Box::new(ProductionResidentEngine::Instant {
                    recognizer,
                    language: *language,
                }))
            }
        }
    }
}

enum ProductionResidentEngine {
    Accurate {
        recognizer: WhisperRecognizer,
        language: Language,
    },
    Instant {
        recognizer: VoskModel,
        language: Language,
    },
}

impl ResidentRecognitionEngine for ProductionResidentEngine {
    fn transcribe(
        &mut self,
        clip: &AudioClip,
        abort: &Arc<AtomicBool>,
    ) -> Result<Transcript, MeasurementFailure> {
        match self {
            Self::Accurate {
                recognizer,
                language,
            } => {
                let options = TranscriptionOptions {
                    language: Some(language.code()),
                    thread_count: None,
                    audio_context: None,
                };
                recognizer
                    .transcribe_detailed(clip, &options, None, Some(abort))
                    .map(|result| result.transcript)
                    .map_err(|error| {
                        if abort.load(Ordering::Acquire) {
                            MeasurementFailure::Cancelled
                        } else {
                            let _ = error;
                            MeasurementFailure::RecognitionFailed
                        }
                    })
            }
            Self::Instant {
                recognizer,
                language,
            } => {
                if *language != Language::English || abort.load(Ordering::Acquire) {
                    return Err(MeasurementFailure::Cancelled);
                }
                let started = Instant::now();
                let mut session = recognizer
                    .session(clip.sample_rate)
                    .map_err(|_| MeasurementFailure::RecognitionFailed)?;
                session
                    .accept_f32(&clip.samples)
                    .map_err(|_| MeasurementFailure::RecognitionFailed)?;
                let text = session
                    .finish()
                    .map_err(|_| MeasurementFailure::RecognitionFailed)?;
                if abort.load(Ordering::Acquire) {
                    return Err(MeasurementFailure::Cancelled);
                }
                Ok(Transcript {
                    text,
                    backend: "vosk",
                    model_load_time: Duration::ZERO,
                    inference_time: started.elapsed(),
                    audio_duration: clip.duration(),
                })
            }
        }
    }
}

struct EngineRequest {
    candidate_id: ContentFreeId,
    runtime: RuntimeCandidate,
    clip: AudioClip,
    cold_load: bool,
    abort: Arc<AtomicBool>,
    response: SyncSender<Result<EngineMeasurement, MeasurementFailure>>,
}

struct EngineClient {
    requests: SyncSender<EngineRequest>,
}

struct NativeWorkerPermit;

impl Drop for NativeWorkerPermit {
    fn drop(&mut self) {
        GLOBAL_NATIVE_BENCHMARK_WORKER.store(false, Ordering::Release);
    }
}

fn start_engine_worker(
    factory: Arc<dyn RecognitionEngineFactory>,
    resources: Arc<dyn HostResourceProbe>,
    control: &BenchmarkControl,
) -> Result<EngineClient, MeasurementFailure> {
    let handoff_deadline = Instant::now() + NATIVE_WORKER_HANDOFF_TIMEOUT.min(control.remaining());
    loop {
        if control.is_cancelled() {
            return Err(if control.remaining().is_zero() {
                MeasurementFailure::DeadlineExceeded
            } else {
                MeasurementFailure::Cancelled
            });
        }
        if GLOBAL_NATIVE_BENCHMARK_WORKER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
        if Instant::now() >= handoff_deadline {
            return Err(MeasurementFailure::LoadFailed);
        }
        thread::sleep(Duration::from_millis(1));
    }
    let (request_tx, request_rx) = mpsc::sync_channel::<EngineRequest>(1);
    match thread::Builder::new()
        .name("phorminx-performance-native".to_owned())
        .spawn(move || {
            let _permit = NativeWorkerPermit;
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                engine_worker(request_rx, factory.as_ref(), resources)
            }));
        }) {
        Ok(_) => Ok(EngineClient {
            requests: request_tx,
        }),
        Err(_) => {
            GLOBAL_NATIVE_BENCHMARK_WORKER.store(false, Ordering::Release);
            Err(MeasurementFailure::LoadFailed)
        }
    }
}

fn engine_worker(
    requests: Receiver<EngineRequest>,
    factory: &dyn RecognitionEngineFactory,
    resources: Arc<dyn HostResourceProbe>,
) {
    let mut loaded_id = None;
    let mut engine: Option<Box<dyn ResidentRecognitionEngine>> = None;
    while let Ok(request) = requests.recv_timeout(NATIVE_WORKER_IDLE_TIMEOUT) {
        let result = run_engine_request(
            &request,
            factory,
            Arc::clone(&resources),
            &mut loaded_id,
            &mut engine,
        );
        let _ = request.response.send(result);
    }
}

fn run_engine_request(
    request: &EngineRequest,
    factory: &dyn RecognitionEngineFactory,
    resources: Arc<dyn HostResourceProbe>,
    loaded_id: &mut Option<ContentFreeId>,
    engine: &mut Option<Box<dyn ResidentRecognitionEngine>>,
) -> Result<EngineMeasurement, MeasurementFailure> {
    if request.abort.load(Ordering::Acquire) {
        return Err(MeasurementFailure::Cancelled);
    }
    let release_started = Instant::now();
    // Sample before loading so the reported memory is a candidate-local delta,
    // not the process lifetime peak or the app's unrelated resident baseline.
    let mut peak = ResourcePeakSampler::start(Arc::clone(&resources), Arc::clone(&request.abort))?;
    let load_started = Instant::now();
    let load_latency = if request.cold_load {
        let replacement = factory.load(&request.runtime)?;
        *engine = Some(replacement);
        *loaded_id = Some(request.candidate_id.clone());
        load_started.elapsed()
    } else {
        if loaded_id.as_ref() != Some(&request.candidate_id) || engine.is_none() {
            return Err(MeasurementFailure::LoadFailed);
        }
        Duration::ZERO
    };
    let transcript = engine
        .as_mut()
        .ok_or(MeasurementFailure::LoadFailed)?
        .transcribe(&request.clip, &request.abort)?;
    let measured_release_latency = release_started.elapsed();
    let sampled = peak.finish()?;
    if request.abort.load(Ordering::Acquire) {
        return Err(MeasurementFailure::Cancelled);
    }
    Ok(EngineMeasurement {
        text: transcript.text,
        load_latency,
        release_latency: measured_release_latency,
        inference_time: transcript.inference_time,
        audio_duration: transcript.audio_duration,
        peak_working_set_mib: sampled.peak_working_set_mib,
        available_memory_mib: sampled.available_memory_mib,
        fallback_count: 0,
        thermal_condition: sampled.thermal_condition,
        contention_condition: sampled.contention_condition,
    })
}

struct PeakSample {
    peak_working_set_mib: u32,
    available_memory_mib: u32,
    thermal_condition: ThermalCondition,
    contention_condition: ContentionCondition,
}

struct ResourcePeakSampler {
    stop: Arc<AtomicBool>,
    result: Receiver<Result<PeakSample, MeasurementFailure>>,
    contention_result: Receiver<Result<ContentionCondition, MeasurementFailure>>,
    resources: Arc<dyn HostResourceProbe>,
    baseline_working_set_mib: u32,
}

impl ResourcePeakSampler {
    fn start(
        resources: Arc<dyn HostResourceProbe>,
        abort: Arc<AtomicBool>,
    ) -> Result<Self, MeasurementFailure> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (result_tx, result) = mpsc::sync_channel(1);
        let (contention_tx, contention_result) = mpsc::sync_channel(1);
        let initial = resources.snapshot()?;
        let baseline_working_set_mib = initial.working_set_mib;
        let thread_resources = Arc::clone(&resources);
        let contention_resources = Arc::clone(&resources);
        let contention_stop = Arc::clone(&stop);
        let contention_abort = Arc::clone(&abort);
        thread::Builder::new()
            .name("phorminx-performance-resources".to_owned())
            .spawn(move || {
                let baseline = initial.working_set_mib;
                let mut peak = baseline;
                let mut available = initial.available_memory_mib;
                let mut thermal = initial.thermal_condition;
                let mut failure = None;
                loop {
                    match thread_resources.snapshot() {
                        Ok(sample) => {
                            peak = peak.max(sample.working_set_mib);
                            available = available.min(sample.available_memory_mib);
                            if sample.thermal_condition != ThermalCondition::Nominal {
                                thermal = sample.thermal_condition;
                            }
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                    if thread_stop.load(Ordering::Acquire) || abort.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let result = failure.map_or_else(
                    || {
                        Ok(PeakSample {
                            peak_working_set_mib: peak.saturating_sub(baseline),
                            available_memory_mib: available,
                            thermal_condition: thermal,
                            contention_condition: initial.contention_condition,
                        })
                    },
                    Err,
                );
                let _ = result_tx.send(result);
            })
            .map_err(|_| MeasurementFailure::ResourceProbeFailed)?;
        if thread::Builder::new()
            .name("phorminx-performance-contention".to_owned())
            .spawn(move || {
                let mut observed = ContentionCondition::Idle;
                let mut failure = None;
                loop {
                    match contention_resources.contention_condition() {
                        Ok(
                            condition @ (ContentionCondition::Contended
                            | ContentionCondition::Unknown),
                        ) => {
                            observed = condition;
                            break;
                        }
                        Ok(ContentionCondition::Idle) => {}
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                    if contention_stop.load(Ordering::Acquire)
                        || contention_abort.load(Ordering::Acquire)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = contention_tx.send(failure.map_or(Ok(observed), Err));
            })
            .is_err()
        {
            stop.store(true, Ordering::Release);
            return Err(MeasurementFailure::ResourceProbeFailed);
        }
        Ok(Self {
            stop,
            result,
            contention_result,
            resources,
            baseline_working_set_mib,
        })
    }

    fn finish(&mut self) -> Result<PeakSample, MeasurementFailure> {
        // Force a final synchronous observation before asking the sampler to
        // stop. This captures resident model memory even when a fake or very
        // fast engine finishes between the periodic sampler's 10 ms ticks.
        let final_sample = self.resources.snapshot();
        self.stop.store(true, Ordering::Release);
        let mut sampled = self
            .result
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| MeasurementFailure::ResourceProbeFailed)??;
        let contention = self
            .contention_result
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| MeasurementFailure::ResourceProbeFailed)??;
        let final_sample = final_sample?;
        sampled.peak_working_set_mib = sampled.peak_working_set_mib.max(
            final_sample
                .working_set_mib
                .saturating_sub(self.baseline_working_set_mib),
        );
        sampled.available_memory_mib = sampled
            .available_memory_mib
            .min(final_sample.available_memory_mib);
        if final_sample.thermal_condition != ThermalCondition::Nominal {
            sampled.thermal_condition = final_sample.thermal_condition;
        }
        if final_sample.contention_condition != ContentionCondition::Idle {
            sampled.contention_condition = final_sample.contention_condition;
        }
        if contention != ContentionCondition::Idle {
            sampled.contention_condition = contention;
        }
        Ok(sampled)
    }
}

impl Drop for ResourcePeakSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub struct ProductionPerformanceAdapter {
    candidates: BTreeMap<ContentFreeId, (BenchmarkCandidate, RuntimeCandidate)>,
    calibration: Arc<TransientCalibrationAudio>,
    factory: Arc<dyn RecognitionEngineFactory>,
    resources: Arc<dyn HostResourceProbe>,
    worker: Mutex<Option<EngineClient>>,
}

impl std::fmt::Debug for ProductionPerformanceAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductionPerformanceAdapter")
            .field("candidate_count", &self.candidates.len())
            .field("calibration", &self.calibration)
            .field("runtime", &"[REDACTED]")
            .finish()
    }
}

impl ProductionPerformanceAdapter {
    #[must_use]
    pub fn new(
        inventory: &ProductionCandidateInventory,
        calibration: Arc<TransientCalibrationAudio>,
    ) -> Self {
        Self::with_dependencies(
            inventory,
            calibration,
            Arc::new(ProductionRecognitionEngineFactory),
            Arc::new(WindowsHostResourceProbe),
        )
    }

    fn with_dependencies(
        inventory: &ProductionCandidateInventory,
        calibration: Arc<TransientCalibrationAudio>,
        factory: Arc<dyn RecognitionEngineFactory>,
        resources: Arc<dyn HostResourceProbe>,
    ) -> Self {
        Self {
            candidates: inventory
                .available
                .iter()
                .map(|entry| {
                    (
                        entry.candidate.candidate_id().clone(),
                        (entry.candidate.clone(), entry.runtime.clone()),
                    )
                })
                .collect(),
            calibration,
            factory,
            resources,
            worker: Mutex::new(None),
        }
    }

    fn stop_worker(worker: &mut Option<EngineClient>) {
        worker.take();
    }
}

impl PerformanceMeasurementAdapter for ProductionPerformanceAdapter {
    fn measure(
        &self,
        candidate: &BenchmarkCandidate,
        case: &CalibrationCase,
        cold_load: bool,
        control: &BenchmarkControl,
    ) -> Result<MeasuredSample, MeasurementFailure> {
        if control.is_cancelled() {
            return Err(if control.remaining().is_zero() {
                MeasurementFailure::DeadlineExceeded
            } else {
                MeasurementFailure::Cancelled
            });
        }
        let (trusted, runtime) = self
            .candidates
            .get(candidate.candidate_id())
            .ok_or(MeasurementFailure::LoadFailed)?;
        if trusted != candidate || runtime.language() != case.language() {
            return Err(MeasurementFailure::LoadFailed);
        }
        let abort = control.cancellation_flag();
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| MeasurementFailure::LoadFailed)?;
        if cold_load {
            Self::stop_worker(&mut worker);
        }
        if worker.is_none() {
            if !cold_load {
                return Err(MeasurementFailure::LoadFailed);
            }
            *worker = Some(start_engine_worker(
                Arc::clone(&self.factory),
                Arc::clone(&self.resources),
                control,
            )?);
        }
        if control.is_cancelled() {
            Self::stop_worker(&mut worker);
            return Err(if control.remaining().is_zero() {
                MeasurementFailure::DeadlineExceeded
            } else {
                MeasurementFailure::Cancelled
            });
        }
        // Do not consume one-use calibration audio until a bounded native
        // worker has actually been reserved for this run.
        let clip = match self.calibration.take(case) {
            Ok(clip) => clip,
            Err(error) => {
                Self::stop_worker(&mut worker);
                return Err(error);
            }
        };
        let request = EngineRequest {
            candidate_id: candidate.candidate_id().clone(),
            runtime: runtime.clone(),
            clip,
            cold_load,
            abort: Arc::clone(&abort),
            response: response_tx,
        };
        match worker
            .as_ref()
            .ok_or(MeasurementFailure::LoadFailed)?
            .requests
            .try_send(request)
        {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                Self::stop_worker(&mut worker);
                return Err(MeasurementFailure::LoadFailed);
            }
        }

        loop {
            let remaining = control.remaining();
            if control.is_cancelled() {
                abort.store(true, Ordering::Release);
                Self::stop_worker(&mut worker);
                return Err(if remaining.is_zero() {
                    MeasurementFailure::DeadlineExceeded
                } else {
                    MeasurementFailure::Cancelled
                });
            }
            match response_rx.recv_timeout(WORKER_POLL.min(remaining)) {
                Ok(Ok(measurement)) => {
                    if case.kind() == CalibrationKind::Silence {
                        Self::stop_worker(&mut worker);
                    }
                    return Ok(MeasuredSample {
                        load_latency: measurement.load_latency,
                        release_latency: measurement.release_latency,
                        inference_time: measurement.inference_time,
                        audio_duration: measurement.audio_duration,
                        peak_working_set_mib: measurement.peak_working_set_mib,
                        available_memory_mib: measurement.available_memory_mib,
                        fallback_count: measurement.fallback_count,
                        thermal_condition: measurement.thermal_condition,
                        contention_condition: measurement.contention_condition,
                        quality: score_transcript(&case.ephemeral_content(), &measurement.text),
                    });
                }
                Ok(Err(error)) => {
                    Self::stop_worker(&mut worker);
                    return Err(error);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Self::stop_worker(&mut worker);
                    return Err(MeasurementFailure::RecognitionFailed);
                }
            }
        }
    }
}

impl Drop for ProductionPerformanceAdapter {
    fn drop(&mut self) {
        if let Ok(worker) = self.worker.get_mut() {
            Self::stop_worker(worker);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NATIVE_WORKER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn id(value: &str) -> ContentFreeId {
        ContentFreeId::new(value).unwrap()
    }

    fn test_candidate(language: Language) -> InstalledBenchmarkCandidate {
        let variant = match language {
            Language::English => AccurateModelVariant::TinyEnglish,
            Language::PortugueseBrazil => AccurateModelVariant::TinyMultilingual,
        };
        let candidate = BenchmarkCandidate::new(
            id(&format!("test-cpu-{}", language.code())),
            EngineKind::Accurate,
            BackendKind::Cpu,
            ModelClass::Tiny,
            Sha256Digest::new(model_for_variant(variant).unwrap().sha256).unwrap(),
            [language],
            false,
        );
        let profile = ApplicationProfile::new(
            candidate.clone(),
            language,
            RecognitionApplication::Accurate {
                model: variant,
                backend: AccurateBackendPreference::Cpu,
                model_path: PathBuf::from("never-read-in-fake"),
            },
        )
        .unwrap();
        InstalledBenchmarkCandidate {
            candidate,
            context: BenchmarkContext {
                protocol_id: id(CalibrationCorpus::PROTOCOL_ID),
                build_id: id("build-test"),
                device_id: id("device-test"),
                driver_id: id("driver-test"),
            },
            profile,
            runtime: RuntimeCandidate::Accurate {
                model: PathBuf::from("never-read-in-fake"),
                expected_model_digest: Sha256Digest::new(
                    model_for_variant(variant).unwrap().sha256,
                )
                .unwrap(),
                backend: WhisperBackendPreference::Cpu,
                language,
            },
        }
    }

    fn inventory(language: Language) -> ProductionCandidateInventory {
        ProductionCandidateInventory {
            available: vec![test_candidate(language)],
            unavailable: Vec::new(),
        }
    }

    fn prepared_audio(language: Language) -> Arc<TransientCalibrationAudio> {
        let audio = Arc::new(TransientCalibrationAudio::pinned_v1(language));
        for (index, prompt) in audio.prompts().iter().enumerate() {
            let sample = if prompt.kind == CalibrationKind::Silence {
                0.0
            } else {
                0.1 + index as f32 / 100.0
            };
            audio
                .submit(
                    &prompt.case_id,
                    AudioClip::new(vec![sample; 3_200], CALIBRATION_SAMPLE_RATE).unwrap(),
                )
                .unwrap();
        }
        assert!(audio.is_ready());
        audio
    }

    #[derive(Default)]
    struct FakeResources;

    impl HostResourceProbe for FakeResources {
        fn snapshot(&self) -> Result<HostResources, MeasurementFailure> {
            Ok(HostResources {
                working_set_mib: 128,
                available_memory_mib: 8_192,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Idle,
            })
        }
    }

    struct FakeFactory {
        loads: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    impl RecognitionEngineFactory for FakeFactory {
        fn load(
            &self,
            _runtime: &RuntimeCandidate,
        ) -> Result<Box<dyn ResidentRecognitionEngine>, MeasurementFailure> {
            self.loads.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(FakeEngine {
                calls: Arc::clone(&self.calls),
            }))
        }
    }

    struct FakeEngine {
        calls: Arc<AtomicUsize>,
    }

    struct BlockingFactory {
        release: Arc<AtomicBool>,
    }

    impl RecognitionEngineFactory for BlockingFactory {
        fn load(
            &self,
            _runtime: &RuntimeCandidate,
        ) -> Result<Box<dyn ResidentRecognitionEngine>, MeasurementFailure> {
            Ok(Box::new(BlockingEngine {
                release: Arc::clone(&self.release),
            }))
        }
    }

    struct BlockingEngine {
        release: Arc<AtomicBool>,
    }

    impl ResidentRecognitionEngine for BlockingEngine {
        fn transcribe(
            &mut self,
            clip: &AudioClip,
            _abort: &Arc<AtomicBool>,
        ) -> Result<Transcript, MeasurementFailure> {
            while !self.release.load(Ordering::Acquire) {
                thread::yield_now();
            }
            Ok(Transcript {
                text: String::new(),
                backend: "fake",
                model_load_time: Duration::ZERO,
                inference_time: Duration::from_millis(1),
                audio_duration: clip.duration(),
            })
        }
    }

    impl ResidentRecognitionEngine for FakeEngine {
        fn transcribe(
            &mut self,
            clip: &AudioClip,
            _abort: &Arc<AtomicBool>,
        ) -> Result<Transcript, MeasurementFailure> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(Transcript {
                text: if clip.rms() == 0.0 {
                    String::new()
                } else {
                    "calibration output".to_owned()
                },
                backend: "fake",
                model_load_time: Duration::ZERO,
                inference_time: Duration::from_millis(2),
                audio_duration: clip.duration(),
            })
        }
    }

    fn control(duration: Duration) -> BenchmarkControl {
        BenchmarkControl::for_host_test(duration)
    }

    #[test]
    fn bilingual_protocol_has_three_speech_and_one_silence_prompt() {
        for language in [Language::English, Language::PortugueseBrazil] {
            let audio = TransientCalibrationAudio::pinned_v1(language);
            assert_eq!(audio.prompts().len(), 4);
            assert_eq!(
                audio
                    .prompts()
                    .iter()
                    .filter(|prompt| prompt.kind == CalibrationKind::Speech)
                    .count(),
                3
            );
            assert_eq!(
                audio
                    .prompts()
                    .iter()
                    .filter(|prompt| prompt.kind == CalibrationKind::Silence)
                    .count(),
                1
            );
            assert!(audio.prompts().iter().all(|prompt| {
                prompt.kind == CalibrationKind::Silence || !prompt.text().is_empty()
            }));
            assert!(!format!("{audio:?}").contains("release notes"));
            assert!(!format!("{:?}", audio.prompts()[0]).contains("Phorminx keeps"));
        }
    }

    #[test]
    fn calibration_is_bounded_exact_and_one_use() {
        let audio = TransientCalibrationAudio::pinned_v1(Language::English);
        let first = audio.prompts()[0].case_id.clone();
        assert!(
            audio
                .submit(
                    &first,
                    AudioClip::new(vec![0.1; 3_200], CALIBRATION_SAMPLE_RATE).unwrap()
                )
                .is_ok()
        );
        assert!(
            audio
                .submit(
                    &first,
                    AudioClip::new(vec![0.1; 3_200], CALIBRATION_SAMPLE_RATE).unwrap()
                )
                .is_err()
        );
        let unknown = id("unknown-case");
        assert!(
            audio
                .submit(
                    &unknown,
                    AudioClip::new(vec![0.1; 3_200], CALIBRATION_SAMPLE_RATE).unwrap()
                )
                .is_err()
        );
        let corpus = CalibrationCorpus::pinned_v1();
        let case = corpus
            .cases_for(Language::English)
            .find(|case| case.case_id() == &first)
            .unwrap();
        assert!(audio.take(case).is_ok());
        assert_eq!(
            audio.take(case).unwrap_err(),
            MeasurementFailure::CaptureFailed
        );
    }

    #[test]
    fn production_adapter_keeps_one_resident_engine_for_cold_and_warm_cases() {
        let _test_lock = NATIVE_WORKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while GLOBAL_NATIVE_BENCHMARK_WORKER.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let language = Language::English;
        let inventory = inventory(language);
        let loads = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = ProductionPerformanceAdapter::with_dependencies(
            &inventory,
            prepared_audio(language),
            Arc::new(FakeFactory {
                loads: Arc::clone(&loads),
                calls: Arc::clone(&calls),
            }),
            Arc::new(FakeResources),
        );
        let corpus = CalibrationCorpus::pinned_v1();
        for (index, case) in corpus.cases_for(language).enumerate() {
            let sample = adapter
                .measure(
                    inventory.available[0].candidate(),
                    case,
                    index == 0,
                    &control(Duration::from_secs(2)),
                )
                .unwrap();
            assert_eq!(sample.contention_condition, ContentionCondition::Idle);
            assert_eq!(sample.peak_working_set_mib, 0);
        }
        assert_eq!(loads.load(Ordering::Acquire), 1);
        assert_eq!(calls.load(Ordering::Acquire), 4);
        let deadline = Instant::now() + Duration::from_secs(1);
        while GLOBAL_NATIVE_BENCHMARK_WORKER.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn benchmark_and_dictation_race_has_exactly_one_winner() {
        for _ in 0..100 {
            let coordinator = Arc::new(WorkloadCoordinator::default());
            let start = Arc::new(Barrier::new(3));
            let finish = Arc::new(Barrier::new(3));
            let left = {
                let coordinator = Arc::clone(&coordinator);
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);
                thread::spawn(move || {
                    start.wait();
                    let lease = coordinator.try_begin_benchmark();
                    let won = lease.is_ok();
                    finish.wait();
                    drop(lease);
                    won
                })
            };
            let right = {
                let coordinator = Arc::clone(&coordinator);
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);
                thread::spawn(move || {
                    start.wait();
                    let lease = coordinator.try_begin(RuntimeActivityKind::Dictation);
                    let won = lease.is_ok();
                    finish.wait();
                    drop(lease);
                    won
                })
            };
            start.wait();
            finish.wait();
            assert_ne!(left.join().unwrap(), right.join().unwrap());
        }
    }

    #[test]
    fn preflight_distinguishes_dictation_compute_thermal_and_memory() {
        let coordinator = Arc::new(WorkloadCoordinator::default());
        let dictation = coordinator
            .try_begin(RuntimeActivityKind::Dictation)
            .unwrap();
        assert!(matches!(
            coordinator.try_begin_benchmark(),
            Err(BenchmarkPreflightFailure::DictationActive)
        ));
        drop(dictation);
        let whisper = coordinator.try_begin(RuntimeActivityKind::Whisper).unwrap();
        assert!(matches!(
            coordinator.try_begin_benchmark(),
            Err(BenchmarkPreflightFailure::ComputeContention)
        ));
        drop(whisper);
        let ollama = coordinator.try_begin(RuntimeActivityKind::Ollama).unwrap();
        assert!(matches!(
            coordinator.try_begin_benchmark(),
            Err(BenchmarkPreflightFailure::ComputeContention)
        ));
        drop(ollama);
        let benchmark = coordinator.try_begin_benchmark().unwrap();
        for kind in [
            RuntimeActivityKind::Dictation,
            RuntimeActivityKind::Whisper,
            RuntimeActivityKind::Ollama,
        ] {
            assert!(matches!(
                coordinator.try_begin(kind),
                Err(BenchmarkPreflightFailure::ComputeContention)
            ));
        }
        drop(benchmark);

        struct Resource(HostResources);
        impl HostResourceProbe for Resource {
            fn snapshot(&self) -> Result<HostResources, MeasurementFailure> {
                Ok(self.0)
            }
        }
        let candidate = &test_candidate(Language::English).candidate;
        let thermal = ProductionBenchmarkPreflight::new(
            Arc::clone(&coordinator),
            Arc::new(Resource(HostResources {
                working_set_mib: 1,
                available_memory_mib: 8_192,
                thermal_condition: ThermalCondition::Elevated,
                contention_condition: ContentionCondition::Idle,
            })),
        );
        assert!(matches!(
            thermal.try_acquire(candidate),
            Err(BenchmarkPreflightFailure::ThermalState)
        ));
        assert!(
            coordinator
                .try_begin(RuntimeActivityKind::Dictation)
                .is_ok()
        );

        let memory = ProductionBenchmarkPreflight::new(
            Arc::clone(&coordinator),
            Arc::new(Resource(HostResources {
                working_set_mib: 1,
                available_memory_mib: MIN_AVAILABLE_MEMORY_MIB - 1,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Idle,
            })),
        );
        assert!(matches!(
            memory.try_acquire(candidate),
            Err(BenchmarkPreflightFailure::SystemContention)
        ));

        let contention = ProductionBenchmarkPreflight::new(
            Arc::clone(&coordinator),
            Arc::new(Resource(HostResources {
                working_set_mib: 1,
                available_memory_mib: 8_192,
                thermal_condition: ThermalCondition::Nominal,
                contention_condition: ContentionCondition::Contended,
            })),
        );
        assert!(matches!(
            contention.try_acquire(candidate),
            Err(BenchmarkPreflightFailure::SystemContention)
        ));

        struct FailedResource;
        impl HostResourceProbe for FailedResource {
            fn snapshot(&self) -> Result<HostResources, MeasurementFailure> {
                Err(MeasurementFailure::ResourceProbeFailed)
            }
        }
        let unavailable =
            ProductionBenchmarkPreflight::new(Arc::clone(&coordinator), Arc::new(FailedResource));
        assert!(matches!(
            unavailable.try_acquire(candidate),
            Err(BenchmarkPreflightFailure::Unavailable)
        ));
        // Every failed preflight must release its atomic benchmark reservation.
        assert!(
            coordinator
                .try_begin(RuntimeActivityKind::Dictation)
                .is_ok()
        );
    }

    #[test]
    fn tree_identity_is_stable_and_detects_payload_changes() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for root in [left.path(), right.path()] {
            fs::create_dir(root.join("nested")).unwrap();
            fs::write(root.join("nested/model.bin"), b"model").unwrap();
            fs::write(root.join("runtime.dll"), b"runtime").unwrap();
        }
        // Receipt metadata is installation-specific and is deliberately not
        // part of the native/model byte identity.
        fs::write(left.path().join(".phorminx-receipt.json"), b"left").unwrap();
        fs::write(right.path().join(".phorminx-receipt.json"), b"right").unwrap();
        assert_eq!(
            hash_tree(left.path()).unwrap(),
            hash_tree(right.path()).unwrap()
        );
        fs::write(right.path().join("runtime.dll"), b"changed").unwrap();
        assert_ne!(
            hash_tree(left.path()).unwrap(),
            hash_tree(right.path()).unwrap()
        );
    }

    #[test]
    fn measurement_sampler_propagates_contention_without_process_peak_bias() {
        struct ContendedProbe {
            snapshots: AtomicUsize,
        }
        impl HostResourceProbe for ContendedProbe {
            fn snapshot(&self) -> Result<HostResources, MeasurementFailure> {
                let call = self.snapshots.fetch_add(1, Ordering::AcqRel);
                Ok(HostResources {
                    working_set_mib: if call == 0 { 100 } else { 175 },
                    available_memory_mib: 4_096,
                    thermal_condition: ThermalCondition::Nominal,
                    contention_condition: ContentionCondition::Idle,
                })
            }

            fn contention_condition(&self) -> Result<ContentionCondition, MeasurementFailure> {
                Ok(ContentionCondition::Contended)
            }
        }
        let mut sampler = ResourcePeakSampler::start(
            Arc::new(ContendedProbe {
                snapshots: AtomicUsize::new(0),
            }),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let sample = sampler.finish().unwrap();
        assert_eq!(sample.peak_working_set_mib, 75);
        assert_eq!(sample.available_memory_mib, 4_096);
        assert_eq!(sample.contention_condition, ContentionCondition::Contended);
    }

    #[test]
    fn vulkan_identity_binds_manifest_and_referenced_driver() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("icd.json");
        let driver = directory.path().join("driver.dll");
        fs::write(&driver, b"driver-one").unwrap();
        fs::write(
            &manifest,
            br#"{"file_format_version":"1.0.0","ICD":{"library_path":"driver.dll","api_version":"1.3.0"}}"#,
        )
        .unwrap();
        let first = hash_vulkan_driver(&manifest).unwrap();
        fs::write(&driver, b"driver-two").unwrap();
        let second = hash_vulkan_driver(&manifest).unwrap();
        assert_eq!(first.0, second.0);
        assert_ne!(first.1, second.1);
    }

    #[test]
    fn a_stalled_native_call_prevents_unbounded_replacement_workers() {
        let _test_lock = NATIVE_WORKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while GLOBAL_NATIVE_BENCHMARK_WORKER.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let language = Language::English;
        let inventory = inventory(language);
        let release = Arc::new(AtomicBool::new(false));
        let adapter = ProductionPerformanceAdapter::with_dependencies(
            &inventory,
            prepared_audio(language),
            Arc::new(BlockingFactory {
                release: Arc::clone(&release),
            }),
            Arc::new(FakeResources),
        );
        let corpus = CalibrationCorpus::pinned_v1();
        let case = corpus.cases_for(language).next().unwrap();
        assert_eq!(
            adapter
                .measure(
                    inventory.available[0].candidate(),
                    case,
                    true,
                    &control(Duration::from_millis(50)),
                )
                .unwrap_err(),
            MeasurementFailure::DeadlineExceeded
        );
        assert!(GLOBAL_NATIVE_BENCHMARK_WORKER.load(Ordering::Acquire));

        let replacement = ProductionPerformanceAdapter::with_dependencies(
            &inventory,
            prepared_audio(language),
            Arc::new(FakeFactory {
                loads: Arc::new(AtomicUsize::new(0)),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(FakeResources),
        );
        assert_eq!(
            replacement
                .measure(
                    inventory.available[0].candidate(),
                    case,
                    true,
                    &control(Duration::from_secs(1)),
                )
                .unwrap_err(),
            MeasurementFailure::LoadFailed
        );

        release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while GLOBAL_NATIVE_BENCHMARK_WORKER.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        // Failing to reserve the process-global worker happens before the
        // replacement adapter consumes its one-use calibration clip.
        assert!(
            replacement
                .measure(
                    inventory.available[0].candidate(),
                    case,
                    true,
                    &control(Duration::from_secs(1)),
                )
                .is_ok()
        );
    }

    #[test]
    fn empty_verified_inventory_is_truthful_and_deterministic() {
        struct FixedIdentity;
        impl IdentityProvider for FixedIdentity {
            fn context(
                &self,
                _include_vulkan: bool,
            ) -> Result<BenchmarkContext, PerformanceRuntimeError> {
                Ok(BenchmarkContext {
                    protocol_id: id(CalibrationCorpus::PROTOCOL_ID),
                    build_id: id("build-fixed"),
                    device_id: id("device-fixed"),
                    driver_id: id("driver-fixed"),
                })
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let settings = SettingsStore::new(directory.path().join("settings.toml")).unwrap();
        let catalog = PinnedCatalog::phorminx().unwrap();
        let discovered = discover_with(&settings, &catalog, &[], &FixedIdentity).unwrap();
        assert!(discovered.available.is_empty());
        assert!(discovered.unavailable.iter().any(|entry| {
            entry.engine == EngineKind::Accurate
                && entry.language == Language::PortugueseBrazil
                && entry.reason == CandidateUnavailableReason::ManagedInventory
        }));
        assert!(discovered.unavailable.iter().any(|entry| {
            entry.engine == EngineKind::Instant
                && entry.language == Language::English
                && entry.reason == CandidateUnavailableReason::RuntimeUnavailable
        }));
        assert!(discovered.context().is_none());
    }

    #[test]
    fn runtime_errors_and_candidates_do_not_debug_paths_or_calibration_text() {
        let rendered = format!("{:?}", PerformanceRuntimeError::HostIdentity);
        assert_eq!(rendered, "PerformanceRuntimeError::HostIdentity");
        let entry = test_candidate(Language::English);
        let rendered = format!("{entry:?}");
        assert!(!rendered.contains("never-read-in-fake"));
    }
}
