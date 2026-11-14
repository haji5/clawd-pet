//! WinEvent hooks (PLAN.md §6c): the system tells us when the foreground
//! window changes and when the window Clawd stands on moves or goes away,
//! so nothing polls. All are out-of-context: the callback runs on this
//! thread, from its message loop.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent, WINEVENTPROC};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZESTART, GetWindowThreadProcessId, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// Installed hooks; dropping this removes them.
pub struct Hooks(Vec<HWINEVENTHOOK>);

impl Drop for Hooks {
    fn drop(&mut self) {
        for h in self.0.drain(..) {
            unsafe { UnhookWinEvent(h) };
        }
    }
}

impl Hooks {
    /// Is `hook` one of these? Two sets of hooks can watch the same thread,
    /// and then one event arrives once through each.
    pub fn has(&self, hook: HWINEVENTHOOK) -> bool {
        self.0.contains(&hook)
    }
}

fn install(ranges: &[(u32, u32)], pid: u32, tid: u32, cb: WINEVENTPROC) -> Hooks {
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let hooks = ranges
        .iter()
        .map(|&(min, max)| unsafe { SetWinEventHook(min, max, std::ptr::null_mut(), cb, pid, tid, flags) })
        .filter(|h| !h.is_null())
        .collect();
    Hooks(hooks)
}

fn owner(hwnd: HWND) -> (u32, u32) {
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    (pid, tid)
}

pub fn thread_of(hwnd: HWND) -> u32 {
    owner(hwnd).1
}

/// Foreground changes anywhere.
pub fn foreground(cb: WINEVENTPROC) -> Hooks {
    install(&[(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND)], 0, 0, cb)
}

/// Moves and resizes of `hwnd`'s thread (F11 changes a window's bounds, not
/// the foreground). Returns the hooks and the thread they watch.
pub fn location(hwnd: HWND, cb: WINEVENTPROC) -> (Hooks, u32) {
    let (pid, tid) = owner(hwnd);
    (install(&[(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE)], pid, tid, cb), tid)
}

/// The events that matter for the window Clawd stands on.
pub fn perch(hwnd: HWND, cb: WINEVENTPROC) -> Hooks {
    let (pid, tid) = owner(hwnd);
    install(
        &[
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZESTART),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE), // destroy, show, hide
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CLOAKED),
        ],
        pid,
        tid,
        cb,
    )
}

/// Windows of one process appearing and going (destroy, show, hide): the
/// Claude desktop app being closed to the tray, reopened, or quit.
pub fn app(pid: u32, cb: WINEVENTPROC) -> Hooks {
    install(&[(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE)], pid, 0, cb)
}
