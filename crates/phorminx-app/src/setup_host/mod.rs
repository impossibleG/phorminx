//! Trusted host boundary for setup, repair, and managed artifacts.
//!
//! UI code receives immutable presentations and opaque [`AuthorizedAction`]
//! values. Persisted plans, remedies, paths, and installer commands never
//! acquire execution authority through this module.

mod catalog;
mod executor;
mod managed;
mod probe;

pub use catalog::{CatalogError, Packaging, PinnedArtifact, PinnedCatalog};
pub use executor::{
    ActionAdapters, ActionEvent, ActionPresentation, AuthorizationError, AuthorizedAction,
    AuthorizedPlan, ConsentError, HostActionError, HttpsFetcher, SetupAuthority, SetupExecutor,
    SetupExecutorError, ShutdownOutcome,
};
pub use managed::{
    AcquisitionLimits, ArtifactFetcher, FetchError, ManagedInstall, ManagedRoot, ManagedRootError,
    RecoveryReport,
};
pub use probe::{NormalizedProbeFact, ProbeFactError, RecognitionProbeState};
