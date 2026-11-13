//! A 32-bit top-down DIB section pushed to the layered window with
//! `UpdateLayeredWindow` + per-pixel alpha.

use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    UPDATELAYEREDWINDOWINFO, ULW_ALPHA, UpdateLayeredWindow, UpdateLayeredWindowIndirect,
};

pub struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
    /// Whole-window opacity, for fading out.
    pub alpha: u8,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..unsafe { std::mem::zeroed() }
            },
            ..unsafe { std::mem::zeroed() }
        };
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Surface { dc, bmp, old, bits: bits as *mut u8, w, h, alpha: 255 })
        }
    }

    /// Premultiplied BGRA, top-down, `w * 4` bytes per row.
    pub fn buf(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }

    /// The whole surface, for `present`.
    pub fn all(&self) -> RECT {
        RECT { left: 0, top: 0, right: self.w, bottom: self.h }
    }

    /// Show the part of the buffer in `dirty` and move the window to (x, y)
    /// in one call, so moving and redrawing never tear.
    pub fn present(&self, hwnd: HWND, x: i32, y: i32, dirty: RECT) -> bool {
        let dst = POINT { x, y };
        let src = POINT { x: 0, y: 0 };
        let size = SIZE { cx: self.w, cy: self.h };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: self.alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: std::ptr::null_mut(),
            pptDst: &dst,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &src,
            crKey: 0,
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: &dirty,
        };
        unsafe { UpdateLayeredWindowIndirect(hwnd, &info) != 0 }
    }

    /// Move the window without touching its pixels.
    pub fn move_to(hwnd: HWND, x: i32, y: i32) -> bool {
        let dst = POINT { x, y };
        unsafe { UpdateLayeredWindow(hwnd, null_mut(), &dst, null(), null_mut(), null(), 0, null(), 0) != 0 }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}
