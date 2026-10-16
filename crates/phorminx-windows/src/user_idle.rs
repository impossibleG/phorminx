use std::time::Duration;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Time since the last keyboard or mouse input in this session, if Windows
/// reports it.
pub fn user_idle_duration() -> Option<Duration> {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // Both counters wrap after ~49.7 days; wrapping subtraction stays correct.
    let elapsed = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
    Some(Duration::from_millis(u64::from(elapsed)))
}
