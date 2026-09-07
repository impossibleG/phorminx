use crate::shortcut::{ALT, CTRL, SHIFT, WIN};
use crate::{Shortcut, ShortcutBindings, TargetSnapshot};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_APP, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static ACTIVATION_KEYS: AtomicU32 = AtomicU32::new(0x20);
static DICTATION_BUSY: AtomicBool = AtomicBool::new(false);
static CAPTURE_SUSPENDED: AtomicBool = AtomicBool::new(false);
const WM_COMMAND: u32 = WM_APP + 1;
const WM_CHOOSE: u32 = WM_APP + 2;
const WM_STOP: u32 = WM_APP + 3;
thread_local! { static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) }; }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldEvent {
    Started { target: Option<TargetSnapshot> },
    Ended,
    LauncherRequested { target: Option<TargetSnapshot> },
    LauncherDictate { target: Option<TargetSnapshot> },
    LauncherDismissed,
}
enum Command {
    Bindings(ShortcutBindings),
    Launcher(bool),
}

/// All keyboard state lives on one message thread. Target snapshots travel in
/// events, never in a mutable last-target global that later keystrokes can overwrite.
pub struct GlobalHoldHotkey {
    events: Receiver<HoldEvent>,
    commands: Sender<Command>,
    hook_thread_id: u32,
    thread: Option<JoinHandle<()>>,
}
impl GlobalHoldHotkey {
    pub fn start() -> Result<Self, HotkeyError> {
        Self::start_with_bindings(ShortcutBindings::default())
    }
    pub fn start_with_bindings(bindings: ShortcutBindings) -> Result<Self, HotkeyError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(HotkeyError::AlreadyRunning);
        }
        publish_activation_keys(bindings);
        DICTATION_BUSY.store(false, Ordering::Release);
        CAPTURE_SUSPENDED.store(false, Ordering::Release);
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-hotkey".to_owned())
            .spawn(move || {
                let result = unsafe { install_and_run(event_tx, command_rx, bindings, &ready_tx) };
                if let Err(error) = result {
                    let _ = ready_tx.try_send(Err(error.to_string()));
                }
                HOOK.with(|state| *state.borrow_mut() = None);
                HOOK_THREAD_ID.store(0, Ordering::Release);
                ACTIVE.store(false, Ordering::Release);
            })
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                HotkeyError::Spawn(error)
            })?;
        let hook_thread_id = match ready_rx.recv() {
            Ok(Ok(id)) => id,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(HotkeyError::Startup(error));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(HotkeyError::StartupChannelClosed);
            }
        };
        Ok(Self {
            events: event_rx,
            commands: command_tx,
            hook_thread_id,
            thread: Some(thread),
        })
    }
    pub fn events(&self) -> &Receiver<HoldEvent> {
        &self.events
    }
    pub fn set_bindings(&self, bindings: ShortcutBindings) -> Result<(), HotkeyError> {
        self.command(Command::Bindings(bindings))
    }
    pub fn set_suspended(&self, suspended: bool) -> Result<(), HotkeyError> {
        set_global_shortcut_capture(suspended)
    }
    pub fn set_launcher_open(&self, open: bool) -> Result<(), HotkeyError> {
        self.command(Command::Launcher(open))
    }
    pub fn set_dictation_busy(&self, busy: bool) {
        DICTATION_BUSY.store(busy, Ordering::Release);
    }
    fn command(&self, command: Command) -> Result<(), HotkeyError> {
        self.commands
            .send(command)
            .map_err(|_| HotkeyError::StartupChannelClosed)?;
        unsafe { PostThreadMessageW(self.hook_thread_id, WM_COMMAND, WPARAM(0), LPARAM(0)) }
            .map_err(HotkeyError::StopMessage)
    }
    pub fn shutdown(mut self) -> Result<(), HotkeyError> {
        self.stop()
    }
    fn stop(&mut self) -> Result<(), HotkeyError> {
        if self.thread.is_none() {
            return Ok(());
        }
        // Never join a live message loop when posting its stop message failed.
        unsafe { PostThreadMessageW(self.hook_thread_id, WM_STOP, WPARAM(0), LPARAM(0)) }
            .map_err(HotkeyError::StopMessage)?;
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            return Err(HotkeyError::ThreadPanicked);
        }
        self.hook_thread_id = 0;
        Ok(())
    }
}
impl Drop for GlobalHoldHotkey {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
pub(crate) fn choose_launcher_action() {
    let id = HOOK_THREAD_ID.load(Ordering::Acquire);
    if id != 0 {
        let _ = unsafe { PostThreadMessageW(id, WM_CHOOSE, WPARAM(0), LPARAM(0)) };
    }
}

/// Setup mode has no hook; suspending it is intentionally a no-op there.
pub fn set_global_shortcut_capture(capturing: bool) -> Result<(), HotkeyError> {
    CAPTURE_SUSPENDED.store(capturing, Ordering::Release);
    Ok(())
}
fn publish_activation_keys(bindings: ShortcutBindings) {
    ACTIVATION_KEYS.store(
        u32::from(bindings.launcher.key)
            | (u32::from(bindings.direct_dictation.map_or(0, |key| key.key)) << 16),
        Ordering::Release,
    );
}
pub(crate) fn activation_keys() -> [u16; 2] {
    let packed = ACTIVATION_KEYS.load(Ordering::Acquire);
    [packed as u16, (packed >> 16) as u16]
}
struct HookState {
    keyboard: KeyboardState,
    events: Sender<HoldEvent>,
    launcher_target: Option<TargetSnapshot>,
}
impl HookState {
    fn dispatch(&mut self, action: KeyAction) {
        if matches!(action, KeyAction::DirectStart | KeyAction::Choose) {
            DICTATION_BUSY.store(true, Ordering::Release);
        }
        let event = match action {
            KeyAction::DirectStart => HoldEvent::Started {
                target: TargetSnapshot::capture(),
            },
            KeyAction::DirectEnd => HoldEvent::Ended,
            KeyAction::Launcher => {
                self.launcher_target = TargetSnapshot::capture();
                HoldEvent::LauncherRequested {
                    target: self.launcher_target,
                }
            }
            KeyAction::Choose => HoldEvent::LauncherDictate {
                target: self.launcher_target.take(),
            },
            KeyAction::Dismiss => {
                self.launcher_target = None;
                HoldEvent::LauncherDismissed
            }
        };
        let _ = self.events.send(event);
    }
}
unsafe fn install_and_run(
    event_tx: Sender<HoldEvent>,
    commands: Receiver<Command>,
    bindings: ShortcutBindings,
    ready_tx: &mpsc::SyncSender<Result<u32, String>>,
) -> windows::core::Result<()> {
    let mut message = MSG::default();
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    let id = unsafe { GetCurrentThreadId() };
    HOOK_THREAD_ID.store(id, Ordering::Release);
    let mut keyboard = KeyboardState::new(bindings);
    for (key, down) in keyboard.down.iter_mut().enumerate() {
        // Seed sided modifiers only. Seeding their aggregate virtual key too
        // would leave it stuck when Windows later reports only sided releases.
        if !matches!(key, 0x10..=0x12) {
            *down = unsafe {
                windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key as i32)
            } < 0;
        }
    }
    HOOK.with(|state| {
        *state.borrow_mut() = Some(HookState {
            keyboard,
            events: event_tx,
            launcher_target: None,
        })
    });
    let module = unsafe { GetModuleHandleW(None)? };
    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_callback),
            Some(HINSTANCE(module.0)),
            0,
        )?
    };
    let _ = ready_tx.send(Ok(id));
    let result = loop {
        let code = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if code == -1 {
            break Err(windows::core::Error::from_thread());
        }
        if code == 0 || message.message == WM_STOP {
            break Ok(());
        }
        HOOK.with(|state| {
            let mut state = state.borrow_mut();
            let Some(state) = state.as_mut() else {
                return;
            };
            if message.message == WM_COMMAND {
                while let Ok(command) = commands.try_recv() {
                    match command {
                        Command::Bindings(bindings) => {
                            publish_activation_keys(bindings);
                            state.keyboard.bindings = bindings;
                            state.keyboard.close_launcher();
                        }
                        Command::Launcher(open) => {
                            state.keyboard.launcher_open = open;
                            if !open {
                                state.keyboard.pending_choice = false;
                            }
                        }
                    }
                }
            } else if message.message == WM_CHOOSE {
                state.keyboard.suspended = CAPTURE_SUSPENDED.load(Ordering::Acquire);
                let target_current = state
                    .launcher_target
                    .is_none_or(|target| target.is_current());
                if let Some(action) = state.keyboard.click_choice(target_current) {
                    state.dispatch(action);
                }
            }
        });
    };
    result.and(unsafe { UnhookWindowsHookEx(hook) })
}
unsafe extern "system" fn keyboard_callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        if !key.flags.contains(LLKHF_INJECTED)
            && matches!(message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
        {
            let consumed = HOOK.with(|state| {
                let mut state = state.borrow_mut();
                let Some(state) = state.as_mut() else {
                    return false;
                };
                state.keyboard.busy = DICTATION_BUSY.load(Ordering::Acquire);
                state.keyboard.suspended = CAPTURE_SUSPENDED.load(Ordering::Acquire);
                if state.keyboard.suspended {
                    state.keyboard.close_launcher();
                }
                if state.keyboard.launcher_open
                    && state
                        .launcher_target
                        .is_some_and(|target| !target.is_current())
                {
                    state.keyboard.close_launcher();
                    state.dispatch(KeyAction::Dismiss);
                }
                let (consume, action) = state
                    .keyboard
                    .input(key.vkCode, matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN));
                if let Some(action) = action {
                    state.dispatch(action);
                }
                consume
            });
            if consumed {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyAction {
    DirectStart,
    DirectEnd,
    Launcher,
    Choose,
    Dismiss,
}
struct KeyboardState {
    bindings: ShortcutBindings,
    down: [bool; 256],
    consumed: [bool; 256],
    direct_held: Option<Shortcut>,
    launcher_open: bool,
    pending_choice: bool,
    busy: bool,
    suspended: bool,
}
impl KeyboardState {
    fn new(bindings: ShortcutBindings) -> Self {
        Self {
            bindings,
            down: [false; 256],
            consumed: [false; 256],
            direct_held: None,
            launcher_open: false,
            pending_choice: false,
            busy: false,
            suspended: false,
        }
    }
    fn close_launcher(&mut self) {
        self.launcher_open = false;
        self.pending_choice = false;
    }
    fn click_choice(&mut self, target_current: bool) -> Option<KeyAction> {
        if !self.launcher_open || self.suspended {
            self.close_launcher();
            return None;
        }
        self.close_launcher();
        self.busy = target_current;
        Some(if target_current {
            KeyAction::Choose
        } else {
            KeyAction::Dismiss
        })
    }
    fn modifiers(&self) -> u8 {
        let any = |keys: &[usize]| keys.iter().any(|key| self.down[*key]);
        (if any(&[0x11, 0xa2, 0xa3]) { CTRL } else { 0 })
            | (if any(&[0x12, 0xa4, 0xa5]) { ALT } else { 0 })
            | (if any(&[0x10, 0xa0, 0xa1]) { SHIFT } else { 0 })
            | (if any(&[0x5b, 0x5c]) { WIN } else { 0 })
    }
    fn input(&mut self, key: u32, pressed: bool) -> (bool, Option<KeyAction>) {
        let Ok(index) = usize::try_from(key) else {
            return (false, None);
        };
        if index >= self.down.len() {
            return (false, None);
        }
        let repeat = self.down[index] && pressed;
        self.down[index] = pressed;
        let consumed = self.consumed[index];
        if !pressed {
            self.consumed[index] = false;
        }
        if self.pending_choice && self.modifiers() == 0 {
            self.close_launcher();
            self.busy = true;
            return (consumed, Some(KeyAction::Choose));
        }
        if !pressed
            && self.direct_held.is_some_and(|held| {
                key == u32::from(held.key) || (self.modifiers() & held.modifiers) != held.modifiers
            })
        {
            self.direct_held = None;
            return (consumed, Some(KeyAction::DirectEnd));
        }
        if consumed || repeat || !pressed || self.suspended {
            return (consumed, None);
        }
        let modifiers = self.modifiers();
        if self.launcher_open && matches!(key, 0x31 | 0x61 | 0x0d | 0x1b) {
            self.consumed[index] = true;
            if modifiers != 0 && key != 0x1b {
                self.pending_choice = true;
                return (true, None);
            }
            self.close_launcher();
            self.busy = key != 0x1b;
            return (
                true,
                Some(if key == 0x1b {
                    KeyAction::Dismiss
                } else {
                    KeyAction::Choose
                }),
            );
        }
        if key == u32::from(self.bindings.launcher.key)
            && modifiers == self.bindings.launcher.modifiers
        {
            self.consumed[index] = true;
            if self.launcher_open {
                self.close_launcher();
                return (true, Some(KeyAction::Dismiss));
            }
            // Arm selection synchronously in the hook, before the app's next
            // poll. Fast shortcut→1 must never leak into the original editor.
            self.launcher_open = !self.busy;
            return (true, Some(KeyAction::Launcher));
        }
        if self
            .bindings
            .direct_dictation
            .is_some_and(|s| key == u32::from(s.key) && modifiers == s.modifiers)
        {
            self.consumed[index] = true;
            self.direct_held = self.bindings.direct_dictation;
            self.close_launcher();
            self.busy = true;
            return (true, Some(KeyAction::DirectStart));
        }
        (false, None)
    }
}
#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("the global hotkey is already running")]
    AlreadyRunning,
    #[error("failed to start the hotkey thread: {0}")]
    Spawn(std::io::Error),
    #[error("the hotkey thread failed during startup: {0}")]
    Startup(String),
    #[error("the hotkey thread is unavailable")]
    StartupChannelClosed,
    #[error("failed to send a hotkey command: {0}")]
    StopMessage(windows::core::Error),
    #[error("the hotkey thread panicked")]
    ThreadPanicked,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suppresses_trigger_repeat_release_not_typing() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        assert_eq!(s.input(0x20, true), (false, None));
        s.input(0x20, false);
        s.input(0xa2, true);
        s.input(0xa5, true);
        assert_eq!(s.input(0x20, true), (true, Some(KeyAction::Launcher)));
        assert_eq!(s.input(0x20, true), (true, None));
        s.input(0xa2, false);
        assert_eq!(s.input(0x20, true), (true, None));
        assert_eq!(s.input(0x20, false), (true, None));
    }
    #[test]
    fn extra_modifiers_do_not_match() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        for key in [0x11, 0x12, 0x10] {
            s.input(key, true);
        }
        assert_eq!(s.input(0x20, true), (false, None));
    }
    #[test]
    fn choices_escape_repeats_and_keyups_do_not_leak() {
        for key in [0x31, 0x61, 0x0d, 0x1b] {
            let mut s = KeyboardState::new(ShortcutBindings::default());
            s.launcher_open = true;
            assert_eq!(
                s.input(key, true),
                (
                    true,
                    Some(if key == 0x1b {
                        KeyAction::Dismiss
                    } else {
                        KeyAction::Choose
                    })
                )
            );
            assert_eq!(s.input(key, true), (true, None));
            assert_eq!(s.input(key, false), (true, None));
            assert!(!s.launcher_open);
        }
    }
    #[test]
    fn choice_waits_for_modifier_release() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        s.launcher_open = true;
        s.input(0x11, true);
        assert_eq!(s.input(0x31, true), (true, None));
        s.input(0x31, false);
        assert_eq!(s.input(0x11, false), (false, Some(KeyAction::Choose)));
        assert!(!s.launcher_open);
    }
    #[test]
    fn direct_hold_ends_once_without_retrigger() {
        let mut s = KeyboardState::new(ShortcutBindings::parse("F8", Some("Ctrl+D")).unwrap());
        s.input(0xa2, true);
        assert_eq!(s.input(0x44, true), (true, Some(KeyAction::DirectStart)));
        assert_eq!(s.input(0xa2, false), (false, Some(KeyAction::DirectEnd)));
        s.input(0xa2, true);
        assert_eq!(s.input(0x44, true), (true, None));
        assert_eq!(s.input(0x44, false), (true, None));
    }
    #[test]
    fn suspended_capture_passes_keys_not_old_gesture_release() {
        let mut s = KeyboardState::new(ShortcutBindings::parse("F8", None).unwrap());
        s.input(0x77, true);
        s.suspended = true;
        s.close_launcher();
        assert_eq!(s.input(0x77, false), (true, None));
        assert_eq!(s.input(0x77, true), (false, None));
        s.suspended = false;
        assert_eq!(s.input(0x77, true), (false, None));
        s.input(0x77, false);
        assert_eq!(s.input(0x77, true), (true, Some(KeyAction::Launcher)));
    }
    #[test]
    fn rapid_launcher_selection_needs_no_host_roundtrip() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        s.input(0x11, true);
        s.input(0x12, true);
        assert_eq!(s.input(0x20, true), (true, Some(KeyAction::Launcher)));
        s.input(0x20, false);
        s.input(0x11, false);
        s.input(0x12, false);
        assert_eq!(s.input(0x31, true), (true, Some(KeyAction::Choose)));
        assert_eq!(s.input(0x31, false), (true, None));
        assert!(s.busy);
    }
    #[test]
    fn launcher_trigger_toggles_palette_but_not_when_dictation_busy() {
        let mut s = KeyboardState::new(ShortcutBindings::parse("F8", None).unwrap());
        assert_eq!(s.input(0x77, true), (true, Some(KeyAction::Launcher)));
        s.input(0x77, false);
        assert_eq!(s.input(0x77, true), (true, Some(KeyAction::Dismiss)));
        s.input(0x77, false);
        s.busy = true;
        assert_eq!(s.input(0x77, true), (true, Some(KeyAction::Launcher)));
        assert!(!s.launcher_open);
        assert_eq!(s.input(0x31, true), (false, None));
    }

    #[test]
    fn cancelling_pending_choice_prevents_delayed_activation_on_release() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        s.launcher_open = true;
        s.input(0x11, true);
        assert_eq!(s.input(0x31, true), (true, None));
        assert!(s.pending_choice);
        s.close_launcher();
        assert_eq!(s.input(0x11, false), (false, None));
        assert_eq!(s.input(0x31, false), (true, None));
    }

    #[test]
    fn unrelated_consumed_key_release_does_not_end_direct_hold() {
        let mut s = KeyboardState::new(ShortcutBindings::parse("F8", Some("Ctrl+D")).unwrap());
        s.input(0x77, true);
        s.input(0x11, true);
        assert_eq!(s.input(0x44, true), (true, Some(KeyAction::DirectStart)));
        assert_eq!(s.input(0x77, false), (true, None));
        s.input(0x10, true);
        s.input(0x41, true);
        assert_eq!(s.input(0x41, false), (false, None));
        assert_eq!(s.input(0x44, false), (true, Some(KeyAction::DirectEnd)));
    }

    #[test]
    fn active_direct_hold_keeps_its_original_chord_across_rebinding() {
        let mut s = KeyboardState::new(ShortcutBindings::parse("F8", Some("Ctrl+D")).unwrap());
        s.input(0x11, true);
        s.input(0x44, true);
        s.bindings = ShortcutBindings::parse("F9", Some("Ctrl+Shift+G")).unwrap();
        assert_eq!(s.input(0x41, false), (false, None));
        assert_eq!(s.input(0x44, false), (true, Some(KeyAction::DirectEnd)));
    }
    #[test]
    fn click_choice_rejects_moved_target_and_suspended_capture() {
        let mut s = KeyboardState::new(ShortcutBindings::default());
        s.launcher_open = true;
        assert_eq!(s.click_choice(false), Some(KeyAction::Dismiss));
        assert!(!s.busy);
        s.launcher_open = true;
        s.suspended = true;
        assert_eq!(s.click_choice(true), None);
        s.suspended = false;
        s.launcher_open = true;
        assert_eq!(s.click_choice(true), Some(KeyAction::Choose));
        assert_eq!(s.click_choice(true), None);
    }
}
