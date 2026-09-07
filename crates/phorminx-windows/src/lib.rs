//! Windows shell integration for Phorminx.

#[cfg(windows)]
mod appearance;
#[cfg(windows)]
mod asset_dialog;
#[cfg(windows)]
mod dialog;
#[cfg(windows)]
mod file;
#[cfg(windows)]
mod history_window;
#[cfg(windows)]
mod hotkey;
#[cfg(windows)]
mod insertion;
#[cfg(windows)]
mod lexicon_window;
#[cfg(windows)]
mod ollama;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod performance;
#[cfg(windows)]
mod profile_window;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod setup_lock;
#[cfg(windows)]
mod shortcut;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod target;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
pub use appearance::{SystemAppearance, system_appearance, system_apps_use_dark_theme};
#[cfg(windows)]
pub use asset_dialog::choose_zip_archive;
#[cfg(windows)]
pub use dialog::show_error_dialog;
#[cfg(windows)]
pub use file::{AtomicReplaceError, atomic_activate_directory, atomic_replace_file};
#[cfg(windows)]
pub use history_window::{HistoryItem, HistoryWindow, HistoryWindowError, HistoryWindowEvent};
#[cfg(windows)]
pub use hotkey::{GlobalHoldHotkey, HoldEvent, HotkeyError, set_global_shortcut_capture};
#[cfg(windows)]
pub use insertion::{ClipboardOnlyReason, InsertionOutcome, copy_and_maybe_paste};
#[cfg(windows)]
pub use lexicon_window::{
    LexiconCasePolicy, LexiconDraft, LexiconItem, LexiconWindow, LexiconWindowError,
    LexiconWindowEvent,
};
#[cfg(windows)]
pub use ollama::{
    AutomaticOllamaInstallAvailability, AutomaticOllamaInstallUnavailable,
    OLLAMA_OFFICIAL_WINDOWS_DOWNLOAD, OllamaInstallConsentError, OllamaInstallReview,
    OllamaInstallation, OllamaInstallationError, OllamaManualInstallAction,
    OllamaModelStorageError, OllamaUninstallGuidance, OpenOllamaDownloadError,
    inspect_ollama_installation, ollama_automatic_install_availability,
    ollama_model_disk_free_bytes, open_official_ollama_download,
};
#[cfg(windows)]
pub use overlay::{OverlayError, OverlayStatus, StatusOverlay};
#[cfg(windows)]
pub use performance::{
    HostResourceError, HostResourceSnapshot, host_cpu_busy_per_mille,
    host_external_cpu_busy_per_mille, host_resource_snapshot, vulkan_driver_manifests,
    windows_system_directory,
};
#[cfg(windows)]
pub use profile_window::{
    ProfileFormatting, ProfileInsertion, ProfileItem, ProfileWindow, ProfileWindowError,
    ProfileWindowEvent,
};
#[cfg(windows)]
pub use settings_window::{
    SettingsAccurateBackend, SettingsAccurateModel, SettingsForm, SettingsFormatting,
    SettingsHistoryRetention, SettingsOllamaLifecycle, SettingsRecognitionMode,
    SettingsRecordingMode, SettingsWindow, SettingsWindowError, SettingsWindowEvent,
};
#[cfg(windows)]
pub use setup_lock::{SetupOperationLock, SetupOperationLockError};
#[cfg(windows)]
pub use shortcut::{
    DEFAULT_LAUNCHER_SHORTCUT, Shortcut, ShortcutAvailabilityError, ShortcutBindings, ShortcutError,
};
#[cfg(windows)]
pub use single_instance::{SingleInstance, SingleInstanceError, activate_existing_window};
#[cfg(windows)]
pub use startup::{
    LaunchAtLoginError, LaunchAtLoginState, launch_at_login_state, set_launch_at_login,
};
#[cfg(windows)]
pub use target::TargetSnapshot;
#[cfg(windows)]
pub use tray::{SystemTray, TrayError, TrayEvent, TrayStatus};

#[cfg(not(windows))]
compile_error!("phorminx-windows only supports Windows");
