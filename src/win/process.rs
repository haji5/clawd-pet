//! Finding the window that hosts a Claude Code process (PLAN.md §8 item 5b).
//! The decisions are in `goto.rs`; this reads the real processes and consoles.

use super::{wide, windows};
use crate::goto::{self, Host, Proc, TopWindow};
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOTOWNER, GetAncestor, IsWindowVisible, SW_SHOWNORMAL};

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
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut start, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        let mut code = 0;
        let running = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE;
        let ok = running && GetProcessTimes(h, &mut start, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(h);
        ok.then_some((start.dwHighDateTime as u64) << 32 | start.dwLowDateTime as u64)
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
}
