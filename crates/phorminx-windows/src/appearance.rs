use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColor};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemAppearance {
    pub high_contrast: bool,
    pub contrast_theme_is_dark: bool,
}

/// Reads the live Windows contrast state and the effective contrast background.
///
/// A failed contrast query is treated as disabled so the shell can still start
/// with its authored theme on restricted or unusual Windows environments.
#[must_use]
pub fn system_appearance() -> SystemAppearance {
    let mut contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `contrast` is initialized with the required structure size and
    // remains alive and uniquely borrowed for the duration of the API call.
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some((&raw mut contrast).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let high_contrast = result.is_ok() && contrast.dwFlags.contains(HCF_HIGHCONTRASTON);
    // SAFETY: GetSysColor is process-independent and COLOR_WINDOW is a valid
    // system color index. COLORREF encodes the channels as 0x00bbggrr.
    let window = unsafe { GetSysColor(COLOR_WINDOW) };
    SystemAppearance {
        high_contrast,
        contrast_theme_is_dark: colorref_is_dark(window),
    }
}

fn colorref_is_dark(color: u32) -> bool {
    let red = color & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = (color >> 16) & 0xff;
    299 * red + 587 * green + 114 * blue < 128_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_background_luminance_distinguishes_black_and_white() {
        assert!(colorref_is_dark(0x000000));
        assert!(!colorref_is_dark(0x00ff_ffff));
    }
}
