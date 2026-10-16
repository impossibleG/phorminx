use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::UI::WindowsAndMessaging::{HICON, LoadIconW};
use windows::core::PCWSTR;

/// Icon resource ids the Phorminx executable may carry. The legacy
/// `winresource` build embeds the icon as id 1; `tauri-build` embeds it as
/// `WINDOWS_APP_ICON_RESOURCE_ID` (32512). Only one of the two exists in a
/// given binary, so callers must try both before falling back to a stock icon.
const APP_ICON_RESOURCE_IDS: [u16; 2] = [1, 32512];

/// Loads the embedded Phorminx icon as a shared handle, which must not be
/// destroyed by the caller.
pub(crate) fn load_app_icon(instance: HINSTANCE) -> Option<HICON> {
    APP_ICON_RESOURCE_IDS.iter().find_map(|&id| {
        unsafe { LoadIconW(Some(instance), PCWSTR(id as usize as *const u16)) }.ok()
    })
}
