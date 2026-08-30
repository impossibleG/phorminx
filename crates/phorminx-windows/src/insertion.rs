use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};

use arboard::{Clipboard, SetExtWindows};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    VK_SPACE, VK_V,
};

use crate::TargetSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertionOutcome {
    Pasted,
    ClipboardOnly(ClipboardOnlyReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOnlyReason {
    TargetUnavailable,
    TargetChanged,
    UnsupportedOrSensitiveTarget,
    ModifiersStillPressed,
    InputInjectionUncertain,
}

/// Places text on the clipboard and pastes only into a verified classic Win32
/// edit control. Clipboard restoration is intentionally disabled in Phase 1.
pub fn copy_and_maybe_paste(
    target: Option<TargetSnapshot>,
    text: &str,
) -> Result<InsertionOutcome, InsertionError> {
    let mut clipboard = Clipboard::new()?;
    clipboard
        .set()
        .exclude_from_monitoring()
        .text(text.to_owned())?;
    if clipboard.get_text()? != text {
        return Err(InsertionError::ClipboardVerification);
    }

    let Some(target) = target else {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetUnavailable,
        ));
    };
    if !target.is_current() {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetChanged,
        ));
    }
    if !target.is_classic_writable_edit() {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::UnsupportedOrSensitiveTarget,
        ));
    }
    if !wait_for_modifier_release(Duration::from_millis(750)) {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::ModifiersStillPressed,
        ));
    }
    if !target.is_current() || !target.is_classic_writable_edit() {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::TargetChanged,
        ));
    }

    let inputs = [
        key_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_V, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        return Ok(InsertionOutcome::ClipboardOnly(
            ClipboardOnlyReason::InputInjectionUncertain,
        ));
    }

    Ok(InsertionOutcome::Pasted)
}

fn wait_for_modifier_release(timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if ![VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN, VK_SPACE]
            .into_iter()
            .any(key_is_down)
        {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn key_is_down(key: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(key.0)) }) as u16 & 0x8000 != 0
}

fn key_input(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InsertionError {
    #[error("clipboard operation failed: {0}")]
    Clipboard(#[from] arboard::Error),
    #[error("clipboard readback did not match the Phorminx payload")]
    ClipboardVerification,
}
