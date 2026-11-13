//! The tooltip over Clawd itself: the sessions and what they're doing
//! (PLAN.md §8 item 5). Created on the first hover, and `comctl32.dll` is
//! loaded only then, so the hook and a pet nobody hovers don't pay for it.
//!
//! The control watches the pet window's mouse messages itself
//! (`TTF_SUBCLASS`), which covers the delay, the fade and hiding on leave
//! without a timer of ours, and asks for its text when it's about to show.

use super::wide;
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const MAX_WIDTH: isize = 420;
const SHOW_AFTER_MS: isize = 500;
const SHOW_FOR_MS: isize = 20_000;
/// `LPSTR_TEXTCALLBACKW`: ask for the text with `TTN_GETDISPINFOW`.
const TEXT_CALLBACK: *mut u16 = -1isize as *mut u16;

pub struct Tip(HWND);

/// Tooltips are a common control; the manifest selects version 6.
unsafe fn init_common_controls() -> bool {
    type Init = unsafe extern "system" fn(*const INITCOMMONCONTROLSEX) -> i32;
    unsafe {
        let lib = LoadLibraryW(wide("comctl32.dll").as_ptr());
        if lib.is_null() {
            return false;
        }
        let Some(f) = GetProcAddress(lib, c"InitCommonControlsEx".as_ptr().cast()) else { return false };
        // SAFETY: that export has exactly this signature.
        let init = std::mem::transmute::<unsafe extern "system" fn() -> isize, Init>(f);
        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES };
        init(&icc) != 0
    }
}

impl Tip {
    pub fn new(owner: HWND) -> Option<Tip> {
        unsafe {
            if !init_common_controls() {
                return None;
            }
            let class = wide("tooltips_class32");
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }
            let mut tool: TTTOOLINFOW = std::mem::zeroed();
            tool.cbSize = size_of::<TTTOOLINFOW>() as u32;
            tool.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
            tool.hwnd = owner;
            tool.uId = owner as usize;
            tool.lpszText = TEXT_CALLBACK;
            if SendMessageW(hwnd, TTM_ADDTOOLW, 0, &raw const tool as LPARAM) == 0 {
                DestroyWindow(hwnd);
                return None;
            }
            SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, 0, MAX_WIDTH); // also makes newlines work
            SendMessageW(hwnd, TTM_SETDELAYTIME, TTDT_INITIAL as usize, SHOW_AFTER_MS);
            SendMessageW(hwnd, TTM_SETDELAYTIME, TTDT_AUTOPOP as usize, SHOW_FOR_MS);
            Some(Tip(hwnd))
        }
    }

    /// Off while Clawd is being carried around.
    pub fn set_active(&self, on: bool) {
        unsafe { SendMessageW(self.0, TTM_ACTIVATE, on as usize, 0) };
    }

    pub fn tools(&self) -> isize {
        unsafe { SendMessageW(self.0, TTM_GETTOOLCOUNT, 0, 0) }
    }
}

/// Answer the control's `TTN_GETDISPINFOW`. `store` has to outlive the call:
/// the control reads the text after this returns.
pub unsafe fn answer(info: *mut NMTTDISPINFOW, text: &str, store: &mut Vec<u16>) {
    *store = wide(text);
    unsafe { (*info).lpszText = store.as_mut_ptr() };
}
