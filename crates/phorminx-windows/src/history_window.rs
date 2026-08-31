use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HMENU, IDC_ARROW, IsWindow, LoadCursorW, MSG, PostQuitMessage,
    PostThreadMessageW, RegisterClassW, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, UnregisterClassW,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_QUIT,
    WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);
const CLASS_NAME: PCWSTR = w!("PhorminxHistoryWindow");
const ID_PREVIOUS: usize = 101;
const ID_NEXT: usize = 102;
const ID_COPY_RAW: usize = 103;
const ID_COPY_SELECTED: usize = 104;
const ID_CLEAR: usize = 105;
const ID_CLOSE: usize = 106;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryItem {
    pub id: i64,
    pub created_label: String,
    pub raw: String,
    pub normalized: Option<String>,
    pub cleaned: Option<String>,
    pub selected: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryWindowEvent {
    CopyRaw(String),
    CopySelected(String),
    ClearRequested,
    Closed,
}

pub struct HistoryWindow {
    events: Receiver<HistoryWindowEvent>,
    window_bits: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl HistoryWindow {
    pub fn start(items: Vec<HistoryItem>) -> Result<Self, HistoryWindowError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(HistoryWindowError::AlreadyRunning);
        }
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-history".to_owned())
            .spawn(move || run_window(items, event_tx, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                HistoryWindowError::Spawn(error)
            })?;
        let (window_bits, thread_id) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(HistoryWindowError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(HistoryWindowError::StartupChannelClosed);
            }
        };
        Ok(Self {
            events: event_rx,
            window_bits,
            thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<HistoryWindowEvent> {
        &self.events
    }

    pub fn focus(&self) -> Result<(), HistoryWindowError> {
        if unsafe { SetForegroundWindow(window(self.window_bits)) }.as_bool() {
            Ok(())
        } else {
            Err(HistoryWindowError::Focus)
        }
    }

    pub fn shutdown(mut self) -> Result<(), HistoryWindowError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), HistoryWindowError> {
        if self.thread.is_none() {
            return Ok(());
        }
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
            .map_err(HistoryWindowError::PostClose)?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| HistoryWindowError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for HistoryWindow {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    items: Vec<HistoryItem>,
    index: usize,
    events: Sender<HistoryWindowEvent>,
    heading: HWND,
    body: HWND,
}

fn run_window(
    items: Vec<HistoryItem>,
    events: Sender<HistoryWindowEvent>,
    ready: mpsc::SyncSender<Result<(usize, u32), String>>,
) {
    let result = unsafe { create_and_run(items, events, &ready) };
    if let Err(error) = result {
        let _ = ready.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    items: Vec<HistoryItem>,
    events: Sender<HistoryWindowEvent>,
    ready: &mpsc::SyncSender<Result<(usize, u32), String>>,
) -> windows::core::Result<()> {
    let module = unsafe { GetModuleHandleW(None)? };
    let instance = HINSTANCE(module.0);
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let mut state = Box::new(WindowState {
        items,
        index: 0,
        events,
        heading: HWND::default(),
        body: HWND::default(),
    });
    let state_pointer = (&mut *state as *mut WindowState).cast();
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            w!("Phorminx History"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            160,
            90,
            760,
            620,
            None,
            None,
            Some(instance),
            Some(state_pointer),
        )?
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    if ready
        .send(Ok((hwnd.0 as usize, unsafe { GetCurrentThreadId() })))
        .is_err()
    {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
    let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance)) };
    drop(state);
    Ok(())
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
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            LRESULT(1)
        }
        WM_CREATE => {
            if let Some(state) = unsafe { state(hwnd) }
                && unsafe { create_controls(hwnd, state) }.is_err()
            {
                return LRESULT(-1);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if let Some(state) = unsafe { state(hwnd) } {
                match wparam.0 & 0xffff {
                    ID_PREVIOUS => {
                        state.index = state.index.saturating_sub(1);
                        unsafe { refresh(state) };
                    }
                    ID_NEXT => {
                        if !state.items.is_empty() {
                            state.index = (state.index + 1).min(state.items.len() - 1);
                        }
                        unsafe { refresh(state) };
                    }
                    ID_COPY_RAW => {
                        if let Some(item) = state.items.get(state.index) {
                            let _ = state
                                .events
                                .send(HistoryWindowEvent::CopyRaw(item.raw.clone()));
                        }
                    }
                    ID_COPY_SELECTED => {
                        if let Some(item) = state.items.get(state.index) {
                            let _ = state
                                .events
                                .send(HistoryWindowEvent::CopySelected(item.selected.clone()));
                        }
                    }
                    ID_CLEAR => {
                        let _ = state.events.send(HistoryWindowEvent::ClearRequested);
                    }
                    ID_CLOSE => {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state) = unsafe { state(hwnd) } {
                let _ = state.events.send(HistoryWindowEvent::Closed);
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn create_controls(hwnd: HWND, state: &mut WindowState) -> windows::core::Result<()> {
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    state.heading = unsafe {
        control(
            hwnd,
            w!("STATIC"),
            0,
            20,
            18,
            704,
            24,
            WS_CHILD | WS_VISIBLE,
            font,
        )?
    };
    state.body = unsafe {
        control(
            hwnd,
            w!("EDIT"),
            0,
            20,
            48,
            704,
            430,
            WS_CHILD
                | WS_VISIBLE
                | WS_BORDER
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32),
            font,
        )?
    };
    for (id, text, x, width, default) in [
        (ID_PREVIOUS, w!("Previous"), 20, 90, false),
        (ID_NEXT, w!("Next"), 120, 80, false),
        (ID_COPY_RAW, w!("Copy raw"), 222, 100, false),
        (ID_COPY_SELECTED, w!("Copy output"), 332, 110, false),
        (ID_CLEAR, w!("Clear history"), 464, 120, false),
        (ID_CLOSE, w!("Close"), 624, 100, true),
    ] {
        let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
        if default {
            style |= WINDOW_STYLE(BS_DEFPUSHBUTTON as u32);
        }
        let _ = unsafe { control(hwnd, w!("BUTTON"), id, x, 500, width, 32, style, font)? };
        let button =
            unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(hwnd), id as i32) }?;
        let _ = unsafe { SetWindowTextW(button, text) };
    }
    unsafe { refresh(state) };
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn control(
    parent: HWND,
    class: PCWSTR,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    style: WINDOW_STYLE,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let control = unsafe {
        CreateWindowExW(
            if class == w!("EDIT") {
                WS_EX_CLIENTEDGE
            } else {
                Default::default()
            },
            class,
            PCWSTR::null(),
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            (id != 0).then_some(HMENU(id as *mut core::ffi::c_void)),
            None,
            None,
        )?
    };
    unsafe {
        SendMessageW(
            control,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        )
    };
    Ok(control)
}

unsafe fn refresh(state: &WindowState) {
    let (heading, body) = state.items.get(state.index).map_or_else(
        || {
            (
                "No saved dictations".to_owned(),
                "History is empty or disabled.".to_owned(),
            )
        },
        |item| {
            (
                format!(
                    "{} of {} — {}",
                    state.index + 1,
                    state.items.len(),
                    item.created_label
                ),
                render_item(item),
            )
        },
    );
    let heading = wide(&heading);
    let body = wide(&body);
    let _ = unsafe { SetWindowTextW(state.heading, PCWSTR(heading.as_ptr())) };
    let _ = unsafe { SetWindowTextW(state.body, PCWSTR(body.as_ptr())) };
}

fn render_item(item: &HistoryItem) -> String {
    format!(
        "RAW\r\n{}\r\n\r\nNORMALIZED\r\n{}\r\n\r\nCLEANED\r\n{}\r\n\r\nSELECTED OUTPUT\r\n{}",
        item.raw,
        item.normalized.as_deref().unwrap_or("—"),
        item.cleaned.as_deref().unwrap_or("—"),
        item.selected
    )
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    unsafe { pointer.as_mut() }
}

fn window(bits: usize) -> HWND {
    HWND(bits as *mut core::ffi::c_void)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryWindowError {
    #[error("the history window is already open")]
    AlreadyRunning,
    #[error("failed to start the history window thread: {0}")]
    Spawn(std::io::Error),
    #[error("history window startup failed: {0}")]
    Startup(String),
    #[error("history window exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to focus the history window")]
    Focus,
    #[error("failed to close the history window: {0}")]
    PostClose(windows::core::Error),
    #[error("the history window thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_keeps_all_recoverable_variants_distinct() {
        let rendered = render_item(&HistoryItem {
            id: 1,
            created_label: "now".to_owned(),
            raw: "raw".to_owned(),
            normalized: Some("normal".to_owned()),
            cleaned: Some("clean".to_owned()),
            selected: "selected".to_owned(),
        });
        for expected in [
            "RAW",
            "raw",
            "NORMALIZED",
            "normal",
            "CLEANED",
            "clean",
            "SELECTED OUTPUT",
            "selected",
        ] {
            assert!(rendered.contains(expected));
        }
    }

    #[test]
    fn wide_strings_are_terminated() {
        assert_eq!(wide("A"), [65, 0]);
    }
}
