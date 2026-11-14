//! Windows-only platform code. Everything outside this module is portable.

pub mod autostart;
pub mod chime;
pub mod launch;
pub mod foreground;
pub mod http;
pub mod monitor;
pub mod process;
pub mod render;
pub mod tip;
pub mod tray;
pub mod window;
pub mod windows;

/// NUL-terminated UTF-16 for Win32 `PCWSTR` arguments.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The local time, for moods and seasons.
pub fn local_clock() -> crate::mood::Clock {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    crate::mood::Clock { hour: t.wHour as u8, weekday: t.wDayOfWeek as u8, month: t.wMonth as u8 }
}

/// A message box with no owner: it would disable the pet window for as long
/// as it's open. It runs a modal loop, so re-fetch `app()` afterwards.
pub fn message_box(text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW};
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    let (text, title) = (wide(text), wide("Clawd"));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_SETFOREGROUND | icon) };
}

/// A Yes/No question, with no owner like `message_box`. True for Yes.
pub fn ask(text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_YESNO, MessageBoxW};
    let (text, title) = (wide(text), wide("Clawd"));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND) == IDYES }
}
