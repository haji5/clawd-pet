//! Where Clawd stands: corner-anchored positions that survive resolution, DPI
//! and taskbar changes (PLAN.md §4 "Placement and anchoring"). Pure.
//!
//! Offsets are stored in DIPs (96-dpi pixels) relative to the nearest corner
//! of the monitor's work area; everything else here is physical pixels.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

impl Corner {
    fn left(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::BottomLeft)
    }
    fn top(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::TopRight)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Position {
    /// Monitor device name (`\\.\DISPLAY1`). Empty = primary monitor.
    pub monitor: String,
    pub anchor: Corner,
    /// DIPs from the anchor corner's vertical edge to the window's near edge.
    pub dx: i32,
    /// DIPs from the anchor corner's horizontal edge to the window's near edge.
    pub dy: i32,
    /// Feet on the bottom edge of the work area (top of the taskbar); `dy` is ignored.
    pub on_taskbar: bool,
}

impl Default for Position {
    /// The default spot: bottom-right of the primary monitor, on the taskbar.
    fn default() -> Self {
        Position { monitor: String::new(), anchor: Corner::BottomRight, dx: 48, dy: 0, on_taskbar: true }
    }
}

fn to_px(dip: i32, dpi: u32) -> i32 {
    (dip as f64 * dpi as f64 / 96.0).round() as i32
}
fn to_dip(px: i32, dpi: u32) -> i32 {
    (px as f64 * 96.0 / dpi as f64).round() as i32
}

/// Keep a `w`×`h` window fully inside `work`.
pub fn clamp(x: i32, y: i32, w: i32, h: i32, work: Rect) -> (i32, i32) {
    (x.clamp(work.l, (work.r - w).max(work.l)), y.clamp(work.t, (work.b - h).max(work.t)))
}

/// Window top-left for `pos` on a monitor with work area `work` at `dpi`.
pub fn resolve(pos: &Position, work: Rect, w: i32, h: i32, dpi: u32) -> (i32, i32) {
    let (dx, dy) = (to_px(pos.dx, dpi), to_px(pos.dy, dpi));
    let x = if pos.anchor.left() { work.l + dx } else { work.r - w - dx };
    let y = if pos.on_taskbar {
        work.b - h
    } else if pos.anchor.top() {
        work.t + dy
    } else {
        work.b - h - dy
    };
    clamp(x, y, w, h, work)
}

/// Describe a window at (x, y) relative to the nearest corner of `work`.
pub fn anchor_for(x: i32, y: i32, w: i32, h: i32, work: Rect, dpi: u32, monitor: &str) -> Position {
    let left = x + w / 2 < (work.l + work.r) / 2;
    let on_taskbar = y + h == work.b;
    let top = !on_taskbar && y + h / 2 < (work.t + work.b) / 2;
    let anchor = match (top, left) {
        (true, true) => Corner::TopLeft,
        (true, false) => Corner::TopRight,
        (false, true) => Corner::BottomLeft,
        (false, false) => Corner::BottomRight,
    };
    let dx = if left { x - work.l } else { work.r - (x + w) };
    let dy = if top { y - work.t } else { work.b - (y + h) };
    Position { monitor: monitor.into(), anchor, dx: to_dip(dx, dpi), dy: to_dip(dy, dpi), on_taskbar }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1920×1080 monitor with a 48 px taskbar at the bottom.
    const WORK: Rect = Rect { l: 0, t: 0, r: 1920, b: 1032 };

    #[test]
    fn default_spot_stands_on_taskbar_bottom_right() {
        let (x, y) = resolve(&Position::default(), WORK, 96, 72, 96);
        assert_eq!((x, y), (1920 - 96 - 48, 1032 - 72));
        // At 150 % the offset scales with DPI.
        let (x, _) = resolve(&Position::default(), WORK, 144, 108, 144);
        assert_eq!(x, 1920 - 144 - 72);
    }

    #[test]
    fn anchor_roundtrips_in_every_quadrant() {
        // All positions fit inside WORK for both window sizes (96×72 and 144×108).
        for (x, y) in [(10, 20), (1700, 30), (40, 900), (1500, 924), (900, 500)] {
            let p = anchor_for(x, y, 96, 72, WORK, 96, "m");
            assert_eq!(resolve(&p, WORK, 96, 72, 96), (x, y), "{p:?}");
            // At 150 % the DIP conversion may round by at most one pixel.
            let p = anchor_for(x, y, 144, 108, WORK, 144, "m");
            let (rx, ry) = resolve(&p, WORK, 144, 108, 144);
            assert!((rx - x).abs() <= 1 && (ry - y).abs() <= 1, "{p:?} → {rx},{ry}");
        }
    }

    #[test]
    fn anchor_survives_resolution_change() {
        let p = anchor_for(1920 - 96 - 48, 1032 - 72, 96, 72, WORK, 96, "");
        assert!(p.on_taskbar);
        assert_eq!(p.anchor, Corner::BottomRight);
        // Switch to 2560×1440 with a taller taskbar: still bottom-right, still on the taskbar.
        let big = Rect { l: 0, t: 0, r: 2560, b: 1380 };
        assert_eq!(resolve(&p, big, 96, 72, 96), (2560 - 96 - 48, 1380 - 72));
    }

    #[test]
    fn taskbar_on_top_means_bottom_screen_edge() {
        // Taskbar at the top: work area starts at y=48 and ends at the screen bottom.
        let work = Rect { l: 0, t: 48, r: 1920, b: 1080 };
        assert_eq!(resolve(&Position::default(), work, 96, 72, 96).1, 1080 - 72);
    }

    #[test]
    fn offscreen_position_is_clamped_into_work_area() {
        let p = Position { dx: 99999, dy: 99999, on_taskbar: false, ..Default::default() };
        assert_eq!(resolve(&p, WORK, 96, 72, 96), (0, 0));
    }
}
