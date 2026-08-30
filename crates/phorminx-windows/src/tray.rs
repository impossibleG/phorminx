use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    HICON, IDI_APPLICATION, IsWindow, KillTimer, LoadIconW, MF_SEPARATOR, MF_STRING, MSG,
    PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, UnregisterClassW, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NCDESTROY, WM_NULL, WM_QUIT, WM_TIMER, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);

const CLASS_NAME: PCWSTR = w!("PhorminxSystemTray");
const ICON_ID: u32 = 1;
const WM_TRAY: u32 = WM_APP + 20;
const WM_STATUS: u32 = WM_APP + 21;
const RETRY_TIMER_ID: usize = 1;
const RETRY_INTERVAL_MS: u32 = 2_000;
const COMMAND_SETTINGS: usize = 1;
const COMMAND_QUIT: usize = 2;
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayEvent {
    OpenSettings,
    QuitRequested,
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayStatus {
    Loading = 0,
    Ready = 1,
    Listening = 2,
    Transcribing = 3,
    Error = 4,
}

impl TrayStatus {
    fn from_message(value: usize) -> Option<Self> {
        match value {
            0 => Some(Self::Loading),
            1 => Some(Self::Ready),
            2 => Some(Self::Listening),
            3 => Some(Self::Transcribing),
            4 => Some(Self::Error),
            _ => None,
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::Loading => "Phorminx - Loading",
            Self::Ready => "Phorminx - Ready",
            Self::Listening => "Phorminx - Listening",
            Self::Transcribing => "Phorminx - Transcribing",
            Self::Error => "Phorminx - Attention needed",
        }
    }
}

pub struct SystemTray {
    events: Receiver<TrayEvent>,
    window_bits: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl SystemTray {
    pub fn start() -> Result<Self, TrayError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(TrayError::AlreadyRunning);
        }

        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-tray".to_owned())
            .spawn(move || run_tray(event_tx, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                TrayError::Spawn(error)
            })?;

        let (window_bits, thread_id) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(TrayError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(TrayError::StartupChannelClosed);
            }
        };

        Ok(Self {
            events: event_rx,
            window_bits,
            thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<TrayEvent> {
        &self.events
    }

    pub fn set_status(&self, status: TrayStatus) -> Result<(), TrayError> {
        unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_STATUS,
                WPARAM(status as usize),
                LPARAM(0),
            )
        }
        .map_err(TrayError::PostStatus)
    }

    pub fn shutdown(mut self) -> Result<(), TrayError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), TrayError> {
        if self.thread.is_none() {
            return Ok(());
        }

        let post_result = unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if post_result.is_err() {
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }

        let join_result = self.thread.take().map(JoinHandle::join);
        self.window_bits = 0;
        self.thread_id = 0;
        if let Some(Err(_)) = join_result {
            return Err(TrayError::ThreadPanicked);
        }
        post_result.map_err(TrayError::PostClose)
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    events: Sender<TrayEvent>,
    taskbar_created: u32,
    icon: HICON,
    icon_added: bool,
    status: TrayStatus,
    retry_timer: usize,
}

