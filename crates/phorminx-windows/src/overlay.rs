use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, CreateSolidBrush, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, GetStockObject,
    HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetClientRect, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, HTTRANSPARENT,
    HWND_TOPMOST, IsWindow, KillTimer, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

static ACTIVE: AtomicBool = AtomicBool::new(false);

const CLASS_NAME: windows::core::PCWSTR = w!("PhorminxStatusOverlay");
const WM_STATUS: u32 = WM_APP + 10;
const INITIAL_TIMER_ID: usize = 1;

#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayStatus {
    Hidden = 0,
    Loading = 1,
    Ready = 2,
    Listening = 3,
    Transcribing = 4,
    Inserted = 5,
    ClipboardReady = 6,
    NoSpeech = 7,
    Error = 8,
    Cleaning = 9,
}

impl OverlayStatus {
    fn from_message(value: usize) -> Option<Self> {
        match value {
            0 => Some(Self::Hidden),
            1 => Some(Self::Loading),
            2 => Some(Self::Ready),
            3 => Some(Self::Listening),
            4 => Some(Self::Transcribing),
            5 => Some(Self::Inserted),
            6 => Some(Self::ClipboardReady),
            7 => Some(Self::NoSpeech),
            8 => Some(Self::Error),
            9 => Some(Self::Cleaning),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hidden => "",
            Self::Loading => "Phorminx is loading...",
            Self::Ready => "Phorminx is ready",
            Self::Listening => "Listening...",
            Self::Transcribing => "Transcribing...",
            Self::Cleaning => "Cleaning locally...",
            Self::Inserted => "Inserted",
            Self::ClipboardReady => "Ready to paste",
            Self::NoSpeech => "No clear speech detected",
            Self::Error => "Something went wrong",
        }
    }

    fn hide_after_ms(self) -> Option<u32> {
        match self {
            Self::Ready => Some(1_200),
            Self::Inserted => Some(1_500),
            Self::ClipboardReady => Some(4_000),
            Self::NoSpeech => Some(2_500),
            Self::Error => Some(5_000),
            Self::Hidden
            | Self::Loading
            | Self::Listening
            | Self::Transcribing
            | Self::Cleaning => None,
        }
    }
}

pub struct StatusOverlay {
    window_bits: usize,
    thread: Option<JoinHandle<()>>,
}

impl StatusOverlay {
    pub fn start() -> Result<Self, OverlayError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(OverlayError::AlreadyRunning);
        }

        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-overlay".to_owned())
            .spawn(move || run_overlay(ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                OverlayError::Spawn(error)
            })?;

        let window_bits = match ready_rx.recv() {
            Ok(Ok(window_bits)) => window_bits,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(OverlayError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(OverlayError::StartupChannelClosed);
            }
        };

        Ok(Self {
            window_bits,
            thread: Some(thread),
        })
    }

    pub fn set(&self, status: OverlayStatus) -> Result<(), OverlayError> {
        unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_STATUS,
                WPARAM(status as usize),
                LPARAM(0),
            )
        }
        .map_err(OverlayError::PostStatus)
    }

    pub fn shutdown(mut self) -> Result<(), OverlayError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), OverlayError> {
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
        let join_result = self.thread.take().map(JoinHandle::join);
        self.window_bits = 0;

        if let Some(Err(_)) = join_result {
            return Err(OverlayError::ThreadPanicked);
        }
        post_result.map_err(OverlayError::PostClose)
    }
}

impl Drop for StatusOverlay {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    status: OverlayStatus,
    timer_id: usize,
    next_timer_id: usize,
}

fn run_overlay(ready_tx: mpsc::SyncSender<Result<usize, String>>) {
    let result = unsafe { create_and_run(&ready_tx) };
    if let Err(error) = result {
        let _ = ready_tx.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    ready_tx: &mpsc::SyncSender<Result<usize, String>>,
) -> windows::core::Result<()> {
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let module = unsafe { GetModuleHandleW(None)? };
    let instance = HINSTANCE(module.0);
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }

    let mut state = Box::new(WindowState {
        status: OverlayStatus::Hidden,
        timer_id: 0,
        next_timer_id: INITIAL_TIMER_ID,
    });
    let state_pointer = (&mut *state as *mut WindowState).cast();
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            CLASS_NAME,
            w!("Phorminx"),
            WS_POPUP,
            0,
            0,
            300,
            52,
            None,
            None,
            Some(instance),
            Some(state_pointer),
        )
    } {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
            return Err(error);
        }
    };

    if ready_tx.send(Ok(hwnd.0 as usize)).is_err() {
        let _ = unsafe { DestroyWindow(hwnd) };
        let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
        return Ok(());
    }

    let mut message = MSG::default();
    let loop_result = loop {
        let code = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if code == -1 {
            break Err(windows::core::Error::from_thread());
        }
        if code == 0 {
            break Ok(());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    };

    if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    let unregister_result = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
    drop(state);
    loop_result.and(unregister_result)
}

