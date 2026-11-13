//! Notification-area icon. The icon is drawn at runtime from the idle frame,
//! so no `.ico` asset is needed for the tray.

use super::wide;
use crate::compose::Canvas;
use crate::sprite::Frame;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

pub const WM_APP_TRAY: u32 = 0x8000 + 1; // WM_APP + 1
const ICON_ID: u32 = 1;

/// An HICON with the frame drawn crisply: integer scale, padded to the
/// system's small-icon size.
pub fn icon_from_frame(f: &Frame) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
    let k = (size / f.w.max(f.h)).max(1);
    let logical = size / k;
    let mut c = Canvas::new(logical, logical);
    c.blit(f, (logical as i32 - f.w as i32) / 2, (logical as i32 - f.h as i32) / 2, false);
    let px = logical * k;
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: px as i32,
                biHeight: -(px as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(std::ptr::null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if color.is_null() {
            return std::ptr::null_mut();
        }
        // Pixels are fully opaque or fully transparent, so premultiplied == straight alpha.
        c.to_bgra_premul(k, std::slice::from_raw_parts_mut(bits as *mut u8, px * px * 4));
        let mask_bits = vec![0u8; px.div_ceil(16) * 2 * px];
        let mask = CreateBitmap(px as i32, px as i32, 1, 1, mask_bits.as_ptr() as _);
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    /// The highlighted icon it pulses to while being ignored.
    alert: HICON,
    alert_on: bool,
    tip: String,
}

impl Tray {
    pub fn new(hwnd: HWND, icon: HICON, alert: HICON) -> Tray {
        let t = Tray { hwnd, icon, alert, alert_on: false, tip: "Clawd".into() };
        t.add();
        t
    }

    pub fn alert_on(&self) -> bool {
        self.alert_on
    }

    pub fn set_alert(&mut self, on: bool) {
        if self.alert_on != on && !self.alert.is_null() {
            self.alert_on = on;
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data(NIF_ICON)) };
        }
    }

    /// Only the fields named in `flags` reach Explorer, and it redraws what
    /// it's given, so a changed tooltip doesn't resend the icon.
    fn data(&self, flags: u32) -> NOTIFYICONDATAW {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = ICON_ID;
        nid.uFlags = flags;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = if self.alert_on { self.alert } else { self.icon };
        let tip = wide(&self.tip);
        let n = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        nid
    }

    /// Also used to re-add the icon after Explorer restarts (`TaskbarCreated`).
    pub fn add(&self) {
        unsafe { Shell_NotifyIconW(NIM_ADD, &self.data(NIF_MESSAGE | NIF_ICON | NIF_TIP)) };
    }

    /// Tooltip text, max 127 UTF-16 units (longer text is cut).
    pub fn set_tip(&mut self, tip: &str) {
        if self.tip != tip {
            self.tip = tip.into();
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.data(NIF_TIP)) };
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.data(0));
            for icon in [self.icon, self.alert] {
                if !icon.is_null() {
                    DestroyIcon(icon);
                }
            }
        }
    }
}
