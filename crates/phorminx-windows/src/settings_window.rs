use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CREATESTRUCTW,
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, GWLP_USERDATA, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IsWindow, LoadCursorW, MB_ICONERROR,
    MB_OK, MSG, MessageBoxW, PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW,
    SM_CXSCREEN, SM_CYSCREEN, SW_RESTORE, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, UnregisterClassW,
    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NCDESTROY,
    WM_QUIT, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);

const CLASS_NAME: PCWSTR = w!("PhorminxSettingsWindow");
const WM_FOCUS_WINDOW: u32 = WM_APP + 40;
const WM_SHOW_ERROR: u32 = WM_APP + 41;

const ID_MODEL: usize = 101;
const ID_LANGUAGE: usize = 102;
const ID_MINIMUM_RMS: usize = 103;
const ID_FORMATTING: usize = 104;
const ID_CUSTOM: usize = 105;
const ID_BROWSE: usize = 106;
const ID_SAVE: usize = 201;
const ID_CANCEL: usize = 202;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsFormatting {
    Raw,
    Light,
    Balanced,
    Strong,
    Custom,
}

impl SettingsFormatting {
    fn index(self) -> usize {
        match self {
            Self::Raw => 0,
            Self::Light => 1,
            Self::Balanced => 2,
            Self::Strong => 3,
            Self::Custom => 4,
        }
    }

