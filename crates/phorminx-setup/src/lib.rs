//! Pure setup, repair, and recommendation domain for Phorminx.
//!
//! This crate deliberately performs no I/O.  It describes observed local
//! capabilities, deterministic plans, consent and action lifecycles, managed
//! asset ownership, crash-recovery journals, and content-free benchmark
//! recommendations.  Desktop adapters remain responsible for every network,
//! filesystem, process, native-library, and platform operation.

mod action;
mod asset;
mod benchmark;
mod capability;
mod planner;
mod recommendation;

pub use action::{
    ActionCommand, ActionError, ActionFailure, ActionId, ActionKey, ActionPhase, ActionProgress,
    ActionRuntime, ActionState, ActionTicket, CancellationOutcome, ConsentCategory, Coordinator,
    CoordinatorError, ProbeTicket, RollbackCause, RollbackPolicy, RollbackResidue, SetupAction,
};
pub use asset::{
    AcquiredArtifact, ArtifactDescriptor, ArtifactKind, AssetId, AssetLocation, AssetReceipt,
    AssetRegistry, CrashJournal, JournalError, ManagedAsset, ManagedSlot, OwnerMarker,
    OwnershipError, Sha256Digest, SignerRequirement,
};
pub use benchmark::{
    BackendKind, BenchmarkEvidence, BenchmarkProtocol, BenchmarkSampleSummary, ContentFreeId,
    ContentionCondition, EngineKind, ModelClass, ThermalCondition,
};
pub use capability::{
    Capability, CapabilityId, CapabilityRecord, CapabilityRecordError, CapabilityValue,
    DegradedReason, Generation, Language, Observation, ReadyAuthority, Remedy, Requirement,
    UnavailableReason, Usability,
};
pub use planner::{
    DesiredConfiguration, FormattingChoice, PersistedSetupPlan, PlanError, PlannedAction, Planner,
    PlanningPolicy, RecognitionChoice, SetupPlan,
};
pub use recommendation::{
    CandidateEvidence, ExclusionReason, Recommendation, RecommendationEngine,
    RecommendationOutcome, RecommendationPolicy, RecommendationPolicyError,
    RecommendationPreference, RejectedCandidate, TrustedCandidateIdentity,
};

/// Adapter implemented by a host that can observe a capability.
pub trait CapabilityProbe {
    type Error;

    fn probe(&self, capability: &CapabilityId) -> Result<CapabilityValue, Self::Error>;
}

/// Adapter implemented by a host-owned verified artifact store.
pub trait ArtifactStore {
    type Error;

    fn stage(&self, asset: &AssetId, action: &ActionId) -> Result<ManagedSlot, Self::Error>;
    fn verify(
        &self,
        descriptor: &ArtifactDescriptor,
        slot: &ManagedSlot,
    ) -> Result<AcquiredArtifact, Self::Error>;
    fn promote(&self, asset: &ManagedAsset) -> Result<AssetReceipt, Self::Error>;
    fn rollback(&self, asset: &ManagedAsset) -> Result<(), Self::Error>;
}

/// Adapter implemented by a host that can acquire bytes into an already
/// allocated managed staging slot. Verification and promotion stay separate.
pub trait ArtifactSource {
    type Error;

    fn acquire(
        &self,
        descriptor: &ArtifactDescriptor,
        staging: &ManagedSlot,
    ) -> Result<AcquiredArtifact, Self::Error>;
}

/// Adapter for full recognizer validation and activation. Layout checks alone
/// must not implement this contract as successful readiness.
pub trait RecognitionAdapter {
    type Error;

    fn validate(&self, capability: &CapabilityId) -> Result<CapabilityValue, Self::Error>;
    fn activate(&self, capability: &CapabilityId) -> Result<ReadyAuthority, Self::Error>;
}

/// Adapter for shared external tools such as Ollama.
///
/// The pure domain never assumes that a shared tool can be uninstalled.
pub trait ExternalToolAdapter {
    type Error;

    fn inspect(&self, tool: &ContentFreeId) -> Result<CapabilityValue, Self::Error>;
    fn guided_install(&self, tool: &ContentFreeId) -> Result<(), Self::Error>;
    fn start(&self, tool: &ContentFreeId) -> Result<(), Self::Error>;
    fn pull_model(&self, digest: &Sha256Digest) -> Result<(), Self::Error>;
    fn remove_managed_model(&self, receipt: &AssetReceipt) -> Result<(), Self::Error>;
}

/// Adapter for the current-user launch-at-login setting.
pub trait StartupAdapter {
    type Error;

    fn inspect(&self) -> Result<CapabilityValue, Self::Error>;
    fn apply_and_verify(&self, enabled: bool) -> Result<(), Self::Error>;
}

/// Adapter for a host that runs the versioned content-free benchmark protocol.
pub trait BenchmarkRunner {
    type Error;

    fn run(
        &self,
        protocol: &BenchmarkProtocol,
        candidate: &ContentFreeId,
    ) -> Result<BenchmarkEvidence, Self::Error>;
}

/// Injectable time source for journal timestamps and deterministic tests.
pub trait Clock {
    fn now_epoch_ms(&self) -> u64;
}
