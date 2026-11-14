//! Real windows as `perch::WindowInfo`s (PLAN.md §6c): `EnumWindows`, DWM
//! bounds and cloaking. Window titles are never read.

use super::monitor;
use crate::goto::TopWindow;
use crate::perch::{self, Fit, Screen, WindowInfo};
use crate::placement::Rect;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWM_CLOAKED_APP, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Monitors as the pure code sees them.
pub fn screens() -> Vec<Screen> {
    monitor::all().into_iter().map(|m| Screen { full: m.full, work: m.work }).collect()
}

fn bounds(hwnd: HWND) -> Rect {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let hr = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut _ as *mut _, size_of::<RECT>() as u32) };
    if hr != 0 {
        // No DWM bounds (exclusive fullscreen, for one): the plain rectangle.
        unsafe { GetWindowRect(hwnd, &mut r) };
    }
    Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
}

/// Who cloaked it (`DWM_CLOAKED_*`); 0 = not cloaked.
fn cloak(hwnd: HWND) -> u32 {
    let mut v = 0u32;
    let hr = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut v as *mut _ as *mut _, size_of::<u32>() as u32) };
    if hr == 0 { v } else { 0 }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn process_name(hwnd: HWND) -> String {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 512];
    let mut n = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) } != 0;
    unsafe { CloseHandle(h) };
    if !ok {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..n as usize]);
    path.rsplit('\\').next().unwrap_or_default().to_string()
}

/// `None` if the window no longer exists. The process name (for the
/// deny-list) is only looked up with `with_process`.
pub fn info(hwnd: HWND, own: HWND, with_process: bool) -> Option<WindowInfo> {
    unsafe {
        if IsWindow(hwnd) == 0 {
            return None;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let caption = style & WS_CAPTION == WS_CAPTION;
        let app_window = ex & WS_EX_APPWINDOW != 0;
        let tool = ex & WS_EX_TOOLWINDOW != 0;
        Some(WindowInfo {
            id: hwnd as usize,
            bounds: bounds(hwnd),
            visible: IsWindowVisible(hwnd) != 0,
            iconic: IsIconic(hwnd) != 0,
            zoomed: IsZoomed(hwnd) != 0,
            cloaked: cloak(hwnd) != 0,
            own: hwnd == own,
            tool,
            caption,
            app_window,
            click_through: ex & WS_EX_TRANSPARENT != 0,
            layered: ex & WS_EX_LAYERED != 0,
            class: class_name(hwnd),
            process: if with_process && !tool && (caption || app_window) { process_name(hwnd) } else { String::new() },
        })
    }
}

/// Handles of every visible top-level window, topmost first.
fn visible_handles() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, out: LPARAM) -> i32 {
        if unsafe { IsWindowVisible(h) } != 0 {
            unsafe { &mut *(out as *mut Vec<HWND>) }.push(h);
        }
        1
    }
    let mut handles: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(cb), &mut handles as *mut _ as LPARAM) };
    handles
}

/// Every visible top-level window, topmost first.
pub fn list(own: HWND) -> Vec<WindowInfo> {
    visible_handles().into_iter().filter_map(|h| info(h, own, true)).collect()
}

/// The same windows, with what's needed to find a session's host.
pub fn top_windows() -> Vec<TopWindow> {
    visible_handles().into_iter().map(|h| top_window(h, pid_of(h))).collect()
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    pid
}

/// The rest of a `TopWindow`, which costs two DWM reads.
fn top_window(h: HWND, pid: u32) -> TopWindow {
    unsafe {
        let r = bounds(h);
        TopWindow {
            id: h as usize,
            pid,
            owned: !GetWindow(h, GW_OWNER).is_null(),
            tool: GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0,
            cloaked_by_app: cloak(h) & DWM_CLOAKED_APP != 0,
            empty: IsIconic(h) == 0 && (r.r <= r.l || r.b <= r.t),
        }
    }
}

