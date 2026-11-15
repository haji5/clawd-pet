//! Finding the window that hosts a Claude Code process (PLAN.md §8 item 5b).
//! The decisions are in `goto.rs`; this reads the real processes and consoles.

use super::{wide, windows};
use crate::goto::{self, Host, Proc, TopWindow};
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, INFINITE, OpenProcess, PROCESS_BASIC_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEONLYONCE,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOTOWNER, GetAncestor, IsWindowVisible, PostMessageW, SW_SHOWNORMAL};

/// Every running process.
fn all_processes() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(Proc { pid: e.th32ProcessID, parent: e.th32ParentProcessID, name: String::from_utf16_lossy(&e.szExeFile[..len]) });
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// For the hook: the process that started it. Asks the kernel about this
/// one process: a Toolhelp snapshot measured ~15 ms here, as much as the
/// hook's whole budget.
pub fn parent_of_this_process() -> Option<u32> {
    unsafe {
        let mut info: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
        let status = NtQueryInformationProcess(
            GetCurrentProcess(),
            ProcessBasicInformation,
            &mut info as *mut _ as *mut _,
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        (status == 0 && info.InheritedFromUniqueProcessId != 0).then_some(info.InheritedFromUniqueProcessId as u32)
    }
}

/// What `GetExitCodeProcess` gives for a process that hasn't exited.
const STILL_ACTIVE: u32 = 259;

/// When `pid` started, in `FILETIME` ticks. `None` if there's no such
/// process, it can't be opened, or it has exited: a process that ended
/// stays around, pid and all, for as long as anyone holds a handle to it
/// (the shell that started it, say), and that is no host for a session.
pub fn created(pid: u32) -> Option<u64> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let start = created_from(h);
        CloseHandle(h);
        start
    }
}

/// `created`, for a process handle opened with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
unsafe fn created_from(h: HANDLE) -> Option<u64> {
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut start, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    let mut code = 0;
    let running = unsafe { GetExitCodeProcess(h, &mut code) } != 0 && code == STILL_ACTIVE;
    let ok = running && unsafe { GetProcessTimes(h, &mut start, &mut exit, &mut kernel, &mut user) } != 0;
    ok.then_some((start.dwHighDateTime as u64) << 32 | start.dwLowDateTime as u64)
}

/// Posted to the pet window when a watched process may have ended. A plain
/// wake-up: the window asks each watch (`ExitWatch::ended`).
pub const WM_APP_EXITED: u32 = 0x8000 + 6; // WM_APP + 6

/// A process Clawd is told about when it ends. Holds the process open, so
/// its pid can't be reused meanwhile. Lives on the UI thread; the thread
/// pool's wait only posts `WM_APP_EXITED`.
pub struct ExitWatch {
    process: HANDLE,
    wait: HANDLE,
}

/// Watch `pid`: when it ends, `hwnd` gets `WM_APP_EXITED`. `None` if it
/// can't be opened or has already exited: such a process is no host (the
/// same rule as `created`). One that exits after the open is still
/// reported, at once.
pub fn watch(pid: u32, hwnd: HWND) -> Option<(Host, ExitWatch)> {
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        // From the same handle, so the time belongs to the process watched.
        let Some(created) = created_from(process) else {
            CloseHandle(process);
            return None;
        };
        let mut wait: HANDLE = std::ptr::null_mut();
        if RegisterWaitForSingleObject(&mut wait, process, Some(exited), hwnd as _, INFINITE, WT_EXECUTEONLYONCE) == 0 {
            CloseHandle(process);
            return None;
        }
        Some((Host { pid, created }, ExitWatch { process, wait }))
    }
}

/// Runs on a thread-pool thread: only posts (a failed post, after the
/// window is gone, is ignored). Must not panic and never touches `App`.
unsafe extern "system" fn exited(hwnd: *mut core::ffi::c_void, _timed_out: bool) {
    unsafe { PostMessageW(hwnd as HWND, WM_APP_EXITED, 0, 0) };
}

impl ExitWatch {
    /// Has the process ended? A zero wait: free to ask.
    pub fn ended(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        unsafe {
            // `INVALID_HANDLE_VALUE`: wait for a callback that is running (one
            // `PostMessageW`), so none runs once the watch is gone. Never
            // drop a watch from inside the callback.
            UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);
            CloseHandle(self.process);
        }
    }
}

/// Wait up to `ms` for process `pid` to end. A pid that can't be opened
/// has already gone (or isn't ours to wait for).
pub fn wait_for_exit(pid: u32, ms: u32) {
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, ms);
            CloseHandle(h);
        }
    }
}

