//! Unified Phorminx application shell.
//!
//! This crate owns presentation and navigation only. It consumes privacy-safe,
//! immutable [`ShellSnapshot`] values and emits [`ShellEvent`] intent. The existing
//! runtime remains responsible for validation and every side effect.

mod app;
pub mod components;
mod deletion;
mod gallery;
mod markdown;
pub mod model;
mod pages;
mod studio;
pub mod theme;
pub mod workspace;

pub use app::PhorminxUi;
pub use gallery::ComponentGallery;
pub use model::{
    AccurateBackend, AccurateModel, AppearancePreference, ApplicationProfile,
    BenchmarkCandidateView, BenchmarkEvidenceView, BenchmarkUnavailableView,
    CalibrationCaptureState, CalibrationPromptView, FormattingStrength, GalleryScenario,
    HistoryItem, HistoryLoadedText, HistoryVariant, HistoryVariantAvailability, InlineNotice,
    LexiconCasePolicy, LexiconDraft, LexiconEntry, LibraryEmbeddingModel, LibraryIndexSnapshot,
    LibraryIndexState, LibrarySearchHit, LibrarySearchMode, LibrarySearchStatus, LibrarySnapshot,
    ModelSystem, NoticeKind, OllamaLifecycle, OllamaModelChoice, OllamaOperationState,
    OllamaSetupSnapshot, OllamaSetupState, PerformancePreference, PerformanceRecommendationView,
    PerformanceRollbackState, PerformanceRunState, PerformanceSetupSnapshot, ProfileDraft,
    ProfileInsertion, Readiness, RecognitionMode, RecordingMode, Route, RuntimeStatus,
    SettingsSnapshot, SetupAction, SetupCapability, SetupRecommendation, SetupSnapshot, SetupStage,
    ShellEvent, ShellSnapshot, SystemReadiness,
};
