//! Perching on windows and spotting fullscreen apps (PLAN.md §6c). Pure: the
//! Win32 side (`win::windows`) turns real windows into `WindowInfo`s and
//! everything is decided here. Units are physical pixels; `scale` = DPI/96
//! turns the DIP tunables in `feel.rs` into pixels.

use crate::feel::*;
use crate::interaction::{Shake, Trail};
use crate::physics::Floor;
use crate::placement::Rect;
use crate::tracker::Ms;

/// A window handle as a plain number (0 is the ground, see `Floor::owner`).
pub type WinId = usize;

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: WinId,
    /// The visible edges (DWM extended frame bounds).
    pub bounds: Rect,
    pub visible: bool,
    pub iconic: bool,
    pub zoomed: bool,
    pub cloaked: bool,
    /// Clawd's own window.
    pub own: bool,
    pub tool: bool,
    pub caption: bool,
    pub app_window: bool,
    /// `WS_EX_TRANSPARENT`: mouse input goes straight through.
    pub click_through: bool,
    pub layered: bool,
    pub class: String,
    /// Executable file name; empty when it wasn't looked up.
    pub process: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub full: Rect,
    pub work: Rect,
}

/// What eligibility depends on besides the window itself.
pub struct Fit<'a> {
    pub screens: &'a [Screen],
    pub deny: &'a [String],
    /// Room Clawd needs above a window (px).
    pub clawd_h: i32,
    pub scale: f64,
}

/// Does `b` cover a whole monitor (within the tolerance)? Catches exclusive
/// and borderless fullscreen alike, whatever the window style says.
pub fn covers_monitor(b: Rect, screens: &[Screen]) -> bool {
    let t = FULLSCREEN_TOLERANCE_PX;
    screens.iter().any(|s| b.l <= s.full.l + t && b.t <= s.full.t + t && b.r >= s.full.r - t && b.b >= s.full.b - t)
}

/// The monitor holding the middle of the window's top edge.
fn screen_of(b: Rect, screens: &[Screen]) -> Option<&Screen> {
    let x = (b.l + b.r) / 2;
    screens.iter().find(|s| x >= s.full.l && x < s.full.r && b.t >= s.full.t && b.t < s.full.b)
}

