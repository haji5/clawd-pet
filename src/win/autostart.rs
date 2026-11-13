//! "Start with Windows": a value under the per-user Run key (PLAN.md §7).

use super::wide;
use std::path::Path;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "ClawdPet";

fn exists(key: &str) -> bool {
    let (k, v) = (wide(key), wide(VALUE));
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
    };
    status == ERROR_SUCCESS
}

fn set(key: &str, data: &str) -> Result<(), String> {
    let (k, v, d) = (wide(key), wide(VALUE), wide(data));
    let status = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), REG_SZ, d.as_ptr() as _, (d.len() * 2) as u32) };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("Could not turn on Start with Windows (error {status}).")) }
}

fn delete(key: &str) -> Result<(), String> {
    let (k, v) = (wide(key), wide(VALUE));
    let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr()) };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("Could not turn off Start with Windows (error {status}).")) }
}

pub fn enabled() -> bool {
    exists(RUN_KEY)
}

pub fn enable(exe: &Path) -> Result<(), String> {
    set(RUN_KEY, &format!("\"{}\"", exe.display()))
}

pub fn disable() -> Result<(), String> {
    delete(RUN_KEY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;

    /// On a scratch key, never the real Run key.
    #[test]
    fn set_exists_delete() {
        let key = format!(r"Software\ClawdPetTest-{}", std::process::id());
        assert!(!exists(&key));
        set(&key, r#""C:\some path\clawd-pet.exe""#).unwrap();
        assert!(exists(&key));
        delete(&key).unwrap();
        assert!(!exists(&key));
        assert!(delete(&key).is_err());
        assert_eq!(unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(&key).as_ptr()) }, ERROR_SUCCESS);
    }
}
