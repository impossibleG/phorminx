//! Application orchestration for Phorminx.

pub mod feedback;
mod history_loader;
pub mod incremental;
pub mod launcher_messaging;
mod library_search;
pub mod meeting_audio;
pub mod meeting_memory;
pub mod meeting_workspace;
pub mod model;
pub mod ollama_onboarding;
pub mod ollama_recovery;
pub mod performance;
pub mod performance_runtime;
pub mod product_shell;
pub mod runtime;
pub mod settings;
pub mod setup_center;
pub mod setup_features;
pub mod setup_host;
pub mod tauri_contract;
#[cfg(feature = "tauri-preview")]
pub mod tauri_meetings;
#[cfg(feature = "tauri-shell")]
mod tauri_messaging;
#[cfg(feature = "tauri-shell")]
pub mod tauri_shell;
pub mod text_recovery;
pub mod ui_bridge;
pub mod update_center;
pub mod updates;
pub mod vosk_models;
