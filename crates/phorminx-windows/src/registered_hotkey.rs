//! A single `RegisterHotKey` press shortcut on its own message thread.
//!
//! Unlike the dictation hold hotkey this never observes other keystrokes: Windows
//! delivers `WM_HOTKEY` only for the exact registered combination, and the
//! registration fails cleanly when another application already owns it.
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
};

use crate::shortcut::{ALT, CTRL, SHIFT, WIN};
use crate::{Shortcut, ShortcutAvailabilityError};

const HOTKEY_ID: i32 = 0x5f80;

pub struct RegisteredHotkey {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    shortcut: Shortcut,
}

impl RegisteredHotkey {
    /// Registers `shortcut` and calls `on_press` on the hotkey thread for each
    /// press. Keep the callback short; hand real work to another thread.
    pub fn register(
        shortcut: Shortcut,
        on_press: impl Fn() + Send + 'static,
    ) -> Result<Self, ShortcutAvailabilityError> {
        let unavailable = || ShortcutAvailabilityError {
            shortcut: shortcut.to_string(),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-capture-hotkey".into())
            .spawn(move || {
                let mut message = MSG::default();
                // Create this thread's message queue before publishing its id.
                let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
                let bits = (if shortcut.modifiers & ALT != 0 { 1 } else { 0 })
                    | (if shortcut.modifiers & CTRL != 0 { 2 } else { 0 })
                    | (if shortcut.modifiers & SHIFT != 0 {
                        4
                    } else {
                        0
                    })
                    | (if shortcut.modifiers & WIN != 0 { 8 } else { 0 });
                let registered = unsafe {
                    RegisterHotKey(
                        None,
                        HOTKEY_ID,
                        HOT_KEY_MODIFIERS(bits) | MOD_NOREPEAT,
                        u32::from(shortcut.key),
                    )
                };
                let thread_id = unsafe { GetCurrentThreadId() };
                if registered.is_err() {
                    let _ = ready_tx.send(None);
                    return;
                }
                let _ = ready_tx.send(Some(thread_id));
                loop {
                    let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
                    if result.0 <= 0 {
                        break;
                    }
                    if message.message == WM_HOTKEY && message.wParam.0 as i32 == HOTKEY_ID {
                        on_press();
                    }
                }
                let _ = unsafe { UnregisterHotKey(None, HOTKEY_ID) };
            })
            .map_err(|_| unavailable())?;
        match ready_rx.recv() {
            Ok(Some(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
                shortcut,
            }),
            _ => {
                let _ = thread.join();
                Err(unavailable())
            }
        }
    }

    pub fn shortcut(&self) -> Shortcut {
        self.shortcut
    }
}

impl Drop for RegisteredHotkey {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_shortcut_cannot_be_registered_twice_and_is_released_on_drop() {
        // An unusual combination keeps this test away from real user shortcuts.
        let shortcut = Shortcut::parse("Ctrl+Alt+Shift+F23").unwrap();
        let first = RegisteredHotkey::register(shortcut, || {}).expect("first registration");
        assert_eq!(first.shortcut(), shortcut);
        assert!(RegisteredHotkey::register(shortcut, || {}).is_err());
        drop(first);
        let again = RegisteredHotkey::register(shortcut, || {}).expect("released on drop");
        drop(again);
    }
}