fn run_tray(events: Sender<TrayEvent>, ready_tx: mpsc::SyncSender<Result<(usize, u32), String>>) {
    let result = unsafe { create_and_run(events, &ready_tx) };
    if let Err(error) = result {
        let _ = ready_tx.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    events: Sender<TrayEvent>,
    ready_tx: &mpsc::SyncSender<Result<(usize, u32), String>>,
) -> Result<(), TrayThreadError> {
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    if taskbar_created == 0 {
        return Err(TrayThreadError::Windows("RegisterWindowMessageW"));
    }

    let module = unsafe { GetModuleHandleW(None) }.map_err(TrayThreadError::Api)?;
    let instance = HINSTANCE(module.0);
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(TrayThreadError::Api(windows::core::Error::from_thread()));
    }

    let icon = match unsafe { LoadIconW(None, IDI_APPLICATION) } {
        Ok(icon) => icon,
        Err(error) => {
            let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
            return Err(TrayThreadError::Api(error));
        }
    };
    let mut state = Box::new(WindowState {
        events,
        taskbar_created,
        icon,
        icon_added: false,
        status: TrayStatus::Loading,
        retry_timer: 0,
    });
    let state_pointer = (&mut *state as *mut WindowState).cast();
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("Phorminx"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            Some(state_pointer),
        )
    } {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
            return Err(TrayThreadError::Api(error));
        }
    };

    if unsafe { !add_icon(hwnd, &mut state) } {
        let _ = unsafe { DestroyWindow(hwnd) };
        let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
        return Err(TrayThreadError::Windows(
            "Shell_NotifyIconW(NIM_ADD/NIM_SETVERSION)",
        ));
    }

    let thread_id = unsafe { GetCurrentThreadId() };
    if ready_tx.send(Ok((hwnd.0 as usize, thread_id))).is_err() {
        unsafe { remove_icon(hwnd, &mut state) };
        let _ = unsafe { DestroyWindow(hwnd) };
        let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
        return Ok(());
    }

    let mut message = MSG::default();
    let loop_result = loop {
        let code = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if code == -1 {
            break Err(TrayThreadError::Api(windows::core::Error::from_thread()));
        }
        if code == 0 {
            break Ok(());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    };

    unsafe { remove_icon(hwnd, &mut state) };
    if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    let unregister_result = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
    drop(state);
    loop_result.and(unregister_result.map_err(TrayThreadError::Api))
}

unsafe extern "system" fn window_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        return LRESULT(1);
    }

    if let Some(state) = unsafe { window_state(hwnd) }
        && message == state.taskbar_created
    {
        state.icon_added = false;
        if unsafe { !add_icon(hwnd, state) } && state.retry_timer == 0 {
            state.retry_timer =
                unsafe { SetTimer(Some(hwnd), RETRY_TIMER_ID, RETRY_INTERVAL_MS, None) };
        }
        return LRESULT(0);
    }

    match message {
        WM_TRAY => {
            handle_tray_callback(hwnd, wparam, lparam);
            LRESULT(0)
        }
        WM_STATUS => {
            if let Some(status) = TrayStatus::from_message(wparam.0)
                && let Some(state) = unsafe { window_state(hwnd) }
            {
                state.status = status;
                unsafe { update_tooltip(hwnd, state) };
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == RETRY_TIMER_ID
                && let Some(state) = unsafe { window_state(hwnd) }
                && unsafe { add_icon(hwnd, state) }
            {
                let _ = unsafe { KillTimer(Some(hwnd), RETRY_TIMER_ID) };
                state.retry_timer = 0;
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(state) = unsafe { window_state(hwnd) } {
                if state.retry_timer != 0 {
                    let _ = unsafe { KillTimer(Some(hwnd), state.retry_timer) };
                    state.retry_timer = 0;
                }
                unsafe { remove_icon(hwnd, state) };
            }
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn handle_tray_callback(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    let packed = lparam.0 as u32;
    let notification = packed & 0xffff;
    let icon_id = packed >> 16;
    if icon_id != ICON_ID {
        return;
    }

    match notification {
        WM_CONTEXTMENU => {
            let sender = unsafe { window_state(hwnd) }.map(|state| state.events.clone());
            if let Some(sender) = sender {
                let mut point = POINT {
                    x: signed_low_word(wparam.0),
                    y: signed_high_word(wparam.0),
                };
                if point.x == -1 && point.y == -1 {
                    let _ = unsafe { GetCursorPos(&mut point) };
                }
                if let Some(event) = unsafe { show_context_menu(hwnd, point) } {
                    let _ = sender.send(event);
                }
            }
        }
        NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONDBLCLK => {
            if let Some(state) = unsafe { window_state(hwnd) } {
                let _ = state.events.send(TrayEvent::OpenSettings);
            }
        }
        _ => {}
    }
}

unsafe fn show_context_menu(hwnd: HWND, point: POINT) -> Option<TrayEvent> {
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    let selected = (|| {
        unsafe { AppendMenuW(menu, MF_STRING, COMMAND_SETTINGS, w!("Settings...")) }.ok()?;
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) }.ok()?;
        unsafe { AppendMenuW(menu, MF_STRING, COMMAND_QUIT, w!("Quit Phorminx")) }.ok()?;
        let _ = unsafe { SetForegroundWindow(hwnd) };
        let command = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            )
        };
        let _ = unsafe { PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
        match command.0 as usize {
            COMMAND_SETTINGS => Some(TrayEvent::OpenSettings),
            COMMAND_QUIT => Some(TrayEvent::QuitRequested),
            _ => None,
        }
    })();
    let _ = unsafe { DestroyMenu(menu) };
    selected
}

unsafe fn add_icon(hwnd: HWND, state: &mut WindowState) -> bool {
    if state.icon_added {
        return true;
    }
    let mut data = notify_data(
        hwnd,
        state.icon,
        state.status,
        NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
    );
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        return false;
    }
    data.uFlags = NOTIFY_ICON_DATA_FLAGS::default();
    data.Anonymous = NOTIFYICONDATAW_0 {
        uVersion: NOTIFYICON_VERSION_4,
    };
    if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        return false;
    }
    state.icon_added = true;
    true
}