/// Does that process have a window a person can see and use? Minimized
/// counts; closed to the tray doesn't. Only that process's windows are
/// read through DWM.
pub fn has_app_window(pid: u32) -> bool {
    visible_handles().into_iter().filter(|&h| pid_of(h) == pid).any(|h| top_window(h, pid).is_app_window())
}

pub fn exists(hwnd: HWND) -> bool {
    unsafe { IsWindow(hwnd) != 0 }
}

/// Bring a window to the front. Only ever after the user's own click:
/// Windows allows it then, and a pet that grabs the focus by itself would
/// be a nuisance. Never un-maximizes: only a minimized window is restored.
pub fn bring_forward(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd) == 0 {
            crate::dlog!("SetForegroundWindow refused");
        }
    }
}

/// Class and process name, for the debug log.
pub fn describe(hwnd: HWND) -> String {
    format!("{} {}", class_name(hwnd), process_name(hwnd))
}

pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Put `me` directly above `target`, so whatever covers `target` covers it too.
pub fn place_above(me: HWND, target: HWND) {
    const FLAGS: SET_WINDOW_POS_FLAGS = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    unsafe {
        // No effect if it's already non-topmost.
        SetWindowPos(me, HWND_NOTOPMOST, 0, 0, 0, 0, FLAGS);
        let prev = GetWindow(target, GW_HWNDPREV);
        if prev == me {
            return;
        }
        // Inserting below a topmost window would make `me` topmost too.
        let top = prev.is_null() || GetWindowLongW(prev, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0;
        SetWindowPos(me, if top { HWND_TOP } else { prev }, 0, 0, 0, 0, FLAGS);
    }
}

/// `--dump-windows <file>`: what Clawd would make of every window right now
/// (classes and process names only). For checking without showing the pet.
pub fn dump(path: &str, size: u32, deny: &[String]) -> std::io::Result<()> {
    use std::fmt::Write;
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    };
    let mut o = String::new();
    let scrs = screens();
    let monitors = monitor::all();
    let windows = list(std::ptr::null_mut());
    let fg = info(foreground(), std::ptr::null_mut(), true);
    let _ = writeln!(o, "foreground: {:?} → {:?}", fg.as_ref().map(|w| (&w.class, &w.process)), fg.as_ref().map(|w| perch::classify_foreground(w, &scrs)));
    for m in &monitors {
        let scale = m.dpi as f64 / 96.0;
        let clawd_h = crate::compose::CANVAS_H as i32 * super::window::window_px(size, m.dpi);
        let fit = Fit { screens: &scrs, deny, clawd_h, scale };
        let _ = writeln!(o, "\nmonitor {} full={:?} work={:?} dpi={} clawd_h={clawd_h}", m.name, m.full, m.work, m.dpi);
        let s = Screen { full: m.full, work: m.work };
        for f in perch::surfaces(&windows, &s, &fit, None) {
            let w = windows.iter().find(|w| w.id == f.owner);
            let _ = writeln!(o, "  surface y={} x={}..{} {:?}", f.y, f.x0, f.x1, w.map(|w| (&w.class, &w.process)));
        }
    }
    let m = monitors.first();
    let (scale, clawd_h) = m.map_or((1.0, 96), |m| (m.dpi as f64 / 96.0, crate::compose::CANVAS_H as i32 * super::window::window_px(size, m.dpi)));
    let fit = Fit { screens: &scrs, deny, clawd_h, scale };
    let _ = writeln!(o, "\nwindows, topmost first:");
    for w in &windows {
        let _ = writeln!(
            o,
            "  {:40} {:24} {:?} zoomed={} cloaked={} tool={} caption={} eligible={} occludes={}",
            w.class,
            w.process,
            w.bounds,
            w.zoomed,
            w.cloaked,
            w.tool,
            w.caption,
            perch::eligible(w, &fit),
            perch::occludes(w)
        );
    }
    std::fs::write(path, o)
}
