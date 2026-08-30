//! Windows shell integration for Phorminx.

#[cfg(windows)]
mod hotkey;
#[cfg(windows)]
mod insertion;
#[cfg(windows)]
mod target;

#[cfg(windows)]
pub use hotkey::{GlobalHoldHotkey, HoldEvent, HotkeyError};
#[cfg(windows)]
pub use insertion::{ClipboardOnlyReason, InsertionOutcome, copy_and_maybe_paste};
#[cfg(windows)]
pub use target::TargetSnapshot;

#[cfg(not(windows))]
compile_error!("phorminx-windows only supports Windows");
