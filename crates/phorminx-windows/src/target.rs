use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, HWND, LPARAM, SetLastError, WPARAM};
use windows::Win32::UI::Controls::EM_GETPASSWORDCHAR;
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    ES_PASSWORD, ES_READONLY, GUITHREADINFO, GWL_STYLE, GetClassNameW, GetForegroundWindow,
    GetGUIThreadInfo, GetWindowLongPtrW, GetWindowThreadProcessId, IsChild, IsWindowVisible,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SendMessageTimeoutW,
};

/// Identity of the foreground window and focused child at activation time.
/// Raw handle values keep this type safe to send across the hook channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetSnapshot {
    foreground: usize,
    process_id: u32,
    thread_id: u32,
    focus: usize,
}

static TARGET_VALID: AtomicBool = AtomicBool::new(false);
static TARGET_FOREGROUND: AtomicUsize = AtomicUsize::new(0);
static TARGET_PROCESS_ID: AtomicU32 = AtomicU32::new(0);
static TARGET_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static TARGET_FOCUS: AtomicUsize = AtomicUsize::new(0);

impl TargetSnapshot {
    pub(crate) fn capture() -> Option<Self> {
        unsafe { capture_target() }
    }

    pub(crate) fn is_current(self) -> bool {
        Self::capture() == Some(self)
    }

    pub(crate) fn is_classic_writable_edit(self) -> bool {
        if !self.is_current() {
            return false;
        }

        let focus = hwnd(self.focus);
        let mut class_name = [0_u16; 256];
        let class_length = unsafe { GetClassNameW(focus, &mut class_name) };
        if class_length <= 0 || class_length as usize >= class_name.len() - 1 {
            return false;
        }
        let Ok(class_name) = String::from_utf16(&class_name[..class_length as usize]) else {
            return false;
        };
        if !class_name.eq_ignore_ascii_case("Edit") {
            return false;
        }

        unsafe { SetLastError(ERROR_SUCCESS) };
        let raw_style = unsafe { GetWindowLongPtrW(focus, GWL_STYLE) };
        if raw_style == 0 && unsafe { GetLastError() } != ERROR_SUCCESS {
            return false;
        }
        let style = raw_style as u32;
        if style & ES_PASSWORD as u32 != 0 || style & ES_READONLY as u32 != 0 {
            return false;
        }
        if !unsafe { IsWindowEnabled(focus) }.as_bool()
            || !unsafe { IsWindowVisible(focus) }.as_bool()
        {
            return false;
        }

        let mut password_character = 0_usize;
        let sent = unsafe {
            SendMessageTimeoutW(
                focus,
                EM_GETPASSWORDCHAR,
                WPARAM(0),
                LPARAM(0),
                SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                50,
                Some(&mut password_character),
            )
        };

        sent.0 != 0 && password_character == 0 && self.is_current()
    }
}

pub(crate) fn capture_activation_target() {
    TARGET_VALID.store(false, Ordering::Release);
    let Some(target) = TargetSnapshot::capture() else {
        return;
    };

    TARGET_FOREGROUND.store(target.foreground, Ordering::Relaxed);
    TARGET_PROCESS_ID.store(target.process_id, Ordering::Relaxed);
    TARGET_THREAD_ID.store(target.thread_id, Ordering::Relaxed);
    TARGET_FOCUS.store(target.focus, Ordering::Relaxed);
    TARGET_VALID.store(true, Ordering::Release);
}

pub(crate) fn activation_target() -> Option<TargetSnapshot> {
    if !TARGET_VALID.load(Ordering::Acquire) {
        return None;
    }

    Some(TargetSnapshot {
        foreground: TARGET_FOREGROUND.load(Ordering::Relaxed),
        process_id: TARGET_PROCESS_ID.load(Ordering::Relaxed),
        thread_id: TARGET_THREAD_ID.load(Ordering::Relaxed),
        focus: TARGET_FOCUS.load(Ordering::Relaxed),
    })
}

unsafe fn capture_target() -> Option<TargetSnapshot> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return None;
    }

    let mut process_id = 0;
    let thread_id = unsafe { GetWindowThreadProcessId(foreground, Some(&mut process_id)) };
    if thread_id == 0 || process_id == 0 {
        return None;
    }

    let mut gui = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread_id, &mut gui) }.ok()?;

    let focus = gui.hwndFocus;
    if focus.is_invalid()
        || gui.hwndActive != foreground
        || focus == foreground
        || !unsafe { IsChild(foreground, focus) }.as_bool()
    {
        return None;
    }

    let mut focus_process_id = 0;
    let focus_thread_id = unsafe { GetWindowThreadProcessId(focus, Some(&mut focus_process_id)) };
    if focus_thread_id != thread_id || focus_process_id != process_id {
        return None;
    }

    let foreground_again = unsafe { GetForegroundWindow() };
    let mut process_id_again = 0;
    let thread_id_again =
        unsafe { GetWindowThreadProcessId(foreground_again, Some(&mut process_id_again)) };
    if foreground_again != foreground
        || thread_id_again != thread_id
        || process_id_again != process_id
    {
        return None;
    }

    Some(TargetSnapshot {
        foreground: foreground.0 as usize,
        process_id,
        thread_id,
        focus: focus.0 as usize,
    })
}

fn hwnd(bits: usize) -> HWND {
    HWND(bits as *mut core::ffi::c_void)
}
