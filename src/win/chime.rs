//! Plays the attention chime. `winmm.dll` is loaded on first use, so the
//! hook (and a pet with the sound off) never pays for it.

use super::wide;
use std::sync::OnceLock;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const SND_ASYNC: u32 = 0x1;
const SND_NODEFAULT: u32 = 0x2;
const SND_MEMORY: u32 = 0x4;

type PlaySound = unsafe extern "system" fn(*const u8, *mut core::ffi::c_void, u32) -> i32;

fn player() -> Option<PlaySound> {
    static PLAYER: OnceLock<Option<PlaySound>> = OnceLock::new();
    *PLAYER.get_or_init(|| unsafe {
        let lib = LoadLibraryW(wide("winmm.dll").as_ptr());
        if lib.is_null() {
            return None;
        }
        // SAFETY: PlaySoundW has exactly this signature; the first argument
        // is a pointer to the WAV image because of SND_MEMORY.
        GetProcAddress(lib, c"PlaySoundW".as_ptr().cast()).map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, PlaySound>(f))
    })
}

/// `--debug-log` check: the sound can be played, without playing it.
pub fn available() -> bool {
    player().is_some()
}

pub fn play() {
    // Lives forever: SND_ASYNC plays from this memory after the call returns.
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(crate::sound::chime);
    if let Some(play) = player() {
        unsafe { play(wav.as_ptr(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
    }
}
