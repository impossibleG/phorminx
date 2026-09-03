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
mod overlay;
#[cfg(windows)]
mod profile_window;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod setup_lock;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod target;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
pub use appearance::{SystemAppearance, system_appearance};
#[cfg(windows)]
pub use asset_dialog::choose_zip_archive;
#[cfg(windows)]
pub use dialog::show_error_dialog;
#[cfg(windows)]
pub use file::{AtomicReplaceError, atomic_activate_directory, atomic_replace_file};
#[cfg(windows)]
pub use history_window::{HistoryItem, HistoryWindow, HistoryWindowError, HistoryWindowEvent};
#[cfg(windows)]
pub use hotkey::{GlobalHoldHotkey, HoldEvent, HotkeyError};
#[cfg(windows)]
pub use insertion::{ClipboardOnlyReason, InsertionOutcome, copy_and_maybe_paste};
#[cfg(windows)]
pub use lexicon_window::{
    LexiconCasePolicy, LexiconDraft, LexiconItem, LexiconWindow, LexiconWindowError,
    LexiconWindowEvent,
};
#[cfg(windows)]
pub use overlay::{OverlayError, OverlayStatus, StatusOverlay};
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
