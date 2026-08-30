use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_MENU, VK_RCONTROL, VK_RMENU, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_APP, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::TargetSnapshot;
use crate::target::{activation_target, capture_activation_target};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static KEYS_DOWN: AtomicU8 = AtomicU8::new(0);

const LEFT_CONTROL: u8 = 1 << 0;
const RIGHT_CONTROL: u8 = 1 << 1;
const GENERIC_CONTROL: u8 = 1 << 2;
const LEFT_ALT: u8 = 1 << 3;
const RIGHT_ALT: u8 = 1 << 4;
const GENERIC_ALT: u8 = 1 << 5;
const SPACE: u8 = 1 << 6;

const CONTROL: u8 = LEFT_CONTROL | RIGHT_CONTROL | GENERIC_CONTROL;
const ALT: u8 = LEFT_ALT | RIGHT_ALT | GENERIC_ALT;

const WM_HOLD: u32 = WM_APP + 1;
const WM_RELEASE: u32 = WM_APP + 2;
const WM_STOP: u32 = WM_APP + 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldEvent {
    Started { target: Option<TargetSnapshot> },
    Ended,
}

/// Owns the low-level Ctrl+Alt+Space hold/release hook and its message thread.
pub struct GlobalHoldHotkey {
    events: Receiver<HoldEvent>,
    hook_thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl GlobalHoldHotkey {
    pub fn start() -> Result<Self, HotkeyError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(HotkeyError::AlreadyRunning);
        }

        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-hotkey".to_owned())
            .spawn(move || run_hook_loop(event_tx, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                HotkeyError::Spawn(error)
            })?;

        let hook_thread_id = match ready_rx.recv() {
            Ok(Ok(thread_id)) => thread_id,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(HotkeyError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(HotkeyError::StartupChannelClosed);
            }
        };

        Ok(Self {
            events: event_rx,
            hook_thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<HoldEvent> {
        &self.events
    }

    pub fn shutdown(mut self) -> Result<(), HotkeyError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), HotkeyError> {
        if self.thread.is_none() {
            return Ok(());
        }

        let post_result =
            unsafe { PostThreadMessageW(self.hook_thread_id, WM_STOP, WPARAM(0), LPARAM(0)) };
        let join_result = self.thread.take().map(JoinHandle::join);
        self.hook_thread_id = 0;

        if let Some(Err(_)) = join_result {
            return Err(HotkeyError::ThreadPanicked);
        }
        post_result.map_err(HotkeyError::StopMessage)
    }
}

impl Drop for GlobalHoldHotkey {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_hook_loop(event_tx: Sender<HoldEvent>, ready_tx: mpsc::SyncSender<Result<u32, String>>) {
    let result = unsafe { install_and_run(event_tx, &ready_tx) };
    if let Err(error) = result {
        let _ = ready_tx.try_send(Err(error.to_string()));
    }
    KEYS_DOWN.store(0, Ordering::Release);
    HOOK_THREAD_ID.store(0, Ordering::Release);
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn install_and_run(
    event_tx: Sender<HoldEvent>,
    ready_tx: &mpsc::SyncSender<Result<u32, String>>,
) -> windows::core::Result<()> {
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    HOOK_THREAD_ID.store(thread_id, Ordering::Release);
    KEYS_DOWN.store(0, Ordering::Release);

    let module = unsafe { GetModuleHandleW(None)? };
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_callback),
            Some(HINSTANCE(module.0)),
            0,
        )?
    };
    let _ = ready_tx.send(Ok(thread_id));

    let loop_result = loop {
        let code = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if code == -1 {
            break Err(windows::core::Error::from_thread());
        }
        if code == 0 || message.message == WM_STOP {
            break Ok(());
        }

        let event = match message.message {
            WM_HOLD => Some(HoldEvent::Started {
                target: activation_target(),
            }),
            WM_RELEASE => Some(HoldEvent::Ended),
            _ => None,
        };
        if let Some(event) = event {
            let _ = event_tx.send(event);
        }
    };

    let unhook_result = unsafe { UnhookWindowsHookEx(hook) };
    loop_result.and(unhook_result)
}

unsafe extern "system" fn keyboard_callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let keyboard = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if !keyboard.flags.contains(LLKHF_INJECTED) {
            let message = wparam.0 as u32;
            let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);

            if (down || up)
                && let Some(bit) = key_bit(keyboard.vkCode)
            {
                let old = if down {
                    KEYS_DOWN.fetch_or(bit, Ordering::Relaxed)
                } else {
                    KEYS_DOWN.fetch_and(!bit, Ordering::Relaxed)
                };
                let new = if down { old | bit } else { old & !bit };

                if chord(old) != chord(new) {
                    let thread_id = HOOK_THREAD_ID.load(Ordering::Relaxed);
                    if thread_id != 0 {
                        let became_held = chord(new);
                        if became_held {
                            capture_activation_target();
                        }
                        let message = if became_held { WM_HOLD } else { WM_RELEASE };
                        let _ =
                            unsafe { PostThreadMessageW(thread_id, message, WPARAM(0), LPARAM(0)) };
                    }
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn chord(bits: u8) -> bool {
    bits & CONTROL != 0 && bits & ALT != 0 && bits & SPACE != 0
}

fn key_bit(virtual_key: u32) -> Option<u8> {
    match virtual_key as u16 {
        value if value == VK_LCONTROL.0 => Some(LEFT_CONTROL),
        value if value == VK_RCONTROL.0 => Some(RIGHT_CONTROL),
        value if value == VK_CONTROL.0 => Some(GENERIC_CONTROL),
        value if value == VK_LMENU.0 => Some(LEFT_ALT),
        value if value == VK_RMENU.0 => Some(RIGHT_ALT),
        value if value == VK_MENU.0 => Some(GENERIC_ALT),
        value if value == VK_SPACE.0 => Some(SPACE),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("the global hold hotkey is already running")]
    AlreadyRunning,
    #[error("failed to start the hotkey thread: {0}")]
    Spawn(std::io::Error),
    #[error("the hotkey thread failed during startup: {0}")]
    Startup(String),
    #[error("the hotkey thread exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to stop the hotkey thread: {0}")]
    StopMessage(windows::core::Error),
    #[error("the hotkey thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_requires_control_alt_and_space() {
        assert!(!chord(LEFT_CONTROL | LEFT_ALT));
        assert!(!chord(LEFT_CONTROL | SPACE));
        assert!(chord(RIGHT_CONTROL | LEFT_ALT | SPACE));
    }

    #[test]
    fn maps_sided_and_generic_virtual_keys() {
        assert_eq!(key_bit(u32::from(VK_LCONTROL.0)), Some(LEFT_CONTROL));
        assert_eq!(key_bit(u32::from(VK_RMENU.0)), Some(RIGHT_ALT));
        assert_eq!(key_bit(u32::from(VK_SPACE.0)), Some(SPACE));
        assert_eq!(key_bit(0), None);
    }
}
