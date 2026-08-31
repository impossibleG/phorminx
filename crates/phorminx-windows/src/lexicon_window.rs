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
    CB_SETCURSEL, CBS_DROPDOWNLIST, CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOHSCROLL, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDC_ARROW, IsWindow, LoadCursorW, MSG,
    PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, SW_SHOWNORMAL, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage,
    UnregisterClassW, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE,
    WM_NCDESTROY, WM_QUIT, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);
const CLASS_NAME: PCWSTR = w!("PhorminxLexiconWindow");
const ID_CANONICAL: usize = 101;
const ID_ALIAS: usize = 102;
const ID_LANGUAGE: usize = 103;
const ID_APP: usize = 104;
const ID_CASE: usize = 105;
const ID_ENABLED: usize = 106;
const ID_PREVIOUS: usize = 201;
const ID_NEXT: usize = 202;
const ID_NEW: usize = 203;
const ID_DELETE: usize = 204;
const ID_SAVE: usize = 205;
const ID_CLOSE: usize = 206;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexiconCasePolicy {
    PreserveInput,
    UseCanonical,
    Lowercase,
    Uppercase,
}

impl LexiconCasePolicy {
    fn index(self) -> usize {
        match self {
            Self::PreserveInput => 0,
            Self::UseCanonical => 1,
            Self::Lowercase => 2,
            Self::Uppercase => 3,
        }
    }

