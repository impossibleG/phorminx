//! Application orchestration for Phorminx.

pub mod benchmarks;
pub mod feedback;
#[cfg(test)]
mod frontend_tests;
mod history_loader;
pub mod incremental;
pub mod investigation_assistant;
pub mod investigation_links;
pub mod investigation_ocr;
pub mod investigations;
pub mod launcher_messaging;
pub mod message_arrival;
mod library_search;
pub mod lifecycle;
pub mod meeting_audio;
pub mod meeting_memory;
pub mod meeting_workspace;
pub mod model;
pub mod ollama_onboarding;
pub mod ollama_recovery;
pub mod performance;
pub mod performance_runtime;
pub mod product_shell;
pub mod prompts;
pub mod review;
pub mod runtime;
pub mod settings;
pub mod setup_center;
pub mod setup_features;
pub mod setup_host;
pub mod tauri_contract;
#[cfg(feature = "tauri-shell")]
pub mod tauri_investigations;
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
