//! Trusted Ollama onboarding primitives.
//!
//! Model choices and upstream identities are compiled into Phorminx. Callers
//! cannot turn UI text into a registry name or a command line. The production
//! transport talks only to an IP-literal loopback Ollama API and never enables
//! Ollama's `insecure` registry option.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{CancellationToken, OllamaEndpoint};

const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROGRESS_LINE_BYTES: usize = 16 * 1024;
const MAX_PROGRESS_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROGRESS_EVENTS: usize = 10_000;
const DISK_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
static GLOBAL_PULL_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

fn global_pull_lock() -> Arc<Mutex<()>> {
    Arc::clone(GLOBAL_PULL_LOCK.get_or_init(|| Arc::new(Mutex::new(()))))
}

/// A stable key for one model Phorminx has reviewed for transcript cleanup.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CuratedModelId {
    Gemma3OneB,
    Llama32OneB,
    Qwen25ThreeBInstruct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CuratedLanguage {
    English,
    PortugueseBrazil,
}

/// Immutable acquisition identity and conservative resource disclosure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CuratedModel {
    id: CuratedModelId,
    display_name: &'static str,
    summary: &'static str,
    fully_qualified_name: &'static str,
    local_name: &'static str,
    manifest_sha256: &'static str,
    manifest_bytes: u64,
    minimum_free_disk_bytes: u64,
    recommended_system_ram_bytes: u64,
    minimum_ollama_version: OllamaVersion,
    languages: &'static [CuratedLanguage],
}

impl CuratedModel {
    #[must_use]
    pub const fn id(self) -> CuratedModelId {
        self.id
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        self.display_name
    }

    #[must_use]
    pub const fn summary(self) -> &'static str {
        self.summary
    }

    /// Exact source submitted to Ollama. This is never assembled from user input.
    #[must_use]
    pub const fn fully_qualified_name(self) -> &'static str {
        self.fully_qualified_name
    }

    #[must_use]
    pub const fn local_name(self) -> &'static str {
        self.local_name
    }

    /// Expected SHA-256 of the official registry manifest at catalog publication.
    #[must_use]
    pub const fn manifest_sha256(self) -> &'static str {
        self.manifest_sha256
    }

    /// Sum of the manifest's declared layer sizes at catalog publication.
    #[must_use]
    pub const fn manifest_bytes(self) -> u64 {
        self.manifest_bytes
    }

    /// Space disclosed before acquisition, including a conservative staging reserve.
    #[must_use]
    pub const fn minimum_free_disk_bytes(self) -> u64 {
        self.minimum_free_disk_bytes
    }

    /// Conservative recommendation, not a claim about exact runtime allocation.
    #[must_use]
    pub const fn recommended_system_ram_bytes(self) -> u64 {
        self.recommended_system_ram_bytes
    }

    #[must_use]
    pub const fn minimum_ollama_version(self) -> OllamaVersion {
        self.minimum_ollama_version
    }

    #[must_use]
    pub const fn languages(self) -> &'static [CuratedLanguage] {
        self.languages
    }
}

const EN_PT: &[CuratedLanguage] = &[CuratedLanguage::English, CuratedLanguage::PortugueseBrazil];

const CURATED_MODELS: [CuratedModel; 3] = [
    CuratedModel {
        id: CuratedModelId::Gemma3OneB,
        display_name: "Gemma 3 1B",
        summary: "Small multilingual cleanup model; the lightest recommended starting point.",
        fully_qualified_name: "registry.ollama.ai/library/gemma3:1b",
        local_name: "gemma3:1b",
        manifest_sha256: "8648f39daa8fbf5b18c7b4e6a8fb4990c692751d49917417b8842ca5758e7ffc",
        manifest_bytes: 815_319_299,
        minimum_free_disk_bytes: 815_319_299 + DISK_RESERVE_BYTES,
        recommended_system_ram_bytes: 4 * 1024 * 1024 * 1024,
        minimum_ollama_version: OllamaVersion::new(0, 6, 0),
        languages: EN_PT,
    },
    CuratedModel {
        id: CuratedModelId::Llama32OneB,
        display_name: "Llama 3.2 1B",
        summary: "Compact multilingual rewriting model with explicit Portuguese support.",
        fully_qualified_name: "registry.ollama.ai/library/llama3.2:1b",
        local_name: "llama3.2:1b",
        manifest_sha256: "baf6a787fdffd633537aa2eb51cfd54cb93ff08e28040095462bb63daf552878",
        manifest_bytes: 1_321_097_844,
        minimum_free_disk_bytes: 1_321_097_844 + DISK_RESERVE_BYTES,
        recommended_system_ram_bytes: 4 * 1024 * 1024 * 1024,
        minimum_ollama_version: OllamaVersion::new(0, 6, 0),
        languages: EN_PT,
    },
    CuratedModel {
        id: CuratedModelId::Qwen25ThreeBInstruct,
        display_name: "Qwen 2.5 3B Instruct",
        summary: "Larger instruction-following model for stronger structural cleanup.",
        fully_qualified_name: "registry.ollama.ai/library/qwen2.5:3b-instruct",
        local_name: "qwen2.5:3b-instruct",
        manifest_sha256: "357c53fb659c5076de1d65ccb0b397446227b71a42be9d1603d46168015c9e4b",
        manifest_bytes: 1_929_912_945,
        minimum_free_disk_bytes: 1_929_912_945 + DISK_RESERVE_BYTES,
        recommended_system_ram_bytes: 8 * 1024 * 1024 * 1024,
        minimum_ollama_version: OllamaVersion::new(0, 6, 0),
        languages: EN_PT,
    },
];

#[derive(Clone, Copy, Debug, Default)]
pub struct CuratedModelCatalog;

