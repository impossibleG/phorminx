use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontIndirectW, CreateRoundRectRgn, CreateSolidBrush, DEFAULT_GUI_FONT,
    DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetMonitorInfoW, GetStockObject, HGDIOBJ, InvalidateRect, LOGFONTW,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, GWLP_USERDATA, GetClientRect, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IsWindow, KillTimer, LoadIconW, MA_NOACTIVATE, MSG,
    PostMessageW, PostQuitMessage, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    UnregisterClassW, WM_APP, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

static ACTIVE: AtomicBool = AtomicBool::new(false);

const CLASS_NAME: windows::core::PCWSTR = w!("PhorminxStatusOverlay");
const WM_STATUS: u32 = WM_APP + 10;
const WM_LAUNCHER_ACTIONS: u32 = WM_APP + 11;
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
    LauncherLight = 10,
    LauncherDark = 11,
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
            10 => Some(Self::LauncherLight),
            11 => Some(Self::LauncherDark),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hidden => "",
            Self::LauncherLight | Self::LauncherDark => "Dictate",
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
            | Self::LauncherLight
            | Self::LauncherDark
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
    launcher_actions: Arc<Mutex<Vec<(u8, String)>>>,
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
        let launcher_actions = Arc::new(Mutex::new(Vec::new()));
        let thread_actions = launcher_actions.clone();
        let thread = thread::Builder::new()
            .name("phorminx-overlay".to_owned())
            .spawn(move || run_overlay(ready_tx, thread_actions))
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
            launcher_actions,
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

    /// Only public action names and assigned keys cross into this overlay thread.
    /// Credentials, endpoints, prompts, and transcript text are never supplied.
    pub fn set_launcher_actions(&self, actions: Vec<(u8, String)>) -> Result<(), OverlayError> {
        *self
            .launcher_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = clean_launcher_actions(actions);
        unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_LAUNCHER_ACTIONS,
                WPARAM(0),
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
    launcher_actions: Arc<Mutex<Vec<(u8, String)>>>,
    /// Snapshot used for both painting and hit testing. A changed configuration
    /// never maps an old visible row to a new action before the redraw message.
    launcher_rows: Vec<(u8, String)>,
    launcher_row_height: i32,
}

