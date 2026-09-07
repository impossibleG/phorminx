use std::mem::size_of;
use std::path::Path;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, HWND, LPARAM, SetLastError, WPARAM,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Controls::EM_GETPASSWORDCHAR;
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    ES_PASSWORD, ES_READONLY, GUITHREADINFO, GWL_STYLE, GetClassNameW, GetForegroundWindow,
    GetGUIThreadInfo, GetWindowLongPtrW, GetWindowThreadProcessId, IsChild, IsWindowVisible,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SendMessageTimeoutW,
};
use windows::core::PWSTR;

/// Identity of the foreground window and focused child at activation time.
/// Raw handle values keep this type safe to send across the hook channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetSnapshot {
    foreground: usize,
    process_id: u32,
    thread_id: u32,
    focus: usize,
}

impl TargetSnapshot {
    pub(crate) fn capture() -> Option<Self> {
        unsafe { capture_target() }
    }

    pub(crate) fn is_current(self) -> bool {
        Self::capture() == Some(self)
    }

    /// Resolves only the executable basename for privacy-safe app profiles and
    /// history metadata. Full image paths never leave this function.
    pub fn executable_name(self) -> Option<String> {
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, self.process_id) }
                .ok()?;
        let process = ProcessHandle(process);
        let mut image = vec![0_u16; 32_768];
        let mut length = image.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                process.0,
                PROCESS_NAME_WIN32,
                PWSTR(image.as_mut_ptr()),
                &mut length,
            )
        }
        .ok()?;
        image.truncate(length as usize);
        basename_from_image_path(&String::from_utf16(image.as_slice()).ok()?)
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

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn basename_from_image_path(image_path: &str) -> Option<String> {
    if image_path.ends_with(['/', '\\']) {
        return None;
    }
    let basename = Path::new(image_path).file_name()?.to_str()?;
    if basename.is_empty()
        || basename.contains(['/', '\\', ':'])
        || basename.chars().any(char::is_control)
    {
        return None;
    }
    Some(basename.to_owned())
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

#[cfg(test)]
mod tests {
    use super::basename_from_image_path;

    #[test]
    fn image_paths_are_reduced_to_safe_basenames() {
        assert_eq!(
            basename_from_image_path(r"C:\Program Files\Editor\editor.exe").as_deref(),
            Some("editor.exe")
        );
        assert_eq!(basename_from_image_path(""), None);
        assert_eq!(basename_from_image_path(r"C:\folder\"), None);
        assert_eq!(basename_from_image_path("bad\0name.exe"), None);
    }
}
