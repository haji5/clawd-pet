//! Monitors: work areas, device names and effective DPI.

use crate::placement::Rect;
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub full: Rect,
    pub work: Rect,
    pub dpi: u32,
}

fn rect(r: RECT) -> Rect {
    Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
}

fn info(h: HMONITOR) -> Option<Monitor> {
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO) } == 0 {
        return None;
    }
    let len = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
    let (mut dx, mut dy) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } != 0 {
        dx = 96;
    }
    Some(Monitor {
        name: String::from_utf16_lossy(&mi.szDevice[..len]),
        full: rect(mi.monitorInfo.rcMonitor),
        work: rect(mi.monitorInfo.rcWork),
        dpi: dx.max(48),
    })
}

pub fn all() -> Vec<Monitor> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> i32 {
        let out = unsafe { &mut *(out as *mut Vec<Monitor>) };
        out.extend(info(h));
        1
    }
    let mut v: Vec<Monitor> = Vec::new();
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(cb), &mut v as *mut _ as LPARAM) };
    v
}

pub fn primary() -> Monitor {
    let h = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    info(h).unwrap_or(Monitor {
        name: String::new(),
        full: Rect { l: 0, t: 0, r: 1920, b: 1080 },
        work: Rect { l: 0, t: 0, r: 1920, b: 1040 },
        dpi: 96,
    })
}

/// The monitor called `name`, or the primary one if it's gone (or `name` is empty).
pub fn by_name_or_primary(name: &str) -> Monitor {
    if !name.is_empty()
        && let Some(m) = all().into_iter().find(|m| m.name == name)
    {
        return m;
    }
    primary()
}

pub fn at_point(x: i32, y: i32) -> Monitor {
    let h = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    info(h).unwrap_or_else(primary)
}
