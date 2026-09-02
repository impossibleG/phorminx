use std::mem::size_of;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::core::{PCWSTR, PWSTR};

/// Opens an explicit user-driven archive picker. No archive is downloaded or
/// executed; the caller must verify the selected bytes before extraction.
pub fn choose_zip_archive(title: &str) -> Option<PathBuf> {
    const MAX_PATH_CHARS: usize = 32_768;
    let mut path = vec![0_u16; MAX_PATH_CHARS];
    let filter = wide("ZIP archives (*.zip)\0*.zip\0All files (*.*)\0*.*\0");
    let title = wide(title);
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND::default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: MAX_PATH_CHARS as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return None;
    }
    let length = path.iter().position(|code_unit| *code_unit == 0)?;
    Some(PathBuf::from(String::from_utf16_lossy(&path[..length])))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
