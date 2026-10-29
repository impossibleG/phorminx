//! Starts the Windows screen snip and lets callers notice when its picture
//! reaches the clipboard.

use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::w;

#[derive(Debug, thiserror::Error)]
pub enum ScreenSnipError {
    #[error("Windows could not start the screen snip")]
    ShellOpenFailed,
}

/// Opens the system snipping overlay (`ms-screenclip:`). The snip lands on
/// the clipboard; compare [`clipboard_sequence`] before and after.
pub fn start_screen_snip() -> Result<(), ScreenSnipError> {
    // SAFETY: fixed, NUL-terminated constants only; no user text reaches the shell.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("ms-screenclip:"),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        Err(ScreenSnipError::ShellOpenFailed)
    } else {
        Ok(())
    }
}

/// The clipboard's change counter; it moves whenever anything is copied.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: no arguments; reads a per-session counter.
    unsafe { GetClipboardSequenceNumber() }
}