    fn from_index(index: isize) -> Option<Self> {
        match index {
            0 => Some(Self::Raw),
            1 => Some(Self::Light),
            2 => Some(Self::Balanced),
            3 => Some(Self::Strong),
            4 => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsForm {
    pub model_path: String,
    pub model_status: String,
    pub microphone_status: String,
    pub language: String,
    pub minimum_rms: String,
    pub formatting: SettingsFormatting,
    pub custom_instructions: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsWindowEvent {
    SaveAndRestart(SettingsForm),
    Closed,
}

pub struct SettingsWindow {
    events: Receiver<SettingsWindowEvent>,
    errors: Arc<Mutex<Option<String>>>,
    window_bits: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl SettingsWindow {
    pub fn start(form: SettingsForm) -> Result<Self, SettingsWindowError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SettingsWindowError::AlreadyRunning);
        }

        let errors = Arc::new(Mutex::new(None));
        let thread_errors = Arc::clone(&errors);
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-settings".to_owned())
            .spawn(move || run_window(form, event_tx, thread_errors, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                SettingsWindowError::Spawn(error)
            })?;

        let (window_bits, thread_id) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(SettingsWindowError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(SettingsWindowError::StartupChannelClosed);
            }
        };

        Ok(Self {
            events: event_rx,
            errors,
            window_bits,
            thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<SettingsWindowEvent> {
        &self.events
    }

    pub fn focus(&self) -> Result<(), SettingsWindowError> {
        unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_FOCUS_WINDOW,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(SettingsWindowError::PostFocus)
    }

    pub fn show_error(&self, message: String) -> Result<(), SettingsWindowError> {
        *self
            .errors
            .lock()
            .map_err(|_| SettingsWindowError::ErrorLockPoisoned)? = Some(message);
        unsafe {
            PostMessageW(
                Some(window(self.window_bits)),
                WM_SHOW_ERROR,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(SettingsWindowError::PostError)
    }

    pub fn shutdown(mut self) -> Result<(), SettingsWindowError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), SettingsWindowError> {
        if self.thread.is_none() {
            return Ok(());
        }
        let post_result = if unsafe { IsWindow(Some(window(self.window_bits))) }.as_bool() {
            unsafe {
                PostMessageW(
                    Some(window(self.window_bits)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
        } else {
            Ok(())
        };
        if post_result.is_err() {
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        let join_result = self.thread.take().map(JoinHandle::join);
        self.window_bits = 0;
        self.thread_id = 0;
        if let Some(Err(_)) = join_result {
            return Err(SettingsWindowError::ThreadPanicked);
        }
        post_result.map_err(SettingsWindowError::PostClose)
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    initial: SettingsForm,
    events: Sender<SettingsWindowEvent>,
    errors: Arc<Mutex<Option<String>>>,
    model: HWND,
    language: HWND,
    minimum_rms: HWND,
    formatting: HWND,
    custom: HWND,
}

fn run_window(
    form: SettingsForm,
    events: Sender<SettingsWindowEvent>,
    errors: Arc<Mutex<Option<String>>>,
    ready_tx: mpsc::SyncSender<Result<(usize, u32), String>>,
) {
    let result = unsafe { create_and_run(form, events, errors, &ready_tx) };
    if let Err(error) = result {
        let _ = ready_tx.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    form: SettingsForm,
    events: Sender<SettingsWindowEvent>,
    errors: Arc<Mutex<Option<String>>>,
    ready_tx: &mpsc::SyncSender<Result<(usize, u32), String>>,
) -> windows::core::Result<()> {
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let module = unsafe { GetModuleHandleW(None)? };
    let instance = HINSTANCE(module.0);
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW)? };
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(windows::core::Error::from_thread());
    }

    let mut state = Box::new(WindowState {
        initial: form,
        events,
        errors,
        model: HWND::default(),
        language: HWND::default(),
        minimum_rms: HWND::default(),
        formatting: HWND::default(),
        custom: HWND::default(),
    });
    let state_pointer = (&mut *state as *mut WindowState).cast();
    let system_dpi = unsafe { GetDpiForSystem() }.max(96) as i32;
    let width = 560 * system_dpi / 96;
    let height = 580 * system_dpi / 96;
    let x = (unsafe { GetSystemMetrics(SM_CXSCREEN) } - width).max(0) / 2;
    let y = (unsafe { GetSystemMetrics(SM_CYSCREEN) } - height).max(0) / 2;
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS_NAME,
            w!("Phorminx Settings"),
            style,
            x,
            y,
            width,
            height,
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

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
    if ready_tx
        .send(Ok((hwnd.0 as usize, unsafe { GetCurrentThreadId() })))
        .is_err()
    {
        let _ = unsafe { DestroyWindow(hwnd) };
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
        WM_CREATE => {
            if let Some(state) = unsafe { window_state(hwnd) }
                && unsafe { create_controls(hwnd, state) }.is_err()
            {
                return LRESULT(-1);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let command = wparam.0 & 0xffff;
            match command {
                ID_SAVE => {
                    if let Some(state) = unsafe { window_state(hwnd) }
                        && let Some(form) = unsafe { read_form(state) }
                    {
                        let _ = state.events.send(SettingsWindowEvent::SaveAndRestart(form));
                    }
                }
                ID_CANCEL => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                ID_BROWSE => {
                    if let Some(state) = unsafe { window_state(hwnd) }
                        && let Some(path) = unsafe { choose_model_file(hwnd, state.model) }
                    {
                        let path = wide(&path);
                        let _ = unsafe { SetWindowTextW(state.model, PCWSTR(path.as_ptr())) };
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_FOCUS_WINDOW => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_SHOW_ERROR => {
            if let Some(state) = unsafe { window_state(hwnd) }
                && let Ok(mut message) = state.errors.lock()
                && let Some(message) = message.take()
            {
                let message = wide(&message);
                unsafe {
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR(message.as_ptr()),
                        w!("Could not save settings"),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state) = unsafe { window_state(hwnd) } {
                let _ = state.events.send(SettingsWindowEvent::Closed);
            }
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

unsafe fn create_controls(hwnd: HWND, state: &mut WindowState) -> windows::core::Result<()> {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let scale = |value: i32| value * dpi as i32 / 96;
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    unsafe {
        create_label(
            hwnd,
            w!("Speech recognition model"),
            24,
            22,
            500,
            20,
            scale,
            font,
        )?;
        state.model = create_edit(
            hwnd,
            ID_MODEL,
            &state.initial.model_path,
            24,
            46,
            400,
            25,
            false,
            scale,
            font,
        )?;
        create_button(
            hwnd,
            ID_BROWSE,
            w!("Browse..."),
            434,
            46,
            90,
            25,
            false,
            scale,
            font,
        )?;
        let model_status = wide(&state.initial.model_status);
        create_label(
            hwnd,
            PCWSTR(model_status.as_ptr()),
            24,
            77,
            500,
            20,
            scale,
            font,
        )?;
        create_label(hwnd, w!("Language"), 24, 108, 220, 20, scale, font)?;
        create_label(
            hwnd,
            w!("Minimum speech level (RMS)"),
            284,
            108,
            240,
            20,
            scale,
            font,
        )?;
        state.language = create_edit(
            hwnd,
            ID_LANGUAGE,
            &state.initial.language,
            24,
            132,
            220,
            25,
            false,
            scale,
            font,
        )?;
        state.minimum_rms = create_edit(
            hwnd,
            ID_MINIMUM_RMS,
            &state.initial.minimum_rms,
            284,
            132,
            240,
            25,
            false,
            scale,
            font,
        )?;
        create_label(
            hwnd,
            w!("Formatting strength"),
            24,
            174,
            500,
            20,
            scale,
            font,
        )?;
        state.formatting = create_combo(
            hwnd,
            state.initial.formatting,
            24,
            198,
            500,
            180,
            scale,
            font,
        )?;
        create_label(
            hwnd,
            w!("Custom instructions (used by Custom formatting)"),
            24,
            240,
            500,
            20,
            scale,
            font,
        )?;
        state.custom = create_edit(
            hwnd,
            ID_CUSTOM,
            &state.initial.custom_instructions,
            24,
            264,
            500,
            105,
            true,
            scale,
            font,
        )?;
        create_label(
            hwnd,
            w!(
                "Balanced, Strong, and Custom will activate after a compatible local Ollama model is configured."
            ),
            24,
            380,
            500,
            30,
            scale,
            font,
        )?;
        let microphone_status = wide(&state.initial.microphone_status);
        create_label(
            hwnd,
            PCWSTR(microphone_status.as_ptr()),
            24,
            418,
            500,
            20,
            scale,
            font,
        )?;
        create_button(
            hwnd,
            ID_CANCEL,
            w!("Cancel"),
            296,
            466,
            92,
            30,
            false,
            scale,
            font,
        )?;
        create_button(
            hwnd,
            ID_SAVE,
            w!("Save and Restart"),
            398,
            466,
            126,
            30,
            true,
            scale,
            font,
        )?;
        let _ = SetFocus(Some(state.model));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_label(
    parent: HWND,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: impl Fn(i32) -> i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    unsafe {
        create_control(
            parent,
            w!("STATIC"),
            text,
            WS_CHILD | WS_VISIBLE,
            None,
            x,
            y,
            width,
            height,
            scale,
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_edit(
    parent: HWND,
    id: usize,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    multiline: bool,
    scale: impl Fn(i32) -> i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let text = wide(text);
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER;
    style |= if multiline {
        WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | WS_VSCROLL.0 as i32) as u32)
    } else {
        WINDOW_STYLE(ES_AUTOHSCROLL as u32)
    };
    unsafe {
        create_control(
            parent,
            w!("EDIT"),
            PCWSTR(text.as_ptr()),
            style,
            Some(id),
            x,
            y,
            width,
            height,
            scale,
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_combo(
    parent: HWND,
    selected: SettingsFormatting,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: impl Fn(i32) -> i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let style =
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
    let combo = unsafe {
        create_control(
            parent,
            w!("COMBOBOX"),
            PCWSTR::null(),
            style,
            Some(ID_FORMATTING),
            x,
            y,
            width,
            height,
            scale,
            font,
        )?
    };
    for label in [
        "Raw - exact recognizer output",
        "Light - spacing and punctuation cleanup",
        "Balanced - local AI cleanup",
        "Strong - local AI rewrite",
        "Custom - your instructions",
    ] {
        let label = wide(label);
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(label.as_ptr() as isize)),
            );
        }
    }
    unsafe {
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(selected.index())), None);
    }
    Ok(combo)
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_button(
    parent: HWND,
    id: usize,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    default: bool,
    scale: impl Fn(i32) -> i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
    if default {
        style |= WINDOW_STYLE(BS_DEFPUSHBUTTON as u32);
    }
    unsafe {
        create_control(
            parent,
            w!("BUTTON"),
            text,
            style,
            Some(id),
            x,
            y,
            width,
            height,
            scale,
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_control(
    parent: HWND,
    class_name: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    id: Option<usize>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: impl Fn(i32) -> i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let menu = id.map(|id| HMENU(id as *mut core::ffi::c_void));
    let control = unsafe {
        CreateWindowExW(
            if class_name == w!("EDIT") {
                WS_EX_CLIENTEDGE
            } else {
                Default::default()
            },
            class_name,
            text,
            style,
            scale(x),
            scale(y),
            scale(width),
            scale(height),
            Some(parent),
            menu,
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
        );
    }
    Ok(control)
}

unsafe fn read_form(state: &WindowState) -> Option<SettingsForm> {
    let selected = unsafe { SendMessageW(state.formatting, CB_GETCURSEL, None, None) }.0;
    Some(SettingsForm {
        model_path: unsafe { read_text(state.model) },
        model_status: state.initial.model_status.clone(),
        microphone_status: state.initial.microphone_status.clone(),
        language: unsafe { read_text(state.language) },
        minimum_rms: unsafe { read_text(state.minimum_rms) },
        formatting: SettingsFormatting::from_index(selected)?,
        custom_instructions: unsafe { read_text(state.custom) },
    })
}

unsafe fn choose_model_file(owner: HWND, model_edit: HWND) -> Option<String> {
    const MAX_PATH_CHARS: usize = 32_768;
    let current = unsafe { read_text(model_edit) };
    let mut path = vec![0_u16; MAX_PATH_CHARS];
    for (destination, source) in path.iter_mut().zip(current.encode_utf16()) {
        *destination = source;
    }
    let filter = wide("Whisper GGML models (*.bin)\0*.bin\0All files (*.*)\0*.*\0");
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: MAX_PATH_CHARS as u32,
        lpstrTitle: w!("Choose a Whisper model"),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return None;
    }
    let length = path.iter().position(|code_unit| *code_unit == 0)?;
    Some(String::from_utf16_lossy(&path[..length]))
}

unsafe fn read_text(hwnd: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buffer = vec![0; length + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn window_state(hwnd: HWND) -> Option<&'static mut WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    unsafe { pointer.as_mut() }
}

fn window(bits: usize) -> HWND {
    HWND(bits as *mut core::ffi::c_void)
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsWindowError {
    #[error("the Phorminx settings window is already running")]
    AlreadyRunning,
    #[error("failed to start the settings thread: {0}")]
    Spawn(std::io::Error),
    #[error("the settings window failed during startup: {0}")]
    Startup(String),
    #[error("the settings thread exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to focus the settings window: {0}")]
    PostFocus(windows::core::Error),
    #[error("failed to show a settings validation error: {0}")]
    PostError(windows::core::Error),
    #[error("the settings error queue is poisoned")]
    ErrorLockPoisoned,
    #[error("failed to close the settings window: {0}")]
    PostClose(windows::core::Error),
    #[error("the settings thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_indices_are_stable() {
        for (index, formatting) in [
            SettingsFormatting::Raw,
            SettingsFormatting::Light,
            SettingsFormatting::Balanced,
            SettingsFormatting::Strong,
            SettingsFormatting::Custom,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(formatting.index(), index);
            assert_eq!(
                SettingsFormatting::from_index(index as isize),
                Some(formatting)
            );
        }
        assert_eq!(SettingsFormatting::from_index(-1), None);
    }

    #[test]
    fn wide_strings_are_null_terminated() {
        assert_eq!(wide("Phorminx"), [80, 104, 111, 114, 109, 105, 110, 120, 0]);
    }
}
