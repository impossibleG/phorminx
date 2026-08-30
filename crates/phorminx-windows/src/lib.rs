//! Windows shell integration for Phorminx.

#[cfg(windows)]
mod hotkey;
#[cfg(windows)]
mod insertion;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod target;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
pub use hotkey::{GlobalHoldHotkey, HoldEvent, HotkeyError};
#[cfg(windows)]
pub use insertion::{ClipboardOnlyReason, InsertionOutcome, copy_and_maybe_paste};
#[cfg(windows)]
pub use overlay::{OverlayError, OverlayStatus, StatusOverlay};
#[cfg(windows)]
pub use target::TargetSnapshot;
#[cfg(windows)]
pub use tray::{SystemTray, TrayError, TrayEvent, TrayStatus};

#[cfg(not(windows))]
compile_error!("phorminx-windows only supports Windows");