fn run_overlay(
    ready_tx: mpsc::SyncSender<Result<usize, String>>,
    actions: Arc<Mutex<Vec<(u8, String)>>>,
) {
    let result = unsafe { create_and_run(&ready_tx, actions) };
    if let Err(error) = result {
        let _ = ready_tx.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    ready_tx: &mpsc::SyncSender<Result<usize, String>>,
    actions: Arc<Mutex<Vec<(u8, String)>>>,
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
        launcher_actions: actions,
        launcher_rows: launcher_rows(&[]),
        launcher_row_height: 54,
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
        WM_LAUNCHER_ACTIONS => {
            if let Some(state) = unsafe { window_state(hwnd) }
                && is_launcher(state.status)
            {
                unsafe { update_window(hwnd, state, state.status) };
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
        WM_NCHITTEST => LRESULT(
            if let Some(state) = unsafe { window_state(hwnd) }
                && is_launcher(state.status)
            {
                HTCLIENT as isize
            } else {
                HTTRANSPARENT as isize
            },
        ),
        WM_LBUTTONUP => {
            if let Some(state) = unsafe { window_state(hwnd) }
                && is_launcher(state.status)
            {
                let x = (lparam.0 as u16) as i16 as i32;
                let y = ((lparam.0 >> 16) as u16) as i16 as i32;
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                if let Some(choice) =
                    launcher_hit_test(x, y, dpi, &state.launcher_rows, state.launcher_row_height)
                {
                    crate::hotkey::choose_launcher_action(choice);
                }
            }
            LRESULT(0)
        }
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
    let launcher = is_launcher(status);
    if launcher {
        state.launcher_rows = launcher_rows(
            &state
                .launcher_actions
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
    let width = scale(if launcher { 360 } else { 300 });
    let margin = scale(48);
    let work = monitor_info.rcWork;
    if launcher {
        let available =
            ((work.bottom - work.top - margin - scale(12)).max(scale(180))) * 96 / dpi as i32;
        state.launcher_row_height = launcher_row_height(state.launcher_rows.len(), available);
    }
    let height = scale(if launcher {
        launcher_height(state.launcher_rows.len(), state.launcher_row_height)
    } else {
        52
    });
    let x = if launcher {
        (work.right - width - scale(24)).max(work.left)
    } else {
        work.left + (work.right - work.left - width) / 2
    };
    let y = (work.bottom - height - margin).max(work.top);
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
    let status = unsafe { window_state(hwnd) }
        .map(|s| s.status)
        .unwrap_or(OverlayStatus::Hidden);
    if is_launcher(status) {
        unsafe { paint_launcher(hwnd, device_context, status) };
        unsafe {
            let _ = EndPaint(hwnd, &paint);
        }
        return;
    }
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

fn is_launcher(status: OverlayStatus) -> bool {
    matches!(
        status,
        OverlayStatus::LauncherLight | OverlayStatus::LauncherDark
    )
}

fn clean_launcher_actions(actions: Vec<(u8, String)>) -> Vec<(u8, String)> {
    let mut result = Vec::new();
    for (slot, name) in actions {
        if !(3..=9).contains(&slot) || result.iter().any(|(existing, _)| *existing == slot) {
            continue;
        }
        let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
        if !name.trim().is_empty() {
            result.push((slot, name.trim().to_owned()));
        }
    }
    result.sort_by_key(|(slot, _)| *slot);
    result
}

fn launcher_rows(actions: &[(u8, String)]) -> Vec<(u8, String)> {
    let mut rows = vec![(1, "Dictate".into()), (2, "Start meeting".into())];
    rows.extend_from_slice(actions);
    rows
}

fn launcher_row_height(row_count: usize, available_height: i32) -> i32 {
    ((available_height - 104) / row_count.max(1) as i32 - 6).clamp(22, 54)
}

fn launcher_height(row_count: usize, row_height: i32) -> i32 {
    104 + row_count as i32 * (row_height + 6)
}

fn launcher_hit_test(
    x: i32,
    y: i32,
    dpi: u32,
    rows: &[(u8, String)],
    row_height: i32,
) -> Option<u8> {
    let scale = |value: i32| value * dpi as i32 / 96;
    if !(scale(16)..scale(344)).contains(&x) {
        return None;
    }
    rows.iter().enumerate().find_map(|(index, (choice, _))| {
        let top = 62 + index as i32 * (row_height + 6);
        (scale(top)..scale(top + row_height))
            .contains(&y)
            .then_some(*choice)
    })
}

unsafe fn paint_launcher(
    hwnd: HWND,
    dc: windows::Win32::Graphics::Gdi::HDC,
    status: OverlayStatus,
) {
    let (rows, row_height) = unsafe { window_state(hwnd) }
        .map(|s| (s.launcher_rows.clone(), s.launcher_row_height))
        .unwrap_or_else(|| (launcher_rows(&[]), 54));
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = |value: i32| value * dpi as i32 / 96;
    let dark = status == OverlayStatus::LauncherDark;
    let background = if dark { 0x001A1817 } else { 0x00F0F3F5 };
    let surface = if dark { 0x002A2724 } else { 0x00E2E7EA };
    let foreground = if dark { 0x00E5E8EB } else { 0x0023211F };
    let muted = if dark { 0x00ADB5BE } else { 0x00636970 };
    let fill = |rectangle: RECT, color: u32| unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        FillRect(dc, &rectangle, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    };
    let rect = |l, t, r, b| RECT {
        left: scale(l),
        top: scale(t),
        right: scale(r),
        bottom: scale(b),
    };
    let height = launcher_height(rows.len(), row_height);
    fill(rect(0, 0, 360, height), background);
    for index in 0..rows.len() {
        let top = 62 + index as i32 * (row_height + 6);
        fill(rect(16, top, 344, top + row_height), surface);
    }
    fill(rect(16, 62, 19, 62 + row_height), 0x004F79A4);
    unsafe {
        SetBkMode(dc, TRANSPARENT);
    }
    let mut font_description = LOGFONTW {
        lfHeight: -scale(14),
        lfWeight: 400,
        ..Default::default()
    };
    for (index, code) in "Segoe UI".encode_utf16().enumerate() {
        font_description.lfFaceName[index] = code;
    }
    let owned_font = unsafe { CreateFontIndirectW(&font_description) };
    let font = if owned_font.is_invalid() {
        unsafe { GetStockObject(DEFAULT_GUI_FONT) }
    } else {
        HGDIOBJ(owned_font.0)
    };
    let previous = unsafe { SelectObject(dc, font) };
    let text = |value: &str, mut bounds: RECT, color: u32| unsafe {
        SetTextColor(dc, COLORREF(color));
        DrawTextW(
            dc,
            &mut value.encode_utf16().collect::<Vec<_>>(),
            &mut bounds,
            DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    };
    if let Ok(module) = unsafe { GetModuleHandleW(None) }
        && let Ok(icon) = unsafe {
            LoadIconW(
                Some(HINSTANCE(module.0)),
                windows::core::PCWSTR(std::ptr::without_provenance::<u16>(1)),
            )
        }
    {
        let _ = unsafe {
            DrawIconEx(
                dc,
                scale(18),
                scale(17),
                icon,
                scale(28),
                scale(28),
                0,
                None,
                DI_NORMAL,
            )
        };
    }
    text("PHORMINX", rect(58, 16, 230, 47), foreground);
    for (index, (slot, label)) in rows.iter().enumerate() {
        let top = 62 + index as i32 * (row_height + 6);
        text(
            &slot.to_string(),
            rect(32, top, 58, top + row_height),
            foreground,
        );
        text(
            label,
            rect(
                67,
                top,
                if *slot == 1 { 275 } else { 332 },
                top + row_height,
            ),
            foreground,
        );
        if *slot == 1 {
            text("Enter", rect(286, top, 339, top + row_height), muted);
        }
    }
    text(
        "Choose your next action",
        rect(20, height - 39, 251, height - 10),
        muted,
    );
    text(
        "Esc to close",
        rect(264, height - 39, 344, height - 10),
        muted,
    );
    unsafe {
        SelectObject(dc, previous);
        if !owned_font.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(owned_font.0));
        }
    }
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
    fn launcher_hit_region_scales_and_does_not_activate_header_or_footer() {
        let rows = launcher_rows(&[(7, "Notes".into())]);
        for dpi in [96, 144, 192] {
            let p = |n| n * dpi as i32 / 96;
            assert_eq!(launcher_hit_test(p(30), p(80), dpi, &rows, 54), Some(1));
            assert_eq!(launcher_hit_test(p(30), p(140), dpi, &rows, 54), Some(2));
            assert_eq!(launcher_hit_test(p(30), p(200), dpi, &rows, 54), Some(7));
            assert_eq!(launcher_hit_test(p(30), p(30), dpi, &rows, 54), None);
            assert_eq!(launcher_hit_test(p(30), p(119), dpi, &rows, 54), None);
            assert_eq!(launcher_hit_test(p(30), p(250), dpi, &rows, 54), None);
            assert_eq!(OverlayStatus::LauncherDark.hide_after_ms(), None);
        }
    }

    #[test]
    fn action_names_are_bounded_slots_sorted_unique_and_empty_defaults_stay_small() {
        let actions = clean_launcher_actions(vec![
            (9, "Z".repeat(400)),
            (7, "  Notes & tasks\n\0  ".into()),
            (7, "Duplicate".into()),
            (1, "Invalid".into()),
            (10, "Invalid".into()),
            (5, " \n ".into()),
        ]);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], (7, "Notes & tasks".into()));
        assert_eq!(actions[1].1.chars().count(), 80);
        assert_eq!(
            launcher_rows(&[]),
            vec![(1, "Dictate".into()), (2, "Start meeting".into())]
        );
        assert_eq!(launcher_height(2, 54), 224);
    }

    #[test]
    fn all_nine_rows_fit_compact_layout_and_hits_follow_visible_slots() {
        let actions: Vec<_> = (3..=9)
            .map(|slot| (slot, format!("Action {slot}")))
            .collect();
        let rows = launcher_rows(&actions);
        for available in [400, 500, 640, 900] {
            let height = launcher_row_height(rows.len(), available);
            assert!(launcher_height(rows.len(), height) <= available);
            for dpi in [96, 120, 144, 192, 288] {
                for (index, (slot, _)) in rows.iter().enumerate() {
                    let top = 62 + index as i32 * (height + 6);
                    let x = 30 * dpi as i32 / 96;
                    let y = (top + height / 2) * dpi as i32 / 96;
                    assert_eq!(launcher_hit_test(x, y, dpi, &rows, height), Some(*slot));
                }
            }
        }
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