/// The window of the console `pid` runs in, if a person can see it. A
/// console says exactly where a CLI lives, even when one terminal process
/// has several windows. A classic console window is visible itself; a
/// terminal's pseudo console is hidden and owned by the terminal's window;
/// the desktop app's console is hidden with no owner, so there's no answer.
fn console_window(pid: u32) -> Option<HWND> {
    unsafe {
        // Don't let a Ctrl+C in that console end the pet during the moment
        // it's attached.
        SetConsoleCtrlHandler(None, 1);
        let attached = AttachConsole(pid) != 0;
        let console = if attached { GetConsoleWindow() } else { std::ptr::null_mut() };
        if attached {
            FreeConsole();
        }
        SetConsoleCtrlHandler(None, 0);
        if console.is_null() {
            return None;
        }
        let window = if IsWindowVisible(console) != 0 { console } else { GetAncestor(console, GA_ROOTOWNER) };
        (!window.is_null() && IsWindowVisible(window) != 0).then_some(window)
    }
}

/// Hand a link to the app registered for it.
pub fn open_link(url: &str) {
    let (verb, url) = (wide("open"), wide(url));
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}

/// One look at the processes and windows, for one click or one menu.
pub struct Finder {
    procs: Vec<Proc>,
    windows: Vec<TopWindow>,
}

impl Finder {
    pub fn new() -> Finder {
        Finder { procs: all_processes(), windows: windows::top_windows() }
    }

    /// The desktop app's process for a session hosted there (`goto::app_process`).
    pub fn app_process(&self, host: Host) -> Option<u32> {
        goto::app_process(&goto::chain(host, &self.procs, created), &self.procs, &self.windows)
    }

    /// The window hosting that process; `None` is the honest answer when
    /// it's gone or nothing fits.
    pub fn window_for(&self, host: Host) -> Option<HWND> {
        let chain = goto::chain(host, &self.procs, created);
        if chain.is_empty() {
            return None;
        }
        console_window(host.pid).or_else(|| goto::pick(&chain, &self.windows).map(|id| id as HWND))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_that_has_exited_has_no_creation_time_even_while_a_handle_is_held() {
        assert!(created(std::process::id()).is_some(), "this test is running");
        // `child` keeps a handle open, so the process object outlives the exit.
        let mut child = std::process::Command::new("cmd").args(["/c", "exit"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(created(pid), None);
        drop(child);
    }

    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW};

    /// A message-only window for this test's thread.
    struct MsgWindow(HWND);

    impl MsgWindow {
        fn new() -> MsgWindow {
            let class = wide("STATIC");
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            MsgWindow(hwnd)
        }

        /// Pump until `WM_APP_EXITED` arrives or `ms` pass; whether it came.
        fn exited_within(&self, ms: u64) -> bool {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < deadline {
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                while unsafe { PeekMessageW(&mut msg, self.0, 0, 0, PM_REMOVE) } != 0 {
                    if msg.message == WM_APP_EXITED {
                        return true;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        }
    }

    impl Drop for MsgWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_watch_posts_when_its_process_ends() {
        let window = MsgWindow::new();
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 2 127.0.0.1 >nul"]).spawn().unwrap();
        let (host, watch) = watch(child.id(), window.0).expect("it's running");
        assert_eq!(host, Host { pid: child.id(), created: created(child.id()).unwrap() });
        assert!(!watch.ended());
        assert!(window.exited_within(10_000), "posted");
        assert!(watch.ended());
        child.wait().unwrap();
    }

    #[test]
    fn a_process_that_has_exited_cant_be_watched() {
        let window = MsgWindow::new();
        let mut child = std::process::Command::new("cmd").args(["/c", "exit"]).spawn().unwrap();
        child.wait().unwrap(); // `child` still holds a handle
        assert!(watch(child.id(), window.0).is_none());
        assert!(watch(u32::MAX - 3, window.0).is_none(), "no such process");
        drop(child);
    }

    #[test]
    fn a_dropped_watch_goes_at_once_and_posts_nothing() {
        let window = MsgWindow::new();
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 30 127.0.0.1 >nul"]).spawn().unwrap();
        let (_, watch) = watch(child.id(), window.0).expect("it's running");
        let t = std::time::Instant::now();
        drop(watch);
        assert!(t.elapsed().as_millis() < 100, "{:?}", t.elapsed());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!window.exited_within(300), "nothing after the drop");
    }

    #[test]
    fn waits_for_a_process_to_end_and_not_for_one_that_has() {
        let mut child = std::process::Command::new("cmd").args(["/c", "ping -n 2 127.0.0.1 >nul"]).spawn().unwrap();
        let t = std::time::Instant::now();
        wait_for_exit(child.id(), 10_000);
        assert!(child.try_wait().unwrap().is_some(), "it has ended");
        assert!(t.elapsed().as_secs() < 9);
        let t = std::time::Instant::now();
        wait_for_exit(child.id(), 10_000); // ended, handle still held: signalled at once
        wait_for_exit(u32::MAX - 3, 10_000); // no such process
        assert!(t.elapsed().as_millis() < 1000);
    }
}