impl CuratedModelCatalog {
    #[must_use]
    pub const fn all(self) -> &'static [CuratedModel] {
        &CURATED_MODELS
    }

    #[must_use]
    pub fn get(self, id: CuratedModelId) -> CuratedModel {
        *CURATED_MODELS
            .iter()
            .find(|model| model.id == id)
            .expect("every curated model id is compiled into the catalog")
    }

    /// Bounded local search. The query never leaves the process.
    pub fn search(self, query: &str) -> Result<Vec<CuratedModel>, CatalogSearchError> {
        if query.len() > 128 || query.chars().any(char::is_control) {
            return Err(CatalogSearchError::InvalidQuery);
        }
        let terms = query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        Ok(CURATED_MODELS
            .iter()
            .copied()
            .filter(|model| {
                let haystack = format!(
                    "{} {} {}",
                    model.display_name, model.summary, model.local_name
                )
                .to_ascii_lowercase();
                terms.iter().all(|term| haystack.contains(term))
            })
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CatalogSearchError {
    #[error("the model search query is invalid")]
    InvalidQuery,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OllamaVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl OllamaVersion {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn parse(value: &str) -> Result<Self, VersionError> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let core = value.split_once(['-', '+']).map_or(value, |parts| parts.0);
        let mut parts = core.split('.');
        let major = parse_version_part(parts.next())?;
        let minor = parse_version_part(parts.next())?;
        let patch = parse_version_part(parts.next())?;
        if parts.next().is_some() {
            return Err(VersionError);
        }
        Ok(Self::new(major, minor, patch))
    }
}

impl fmt::Display for OllamaVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_part(value: Option<&str>) -> Result<u32, VersionError> {
    let value = value.ok_or(VersionError)?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(VersionError);
    }
    value.parse().map_err(|_| VersionError)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the Ollama version response is invalid")]
pub struct VersionError;

#[derive(Clone, Eq, PartialEq)]
pub struct InstalledModelIdentity {
    pub name: String,
    pub manifest_sha256: String,
    pub bytes: u64,
}

impl fmt::Debug for InstalledModelIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledModelIdentity")
            .field("name", &"<redacted>")
            .field("manifest_sha256", &"<redacted>")
            .field("bytes", &self.bytes)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonState {
    NotInstalled,
    InstalledButStopped,
    Incompatible {
        found: OllamaVersion,
        minimum: OllamaVersion,
    },
    Ready {
        version: OllamaVersion,
        models: Vec<InstalledModelIdentity>,
    },
    Unhealthy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CuratedModelState {
    Missing,
    Ready(InstalledModelIdentity),
    IdentityMismatch(InstalledModelIdentity),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelPullReview {
    model: CuratedModel,
    confirmation: String,
}

impl ModelPullReview {
    #[must_use]
    pub const fn model(&self) -> CuratedModel {
        self.model
    }

    #[must_use]
    pub fn confirmation(&self) -> &str {
        &self.confirmation
    }

    pub fn authorize(self, confirmation: &str) -> Result<AuthorizedModelPull, ConsentError> {
        if confirmation != self.confirmation {
            return Err(ConsentError);
        }
        Ok(AuthorizedModelPull { model: self.model })
    }
}

/// A non-cloneable capability created only by exact review confirmation.
#[derive(Debug)]
pub struct AuthorizedModelPull {
    model: CuratedModel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("model acquisition was not explicitly confirmed")]
pub struct ConsentError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelPullProgress {
    pub phase: PullPhase,
    pub completed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullPhase {
    ResolvingManifest,
    Downloading,
    Verifying,
    Activating,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelPullOutcome {
    pub model: CuratedModel,
    pub installed: InstalledModelIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullResidue {
    None,
    ResumableCacheMayRemain,
    ModelMayRemain,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("model acquisition failed ({kind:?}); residual state: {residue:?}")]
pub struct PullFailure {
    pub kind: PullFailureKind,
    pub residue: PullResidue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PullFailureKind {
    Busy,
    Cancelled,
    InstallationUntrusted,
    CapacityUnavailable,
    InsufficientDisk,
    DaemonUnavailable,
    DaemonIncompatible,
    ExistingIdentityMismatch,
    Transport,
    ProtocolViolation,
    PostPullIdentityMismatch,
    RollbackFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TransportError {
    #[error("the local Ollama service is unavailable")]
    Unavailable,
    #[error("the local Ollama request timed out")]
    TimedOut,
    #[error("the local Ollama service returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("the local Ollama response exceeded its bound")]
    ResponseTooLarge,
    #[error("the local Ollama response was malformed")]
    MalformedResponse,
    #[error("the local Ollama response violated the expected protocol")]
    ProtocolViolation,
    #[error("the operation was cancelled")]
    Cancelled,
}

pub trait OllamaOnboardingTransport: Send + Sync {
    fn version(&self, cancel: &CancellationToken) -> Result<OllamaVersion, TransportError>;
    fn installed_models(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<InstalledModelIdentity>, TransportError>;
    fn pull_model(
        &self,
        model: CuratedModel,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(ModelPullProgress),
    ) -> Result<(), TransportError>;
}

pub struct OllamaOnboarding<T> {
    transport: T,
    catalog: CuratedModelCatalog,
    pull_lock: Mutex<()>,
    process_pull_lock: Arc<Mutex<()>>,
}

impl<T: OllamaOnboardingTransport> OllamaOnboarding<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            catalog: CuratedModelCatalog,
            pull_lock: Mutex::new(()),
            process_pull_lock: global_pull_lock(),
        }
    }

    #[cfg(test)]
    fn new_with_pull_lock(transport: T, process_pull_lock: Arc<Mutex<()>>) -> Self {
        Self {
            transport,
            catalog: CuratedModelCatalog,
            pull_lock: Mutex::new(()),
            process_pull_lock,
        }
    }

    #[must_use]
    pub const fn catalog(&self) -> CuratedModelCatalog {
        self.catalog
    }

    pub fn probe(&self, executable_present: bool, cancel: &CancellationToken) -> DaemonState {
        if !executable_present {
            return DaemonState::NotInstalled;
        }
        let version = match self.transport.version(cancel) {
            Ok(version) => version,
            Err(TransportError::Unavailable | TransportError::TimedOut) => {
                return DaemonState::InstalledButStopped;
            }
            Err(_) => return DaemonState::Unhealthy,
        };
        let minimum = OllamaVersion::new(0, 6, 0);
        if version < minimum {
            return DaemonState::Incompatible {
                found: version,
                minimum,
            };
        }
        match self.transport.installed_models(cancel) {
            Ok(models) => DaemonState::Ready { version, models },
            Err(TransportError::Unavailable | TransportError::TimedOut) => {
                DaemonState::InstalledButStopped
            }
            Err(_) => DaemonState::Unhealthy,
        }
    }

    #[must_use]
    pub fn review_pull(&self, id: CuratedModelId) -> ModelPullReview {
        let model = self.catalog.get(id);
        ModelPullReview {
            model,
            confirmation: format!(
                "Download {} ({:.2} GiB disk required)",
                model.display_name,
                model.minimum_free_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
        }
    }

    #[must_use]
    pub fn model_state(
        &self,
        id: CuratedModelId,
        installed: &[InstalledModelIdentity],
    ) -> CuratedModelState {
        let model = self.catalog.get(id);
        match find_model(installed, model) {
            None => CuratedModelState::Missing,
            Some(found) if exact_identity(found, model) => CuratedModelState::Ready(found.clone()),
            Some(found) => CuratedModelState::IdentityMismatch(found.clone()),
        }
    }

    pub fn pull(
        &self,
        authorization: AuthorizedModelPull,
        available_disk_bytes: u64,
        cancel: &CancellationToken,
        mut progress: impl FnMut(ModelPullProgress),
    ) -> Result<ModelPullOutcome, PullFailure> {
        let _global_guard = self
            .process_pull_lock
            .try_lock()
            .map_err(|_| failure(PullFailureKind::Busy, PullResidue::None))?;
        let _guard = self
            .pull_lock
            .try_lock()
            .map_err(|_| failure(PullFailureKind::Busy, PullResidue::None))?;
        let model = authorization.model;
        if cancel.is_cancelled() {
            return Err(failure(PullFailureKind::Cancelled, PullResidue::None));
        }
        let version = self
            .transport
            .version(cancel)
            .map_err(|error| map_preflight_error(error, PullResidue::None))?;
        if version < model.minimum_ollama_version {
            return Err(failure(
                PullFailureKind::DaemonIncompatible,
                PullResidue::None,
            ));
        }
        let before = self
            .transport
            .installed_models(cancel)
            .map_err(|error| map_preflight_error(error, PullResidue::None))?;
        if let Some(installed) = find_model(&before, model) {
            if exact_identity(installed, model) {
                return Ok(ModelPullOutcome {
                    model,
                    installed: installed.clone(),
                });
            }
            return Err(failure(
                PullFailureKind::ExistingIdentityMismatch,
                PullResidue::None,
            ));
        }
        if available_disk_bytes < model.minimum_free_disk_bytes {
            return Err(failure(
                PullFailureKind::InsufficientDisk,
                PullResidue::None,
            ));
        }

        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.transport.pull_model(model, cancel, &mut progress)
        }));
        if let Err(error) = attempt.unwrap_or(Err(TransportError::ProtocolViolation)) {
            return self.recover_uncertain_pull(model, map_pull_error(error));
        }
        if cancel.is_cancelled() {
            return Err(failure(
                PullFailureKind::Cancelled,
                PullResidue::ModelMayRemain,
            ));
        }
        let after = match self.transport.installed_models(cancel) {
            Ok(models) => models,
            Err(_) => {
                return Err(failure(
                    PullFailureKind::PostPullIdentityMismatch,
                    PullResidue::ModelMayRemain,
                ));
            }
        };
        let Some(installed) = find_model(&after, model).cloned() else {
            return Err(failure(
                PullFailureKind::PostPullIdentityMismatch,
                PullResidue::ResumableCacheMayRemain,
            ));
        };
        if !exact_identity(&installed, model) {
            return Err(failure(
                PullFailureKind::PostPullIdentityMismatch,
                PullResidue::ModelMayRemain,
            ));
        }
        Ok(ModelPullOutcome { model, installed })
    }

    fn recover_uncertain_pull(
        &self,
        model: CuratedModel,
        original: PullFailure,
    ) -> Result<ModelPullOutcome, PullFailure> {
        // The connection may have failed after Ollama committed the manifest.
        // Re-read with a fresh token before claiming that only cache residue exists.
        let cleanup = CancellationToken::new();
        let models = match self.transport.installed_models(&cleanup) {
            Ok(models) => models,
            Err(_) => {
                return Err(failure(
                    PullFailureKind::RollbackFailed,
                    PullResidue::ModelMayRemain,
                ));
            }
        };
        if find_model(&models, model).is_some() {
            // The local API provides no transaction/ownership receipt. Another
            // process could have installed this name concurrently, so deleting
            // it would be an unsafe rollback. Report the residue for explicit
            // user reconciliation instead.
            Err(failure(original.kind, PullResidue::ModelMayRemain))
        } else {
            Err(original)
        }
    }
}

fn find_model(
    models: &[InstalledModelIdentity],
    model: CuratedModel,
) -> Option<&InstalledModelIdentity> {
    models.iter().find(|installed| {
        installed.name == model.local_name || installed.name == model.fully_qualified_name
    })
}

fn exact_identity(installed: &InstalledModelIdentity, model: CuratedModel) -> bool {
    installed
        .manifest_sha256
        .eq_ignore_ascii_case(model.manifest_sha256)
        && installed.bytes == model.manifest_bytes
}

fn failure(kind: PullFailureKind, residue: PullResidue) -> PullFailure {
    PullFailure { kind, residue }
}

fn map_preflight_error(error: TransportError, residue: PullResidue) -> PullFailure {
    let kind = match error {
        TransportError::Cancelled => PullFailureKind::Cancelled,
        TransportError::Unavailable | TransportError::TimedOut => {
            PullFailureKind::DaemonUnavailable
        }
        _ => PullFailureKind::Transport,
    };
    failure(kind, residue)
}

fn map_pull_error(error: TransportError) -> PullFailure {
    match error {
        TransportError::Cancelled => failure(
            PullFailureKind::Cancelled,
            PullResidue::ResumableCacheMayRemain,
        ),
        TransportError::ProtocolViolation
        | TransportError::MalformedResponse
        | TransportError::ResponseTooLarge => failure(
            PullFailureKind::ProtocolViolation,
            PullResidue::ResumableCacheMayRemain,
        ),
        _ => failure(
            PullFailureKind::Transport,
            PullResidue::ResumableCacheMayRemain,
        ),
    }
}

/// Production typed client for the fixed loopback Ollama API.
#[derive(Clone, Debug)]
pub struct UreqOnboardingTransport {
    endpoint: OllamaEndpoint,
    short_agent: ureq::Agent,
    pull_agent: ureq::Agent,
}

impl UreqOnboardingTransport {
    #[must_use]
    pub fn loopback() -> Self {
        Self::new(OllamaEndpoint::default())
    }

    #[must_use]
    pub fn new(endpoint: OllamaEndpoint) -> Self {
        Self {
            endpoint,
            short_agent: build_agent(Duration::from_secs(2), Duration::from_secs(5)),
            // Reads wake at least every three seconds, bounding cancellation
            // observation even while a daemon stops sending progress.
            pull_agent: build_agent(Duration::from_secs(3), Duration::from_secs(6 * 60 * 60)),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint.as_str())
    }

    fn read_json(&self, path: &str, cancel: &CancellationToken) -> Result<Vec<u8>, TransportError> {
        check_cancel(cancel)?;
        let response = self
            .short_agent
            .get(&self.url(path))
            .call()
            .map_err(map_ureq_error)?;
        read_bounded(response, MAX_JSON_BYTES, cancel)
    }
}

impl Default for UreqOnboardingTransport {
    fn default() -> Self {
        Self::loopback()
    }
}

impl OllamaOnboardingTransport for UreqOnboardingTransport {
    fn version(&self, cancel: &CancellationToken) -> Result<OllamaVersion, TransportError> {
        #[derive(Deserialize)]
        struct Response {
            version: String,
        }
        let body = self.read_json("/api/version", cancel)?;
        let response: Response =
            serde_json::from_slice(&body).map_err(|_| TransportError::MalformedResponse)?;
        if response.version.len() > 64 || response.version.chars().any(char::is_control) {
            return Err(TransportError::ProtocolViolation);
        }
        OllamaVersion::parse(&response.version).map_err(|_| TransportError::ProtocolViolation)
    }

    fn installed_models(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<InstalledModelIdentity>, TransportError> {
        let body = self.read_json("/api/tags", cancel)?;
        parse_installed_models(&body)
    }

    fn pull_model(
        &self,
        model: CuratedModel,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(ModelPullProgress),
    ) -> Result<(), TransportError> {
        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            insecure: bool,
            stream: bool,
        }
        check_cancel(cancel)?;
        let request = serde_json::to_vec(&Request {
            model: model.fully_qualified_name,
            insecure: false,
            stream: true,
        })
        .map_err(|_| TransportError::ProtocolViolation)?;
        let response = self
            .pull_agent
            .post(&self.url("/api/pull"))
            .header("Content-Type", "application/json")
            .send(request)
            .map_err(map_ureq_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(TransportError::HttpStatus(status));
        }
        let mut reader = BufReader::new(response.into_parts().1.into_reader());
        read_pull_events(&mut reader, model, cancel, progress)
    }
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    digest: String,
    size: u64,
}

fn parse_installed_models(body: &[u8]) -> Result<Vec<InstalledModelIdentity>, TransportError> {
    let response: TagsResponse =
        serde_json::from_slice(body).map_err(|_| TransportError::MalformedResponse)?;
    if response.models.len() > 4096 {
        return Err(TransportError::ProtocolViolation);
    }
    let mut models = Vec::with_capacity(response.models.len());
    for model in response.models {
        if !valid_model_name(&model.name) || !valid_digest(&model.digest) {
            return Err(TransportError::ProtocolViolation);
        }
        models.push(InstalledModelIdentity {
            name: model.name,
            manifest_sha256: model.digest.to_ascii_lowercase(),
            bytes: model.size,
        });
    }
    models.sort_by(|left, right| left.name.cmp(&right.name));
    if models.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(TransportError::ProtocolViolation);
    }
    Ok(models)
}

#[derive(Deserialize)]
struct PullEvent {
    status: String,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    completed: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

fn read_pull_events(
    reader: &mut impl BufRead,
    model: CuratedModel,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ModelPullProgress),
) -> Result<(), TransportError> {
    let mut line = Vec::with_capacity(1024);
    let mut event_count = 0_usize;
    let mut response_bytes = 0_usize;
    let mut succeeded = false;
    loop {
        check_cancel(cancel)?;
        line.clear();
        let read = read_bounded_line(reader, &mut line, MAX_PROGRESS_LINE_BYTES)?;
        if read == 0 {
            break;
        }
        if succeeded {
            // Success is a terminal state, not merely a progress hint.
            return Err(TransportError::ProtocolViolation);
        }
        event_count = event_count.saturating_add(1);
        if event_count > MAX_PROGRESS_EVENTS {
            return Err(TransportError::ResponseTooLarge);
        }
        response_bytes = response_bytes
            .checked_add(read)
            .filter(|total| *total <= MAX_PROGRESS_BYTES)
            .ok_or(TransportError::ResponseTooLarge)?;
        let event: PullEvent =
            serde_json::from_slice(&line).map_err(|_| TransportError::MalformedResponse)?;
        if event.status.is_empty()
            || event.status.len() > 256
            || event.status.chars().any(char::is_control)
            || event.error.is_some()
            || event
                .digest
                .as_deref()
                .is_some_and(|digest| !valid_prefixed_digest(digest))
            || matches!((event.completed, event.total), (Some(done), Some(total)) if done > total)
            || event
                .total
                .is_some_and(|total| total > model.manifest_bytes)
        {
            return Err(TransportError::ProtocolViolation);
        }
        let phase = classify_phase(&event.status)?;
        progress(ModelPullProgress {
            phase,
            completed_bytes: event.completed,
            total_bytes: event.total,
        });
        succeeded = event.status == "success";
    }
    check_cancel(cancel)?;
    if !succeeded {
        return Err(TransportError::ProtocolViolation);
    }
    Ok(())
}

/// Reads a single JSONL record without ever growing `output` past `maximum`.
fn read_bounded_line(
    reader: &mut impl BufRead,
    output: &mut Vec<u8>,
    maximum: usize,
) -> Result<usize, TransportError> {
    let mut total = 0_usize;
    loop {
        let available = reader.fill_buf().map_err(map_io_error)?;
        if available.is_empty() {
            return Ok(total);
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if total.saturating_add(take) > maximum {
            return Err(TransportError::ResponseTooLarge);
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        total += take;
        if output.last() == Some(&b'\n') {
            return Ok(total);
        }
    }
}

fn build_agent(read_timeout: Duration, global_timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_recv_response(Some(read_timeout))
        .timeout_recv_body(Some(read_timeout))
        .timeout_global(Some(global_timeout))
        .build()
        .into()
}

fn read_bounded(
    response: ureq::http::Response<ureq::Body>,
    maximum: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, TransportError> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(TransportError::HttpStatus(status));
    }
    let mut reader = response.into_parts().1.into_reader();
    let mut body = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        check_cancel(cancel)?;
        let read = reader.read(&mut buffer).map_err(map_io_error)?;
        if read == 0 {
            break;
        }
        if body.len().saturating_add(read) > maximum {
            return Err(TransportError::ResponseTooLarge);
        }
        body.extend_from_slice(&buffer[..read]);
    }
    check_cancel(cancel)?;
    Ok(body)
}

fn map_ureq_error(error: ureq::Error) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::TimedOut,
        _ => TransportError::Unavailable,
    }
}

fn map_io_error(error: std::io::Error) -> TransportError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => TransportError::TimedOut,
        _ => TransportError::Unavailable,
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), TransportError> {
    if cancel.is_cancelled() {
        Err(TransportError::Cancelled)
    } else {
        Ok(())
    }
}

fn valid_model_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.is_ascii()
        && !value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b':' | b'_' | b'-')
        })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_prefixed_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(valid_digest)
}