fn is_shell(class: &str) -> bool {
    matches!(class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Can Clawd stand on this window?
pub fn eligible(w: &WindowInfo, fit: &Fit) -> bool {
    if !w.visible || w.iconic || w.own || w.tool || w.cloaked || w.zoomed || is_shell(&w.class) {
        return false;
    }
    if !(w.caption || w.app_window) || covers_monitor(w.bounds, fit.screens) {
        return false;
    }
    if !w.process.is_empty() && fit.deny.iter().any(|d| d.eq_ignore_ascii_case(&w.process)) {
        return false;
    }
    if ((w.bounds.r - w.bounds.l) as f64) < PERCH_MIN_WIDTH_DIP * fit.scale {
        return false;
    }
    screen_of(w.bounds, fit.screens).is_some_and(|s| w.bounds.t >= s.work.t + fit.clawd_h)
}

/// Does this window hide what's behind it? Invisible overlays (click-through
/// or layered tool windows, often as big as the screen) don't count.
pub fn occludes(w: &WindowInfo) -> bool {
    w.visible && !w.iconic && !w.cloaked && !w.own && !w.click_through && !(w.tool && w.layered) && !is_shell(&w.class)
}

/// Remove `[a, b]` from a list of disjoint intervals.
fn cut(segs: Vec<(f64, f64)>, a: f64, b: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(segs.len() + 1);
    for (x0, x1) in segs {
        if b <= x0 || a >= x1 {
            out.push((x0, x1));
            continue;
        }
        if a > x0 {
            out.push((x0, a));
        }
        if b < x1 {
            out.push((b, x1));
        }
    }
    out
}

/// The title bars Clawd can land on, on screen `on`: each eligible window's
/// top edge, minus the parts covered by windows above it. `windows` is in
/// z-order, topmost first (as `EnumWindows` gives them).
pub fn surfaces(windows: &[WindowInfo], on: &Screen, fit: &Fit, exclude: Option<WinId>) -> Vec<Floor> {
    let s = fit.scale;
    let mut out = Vec::new();
    for (i, w) in windows.iter().enumerate() {
        if exclude == Some(w.id) || !eligible(w, fit) {
            continue;
        }
        let y = w.bounds.t;
        if y < on.work.t + fit.clawd_h || y >= on.work.b {
            continue;
        }
        let x0 = (w.bounds.l as f64 + PERCH_INSET_LEFT_DIP * s).max(on.work.l as f64);
        let x1 = (w.bounds.r as f64 - PERCH_INSET_RIGHT_DIP * s).min(on.work.r as f64);
        let mut segs = if x1 > x0 { vec![(x0, x1)] } else { vec![] };
        for o in windows[..i].iter().filter(|o| occludes(o) && o.bounds.t <= y && y < o.bounds.b) {
            segs = cut(segs, o.bounds.l as f64, o.bounds.r as f64);
        }
        out.extend(
            segs.into_iter()
                .filter(|(a, b)| b - a >= PERCH_MIN_SEGMENT_DIP * s)
                .map(|(a, b)| Floor { x0: a, x1: b, y: y as f64, owner: w.id }),
        );
    }
    out
}

/// How the foreground window affects Clawd's visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    Fullscreen,
    Normal,
    /// Alt-Tab, Task View, Start, the lock screen: passing through, so
    /// whatever was decided before stands.
    Transient,
}

pub fn classify_foreground(w: &WindowInfo, screens: &[Screen]) -> Foreground {
    if !w.visible || w.cloaked || w.own {
        return Foreground::Transient;
    }
    match w.class.as_str() {
        "ForegroundStaging" | "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame" | "TaskSwitcherWnd"
        | "Windows.UI.Core.CoreWindow" => return Foreground::Transient,
        c if is_shell(c) => return Foreground::Normal,
        _ => {}
    }
    // A maximized app with an auto-hide taskbar also covers the monitor, but
    // it isn't fullscreen: it still has its title bar.
    if covers_monitor(w.bounds, screens) && !(w.zoomed && w.caption) { Foreground::Fullscreen } else { Foreground::Normal }
}

/// What a move of the perch window meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RideEvent {
    /// Same place as before (a resize of another edge, or a recheck).
    Still,
    Moved,
    /// Shaken hard enough to throw Clawd off.
    KnockedOff,
}

/// Standing on a window: where along it, and how it has been moving.
pub struct Ride {
    pub id: WinId,
    rel_x: f64,
    last: (i32, i32),
    trail: Trail,
    shake_x: Shake,
    shake_y: Shake,
}

impl Ride {
    pub fn new(id: WinId, feet_x: f64, bounds: Rect) -> Self {
        Ride {
            id,
            rel_x: feet_x - bounds.l as f64,
            last: (bounds.l, bounds.t),
            trail: Trail::default(),
            shake_x: Shake::at(bounds.l as f64),
            shake_y: Shake::at(bounds.t as f64),
        }
    }

    /// Where the feet go on a window now at `b`: the same distance from its
    /// left edge, kept clear of the caption buttons.
    pub fn feet(&self, b: Rect, scale: f64) -> (f64, f64) {
        let lo = b.l as f64 + PERCH_INSET_LEFT_DIP * scale;
        let hi = (b.r as f64 - PERCH_INSET_RIGHT_DIP * scale).max(lo);
        ((b.l as f64 + self.rel_x).clamp(lo, hi), b.t as f64)
    }

    pub fn moved(&mut self, now: Ms, left: i32, top: i32, scale: f64) -> RideEvent {
        if self.last == (left, top) {
            return RideEvent::Still;
        }
        self.last = (left, top);
        self.trail.push(now, left as f64, top as f64);
        let min = RIDE_SHAKE_MIN_DIP * scale;
        let x = self.shake_x.track(now, left as f64, min, RIDE_SHAKE_WINDOW_MS);
        let y = self.shake_y.track(now, top as f64, min, RIDE_SHAKE_WINDOW_MS);
        if x.max(y) >= RIDE_SHAKE_REVERSALS { RideEvent::KnockedOff } else { RideEvent::Moved }
    }

    /// The window's recent velocity (px/s), zero once it has stopped.
    pub fn velocity(&self, now: Ms, scale: f64) -> (f64, f64) {
        self.trail.velocity(now, THROW_MAX_DIP_S * scale)
    }
}