    fn from_index(index: isize) -> Option<Self> {
        match index {
            0 => Some(Self::PreserveInput),
            1 => Some(Self::UseCanonical),
            2 => Some(Self::Lowercase),
            3 => Some(Self::Uppercase),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexiconItem {
    pub id: i64,
    pub canonical: String,
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub case_policy: LexiconCasePolicy,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexiconDraft {
    pub id: Option<i64>,
    pub canonical: String,
    pub alias: String,
    pub language: Option<String>,
    pub app_executable: Option<String>,
    pub case_policy: LexiconCasePolicy,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LexiconWindowEvent {
    Save(LexiconDraft),
    Delete(i64),
    Closed,
}

pub struct LexiconWindow {
    events: Receiver<LexiconWindowEvent>,
    window_bits: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl LexiconWindow {
    pub fn start(items: Vec<LexiconItem>) -> Result<Self, LexiconWindowError> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(LexiconWindowError::AlreadyRunning);
        }
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("phorminx-lexicon".to_owned())
            .spawn(move || run_window(items, event_tx, ready_tx))
            .map_err(|error| {
                ACTIVE.store(false, Ordering::Release);
                LexiconWindowError::Spawn(error)
            })?;
        let (window_bits, thread_id) = match ready_rx.recv() {
            Ok(Ok(ready)) => ready,
            Ok(Err(message)) => {
                let _ = thread.join();
                return Err(LexiconWindowError::Startup(message));
            }
            Err(_) => {
                let _ = thread.join();
                return Err(LexiconWindowError::StartupChannelClosed);
            }
        };
        Ok(Self {
            events: event_rx,
            window_bits,
            thread_id,
            thread: Some(thread),
        })
    }

    pub fn events(&self) -> &Receiver<LexiconWindowEvent> {
        &self.events
    }

    pub fn focus(&self) -> Result<(), LexiconWindowError> {
        if unsafe { SetForegroundWindow(window(self.window_bits)) }.as_bool() {
            Ok(())
        } else {
            Err(LexiconWindowError::Focus)
        }
    }

    pub fn shutdown(mut self) -> Result<(), LexiconWindowError> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), LexiconWindowError> {
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
                .map_err(|_| LexiconWindowError::ThreadPanicked)?;
        }
        self.window_bits = 0;
        self.thread_id = 0;
        Ok(())
    }
}

impl Drop for LexiconWindow {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct WindowState {
    items: Vec<LexiconItem>,
    index: usize,
    creating: bool,
    events: Sender<LexiconWindowEvent>,
    heading: HWND,
    canonical: HWND,
    alias: HWND,
    language: HWND,
    app: HWND,
    case_policy: HWND,
    enabled: HWND,
}

fn run_window(
    items: Vec<LexiconItem>,
    events: Sender<LexiconWindowEvent>,
    ready: mpsc::SyncSender<Result<(usize, u32), String>>,
) {
    let result = unsafe { create_and_run(items, events, &ready) };
    if let Err(error) = result {
        let _ = ready.try_send(Err(error.to_string()));
    }
    ACTIVE.store(false, Ordering::Release);
}

unsafe fn create_and_run(
    items: Vec<LexiconItem>,
    events: Sender<LexiconWindowEvent>,
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
    let creating = items.is_empty();
    let mut state = Box::new(WindowState {
        items,
        index: 0,
        creating,
        events,
        heading: HWND::default(),
        canonical: HWND::default(),
        alias: HWND::default(),
        language: HWND::default(),
        app: HWND::default(),
        case_policy: HWND::default(),
        enabled: HWND::default(),
    });
    let pointer = (&mut *state as *mut WindowState).cast();
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            w!("Phorminx Personal Lexicon"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            210,
            120,
            620,
            550,
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
                match wparam.0 & 0xffff {
                    ID_PREVIOUS => {
                        state.creating = false;
                        state.index = state.index.saturating_sub(1);
                        unsafe { refresh(state) };
                    }
                    ID_NEXT => {
                        state.creating = false;
                        if !state.items.is_empty() {
                            state.index = (state.index + 1).min(state.items.len() - 1);
                        }
                        unsafe { refresh(state) };
                    }
                    ID_NEW => {
                        state.creating = true;
                        unsafe { refresh(state) };
                    }
                    ID_DELETE => {
                        if !state.creating
                            && let Some(item) = state.items.get(state.index)
                        {
                            let _ = state.events.send(LexiconWindowEvent::Delete(item.id));
                        }
                    }
                    ID_SAVE => {
                        if let Some(draft) = unsafe { read_draft(state) } {
                            let _ = state.events.send(LexiconWindowEvent::Save(draft));
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
                let _ = state.events.send(LexiconWindowEvent::Closed);
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
    state.heading = unsafe { label(hwnd, w!("Personal aliases"), 24, 18, 540, 24, font)? };
    let fields = [
        (w!("Written form"), ID_CANONICAL, 58),
        (w!("Spoken alias (exact match)"), ID_ALIAS, 126),
        (w!("Language scope (blank = all)"), ID_LANGUAGE, 194),
        (w!("Application executable (blank = all)"), ID_APP, 262),
    ];
    let mut controls = Vec::new();
    for (caption, id, y) in fields {
        let _ = unsafe { label(hwnd, caption, 24, y, 540, 20, font)? };
        controls.push(unsafe { edit(hwnd, id, 24, y + 24, 540, 26, font)? });
    }
    state.canonical = controls[0];
    state.alias = controls[1];
    state.language = controls[2];
    state.app = controls[3];
    let _ = unsafe { label(hwnd, w!("Case policy"), 24, 330, 250, 20, font)? };
    state.case_policy = unsafe { case_combo(hwnd, 24, 354, 250, 130, font)? };
    state.enabled = unsafe { checkbox(hwnd, 306, 354, 258, 26, font)? };
    for (id, text, x, width, default) in [
        (ID_PREVIOUS, w!("Previous"), 24, 82, false),
        (ID_NEXT, w!("Next"), 116, 72, false),
        (ID_NEW, w!("New"), 198, 68, false),
        (ID_DELETE, w!("Delete"), 276, 78, false),
        (ID_CLOSE, w!("Close"), 376, 80, false),
        (ID_SAVE, w!("Save"), 466, 98, true),
    ] {
        let _ = unsafe { button(hwnd, id, text, x, 420, width, 32, default, font)? };
    }
    unsafe { refresh(state) };
    Ok(())
}

unsafe fn refresh(state: &WindowState) {
    let item = (!state.creating)
        .then(|| state.items.get(state.index))
        .flatten();
    let heading = item.map_or_else(
        || "New personal alias".to_owned(),
        |_| format!("Alias {} of {}", state.index + 1, state.items.len()),
    );
    set_text(state.heading, &heading);
    set_text(
        state.canonical,
        item.map_or("", |item| item.canonical.as_str()),
    );
    set_text(state.alias, item.map_or("", |item| item.alias.as_str()));
    set_text(
        state.language,
        item.and_then(|item| item.language.as_deref()).unwrap_or(""),
    );
    set_text(
        state.app,
        item.and_then(|item| item.app_executable.as_deref())
            .unwrap_or(""),
    );
    let case = item.map_or(LexiconCasePolicy::UseCanonical, |item| item.case_policy);
    unsafe {
        SendMessageW(
            state.case_policy,
            CB_SETCURSEL,
            Some(WPARAM(case.index())),
            None,
        )
    };
    let checked = item.is_none_or(|item| item.enabled);
    unsafe {
        SendMessageW(
            state.enabled,
            BM_SETCHECK,
            Some(WPARAM(if checked { BST_CHECKED.0 as usize } else { 0 })),
            None,
        )
    };
}

unsafe fn read_draft(state: &WindowState) -> Option<LexiconDraft> {
    let case = unsafe { SendMessageW(state.case_policy, CB_GETCURSEL, None, None) }.0;
    let optional = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_owned());
    Some(LexiconDraft {
        id: (!state.creating)
            .then(|| state.items.get(state.index).map(|item| item.id))
            .flatten(),
        canonical: unsafe { read_text(state.canonical) },
        alias: unsafe { read_text(state.alias) },
        language: optional(unsafe { read_text(state.language) }),
        app_executable: optional(unsafe { read_text(state.app) }),
        case_policy: LexiconCasePolicy::from_index(case)?,
        enabled: unsafe { SendMessageW(state.enabled, BM_GETCHECK, None, None) }.0
            == BST_CHECKED.0 as isize,
    })
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_control(
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
        create_control(
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

unsafe fn edit(
    parent: HWND,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    unsafe {
        create_control(
            parent,
            w!("EDIT"),
            PCWSTR::null(),
            id,
            x,
            y,
            width,
            height,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            font,
        )
    }
}

unsafe fn case_combo(
    parent: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    let combo = unsafe {
        create_control(
            parent,
            w!("COMBOBOX"),
            PCWSTR::null(),
            ID_CASE,
            x,
            y,
            width,
            height,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            font,
        )?
    };
    for text in [
        "Preserve input case",
        "Use written form",
        "Lowercase",
        "Uppercase",
    ] {
        let text = wide(text);
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            )
        };
    }
    Ok(combo)
}

unsafe fn checkbox(
    parent: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: windows::Win32::Graphics::Gdi::HGDIOBJ,
) -> windows::core::Result<HWND> {
    unsafe {
        create_control(
            parent,
            w!("BUTTON"),
            w!("Enabled"),
            ID_ENABLED,
            x,
            y,
            width,
            height,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            font,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn button(
    parent: HWND,
    id: usize,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    default: bool,
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
pub enum LexiconWindowError {
    #[error("the personal lexicon window is already open")]
    AlreadyRunning,
    #[error("failed to start the personal lexicon thread: {0}")]
    Spawn(std::io::Error),
    #[error("personal lexicon startup failed: {0}")]
    Startup(String),
    #[error("personal lexicon exited before reporting readiness")]
    StartupChannelClosed,
    #[error("failed to focus the personal lexicon window")]
    Focus,
    #[error("failed to close the personal lexicon window: {0}")]
    PostClose(windows::core::Error),
    #[error("the personal lexicon thread panicked")]
    ThreadPanicked,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn shutdown_joins_a_window_that_already_closed_itself() {
        let lexicon = LexiconWindow::start(Vec::new()).unwrap();
        unsafe {
            PostMessageW(
                Some(window(lexicon.window_bits)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .unwrap();
        assert!(matches!(
            lexicon.events().recv_timeout(Duration::from_secs(2)),
            Ok(LexiconWindowEvent::Closed)
        ));
        lexicon.shutdown().unwrap();
    }

    #[test]
    fn case_policy_indices_are_stable() {
        for (index, policy) in [
            LexiconCasePolicy::PreserveInput,
            LexiconCasePolicy::UseCanonical,
            LexiconCasePolicy::Lowercase,
            LexiconCasePolicy::Uppercase,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(policy.index(), index);
            assert_eq!(LexiconCasePolicy::from_index(index as isize), Some(policy));
        }
        assert_eq!(LexiconCasePolicy::from_index(-1), None);
    }

    #[test]
    fn wide_strings_are_terminated() {
        assert_eq!(wide("A"), [65, 0]);
    }
}