fn classify_phase(status: &str) -> Result<PullPhase, TransportError> {
    if status == "success" || status.contains("writing manifest") {
        Ok(PullPhase::Activating)
    } else if status.contains("verifying") {
        Ok(PullPhase::Verifying)
    } else if status.contains("manifest") {
        Ok(PullPhase::ResolvingManifest)
    } else if status.contains("pulling") || status.contains("download") {
        Ok(PullPhase::Downloading)
    } else if status.contains("removing any unused layers") {
        Ok(PullPhase::Activating)
    } else {
        Err(TransportError::ProtocolViolation)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use super::*;

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        path: String,
        body: Vec<u8>,
    }

    struct FakeHttpResponse {
        status: u16,
        body: Vec<u8>,
        delay: Duration,
    }

    struct FakeHttpServer {
        endpoint: OllamaEndpoint,
        wake_address: std::net::SocketAddr,
        requests: Receiver<CapturedRequest>,
        worker: Option<JoinHandle<()>>,
    }

    impl FakeHttpServer {
        fn start(responses: Vec<FakeHttpResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let wake_address = listener.local_addr().unwrap();
            let endpoint = OllamaEndpoint::ipv4(wake_address.port());
            let (sender, requests) = mpsc::channel();
            let worker = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    sender.send(read_http_request(&mut stream)).unwrap();
                    if !response.delay.is_zero() {
                        thread::sleep(response.delay);
                    }
                    let reason = if response.status == 200 {
                        "OK"
                    } else {
                        "Redirect or error"
                    };
                    let header = format!(
                        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.status,
                        reason,
                        response.body.len()
                    );
                    if stream.write_all(header.as_bytes()).is_ok() {
                        let _ = stream.write_all(&response.body);
                    }
                }
            });
            Self {
                endpoint,
                wake_address,
                requests,
                worker: Some(worker),
            }
        }

        fn request(&self) -> CapturedRequest {
            self.requests.recv_timeout(Duration::from_secs(2)).unwrap()
        }
    }

    impl Drop for FakeHttpServer {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
                if !worker.is_finished()
                    && let Ok(mut wake) = TcpStream::connect(self.wake_address)
                {
                    let _ = wake.write_all(
                        b"GET /test-shutdown HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                    );
                }
                worker.join().unwrap();
            }
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            data.extend_from_slice(&buffer[..read]);
            if let Some(position) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let mut request_line = headers.lines().next().unwrap().split_whitespace();
        let method = request_line.next().unwrap().to_owned();
        let path = request_line.next().unwrap().to_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while data.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            data.extend_from_slice(&buffer[..read]);
        }
        CapturedRequest {
            method,
            path,
            body: data[header_end..header_end + content_length].to_vec(),
        }
    }

    struct FakeTransport {
        version: Result<OllamaVersion, TransportError>,
        snapshots: Mutex<VecDeque<Result<Vec<InstalledModelIdentity>, TransportError>>>,
        pull_result: Result<(), TransportError>,
        pulled_names: Mutex<Vec<&'static str>>,
        cancel_during_pull: bool,
    }

    impl FakeTransport {
        fn new(snapshots: Vec<Vec<InstalledModelIdentity>>) -> Self {
            Self {
                version: Ok(OllamaVersion::new(0, 33, 2)),
                snapshots: Mutex::new(snapshots.into_iter().map(Ok).collect()),
                pull_result: Ok(()),
                pulled_names: Mutex::new(Vec::new()),
                cancel_during_pull: false,
            }
        }
    }

    impl OllamaOnboardingTransport for FakeTransport {
        fn version(&self, _cancel: &CancellationToken) -> Result<OllamaVersion, TransportError> {
            self.version
        }

        fn installed_models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<InstalledModelIdentity>, TransportError> {
            self.snapshots
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        fn pull_model(
            &self,
            model: CuratedModel,
            cancel: &CancellationToken,
            progress: &mut dyn FnMut(ModelPullProgress),
        ) -> Result<(), TransportError> {
            self.pulled_names
                .lock()
                .unwrap()
                .push(model.fully_qualified_name);
            progress(ModelPullProgress {
                phase: PullPhase::Downloading,
                completed_bytes: Some(1),
                total_bytes: Some(model.manifest_bytes),
            });
            if self.cancel_during_pull {
                cancel.cancel();
            }
            self.pull_result
        }
    }

    fn installed(model: CuratedModel) -> InstalledModelIdentity {
        InstalledModelIdentity {
            name: model.local_name.to_owned(),
            manifest_sha256: model.manifest_sha256.to_owned(),
            bytes: model.manifest_bytes,
        }
    }

    fn authorize<T: OllamaOnboardingTransport>(
        service: &OllamaOnboarding<T>,
        id: CuratedModelId,
    ) -> AuthorizedModelPull {
        let review = service.review_pull(id);
        let phrase = review.confirmation().to_owned();
        review.authorize(&phrase).unwrap()
    }

    fn isolated_service<T: OllamaOnboardingTransport>(transport: T) -> OllamaOnboarding<T> {
        OllamaOnboarding::new_with_pull_lock(transport, Arc::new(Mutex::new(())))
    }

    #[test]
    fn catalog_search_is_bounded_and_local() {
        let catalog = CuratedModelCatalog;
        assert_eq!(catalog.search("portuguese compact").unwrap().len(), 1);
        assert_eq!(catalog.search("").unwrap().len(), 3);
        assert!(catalog.search(&"x".repeat(129)).is_err());
        assert!(catalog.search("bad\nquery").is_err());
        for model in catalog.all() {
            assert!(valid_digest(model.manifest_sha256));
            assert!(model.minimum_free_disk_bytes > model.manifest_bytes);
        }
    }

    #[test]
    fn version_parser_is_strict_but_accepts_official_prefix_and_suffix() {
        assert_eq!(
            OllamaVersion::parse("v0.33.2-rc1").unwrap(),
            OllamaVersion::new(0, 33, 2)
        );
        for bad in ["", "0.6", "0.6.0.1", "0.x.0", "-0.6.0"] {
            assert!(OllamaVersion::parse(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn exact_confirmation_is_required_and_authorization_is_not_cloneable() {
        let service = isolated_service(FakeTransport::new(vec![]));
        let review = service.review_pull(CuratedModelId::Gemma3OneB);
        assert!(review.clone().authorize("yes").is_err());
        assert!(review.confirmation().contains("disk required"));
    }

    #[test]
    fn concurrent_pull_is_rejected_without_waiting_or_mutating() {
        let service = isolated_service(FakeTransport::new(vec![]));
        let guard = service.pull_lock.lock().unwrap();
        let error = service
            .pull(
                authorize(&service, CuratedModelId::Gemma3OneB),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::Busy);
        assert_eq!(error.residue, PullResidue::None);
        assert!(service.transport.pulled_names.lock().unwrap().is_empty());
        drop(guard);
    }

    #[test]
    fn pull_is_serialized_across_service_instances() {
        let process_lock = Arc::new(Mutex::new(()));
        let global = process_lock.lock().unwrap();
        let service = OllamaOnboarding::new_with_pull_lock(
            FakeTransport::new(vec![]),
            Arc::clone(&process_lock),
        );
        let error = service
            .pull(
                authorize(&service, CuratedModelId::Gemma3OneB),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::Busy);
        assert_eq!(error.residue, PullResidue::None);
        drop(global);
    }

    #[test]
    fn missing_capacity_stops_before_model_acquisition() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let service = isolated_service(FakeTransport::new(vec![Vec::new()]));
        let error = service
            .pull(
                authorize(&service, model.id),
                model.minimum_free_disk_bytes - 1,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::InsufficientDisk);
        assert_eq!(error.residue, PullResidue::None);
        assert!(service.transport.pulled_names.lock().unwrap().is_empty());
    }

    #[test]
    fn probe_distinguishes_missing_stopped_incompatible_and_model_missing() {
        let service = isolated_service(FakeTransport::new(vec![Vec::new()]));
        assert_eq!(
            service.probe(false, &CancellationToken::new()),
            DaemonState::NotInstalled
        );
        assert!(matches!(
            service.probe(true, &CancellationToken::new()),
            DaemonState::Ready { models, .. } if models.is_empty()
        ));

        let mut stopped = FakeTransport::new(vec![]);
        stopped.version = Err(TransportError::Unavailable);
        assert_eq!(
            isolated_service(stopped).probe(true, &CancellationToken::new()),
            DaemonState::InstalledButStopped
        );

        let mut old = FakeTransport::new(vec![]);
        old.version = Ok(OllamaVersion::new(0, 5, 9));
        assert!(matches!(
            isolated_service(old).probe(true, &CancellationToken::new()),
            DaemonState::Incompatible { .. }
        ));
    }

    #[test]
    fn model_state_distinguishes_missing_ready_and_wrong_identity() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let service = isolated_service(FakeTransport::new(vec![]));
        assert_eq!(
            service.model_state(model.id, &[]),
            CuratedModelState::Missing
        );
        assert!(matches!(
            service.model_state(model.id, &[installed(model)]),
            CuratedModelState::Ready(_)
        ));
        let mut wrong = installed(model);
        wrong.bytes += 1;
        assert!(matches!(
            service.model_state(model.id, &[wrong]),
            CuratedModelState::IdentityMismatch(_)
        ));
    }

    #[test]
    fn pull_uses_only_compiled_fully_qualified_source_and_verifies_readback() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let fake = FakeTransport::new(vec![Vec::new(), vec![installed(model)]]);
        let service = isolated_service(fake);
        let action = authorize(&service, model.id);
        let mut events = Vec::new();
        let outcome = service
            .pull(action, u64::MAX, &CancellationToken::new(), |event| {
                events.push(event)
            })
            .unwrap();
        assert_eq!(outcome.installed.manifest_sha256, model.manifest_sha256);
        assert_eq!(
            service.transport.pulled_names.lock().unwrap().as_slice(),
            ["registry.ollama.ai/library/gemma3:1b"]
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn existing_wrong_digest_is_never_overwritten_or_deleted() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let mut wrong = installed(model);
        wrong.manifest_sha256 = "a".repeat(64);
        let service = isolated_service(FakeTransport::new(vec![vec![wrong]]));
        let result = service.pull(
            authorize(&service, model.id),
            u64::MAX,
            &CancellationToken::new(),
            |_| {},
        );
        assert!(matches!(
            result,
            Err(PullFailure {
                kind: PullFailureKind::ExistingIdentityMismatch,
                residue: PullResidue::None
            })
        ));
        assert!(service.transport.pulled_names.lock().unwrap().is_empty());
    }

    #[test]
    fn post_pull_wrong_digest_is_not_deleted_without_ownership_authority() {
        let model = CuratedModelCatalog.get(CuratedModelId::Llama32OneB);
        let mut wrong = installed(model);
        wrong.manifest_sha256 = "b".repeat(64);
        let fake = FakeTransport::new(vec![Vec::new(), vec![wrong], Vec::new()]);
        let service = isolated_service(fake);
        let result = service.pull(
            authorize(&service, model.id),
            u64::MAX,
            &CancellationToken::new(),
            |_| {},
        );
        assert_eq!(
            result.unwrap_err(),
            failure(
                PullFailureKind::PostPullIdentityMismatch,
                PullResidue::ModelMayRemain
            )
        );
    }

    #[test]
    fn uncertain_transport_failure_reconciles_before_reporting_residue() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let mut fake = FakeTransport::new(vec![Vec::new(), vec![installed(model)], Vec::new()]);
        fake.pull_result = Err(TransportError::TimedOut);
        let service = isolated_service(fake);
        let error = service
            .pull(
                authorize(&service, model.id),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::Transport);
        assert_eq!(error.residue, PullResidue::ModelMayRemain);
    }

    #[test]
    fn failed_uncertain_readback_never_claims_only_cache_residue() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let fake = FakeTransport {
            version: Ok(OllamaVersion::new(0, 33, 2)),
            snapshots: Mutex::new(VecDeque::from([
                Ok(Vec::new()),
                Err(TransportError::Unavailable),
            ])),
            pull_result: Err(TransportError::TimedOut),
            pulled_names: Mutex::new(Vec::new()),
            cancel_during_pull: false,
        };
        let service = isolated_service(fake);
        let error = service
            .pull(
                authorize(&service, model.id),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::RollbackFailed);
        assert_eq!(error.residue, PullResidue::ModelMayRemain);
    }

    #[test]
    fn panicking_progress_consumer_is_contained_and_reconciled() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let service = isolated_service(FakeTransport::new(vec![Vec::new(), Vec::new()]));
        let result = service.pull(
            authorize(&service, model.id),
            u64::MAX,
            &CancellationToken::new(),
            |_| panic!("adversarial progress callback"),
        );
        assert!(matches!(
            result,
            Err(PullFailure {
                kind: PullFailureKind::ProtocolViolation,
                residue: PullResidue::ResumableCacheMayRemain
            })
        ));
    }

    #[test]
    fn cancellation_before_pull_has_no_residue_and_mid_pull_is_truthful() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let service = isolated_service(FakeTransport::new(vec![]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            service
                .pull(authorize(&service, model.id), u64::MAX, &cancel, |_| {})
                .unwrap_err()
                .residue,
            PullResidue::None
        );

        let mut fake = FakeTransport::new(vec![Vec::new(), Vec::new()]);
        fake.cancel_during_pull = true;
        let service = isolated_service(fake);
        let error = service
            .pull(
                authorize(&service, model.id),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::Cancelled);
        assert_eq!(error.residue, PullResidue::ModelMayRemain);
    }

    #[test]
    fn protocol_validators_reject_hostile_names_digests_and_statuses() {
        assert!(valid_model_name("registry.ollama.ai/library/gemma3:1b"));
        for bad in ["", "../evil", "model @host", "model\nname"] {
            assert!(!valid_model_name(bad), "accepted {bad:?}");
        }
        assert!(valid_digest(&"a".repeat(64)));
        assert!(!valid_digest(&"a".repeat(63)));
        assert!(classify_phase("executing arbitrary hook").is_err());
    }

    #[test]
    fn malformed_and_hostile_tag_manifests_fail_closed() {
        assert_eq!(
            parse_installed_models(b"not json"),
            Err(TransportError::MalformedResponse)
        );
        for body in [
            format!(
                "{{\"models\":[{{\"name\":\"../evil\",\"digest\":\"{}\",\"size\":1}}]}}",
                "a".repeat(64)
            ),
            "{\"models\":[{\"name\":\"gemma3:1b\",\"digest\":\"short\",\"size\":1}]}".to_owned(),
            format!(
                "{{\"models\":[{{\"name\":\"gemma3:1b\",\"digest\":\"{0}\",\"size\":1}},{{\"name\":\"gemma3:1b\",\"digest\":\"{0}\",\"size\":1}}]}}",
                "a".repeat(64)
            ),
        ] {
            assert_eq!(
                parse_installed_models(body.as_bytes()),
                Err(TransportError::ProtocolViolation)
            );
        }
    }

    #[test]
    fn pull_line_reader_never_allocates_past_its_bound() {
        let hostile = vec![b'x'; MAX_PROGRESS_LINE_BYTES + 100_000];
        let mut reader = BufReader::with_capacity(64 * 1024, hostile.as_slice());
        let mut output = Vec::new();
        assert_eq!(
            read_bounded_line(&mut reader, &mut output, MAX_PROGRESS_LINE_BYTES),
            Err(TransportError::ResponseTooLarge)
        );
        assert!(output.len() <= MAX_PROGRESS_LINE_BYTES);
    }

    #[test]
    fn success_must_be_present_and_terminal() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let cancel = CancellationToken::new();
        let valid = b"{\"status\":\"pulling manifest\"}\n{\"status\":\"success\"}\n";
        assert!(
            read_pull_events(
                &mut BufReader::new(valid.as_slice()),
                model,
                &cancel,
                &mut |_| {}
            )
            .is_ok()
        );

        let missing = b"{\"status\":\"pulling manifest\"}\n";
        assert_eq!(
            read_pull_events(
                &mut BufReader::new(missing.as_slice()),
                model,
                &cancel,
                &mut |_| {}
            ),
            Err(TransportError::ProtocolViolation)
        );

        let trailing = b"{\"status\":\"success\"}\n{\"status\":\"pulling manifest\"}\n";
        assert_eq!(
            read_pull_events(
                &mut BufReader::new(trailing.as_slice()),
                model,
                &cancel,
                &mut |_| {}
            ),
            Err(TransportError::ProtocolViolation)
        );
    }

    #[test]
    fn post_pull_size_must_equal_the_pinned_manifest_total() {
        let model = CuratedModelCatalog.get(CuratedModelId::Gemma3OneB);
        let mut oversized = installed(model);
        oversized.bytes += 1;
        let fake = FakeTransport::new(vec![Vec::new(), vec![oversized], Vec::new()]);
        let service = isolated_service(fake);
        let error = service
            .pull(
                authorize(&service, model.id),
                u64::MAX,
                &CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, PullFailureKind::PostPullIdentityMismatch);
        assert_eq!(error.residue, PullResidue::ModelMayRemain);
    }

    #[test]
    fn errors_are_content_free() {
        let rendered = format!(
            "{:?} {}",
            TransportError::Unavailable,
            failure(
                PullFailureKind::Transport,
                PullResidue::ResumableCacheMayRemain,
            )
        );
        assert!(!rendered.contains("Users"));
        assert!(!rendered.contains("prompt"));
        assert!(!rendered.contains("transcript"));
    }

    #[test]
    fn production_transport_rejects_redirects_and_bounds_stalls() {
        let redirect = FakeHttpServer::start(vec![FakeHttpResponse {
            status: 302,
            body: Vec::new(),
            delay: Duration::ZERO,
        }]);
        let transport = UreqOnboardingTransport {
            endpoint: redirect.endpoint.clone(),
            short_agent: build_agent(Duration::from_millis(50), Duration::from_millis(100)),
            pull_agent: build_agent(Duration::from_millis(50), Duration::from_millis(100)),
        };
        assert_eq!(
            transport.version(&CancellationToken::new()),
            Err(TransportError::HttpStatus(302))
        );
        assert_eq!(redirect.request().path, "/api/version");

        let stalled = FakeHttpServer::start(vec![FakeHttpResponse {
            status: 200,
            body: br#"{"version":"0.33.2"}"#.to_vec(),
            delay: Duration::from_millis(200),
        }]);
        let transport = UreqOnboardingTransport {
            endpoint: stalled.endpoint.clone(),
            short_agent: build_agent(Duration::from_millis(30), Duration::from_millis(60)),
            pull_agent: build_agent(Duration::from_millis(30), Duration::from_millis(60)),
        };
        assert!(matches!(
            transport.version(&CancellationToken::new()),
            Err(TransportError::TimedOut | TransportError::Unavailable)
        ));
    }

    #[test]
    fn production_pull_body_is_typed_pinned_and_never_insecure() {
        let server = FakeHttpServer::start(vec![FakeHttpResponse {
            status: 200,
            body: b"{\"status\":\"pulling manifest\"}\n{\"status\":\"success\"}\n".to_vec(),
            delay: Duration::ZERO,
        }]);
        let transport = UreqOnboardingTransport {
            endpoint: server.endpoint.clone(),
            short_agent: build_agent(Duration::from_secs(1), Duration::from_secs(1)),
            pull_agent: build_agent(Duration::from_secs(1), Duration::from_secs(1)),
        };
        let model = CuratedModelCatalog.get(CuratedModelId::Qwen25ThreeBInstruct);
        let mut progress = Vec::new();
        transport
            .pull_model(model, &CancellationToken::new(), &mut |event| {
                progress.push(event)
            })
            .unwrap();
        let request = server.request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/pull");
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], model.fully_qualified_name);
        assert_eq!(body["insecure"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(progress.last().unwrap().phase, PullPhase::Activating);
    }
}