unsafe fn update_tooltip(hwnd: HWND, state: &WindowState) {
    if !state.icon_added {
        return;
    }
    let data = notify_data(hwnd, state.icon, state.status, NIF_TIP | NIF_SHOWTIP);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

unsafe fn remove_icon(hwnd: HWND, state: &mut WindowState) {
    if !state.icon_added {
        return;
    }
    let data = notify_data(
        hwnd,
        state.icon,
        state.status,
        NOTIFY_ICON_DATA_FLAGS::default(),
    );
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    state.icon_added = false;
}

fn notify_data(
    hwnd: HWND,
    icon: HICON,
    status: TrayStatus,
    flags: NOTIFY_ICON_DATA_FLAGS,
) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: flags,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        szTip: fixed_utf16(status.tooltip()),
        ..Default::default()
    }
}

fn fixed_utf16<const N: usize>(text: &str) -> [u16; N] {
    let mut destination = [0; N];
    for (slot, code_unit) in destination
        .iter_mut()
        .take(N.saturating_sub(1))
        .zip(text.encode_utf16())
    {
        *slot = code_unit;
    }
    destination
}

fn signed_low_word(value: usize) -> i32 {
    value as u16 as i16 as i32
}

fn signed_high_word(value: usize) -> i32 {
    ((value as u32 >> 16) as u16) as i16 as i32
}

unsafe fn window_state(hwnd: HWND) -> Option<&'static mut WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    unsafe { pointer.as_mut() }
}

fn window(bits: usize) -> HWND {
    HWND(bits as *mut core::ffi::c_void)
}

#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("the Phorminx system tray is already running")]
    AlreadyRunning,
    #[error("failed to start the tray thread: {0}")]
    Spawn(std::io::Error),
    #[error("the tray failed during startup: {0}")]
    Startup(String),
    #[error("the tray thread exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to update the tray status: {0}")]
    PostStatus(windows::core::Error),
    #[error("failed to close the tray: {0}")]
    PostClose(windows::core::Error),
    #[error("the tray thread panicked")]
    ThreadPanicked,
}

#[derive(Debug, thiserror::Error)]
enum TrayThreadError {
    #[error("{0} failed")]
    Windows(&'static str),
    #[error(transparent)]
    Api(windows::core::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_utf16_is_terminated_and_truncated() {
        assert_eq!(fixed_utf16::<4>("ab"), [b'a' as u16, b'b' as u16, 0, 0]);
        assert_eq!(
            fixed_utf16::<4>("abcdef"),
            [b'a' as u16, b'b' as u16, b'c' as u16, 0]
        );
    }

    #[test]
    fn tray_coordinates_are_decoded_as_signed_words() {
        let packed = ((20_u32 << 16) | u16::MAX as u32) as usize;
        assert_eq!(signed_low_word(packed), -1);
        assert_eq!(signed_high_word(packed), 20);
    }

    #[test]
    fn statuses_have_stable_message_values() {
        assert_eq!(TrayStatus::from_message(2), Some(TrayStatus::Listening));
        assert_eq!(TrayStatus::from_message(99), None);
    }
}
