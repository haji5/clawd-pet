//! Spawning the pet (or a probe) fully detached from Claude Code.

use std::iter::repeat_n;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use super::wide;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, GetLastError};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, OpenMutexW,
    PROCESS_INFORMATION, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE,
};

/// Held by the running pet for as long as it lives. A pet on a test port
/// (`CLAWD_PET_PORT`) has its own, so checks can run beside the user's pet.
pub fn mutex_name() -> String {
    match crate::ipc::env_port() {
        Some(port) => format!("Local\\ClawdPet-{port}"),
        None => "Local\\ClawdPet".to_string(),
    }
}

/// Is a pet running? Takes microseconds. Any failure other than "no such
/// mutex" counts as yes: a duplicate launch is the worse mistake.
pub fn pet_running() -> bool {
    let name = wide(&mutex_name());
    let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return unsafe { GetLastError() } != ERROR_FILE_NOT_FOUND;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// One argument, quoted so `CommandLineToArgvW` and the Rust runtime read it
/// back unchanged, whatever quotes, spaces or backslashes it holds.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Spawn this exe again with `args`, detached (`spawn_detached`).
///
/// `env` is added to this process's environment first, and so to the new
/// one's: the way to hand over something that shouldn't sit on a command
/// line. Only call this from a process that is about to exit anyway.
pub fn spawn_self_detached(args: &[&str], env: &[(&str, &str)]) {
    let Ok(exe) = std::env::current_exe() else { return };
    for (name, value) in env {
        // SAFETY: the hook has no other threads.
        unsafe { std::env::set_var(name, value) };
    }
    spawn_detached(&exe, args);
}

/// Start `exe` with `args`, detached, without inheriting handles; false if
/// it couldn't be started. It runs from the exe's own folder: the hook's
/// working directory is the user's project, and a long-lived pet would keep
/// that folder open, so it couldn't be renamed or deleted. The environment
/// is this process's, unchanged.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> bool {
    let mut cmd = format!("\"{}\"", exe.display());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&quote_arg(a));
    }
    let dir: Option<Vec<u16>> = exe.parent().map(|d| d.as_os_str().encode_wide().chain(std::iter::once(0)).collect());
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Breaking away from Claude's job can be refused; try without it then.
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
        let mut cmdline = wide(&cmd);
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..unsafe { std::mem::zeroed() } };
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0, // don't inherit Claude's stdio pipes
                flags,
                std::ptr::null(),
                dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                &si,
                &mut pi,
            )
        };
        if ok != 0 {
            unsafe {
                CloseHandle(pi.hThread);
                CloseHandle(pi.hProcess);
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// What a new process would see for this argument.
    fn read_back(arg: &str) -> Vec<String> {
        let line = wide(&format!("x.exe {} end", quote_arg(arg)));
        let mut n = 0;
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut n) };
        assert!(!argv.is_null());
        let out = (1..n as usize)
            .map(|i| unsafe {
                let p = *argv.add(i);
                let len = (0..).take_while(|&j| *p.add(j) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
            })
            .collect();
        unsafe { LocalFree(argv as _) };
        out
    }

    #[test]
    fn arguments_survive_the_command_line() {
        for a in [
            "--hidden",
            r#"{"v":1,"e":"SessionStart","s":"abc","p":"my app","ts":5}"#,
            r"C:\dir with space\",
            r#"back\"slash"#,
            r"two\\ slashes",
            "tab\there",
            "",
        ] {
            assert_eq!(read_back(a), [a, "end"], "{}", quote_arg(a));
        }
    }
}