/// Where to land on a window's surfaces when hopping from `from_x`: the
/// nearest point with the whole body on the surface, on the widest stretch
/// that can hold it (or the middle of the widest if none can).
pub fn hop_target(floors: &[Floor], id: WinId, from_x: f64, half_w: f64) -> Option<(f64, f64)> {
    let mine: Vec<&Floor> = floors.iter().filter(|f| f.owner == id).collect();
    let widest = mine.iter().copied().max_by(|a, b| (a.x1 - a.x0).total_cmp(&(b.x1 - b.x0)))?;
    let (lo, hi) = (widest.x0 + half_w, widest.x1 - half_w);
    let x = if lo <= hi { from_x.clamp(lo, hi) } else { (widest.x0 + widest.x1) / 2.0 };
    Some((x, widest.y))
}

/// Launch velocity for an arc from `from` to `to` (no air drag; see
/// `Sim::hop`) that peaks `HOP_APEX_DIP` above the higher end, but not above
/// `ceiling`.
pub fn hop_velocity(from: (f64, f64), to: (f64, f64), ceiling: f64, scale: f64) -> (f64, f64) {
    let g = GRAVITY_DIP_S2 * scale;
    let high = from.1.min(to.1);
    let apex = (high - HOP_APEX_DIP * scale).max(ceiling).min(high);
    let vy = -(2.0 * g * (from.1 - apex)).sqrt();
    // Count the simulation's own steps: the closed-form flight time is off
    // by a few steps, which is a lot of pixels on a long hop.
    let (mut y, mut v, mut steps) = (from.1, vy, 0u32);
    while steps < 2400 && !(v > 0.0 && y >= to.1) {
        v += g * PHYS_DT;
        y += v * PHYS_DT;
        steps += 1;
    }
    ((to.0 - from.0) / (steps.max(1) as f64 * PHYS_DT), vy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{Event, Sim, State, World};

    // Two 1920×1080 monitors side by side; 40 px taskbars.
    const A: Screen = Screen { full: Rect { l: 0, t: 0, r: 1920, b: 1080 }, work: Rect { l: 0, t: 0, r: 1920, b: 1040 } };
    const B: Screen =
        Screen { full: Rect { l: 1920, t: 0, r: 3840, b: 1080 }, work: Rect { l: 1920, t: 0, r: 3840, b: 1040 } };
    const SCREENS: [Screen; 2] = [A, B];

    fn fit() -> Fit<'static> {
        static DENY: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| vec!["steam.exe".to_string()]);
        Fit { screens: &SCREENS, deny: &DENY, clawd_h: 96, scale: 1.0 }
    }

    fn win(id: WinId, l: i32, t: i32, r: i32, b: i32) -> WindowInfo {
        WindowInfo {
            id,
            bounds: Rect { l, t, r, b },
            visible: true,
            iconic: false,
            zoomed: false,
            cloaked: false,
            own: false,
            tool: false,
            caption: true,
            app_window: false,
            click_through: false,
            layered: false,
            class: "App".into(),
            process: "app.exe".into(),
        }
    }

    #[test]
    fn a_normal_window_is_eligible() {
        assert!(eligible(&win(1, 100, 200, 900, 800), &fit()));
    }

    #[test]
    fn borderless_fullscreen_games_are_never_perches() {
        // No caption either way: the bounds test is what must catch it.
        let mut game = win(1, 0, 0, 1920, 1080);
        assert!(!eligible(&game, &fit()));
        game.caption = false;
        game.app_window = true;
        assert!(!eligible(&game, &fit()));
        // On the second monitor, and slightly oversized.
        assert!(!eligible(&win(2, 1918, -2, 3842, 1082), &fit()));
    }

    #[test]
    fn maximized_cloaked_hidden_and_minimized_windows_are_out() {
        let mut w = win(1, 100, 200, 900, 800);
        w.zoomed = true;
        assert!(!eligible(&w, &fit()), "maximized");
        let mut w = win(1, 100, 200, 900, 800);
        w.cloaked = true;
        assert!(!eligible(&w, &fit()), "another virtual desktop");
        let mut w = win(1, 100, 200, 900, 800);
        w.visible = false;
        assert!(!eligible(&w, &fit()));
        let mut w = win(1, 100, 200, 900, 800);
        w.iconic = true;
        assert!(!eligible(&w, &fit()));
    }

    #[test]
    fn no_room_above_or_too_narrow() {
        assert!(!eligible(&win(1, 100, 95, 900, 800), &fit()), "touching the top");
        assert!(eligible(&win(1, 100, 96, 900, 800), &fit()));
        assert!(!eligible(&win(1, 100, 200, 259, 800), &fit()), "159 wide");
        assert!(eligible(&win(1, 100, 200, 260, 800), &fit()));
    }

    #[test]
    fn styles_and_the_deny_list() {
        let mut w = win(1, 100, 200, 900, 800);
        w.tool = true;
        assert!(!eligible(&w, &fit()), "tool window");
        let mut w = win(1, 100, 200, 900, 800);
        w.caption = false;
        assert!(!eligible(&w, &fit()), "no caption");
        w.app_window = true;
        assert!(eligible(&w, &fit()), "the app-window style is enough");
        let mut w = win(1, 100, 200, 900, 800);
        w.own = true;
        assert!(!eligible(&w, &fit()), "Clawd itself");
        let mut w = win(1, 100, 200, 900, 800);
        w.process = "Steam.EXE".into();
        assert!(!eligible(&w, &fit()), "case-insensitive deny-list");
        let mut w = win(1, 100, 200, 900, 800);
        w.class = "WorkerW".into();
        assert!(!eligible(&w, &fit()), "the desktop");
    }

    #[test]
    fn surface_keeps_clear_of_the_caption_buttons() {
        let f = surfaces(&[win(7, 100, 200, 900, 800)], &A, &fit(), None);
        assert_eq!(f, vec![Floor { x0: 112.0, x1: 750.0, y: 200.0, owner: 7 }]);
    }

    #[test]
    fn a_window_on_top_covers_part_of_the_title_bar() {
        let top = win(1, 400, 100, 600, 500);
        let back = win(2, 100, 200, 900, 800);
        let f = surfaces(&[top.clone(), back.clone()], &A, &fit(), None);
        // `top` has its own surface, the rest is what's left of `back`.
        assert_eq!(
            f,
            vec![
                Floor { x0: 412.0, x1: 450.0, y: 100.0, owner: 1 },
                Floor { x0: 112.0, x1: 400.0, y: 200.0, owner: 2 },
                Floor { x0: 600.0, x1: 750.0, y: 200.0, owner: 2 },
            ]
        );
        // The same window lower in the z-order hides nothing.
        let f = surfaces(&[back, top], &A, &fit(), None);
        assert!(f.contains(&Floor { x0: 112.0, x1: 750.0, y: 200.0, owner: 2 }));
    }

    #[test]
    fn overlays_do_not_hide_anything_but_real_windows_do() {
        let back = win(2, 100, 200, 900, 800);
        let mut overlay = win(1, 0, 0, 1920, 1080);
        overlay.click_through = true;
        overlay.layered = true;
        assert_eq!(surfaces(&[overlay.clone(), back.clone()], &A, &fit(), None).len(), 1);
        overlay.click_through = false;
        overlay.tool = true;
        assert_eq!(surfaces(&[overlay, back.clone()], &A, &fit(), None).len(), 1);
        // A maximized window in front hides the title bar completely.
        let mut max = win(1, 0, 0, 1920, 1040);
        max.zoomed = true;
        assert!(surfaces(&[max, back], &A, &fit(), None).is_empty());
    }

    #[test]
    fn surfaces_belong_to_their_monitor_and_exclude_works() {
        let left = win(1, 100, 200, 900, 800);
        let right = win(2, 2000, 300, 2800, 900);
        let all = [left, right];
        assert_eq!(surfaces(&all, &A, &fit(), None).iter().map(|f| f.owner).collect::<Vec<_>>(), [1]);
        assert_eq!(surfaces(&all, &B, &fit(), None).iter().map(|f| f.owner).collect::<Vec<_>>(), [2]);
        assert!(surfaces(&all, &A, &fit(), Some(1)).is_empty());
    }

    #[test]
    fn a_window_across_two_monitors_is_clipped_to_the_work_area() {
        let wide = win(1, 1500, 300, 2500, 900);
        let f = surfaces(&[wide], &A, &fit(), None);
        assert_eq!(f, vec![Floor { x0: 1512.0, x1: 1920.0, y: 300.0, owner: 1 }]);
    }

    #[test]
    fn insets_and_minimums_scale_with_dpi() {
        let mut fit = fit();
        fit.scale = 2.0;
        fit.clawd_h = 192;
        let w = win(1, 100, 200, 900, 800);
        assert_eq!(surfaces(&[w], &A, &fit, None), vec![Floor { x0: 124.0, x1: 600.0, y: 200.0, owner: 1 }]);
        assert!(!eligible(&win(1, 100, 200, 400, 800), &fit), "300 px is under 160 DIP at 200 %");
    }

    #[test]
    fn falls_to_the_next_surface_when_a_perch_disappears() {
        // Standing on window 1, with window 2 and the taskbar below.
        let windows = [win(1, 200, 300, 1000, 500), win(2, 100, 600, 1200, 1000)];
        let mut floors = vec![Floor { x0: 0.0, x1: 1920.0, y: 1040.0, owner: 0 }];
        floors.extend(surfaces(&windows, &A, &fit(), Some(1)));
        let w = World { floors, left: 0.0, right: 1920.0, top: 0.0, half_w: 32.0, height: 44.0, scale: 1.0 };
        let mut s = Sim::launch(500.0, 300.0, 0.0, 0.0);
        for _ in 0..600 {
            s.step(&w, PHYS_DT);
        }
        assert_eq!((s.y, s.state), (600.0, State::Rest));
        assert_eq!(w.floor_at(s.x, s.y).map(|f| f.owner), Some(2));
    }

    fn fg(l: i32, t: i32, r: i32, b: i32, f: impl FnOnce(&mut WindowInfo)) -> Foreground {
        let mut w = win(1, l, t, r, b);
        f(&mut w);
        classify_foreground(&w, &SCREENS)
    }

    #[test]
    fn fullscreen_detection() {
        assert_eq!(fg(0, 0, 1920, 1080, |w| w.caption = false), Foreground::Fullscreen, "borderless game");
        assert_eq!(fg(1920, 0, 3840, 1080, |w| w.caption = false), Foreground::Fullscreen, "on the other monitor");
        assert_eq!(fg(100, 100, 900, 800, |_| {}), Foreground::Normal);
        assert_eq!(fg(0, 0, 1920, 1040, |w| w.zoomed = true), Foreground::Normal, "maximized");
        // Auto-hide taskbar: a maximized app is as big as the monitor.
        assert_eq!(fg(0, 0, 1920, 1078, |w| w.zoomed = true), Foreground::Normal);
        // Browser F11: zoomed, but the caption is gone.
        assert_eq!(
            fg(0, 0, 1920, 1080, |w| {
                w.zoomed = true;
                w.caption = false;
            }),
            Foreground::Fullscreen
        );
    }

    #[test]
    fn the_shell_is_not_fullscreen_and_passing_ui_changes_nothing() {
        assert_eq!(fg(0, 0, 1920, 1080, |w| w.class = "WorkerW".into()), Foreground::Normal, "clicked the desktop");
        assert_eq!(fg(0, 0, 1920, 1080, |w| w.class = "XamlExplorerHostIslandWindow".into()), Foreground::Transient, "Alt-Tab");
        assert_eq!(fg(0, 0, 1920, 1080, |w| w.cloaked = true), Foreground::Transient);
        assert_eq!(fg(0, 0, 1920, 1080, |w| w.own = true), Foreground::Transient);
    }

    #[test]
    fn riding_keeps_the_distance_from_the_left_edge() {
        let b = Rect { l: 100, t: 200, r: 900, b: 800 };
        let r = Ride::new(1, 300.0, b);
        assert_eq!(r.feet(Rect { l: 400, t: 500, r: 1200, b: 1100 }, 1.0), (600.0, 500.0));
        // Narrowed: pushed left, clear of the caption buttons.
        assert_eq!(r.feet(Rect { l: 100, t: 200, r: 400, b: 800 }, 1.0), (250.0, 200.0));
    }

    #[test]
    fn ordinary_moves_ride_along_and_a_hard_shake_knocks_off() {
        let b = Rect { l: 100, t: 200, r: 900, b: 800 };
        let mut r = Ride::new(1, 300.0, b);
        assert_eq!(r.moved(0, 100, 200, 1.0), RideEvent::Still, "nothing changed");
        // A long drag in one direction, even a fast one.
        for i in 1..50 {
            assert_eq!(r.moved(i * 10, 100 + i as i32 * 30, 200, 1.0), RideEvent::Moved);
        }
        // Small jitters don't count either.
        let mut r = Ride::new(1, 300.0, b);
        for i in 0..40 {
            assert_eq!(r.moved(i * 30, 100 + (i % 2) as i32 * 20, 201, 1.0), RideEvent::Moved);
        }
        // Back and forth, far and fast.
        let mut r = Ride::new(1, 300.0, b);
        let mut last = RideEvent::Still;
        for i in 0..8 {
            last = r.moved(i * 60, 100 + (i % 2) as i32 * 200, 200, 1.0);
            if last == RideEvent::KnockedOff {
                break;
            }
        }
        assert_eq!(last, RideEvent::KnockedOff);
    }

    #[test]
    fn a_slow_shake_does_not_knock_off() {
        let b = Rect { l: 100, t: 200, r: 900, b: 800 };
        let mut r = Ride::new(1, 300.0, b);
        for i in 1..12 {
            assert_eq!(r.moved(i * 700, 100 + (i % 2) as i32 * 200, 200, 1.0), RideEvent::Moved);
        }
    }

    #[test]
    fn ride_velocity_stops_when_the_window_does() {
        let mut r = Ride::new(1, 300.0, Rect { l: 100, t: 200, r: 900, b: 800 });
        r.moved(0, 100, 200, 1.0);
        r.moved(0, 110, 200, 1.0);
        r.moved(50, 160, 200, 1.0);
        assert!((r.velocity(50, 1.0).0 - 1000.0).abs() < 1.0);
        assert_eq!(r.velocity(500, 1.0), (0.0, 0.0));
    }

    #[test]
    fn hop_targets_the_nearest_spot_on_the_widest_stretch() {
        let floors = [
            Floor { x0: 100.0, x1: 200.0, y: 300.0, owner: 1 },
            Floor { x0: 400.0, x1: 900.0, y: 300.0, owner: 1 },
            Floor { x0: 0.0, x1: 2000.0, y: 1040.0, owner: 0 },
        ];
        assert_eq!(hop_target(&floors, 1, 0.0, 32.0), Some((432.0, 300.0)));
        assert_eq!(hop_target(&floors, 1, 600.0, 32.0), Some((600.0, 300.0)));
        assert_eq!(hop_target(&floors, 1, 2000.0, 32.0), Some((868.0, 300.0)));
        assert_eq!(hop_target(&floors, 9, 0.0, 32.0), None);
    }

    #[test]
    fn a_hop_lands_exactly_on_the_target_and_stays() {
        let floors = vec![
            Floor { x0: 400.0, x1: 900.0, y: 300.0, owner: 1 },
            Floor { x0: 0.0, x1: 1920.0, y: 1040.0, owner: 0 },
        ];
        let w = World { floors, left: 0.0, right: 1920.0, top: 0.0, half_w: 32.0, height: 44.0, scale: 1.0 };
        for (from, to) in [((1500.0, 1040.0), (600.0, 300.0)), ((100.0, 150.0), (850.0, 300.0))] {
            let (vx, vy) = hop_velocity(from, to, w.top + w.height, 1.0);
            let mut s = Sim::hop(from.0, from.1, vx, vy);
            let mut events = vec![];
            for _ in 0..1200 {
                events.extend(s.step(&w, PHYS_DT));
            }
            assert_eq!(s.state, State::Rest);
            assert_eq!(s.y, to.1);
            assert!((s.x - to.0).abs() < 0.5, "landed at {} instead of {}", s.x, to.0);
            assert!(!events.iter().any(|e| matches!(e, Event::Bounce(_))));
        }
    }

    #[test]
    fn a_hop_never_peaks_above_the_ceiling() {
        let (_, vy) = hop_velocity((100.0, 110.0), (800.0, 100.0), 90.0, 1.0);
        let rise = vy * vy / (2.0 * GRAVITY_DIP_S2);
        assert!(110.0 - rise >= 89.9, "peaks at {}", 110.0 - rise);
    }
}
