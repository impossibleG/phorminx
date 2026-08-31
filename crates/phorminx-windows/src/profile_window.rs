use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::BST_CHECKED;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CREATESTRUCTW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW,
    IsWindow, LoadCursorW, MSG, PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW,
    SW_SHOWNORMAL, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW,
    ShowWindow, TranslateMessage, UnregisterClassW, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WM_QUIT, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);
const CLASS_NAME: PCWSTR = w!("PhorminxProfileWindow");
const ID_SELECTOR: usize = 101;
const ID_EXECUTABLE: usize = 102;
const ID_FORMATTING: usize = 103;
const ID_CUSTOM: usize = 104;
const ID_LANGUAGE: usize = 105;
const ID_INSERTION: usize = 106;
const ID_DENY: usize = 107;
const ID_DELETE: usize = 201;
const ID_SAVE: usize = 202;
const ID_CLOSE: usize = 203;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileFormatting {
    Raw,
    Light,
    Balanced,
    Strong,
    Custom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileInsertion {
    Automatic,
    Direct,
    Clipboard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileItem {
    pub executable: String,
    pub formatting: ProfileFormatting,
    pub custom_instructions: Option<String>,
    pub language: Option<String>,
    pub insertion: ProfileInsertion,
    pub deny: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileWindowEvent {
    Save(ProfileItem),
    Delete(String),
    Closed,
}

pub struct ProfileWindow {
    events: Receiver<ProfileWindowEvent>,
    window_bits: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl ProfileWindow {
    pub fn start(items: Vec<ProfileItem>) -> Result<Self, ProfileWindowError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ProfileWindowError::AlreadyRunning);
        }
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-profiles".to_owned())
            .spawn(move || run_window(items, event_tx, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                ProfileWindowError::Spawn(error)
            })?;
        let (window_bits, thread_id) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(ProfileWindowError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(ProfileWindowError::StartupChannelClosed);
            }
        };
        Ok(Self {
            events: event_rx,
            window_bits,
            thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<ProfileWindowEvent> {
        &self.events
    }

    pub fn focus(&self) -> Result<(), ProfileWindowError> {
        if unsafe { SetForegroundWindow(window(self.window_bits)) }.as_bool() {
            Ok(())
        } else {
            Err(ProfileWindowError::Focus)
        }
    }

    pub fn shutdown(mut self) -> Result<(), ProfileWindowError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), ProfileWindowError> {
        if self.thread.is_none() {
            return Ok(());
        }
        if unsafe { IsWindow(Some(window(self.window_bits))) }.as_bool()
            && unsafe {
                PostMessageW(
                    Some(window(self.window_bits)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_err()
        {
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| ProfileWindowError::ThreadPanicked)?;
        }
        self.window_bits = 0;
        self.thread_id = 0;
        Ok(())
    }
}

impl Drop for ProfileWindow {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    items: Vec<ProfileItem>,
    events: Sender<ProfileWindowEvent>,
    selector: HWND,
    executable: HWND,
    formatting: HWND,
    custom: HWND,
    language: HWND,
    insertion: HWND,
    deny: HWND,
}

fn run_window(
    items: Vec<ProfileItem>,
    events: Sender<ProfileWindowEvent>,
    ready: mpsc::SyncSender<Result<(usize, u32), String>>,
) {
    let result = unsafe { create_and_run(items, events, &ready) };
    if let Err(error) = result {
        let _ = ready.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    items: Vec<ProfileItem>,
    events: Sender<ProfileWindowEvent>,
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
        events,
        selector: HWND::default(),
        executable: HWND::default(),
        formatting: HWND::default(),
        custom: HWND::default(),
        language: HWND::default(),
        insertion: HWND::default(),
        deny: HWND::default(),
    });
    let pointer = (&mut *state as *mut WindowState).cast();
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            w!("Phorminx Application Profiles"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            220,
            100,
            650,
            640,
            None,
            None,
            Some(instance),
            Some(pointer),
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
                let command = wparam.0 & 0xffff;
                let notification = (wparam.0 >> 16) & 0xffff;
                match command {
                    ID_SELECTOR if notification == CBN_SELCHANGE as usize => {
                        unsafe { load_selected(state) };
                    }
                    ID_SAVE => {
                        if let Some(item) = unsafe { read_item(state) } {
                            let _ = state.events.send(ProfileWindowEvent::Save(item));
                        }
                    }
                    ID_DELETE => {
                        let executable = unsafe { read_text(state.executable) };
                        if !executable.trim().is_empty() {
                            let _ = state.events.send(ProfileWindowEvent::Delete(executable));
                        }
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
                let _ = state.events.send(ProfileWindowEvent::Closed);
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
    let _ = unsafe {
        label(
            hwnd,
            w!("Existing profile or new profile"),
            24,
            20,
            570,
            20,
            font,
        )?
    };
    let mut selector_labels = vec!["New profile".to_owned()];
    selector_labels.extend(state.items.iter().map(|item| item.executable.clone()));
    state.selector = unsafe { combo(hwnd, ID_SELECTOR, &selector_labels, 24, 44, 570, 180, font)? };
    let _ = unsafe {
        label(
            hwnd,
            w!("Executable basename (example: code.exe)"),
            24,
            88,
            570,
            20,
            font,
        )?
    };
    state.executable = unsafe { edit(hwnd, ID_EXECUTABLE, 24, 112, 570, 26, false, font)? };
    let _ = unsafe { label(hwnd, w!("Formatting"), 24, 154, 270, 20, font)? };
    state.formatting = unsafe {
        combo(
            hwnd,
            ID_FORMATTING,
            &[
                "Raw".into(),
                "Light".into(),
                "Balanced".into(),
                "Strong".into(),
                "Custom".into(),
            ],
            24,
            178,
            270,
            150,
            font,
        )?
    };
    let _ = unsafe { label(hwnd, w!("Insertion"), 324, 154, 270, 20, font)? };
    state.insertion = unsafe {
        combo(
            hwnd,
            ID_INSERTION,
            &[
                "Automatic".into(),
                "Direct when safe".into(),
                "Clipboard only".into(),
            ],
            324,
            178,
            270,
            110,
            font,
        )?
    };
    let _ = unsafe {
        label(
            hwnd,
            w!("Language override (blank = global)"),
            24,
            222,
            570,
            20,
            font,
        )?
    };
    state.language = unsafe { edit(hwnd, ID_LANGUAGE, 24, 246, 570, 26, false, font)? };
    let _ = unsafe {
        label(
            hwnd,
            w!("Custom formatting instructions"),
            24,
            290,
            570,
            20,
            font,
        )?
    };
    state.custom = unsafe { edit(hwnd, ID_CUSTOM, 24, 314, 570, 110, true, font)? };
    state.deny = unsafe {
        control(
            hwnd,
            w!("BUTTON"),
            w!("Deny dictation entirely in this application"),
            ID_DENY,
            24,
            446,
            570,
            28,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            font,
        )?
    };
    let _ = unsafe { button(hwnd, ID_DELETE, w!("Delete"), 306, 500, 86, false, font)? };
    let _ = unsafe { button(hwnd, ID_CLOSE, w!("Close"), 402, 500, 86, false, font)? };
    let _ = unsafe {
        button(
            hwnd,
            ID_SAVE,
            w!("Save and Restart"),
            498,
            500,
            96,
            true,
            font,
        )?
    };
    unsafe {
        SendMessageW(state.selector, CB_SETCURSEL, Some(WPARAM(0)), None);
        load_selected(state);
    }
    Ok(())
}

unsafe fn load_selected(state: &WindowState) {
    let index = unsafe { SendMessageW(state.selector, CB_GETCURSEL, None, None) }.0;
    let item = usize::try_from(index)
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| state.items.get(index));
    set_text(
        state.executable,
        item.map_or("", |item| item.executable.as_str()),
    );
    set_text(
        state.language,
        item.and_then(|item| item.language.as_deref()).unwrap_or(""),
    );
    set_text(
        state.custom,
        item.and_then(|item| item.custom_instructions.as_deref())
            .unwrap_or(""),
    );
    unsafe {
        SendMessageW(
            state.formatting,
            CB_SETCURSEL,
            Some(WPARAM(
                item.map_or(1, |item| formatting_index(item.formatting)),
            )),
            None,
        );
        SendMessageW(
            state.insertion,
            CB_SETCURSEL,
            Some(WPARAM(
                item.map_or(0, |item| insertion_index(item.insertion)),
            )),
            None,
        );
        SendMessageW(
            state.deny,
            BM_SETCHECK,
            Some(WPARAM(if item.is_some_and(|item| item.deny) {
                BST_CHECKED.0 as usize
            } else {
                0
            })),
            None,
        );
    }
}

unsafe fn read_item(state: &WindowState) -> Option<ProfileItem> {
    let formatting = unsafe { SendMessageW(state.formatting, CB_GETCURSEL, None, None) }.0;
    let insertion = unsafe { SendMessageW(state.insertion, CB_GETCURSEL, None, None) }.0;
    let optional = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_owned());
    Some(ProfileItem {
        executable: unsafe { read_text(state.executable) }.trim().to_owned(),
        formatting: formatting_from_index(formatting)?,
        custom_instructions: optional(unsafe { read_text(state.custom) }),
        language: optional(unsafe { read_text(state.language) }),
        insertion: insertion_from_index(insertion)?,
        deny: unsafe { SendMessageW(state.deny, BM_GETCHECK, None, None) }.0
            == BST_CHECKED.0 as isize,
    })
}

fn formatting_index(value: ProfileFormatting) -> usize {
    match value {
        ProfileFormatting::Raw => 0,
        ProfileFormatting::Light => 1,
        ProfileFormatting::Balanced => 2,
        ProfileFormatting::Strong => 3,
        ProfileFormatting::Custom => 4,
    }
}

fn formatting_from_index(value: isize) -> Option<ProfileFormatting> {
    [
        ProfileFormatting::Raw,
        ProfileFormatting::Light,
        ProfileFormatting::Balanced,
        ProfileFormatting::Strong,
        ProfileFormatting::Custom,
    ]
    .get(usize::try_from(value).ok()?)
    .copied()
}

fn insertion_index(value: ProfileInsertion) -> usize {
    match value {
        ProfileInsertion::Automatic => 0,
        ProfileInsertion::Direct => 1,
        ProfileInsertion::Clipboard => 2,
    }
}

fn insertion_from_index(value: isize) -> Option<ProfileInsertion> {
    [
        ProfileInsertion::Automatic,
        ProfileInsertion::Direct,
        ProfileInsertion::Clipboard,
    ]
    .get(usize::try_from(value).ok()?)
    .copied()
}

#[allow(clippy::too_many_arguments)]
unsafe fn control(
    parent: HWND,
    class: PCWSTR,
    text: PCWSTR,
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
            text,
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            Some(HMENU(id as *mut core::ffi::c_void)),
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

unsafe fn label(
    parent: HWND,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    unsafe {
        control(
            parent,
            w!("STATIC"),
            text,
            0,
            x,
            y,
            width,
            height,
            WS_CHILD | WS_VISIBLE,
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn edit(
    parent: HWND,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    multiline: bool,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER;
    style |= if multiline {
        WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL) as u32)
    } else {
        WINDOW_STYLE(ES_AUTOHSCROLL as u32)
    };
    unsafe {
        control(
            parent,
            w!("EDIT"),
            PCWSTR::null(),
            id,
            x,
            y,
            width,
            height,
            style,
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn combo(
    parent: HWND,
    id: usize,
    choices: &[String],
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let combo = unsafe {
        control(
            parent,
            w!("COMBOBOX"),
            PCWSTR::null(),
            id,
            x,
            y,
            width,
            height,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            font,
        )?
    };
    for choice in choices {
        let choice = wide(choice);
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(choice.as_ptr() as isize)),
            )
        };
    }
    Ok(combo)
}

#[allow(clippy::too_many_arguments)]
unsafe fn button(
    parent: HWND,
    id: usize,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    default: bool,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
    if default {
        style |= WINDOW_STYLE(BS_DEFPUSHBUTTON as u32);
    }
    unsafe { control(parent, w!("BUTTON"), text, id, x, y, width, 32, style, font) }
}

fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = unsafe { SetWindowTextW(control, PCWSTR(text.as_ptr())) };
}

unsafe fn read_text(control: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(control) }.max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = unsafe { GetWindowTextW(control, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
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
pub enum ProfileWindowError {
    #[error("the application-profile window is already open")]
    AlreadyRunning,
    #[error("failed to start the application-profile thread: {0}")]
    Spawn(std::io::Error),
    #[error("application-profile startup failed: {0}")]
    Startup(String),
    #[error("application-profile window exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to focus the application-profile window")]
    Focus,
    #[error("failed to close the application-profile window: {0}")]
    PostClose(windows::core::Error),
    #[error("the application-profile thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_indices_are_stable_and_bounded() {
        for (index, formatting) in [
            ProfileFormatting::Raw,
            ProfileFormatting::Light,
            ProfileFormatting::Balanced,
            ProfileFormatting::Strong,
            ProfileFormatting::Custom,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(formatting_index(formatting), index);
            assert_eq!(formatting_from_index(index as isize), Some(formatting));
        }
        assert_eq!(formatting_from_index(-1), None);
        assert_eq!(insertion_from_index(99), None);
    }
}
