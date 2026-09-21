//! How long has nobody touched the keyboard or the mouse? A display we draw on goes dark while
//! the user is away: OLEDs burn in.

use std::time::Duration;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

pub fn idle_for() -> Duration {
    let mut last = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut last).as_bool() {
            return Duration::ZERO; // can't tell: behave as if someone is there
        }
        Duration::from_millis(GetTickCount().wrapping_sub(last.dwTime) as u64)
    }
}