unsafe extern "system" fn window_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            LRESULT(1)
        }
        WM_STATUS => {
            if let Some(status) = OverlayStatus::from_message(wparam.0)
                && let Some(state) = unsafe { window_state(hwnd) }
            {
                unsafe { update_window(hwnd, state, status) };
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if let Some(state) = unsafe { window_state(hwnd) }
                && wparam.0 == state.timer_id
            {
                let _ = unsafe { KillTimer(Some(hwnd), state.timer_id) };
                state.timer_id = 0;
                state.status = OverlayStatus::Hidden;
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe { paint_window(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_CLOSE => {
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

unsafe fn update_window(hwnd: HWND, state: &mut WindowState, status: OverlayStatus) {
    if state.timer_id != 0 {
        let _ = unsafe { KillTimer(Some(hwnd), state.timer_id) };
        state.timer_id = 0;
    }
    state.status = status;

    if status == OverlayStatus::Hidden {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return;
    }

    let foreground = unsafe { GetForegroundWindow() };
    let monitor_target = if foreground.is_invalid() {
        hwnd
    } else {
        foreground
    };
    let monitor = unsafe { MonitorFromWindow(monitor_target, MONITOR_DEFAULTTONEAREST) };
    let mut monitor_info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut monitor_info) };
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = |value: i32| value * dpi as i32 / 96;
    let width = scale(300);
    let height = scale(52);
    let margin = scale(48);
    let work = monitor_info.rcWork;
    let x = work.left + (work.right - work.left - width) / 2;
    let y = work.bottom - height - margin;
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    };
    let radius = scale(18);
    let region = unsafe { CreateRoundRectRgn(0, 0, width + 1, height + 1, radius, radius) };
    if !region.is_invalid() && unsafe { SetWindowRgn(hwnd, Some(region), true) } == 0 {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }

    if let Some(milliseconds) = status.hide_after_ms() {
        let timer_id = state.next_timer_id.max(INITIAL_TIMER_ID);
        state.next_timer_id = timer_id.wrapping_add(1).max(INITIAL_TIMER_ID);
        state.timer_id = unsafe { SetTimer(Some(hwnd), timer_id, milliseconds, None) };
    }
}

unsafe fn paint_window(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let device_context = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut rectangle = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rectangle) }.is_ok() {
        let background = unsafe { CreateSolidBrush(COLORREF(0x0022_2222)) };
        unsafe {
            FillRect(device_context, &rectangle, background);
            let _ = DeleteObject(HGDIOBJ(background.0));
            SetBkMode(device_context, TRANSPARENT);
            SetTextColor(device_context, COLORREF(0x00F4_F4F4));
        }

        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        let previous_font = unsafe { SelectObject(device_context, font) };
        let status = unsafe { window_state(hwnd) }
            .map(|state| state.status)
            .unwrap_or(OverlayStatus::Hidden);
        let mut text = status.label().encode_utf16().collect::<Vec<_>>();
        unsafe {
            DrawTextW(
                device_context,
                &mut text,
                &mut rectangle,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(device_context, previous_font);
        }
    }
    unsafe {
        let _ = EndPaint(hwnd, &paint);
    }
}

unsafe fn window_state(hwnd: HWND) -> Option<&'static mut WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    unsafe { pointer.as_mut() }
}

fn window(bits: usize) -> HWND {
    HWND(bits as *mut core::ffi::c_void)
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("the Phorminx status overlay is already running")]
    AlreadyRunning,
    #[error("failed to start the overlay thread: {0}")]
    Spawn(std::io::Error),
    #[error("the overlay failed during startup: {0}")]
    Startup(String),
    #[error("the overlay thread exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to update the overlay: {0}")]
    PostStatus(windows::core::Error),
    #[error("failed to close the overlay: {0}")]
    PostClose(windows::core::Error),
    #[error("the overlay thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_status_messages() {
        assert_eq!(
            OverlayStatus::from_message(3),
            Some(OverlayStatus::Listening)
        );
        assert_eq!(OverlayStatus::from_message(99), None);
    }

    #[test]
    fn active_statuses_are_sticky_and_results_auto_hide() {
        assert_eq!(OverlayStatus::Listening.hide_after_ms(), None);
        assert_eq!(OverlayStatus::Transcribing.hide_after_ms(), None);
        assert_eq!(OverlayStatus::Cleaning.hide_after_ms(), None);
        assert!(OverlayStatus::Inserted.hide_after_ms().is_some());
        assert!(OverlayStatus::ClipboardReady.hide_after_ms().is_some());
    }
}
