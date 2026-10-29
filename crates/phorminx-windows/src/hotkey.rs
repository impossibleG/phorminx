use crate::shortcut::{ALT, CTRL, SHIFT, WIN};
use crate::{Shortcut, ShortcutBindings, TargetSnapshot};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
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
/// Launcher key (3-9) that opens the messaging contacts panel; 0 when unset.
static MESSAGING_SLOT: AtomicU8 = AtomicU8::new(0);
/// Launcher key (3-9) that opens the investigations panel; 0 when unset.
static INVESTIGATIONS_SLOT: AtomicU8 = AtomicU8::new(0);
/// Investigations panel keys: bits 0-9 are keys that do something, bits 16-25
/// keys that keep the panel open (picking an investigation, Switch).
static INVESTIGATION_KEYS: AtomicU32 = AtomicU32::new(0);
const WM_COMMAND: u32 = WM_APP + 1;
const WM_CHOOSE: u32 = WM_APP + 2;
const WM_STOP: u32 = WM_APP + 3;
thread_local! { static HOOK: RefCell<Option<HookState>> = const { RefCell::new(None) }; }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldEvent {
    Started {
        target: Option<TargetSnapshot>,
    },
    Ended,
    LauncherRequested {
        target: Option<TargetSnapshot>,
    },
    LauncherDictate {
        target: Option<TargetSnapshot>,
    },
    LauncherMeetings,
    LauncherAction(u8),
    /// The messaging key was chosen; the launcher stays open as the contacts
    /// panel, where 1-9 pick a contact.
    LauncherContacts,
    LauncherContact(u8),
    /// The investigations key was chosen; the launcher stays open as the
    /// investigations panel.
    LauncherInvestigations,
    /// A key (0-9) in the investigations panel. `panel_open` says whether the
    /// hook kept the panel open for it.
    LauncherInvestigation {
        key: u8,
        panel_open: bool,
    },
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
    /// Sets the launcher key (3-9) that opens the messaging contacts panel.
    /// The hook switches panels itself, so a fast key sequence never leaks.
    pub fn set_messaging_slot(&self, slot: Option<u8>) {
        MESSAGING_SLOT.store(
            slot.filter(|s| (3..=9).contains(s)).unwrap_or(0),
            Ordering::Release,
        );
    }
    /// Sets the launcher key (3-9) that opens the investigations panel.
    pub fn set_investigations_slot(&self, slot: Option<u8>) {
        INVESTIGATIONS_SLOT.store(
            slot.filter(|s| (3..=9).contains(s)).unwrap_or(0),
            Ordering::Release,
        );
    }
    /// Keys (0-9) the investigations panel answers to, and which of them keep
    /// it open. Published before the panel shows, so a fast key is judged by
    /// the panel the user is looking at.
    pub fn set_investigation_panel_keys(&self, keys: &[u8], stay_open: &[u8]) {
        INVESTIGATION_KEYS.store(investigation_key_mask(keys, stay_open), Ordering::Release);
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
pub(crate) fn choose_launcher_action(choice: u8) {
    let id = HOOK_THREAD_ID.load(Ordering::Acquire);
    if id != 0 {
        let _ = unsafe { PostThreadMessageW(id, WM_CHOOSE, WPARAM(choice as usize), LPARAM(0)) };
    }
}

/// Setup mode has no hook; suspending it is intentionally a no-op there.
pub fn set_global_shortcut_capture(capturing: bool) -> Result<(), HotkeyError> {
    CAPTURE_SUSPENDED.store(capturing, Ordering::Release);
    Ok(())
}
fn investigation_key_mask(keys: &[u8], stay_open: &[u8]) -> u32 {
    let bits = |keys: &[u8]| {
        keys.iter()
            .filter(|key| **key <= 9)
            .fold(0u32, |mask, key| mask | 1 << key)
    };
    bits(keys) | (bits(stay_open) & bits(keys)) << 16
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
            KeyAction::Meetings => {
                self.launcher_target = None;
                HoldEvent::LauncherMeetings
            }
            KeyAction::Action(slot) => {
                self.launcher_target = None;
                HoldEvent::LauncherAction(slot)
            }
            KeyAction::Contacts => {
                self.launcher_target = None;
                HoldEvent::LauncherContacts
            }
            KeyAction::Contact(slot) => {
                self.launcher_target = None;
                HoldEvent::LauncherContact(slot)
            }
            KeyAction::Investigations => {
                self.launcher_target = None;
                HoldEvent::LauncherInvestigations
            }
            KeyAction::InvestigationKey(key) => {
                self.launcher_target = None;
                HoldEvent::LauncherInvestigation {
                    key,
                    panel_open: self.keyboard.launcher_open,
                }
            }
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
                            if open {
                                state.keyboard.launcher_open = true;
                            } else {
                                state.keyboard.close_launcher();
                            }
                        }
                    }
                }
            } else if message.message == WM_CHOOSE {
                state.keyboard.suspended = CAPTURE_SUSPENDED.load(Ordering::Acquire);
                let target_current = state
                    .launcher_target
                    .is_none_or(|target| target.is_current());
                state.keyboard.messaging_slot = MESSAGING_SLOT.load(Ordering::Acquire);
                state.keyboard.load_investigation_keys();
                let choice = match message.wParam.0 {
                    1..=9 if state.keyboard.contacts_mode => {
                        KeyAction::Contact(message.wParam.0 as u8)
                    }
                    _ if state.keyboard.contacts_mode => return,
                    key @ 0..=9
                        if state.keyboard.investigations_mode
                            && state.keyboard.investigation_key_valid(key as u8) =>
                    {
                        KeyAction::InvestigationKey(key as u8)
                    }
                    _ if state.keyboard.investigations_mode => return,
                    0 => KeyAction::Action(0),
                    1 => KeyAction::Choose,
                    2 => KeyAction::Meetings,
                    3..=9 => KeyAction::Action(message.wParam.0 as u8),
                    _ => return,
                };
                if let Some(action) = state.keyboard.click_choice(target_current, choice) {
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
                state.keyboard.messaging_slot = MESSAGING_SLOT.load(Ordering::Acquire);
                state.keyboard.load_investigation_keys();
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
    Meetings,
    Action(u8),
    Contacts,
    Contact(u8),
    Investigations,
    InvestigationKey(u8),
    Dismiss,
}
struct KeyboardState {
    bindings: ShortcutBindings,
    down: [bool; 256],
    consumed: [bool; 256],
    direct_held: Option<Shortcut>,
    launcher_open: bool,
    pending_choice: Option<KeyAction>,
    busy: bool,
    suspended: bool,
    contacts_mode: bool,
    messaging_slot: u8,
    investigations_mode: bool,
    investigations_slot: u8,
    investigation_keys: u32,
}
impl KeyboardState {
    fn new(bindings: ShortcutBindings) -> Self {
        Self {
            bindings,
            down: [false; 256],
            consumed: [false; 256],
            direct_held: None,
            launcher_open: false,
            pending_choice: None,
            busy: false,
            suspended: false,
            contacts_mode: false,
            messaging_slot: 0,
            investigations_mode: false,
            investigations_slot: 0,
            investigation_keys: 0,
        }
    }
    fn close_launcher(&mut self) {
        self.launcher_open = false;
        self.pending_choice = None;
        self.contacts_mode = false;
        self.investigations_mode = false;
    }
    fn load_investigation_keys(&mut self) {
        self.investigations_slot = INVESTIGATIONS_SLOT.load(Ordering::Acquire);
        self.investigation_keys = INVESTIGATION_KEYS.load(Ordering::Acquire);
    }
    fn investigation_key_valid(&self, key: u8) -> bool {
        key <= 9 && self.investigation_keys & 1 << key != 0
    }
    fn investigation_key_stays(&self, key: u8) -> bool {
        key <= 9 && self.investigation_keys & 1 << (key + 16) != 0
    }
    /// The messaging and investigations keys turn the open launcher into
    /// their panel instead of closing it.
    fn panel_choice(&self, choice: KeyAction) -> KeyAction {
        let in_panel = self.contacts_mode || self.investigations_mode;
        match choice {
            KeyAction::Action(slot) if !in_panel && slot != 0 && slot == self.messaging_slot => {
                KeyAction::Contacts
            }
            KeyAction::Action(slot)
                if !in_panel && slot != 0 && slot == self.investigations_slot =>
            {
                KeyAction::Investigations
            }
            other => other,
        }
    }
    /// Applies a chosen launcher entry: panels, and investigation keys that
    /// lead to another panel, keep the launcher open; everything else closes it.
    fn finish_choice(&mut self, choice: KeyAction) {
        match choice {
            KeyAction::Contacts => {
                self.pending_choice = None;
                self.contacts_mode = true;
            }
            KeyAction::Investigations => {
                self.pending_choice = None;
                self.investigations_mode = true;
            }
            KeyAction::InvestigationKey(key) if self.investigation_key_stays(key) => {
                self.pending_choice = None;
            }
            _ => {
                self.close_launcher();
                self.busy = choice == KeyAction::Choose;
            }
        }
    }
    fn click_choice(&mut self, target_current: bool, choice: KeyAction) -> Option<KeyAction> {
        if !self.launcher_open || self.suspended {
            self.close_launcher();
            return None;
        }
        let choice = self.panel_choice(choice);
        if !target_current {
            self.close_launcher();
            return Some(KeyAction::Dismiss);
        }
        self.finish_choice(choice);
        Some(choice)
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
        if let Some(choice) = self.pending_choice
            && self.modifiers() == 0
        {
            self.finish_choice(choice);
            return (consumed, Some(choice));
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
        if self.launcher_open
            && self.contacts_mode
            && matches!(key, 0x30..=0x39 | 0x60..=0x69 | 0x0d | 0x1b)
        {
            self.consumed[index] = true;
            let choice = match key {
                0x1b => KeyAction::Dismiss,
                0x31..=0x39 => KeyAction::Contact((key - 0x30) as u8),
                0x61..=0x69 => KeyAction::Contact((key - 0x60) as u8),
                // Zero and Enter pick no contact; swallow them so they never
                // reach the focused window.
                _ => return (true, None),
            };
            if modifiers != 0 && key != 0x1b {
                self.pending_choice = Some(choice);
                return (true, None);
            }
            self.close_launcher();
            return (true, Some(choice));
        }
        if self.launcher_open
            && self.investigations_mode
            && matches!(key, 0x30..=0x39 | 0x60..=0x69 | 0x0d | 0x1b)
        {
            self.consumed[index] = true;
            let digit = match key {
                0x1b => {
                    self.close_launcher();
                    return (true, Some(KeyAction::Dismiss));
                }
                0x30..=0x39 => (key - 0x30) as u8,
                0x60..=0x69 => (key - 0x60) as u8,
                // Enter picks nothing here; swallow it like unused digits.
                _ => return (true, None),
            };
            if !self.investigation_key_valid(digit) {
                return (true, None);
            }
            let choice = KeyAction::InvestigationKey(digit);
            if modifiers != 0 {
                self.pending_choice = Some(choice);
                return (true, None);
            }
            self.finish_choice(choice);
            return (true, Some(choice));
        }
        if self.launcher_open && matches!(key, 0x30..=0x39 | 0x60..=0x69 | 0x0d | 0x1b) {
            let choice = match key {
                0x1b => KeyAction::Dismiss,
                0x32 | 0x62 => KeyAction::Meetings,
                // Zero is a built-in feedback action; saved HTTP slots remain 3..=9.
                0x30 | 0x60 => KeyAction::Action(0),
                0x33..=0x39 => KeyAction::Action((key - 0x30) as u8),
                0x63..=0x69 => KeyAction::Action((key - 0x60) as u8),
                _ => KeyAction::Choose,
            };
            let choice = self.panel_choice(choice);
            self.consumed[index] = true;
            if modifiers != 0 && key != 0x1b {
                self.pending_choice = Some(choice);
                return (true, None);
            }
            self.finish_choice(choice);
            return (true, Some(choice));
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
            self.close_launcher();
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
    fn all_action_digits_and_numpad_digits_preserve_slot_and_suppress_repeat_release() {
        for slot in 3..=9 {
            for base in [0x30, 0x60] {
                let key = base + u32::from(slot);
                let mut state = KeyboardState::new(ShortcutBindings::default());
                state.launcher_open = true;
                assert_eq!(
                    state.input(key, true),
                    (true, Some(KeyAction::Action(slot)))
                );
                assert_eq!(state.input(key, true), (true, None));
                assert_eq!(state.input(key, false), (true, None));
                assert!(!state.launcher_open);
                assert_eq!(state.input(key, true), (false, None));
            }
        }
    }

    #[test]
    fn numbered_actions_wait_for_all_modifiers_and_escape_cancels_pending_action() {
        for slot in 3..=9 {
            let mut state = KeyboardState::new(ShortcutBindings::default());
            state.launcher_open = true;
            state.input(0x11, true);
            state.input(0x12, true);
            let key = 0x30 + u32::from(slot);
            assert_eq!(state.input(key, true), (true, None));
            assert_eq!(state.input(key, false), (true, None));
            assert_eq!(state.input(0x11, false), (false, None));
            assert_eq!(
                state.input(0x12, false),
                (false, Some(KeyAction::Action(slot)))
            );
            state.launcher_open = true;
            state.input(0x11, true);
            state.input(key, true);
            assert_eq!(state.input(0x1b, true), (true, Some(KeyAction::Dismiss)));
            assert_eq!(state.input(0x11, false), (false, None));
        }
    }

    #[test]
    fn numbered_mouse_actions_keep_slot_and_reject_stale_target() {
        for slot in 3..=9 {
            let mut state = KeyboardState::new(ShortcutBindings::default());
            state.launcher_open = true;
            assert_eq!(
                state.click_choice(true, KeyAction::Action(slot)),
                Some(KeyAction::Action(slot))
            );
            assert_eq!(state.click_choice(true, KeyAction::Action(slot)), None);
            state.launcher_open = true;
            assert_eq!(
                state.click_choice(false, KeyAction::Action(slot)),
                Some(KeyAction::Dismiss)
            );
        }
    }
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
        assert!(s.pending_choice.is_some());
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
        assert_eq!(
            s.click_choice(false, KeyAction::Choose),
            Some(KeyAction::Dismiss)
        );
        assert!(!s.busy);
        s.launcher_open = true;
        s.suspended = true;
        assert_eq!(s.click_choice(true, KeyAction::Choose), None);
        s.suspended = false;
        s.launcher_open = true;
        assert_eq!(
            s.click_choice(true, KeyAction::Choose),
            Some(KeyAction::Choose)
        );
        assert_eq!(s.click_choice(true, KeyAction::Choose), None);
    }

    #[test]
    fn workspace_choices_consume_repeat_release_without_starting_dictation() {
        for (key, choice) in [
            (0x30, KeyAction::Action(0)),
            (0x60, KeyAction::Action(0)),
            (0x32, KeyAction::Meetings),
            (0x62, KeyAction::Meetings),
            (0x33, KeyAction::Action(3)),
            (0x63, KeyAction::Action(3)),
        ] {
            let mut state = KeyboardState::new(ShortcutBindings::default());
            state.launcher_open = true;
            assert_eq!(state.input(key, true), (true, Some(choice)));
            assert_eq!(state.input(key, true), (true, None));
            assert_eq!(state.input(key, false), (true, None));
            assert!(!state.busy);
            assert!(!state.launcher_open);
            assert_eq!(state.input(key, true), (false, None));
        }
    }

    #[test]
    fn pending_workspace_choice_retains_identity_until_modifiers_release() {
        for (key, choice) in [(0x32, KeyAction::Meetings), (0x33, KeyAction::Action(3))] {
            let mut state = KeyboardState::new(ShortcutBindings::default());
            state.launcher_open = true;
            state.input(0x11, true);
            assert_eq!(state.input(key, true), (true, None));
            assert_eq!(state.input(key, false), (true, None));
            assert_eq!(state.input(0x11, false), (false, Some(choice)));
            assert!(!state.busy);
        }
    }

    #[test]
    fn workspace_mouse_choices_keep_focus_and_capture_safety_guards() {
        for choice in [KeyAction::Meetings, KeyAction::Action(3)] {
            let mut state = KeyboardState::new(ShortcutBindings::default());
            state.launcher_open = true;
            assert_eq!(state.click_choice(false, choice), Some(KeyAction::Dismiss));
            state.launcher_open = true;
            assert_eq!(state.click_choice(true, choice), Some(choice));
            assert!(!state.busy);
            assert_eq!(state.click_choice(true, choice), None);
            state.launcher_open = true;
            state.suspended = true;
            assert_eq!(state.click_choice(true, choice), None);
        }
    }

    fn messaging_state(slot: u8) -> KeyboardState {
        let mut state = KeyboardState::new(ShortcutBindings::default());
        state.messaging_slot = slot;
        state.launcher_open = true;
        state
    }

    #[test]
    fn messaging_key_turns_the_launcher_into_the_contacts_panel() {
        let mut state = messaging_state(5);
        assert_eq!(state.input(0x35, true), (true, Some(KeyAction::Contacts)));
        assert!(state.launcher_open && state.contacts_mode);
        assert_eq!(state.input(0x35, false), (true, None));
        // In the contacts panel 1 and 2 pick contacts, not dictation or meetings.
        assert_eq!(state.input(0x31, true), (true, Some(KeyAction::Contact(1))));
        assert!(!state.launcher_open && !state.contacts_mode);
        assert_eq!(state.input(0x31, false), (true, None));
        let mut state = messaging_state(5);
        state.input(0x35, true);
        assert_eq!(state.input(0x62, true), (true, Some(KeyAction::Contact(2))));
    }

    #[test]
    fn contacts_panel_swallows_zero_and_enter_and_escape_closes_it() {
        let mut state = messaging_state(9);
        state.input(0x39, true);
        state.input(0x39, false);
        assert_eq!(state.input(0x30, true), (true, None));
        assert_eq!(state.input(0x0d, true), (true, None));
        assert!(state.contacts_mode);
        assert_eq!(state.input(0x1b, true), (true, Some(KeyAction::Dismiss)));
        assert!(!state.launcher_open && !state.contacts_mode);
    }

    #[test]
    fn other_slots_keep_running_actions_when_messaging_has_a_key() {
        let mut state = messaging_state(5);
        assert_eq!(state.input(0x34, true), (true, Some(KeyAction::Action(4))));
        assert!(!state.contacts_mode);
        let mut state = messaging_state(0);
        assert_eq!(state.input(0x35, true), (true, Some(KeyAction::Action(5))));
    }

    #[test]
    fn messaging_key_with_modifiers_opens_contacts_on_release() {
        let mut state = messaging_state(6);
        state.input(0x11, true);
        assert_eq!(state.input(0x36, true), (true, None));
        assert_eq!(state.input(0x11, false), (false, Some(KeyAction::Contacts)));
        assert!(state.launcher_open && state.contacts_mode);
    }

    #[test]
    fn clicking_in_the_contacts_panel_picks_a_contact() {
        let mut state = messaging_state(7);
        assert_eq!(
            state.click_choice(true, KeyAction::Action(7)),
            Some(KeyAction::Contacts)
        );
        assert!(state.launcher_open && state.contacts_mode);
        assert_eq!(
            state.click_choice(true, KeyAction::Contact(3)),
            Some(KeyAction::Contact(3))
        );
        assert!(!state.launcher_open);
    }

    /// The investigations panel on key 9 with the given panel keys.
    fn investigations_state(keys: &[u8], stay_open: &[u8]) -> KeyboardState {
        let mut state = KeyboardState::new(ShortcutBindings::default());
        state.investigations_slot = 9;
        state.investigation_keys = investigation_key_mask(keys, stay_open);
        state.launcher_open = true;
        state
    }

    #[test]
    fn investigation_key_mask_ignores_stay_bits_for_unused_keys() {
        let mask = investigation_key_mask(&[1, 2, 9], &[1, 8]);
        let mut state = KeyboardState::new(ShortcutBindings::default());
        state.investigation_keys = mask;
        assert!(state.investigation_key_valid(1) && state.investigation_key_stays(1));
        assert!(state.investigation_key_valid(9) && !state.investigation_key_stays(9));
        assert!(!state.investigation_key_valid(8) && !state.investigation_key_stays(8));
        assert!(!state.investigation_key_valid(10));
    }

    #[test]
    fn investigations_key_opens_the_panel_and_its_keys_never_leak() {
        // The capture panel: 1-5, 8 Switch (stays), 9 Stop, 0 Open.
        let mut state = investigations_state(&[0, 1, 2, 3, 4, 5, 8, 9], &[8]);
        assert_eq!(
            state.input(0x39, true),
            (true, Some(KeyAction::Investigations))
        );
        assert!(state.launcher_open && state.investigations_mode);
        assert_eq!(state.input(0x39, false), (true, None));
        // 8 switches to the list: the panel stays open.
        assert_eq!(
            state.input(0x38, true),
            (true, Some(KeyAction::InvestigationKey(8)))
        );
        assert!(state.launcher_open && state.investigations_mode);
        state.input(0x38, false);
        // Unused keys and Enter are swallowed without an action.
        assert_eq!(state.input(0x36, true), (true, None));
        assert_eq!(state.input(0x0d, true), (true, None));
        assert!(state.investigations_mode);
        // 1 (Voice note) closes the panel; 1 never starts plain dictation.
        assert_eq!(
            state.input(0x61, true),
            (true, Some(KeyAction::InvestigationKey(1)))
        );
        assert!(!state.launcher_open && !state.investigations_mode && !state.busy);
        assert_eq!(state.input(0x61, false), (true, None));
    }

    #[test]
    fn picking_an_investigation_keeps_the_panel_and_escape_closes_it() {
        let mut state = investigations_state(&[1, 2, 9], &[1, 2]);
        state.input(0x39, true);
        state.input(0x39, false);
        assert_eq!(
            state.input(0x32, true),
            (true, Some(KeyAction::InvestigationKey(2)))
        );
        assert!(state.launcher_open && state.investigations_mode);
        state.input(0x32, false);
        assert_eq!(state.input(0x1b, true), (true, Some(KeyAction::Dismiss)));
        assert!(!state.launcher_open && !state.investigations_mode);
        // Reopening the launcher starts on the main panel again.
        let mut state = investigations_state(&[1, 9], &[1]);
        state.investigations_mode = true;
        state.launcher_open = false;
        state.input(0x11, true);
        state.input(0x12, true);
        assert_eq!(state.input(0x20, true), (true, Some(KeyAction::Launcher)));
        assert!(!state.investigations_mode);
    }

    #[test]
    fn investigation_keys_wait_for_modifiers_and_clicks_follow_the_mask() {
        let mut state = investigations_state(&[2, 9], &[]);
        state.input(0x39, true);
        state.input(0x39, false);
        state.input(0x11, true);
        assert_eq!(state.input(0x32, true), (true, None));
        assert_eq!(
            state.input(0x11, false),
            (false, Some(KeyAction::InvestigationKey(2)))
        );
        assert!(!state.launcher_open);
        let mut state = investigations_state(&[1, 9], &[1]);
        assert_eq!(
            state.click_choice(true, KeyAction::Action(9)),
            Some(KeyAction::Investigations)
        );
        assert_eq!(
            state.click_choice(true, KeyAction::InvestigationKey(1)),
            Some(KeyAction::InvestigationKey(1))
        );
        assert!(state.launcher_open && state.investigations_mode);
    }

    #[test]
    fn messaging_and_investigations_slots_coexist() {
        let mut state = investigations_state(&[1], &[]);
        state.messaging_slot = 3;
        assert_eq!(state.input(0x33, true), (true, Some(KeyAction::Contacts)));
        // Inside the contacts panel the investigations key picks a contact.
        state.input(0x33, false);
        assert_eq!(state.input(0x39, true), (true, Some(KeyAction::Contact(9))));
        let mut state = investigations_state(&[1], &[]);
        state.messaging_slot = 3;
        assert_eq!(state.input(0x34, true), (true, Some(KeyAction::Action(4))));
    }
}
