//! Unified Phorminx application shell.
//!
//! This crate owns presentation and navigation only. It consumes privacy-safe,
//! immutable [`ShellSnapshot`] values and emits [`ShellEvent`] intent. The existing
//! runtime remains responsible for validation and every side effect.

mod app;
pub mod components;
mod gallery;
pub mod model;
mod pages;
pub mod theme;

pub use app::PhorminxUi;
pub use gallery::ComponentGallery;
pub use model::{
    AccurateBackend, AccurateModel, AppearancePreference, ApplicationProfile, FormattingStrength,
    GalleryScenario, HistoryItem, HistoryVariant, InlineNotice, LexiconCasePolicy, LexiconDraft,
    LexiconEntry, ModelSystem, NoticeKind, OllamaLifecycle, ProfileDraft, ProfileInsertion,
    Readiness, RecognitionMode, RecordingMode, Route, RuntimeStatus, SettingsSnapshot, ShellEvent,
    ShellSnapshot, SystemReadiness,
};
