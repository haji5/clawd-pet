//! The pointer state machine (PLAN.md §6b): press, hold, drag, shake, throw
//! velocity and click counting. Pure: positions are physical pixels, `now`
//! is ms, and `scale` = DPI/96 turns the DIP tunables into pixels.

use crate::feel::*;
use crate::tracker::Ms;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Pet,
    Giggle,
    Dizzy,
}

/// What a mouse move or a timer tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    None,
    /// Held without moving for `HOLD_START_MS`.
    HoldStart,
    /// Moved past the threshold: the press is now a drag.
    DragStart,
    /// Shaken while dragging (once per drag).
    Shake,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Release {
    Click(Click),
    /// Released after squishing.
    HoldEnd,
    /// Released after dragging, with the cursor velocity in px/s.
    Drop { vx: f64, vy: f64, shaken: bool },
}

struct Press {
    t0: Ms,
    x0: f64,
    y0: f64,
    dragging: bool,
    holding: bool,
    shaken: bool,
}

/// Counts direction reversals along one axis (the cursor shaking Clawd, or
/// a window shaking Clawd off).
#[derive(Default)]
pub struct Shake {
    /// Direction of the current stroke (-1, 0, 1) and its extreme.
    dir: i8,
    extreme: f64,
    reversals: VecDeque<Ms>,
}

impl Shake {
    pub fn at(x: f64) -> Self {
        Shake { extreme: x, ..Default::default() }
    }

    /// Feed a position; returns the reversals of at least `min` px within
    /// the last `window` ms.
    pub fn track(&mut self, now: Ms, x: f64, min: f64, window: Ms) -> usize {
        let d = x - self.extreme;
        let dir = if d > 0.0 { 1 } else if d < 0.0 { -1 } else { 0 };
        if self.dir == 0 {
            if d.abs() >= min {
                self.dir = dir;
                self.extreme = x;
            }
        } else if dir == self.dir {
            self.extreme = x; // still going the same way: push the extreme out
        } else if d.abs() >= min {
            self.dir = dir;
            self.extreme = x;
            self.reversals.push_back(now);
        }
        while self.reversals.front().is_some_and(|&t| now.saturating_sub(t) > window) {
            self.reversals.pop_front();
        }
        self.reversals.len()
    }
}

/// The last few positions of something moving, for its velocity.
#[derive(Default)]
pub struct Trail(VecDeque<(Ms, f64, f64)>);

impl Trail {
    pub fn push(&mut self, now: Ms, x: f64, y: f64) {
        self.0.push_back((now, x, y));
        while self.0.len() > 8 {
            self.0.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Velocity (px/s) over the last `THROW_SAMPLE_MS`, clamped to `max`;
    /// zero if the last sample is older than that.
    pub fn velocity(&self, now: Ms, max: f64) -> (f64, f64) {
        let recent: Vec<_> = self.0.iter().filter(|s| now.saturating_sub(s.0) <= THROW_SAMPLE_MS).collect();
        let (Some(a), Some(b)) = (recent.first(), self.0.back()) else { return (0.0, 0.0) };
        // No movement for a while: it's resting.
        if now.saturating_sub(b.0) > THROW_SAMPLE_MS {
            return (0.0, 0.0);
        }
        let dt = (b.0.saturating_sub(a.0)).max(8) as f64 / 1000.0;
        let (mut vx, mut vy) = ((b.1 - a.1) / dt, (b.2 - a.2) / dt);
        let speed = vx.hypot(vy);
        if speed > max {
            vx *= max / speed;
            vy *= max / speed;
        }
        (vx, vy)
    }
}

pub struct Hand {
    pub scale: f64,
    press: Option<Press>,
    clicks: VecDeque<Ms>,
    trail: Trail,
    shake: Shake,
}

impl Hand {
    pub fn new(scale: f64) -> Self {
        Hand { scale, press: None, clicks: VecDeque::new(), trail: Trail::default(), shake: Shake::default() }
    }

    pub fn pressed(&self) -> bool {
        self.press.is_some()
    }
    pub fn dragging(&self) -> bool {
        self.press.as_ref().is_some_and(|p| p.dragging)
    }
    /// This drag has been shaken.
    pub fn shaken(&self) -> bool {
        self.press.as_ref().is_some_and(|p| p.shaken)
    }

    /// Ms until a press still waiting to become a hold gets there (0 = due
    /// now). `None` when it can't become one any more. USER timers fire up
    /// to a clock tick (~16 ms) early, so the window waits out the rest.
    pub fn hold_due_in(&self, now: Ms) -> Option<Ms> {
        match &self.press {
            Some(p) if !p.dragging && !p.holding => Some((p.t0 + HOLD_START_MS).saturating_sub(now)),
            _ => None,
        }
    }

    pub fn down(&mut self, now: Ms, x: f64, y: f64) {
        self.press = Some(Press { t0: now, x0: x, y0: y, dragging: false, holding: false, shaken: false });
        self.trail.clear();
        self.shake = Shake::default();
        self.trail.push(now, x, y);
    }

    pub fn moved(&mut self, now: Ms, x: f64, y: f64) -> Event {
        let threshold = DRAG_THRESHOLD_DIP * self.scale;
        let Some(p) = self.press.as_mut() else { return Event::None };
        if !p.dragging {
            if (x - p.x0).abs().max((y - p.y0).abs()) <= threshold {
                return Event::None;
            }
            p.dragging = true;
            p.holding = false;
            self.trail.push(now, x, y);
            self.shake = Shake::at(x);
            return Event::DragStart;
        }
        self.trail.push(now, x, y);
        let reversals = self.shake.track(now, x, SHAKE_MIN_DIP * self.scale, SHAKE_WINDOW_MS);
        let p = self.press.as_mut().unwrap();
        if !p.shaken && reversals >= SHAKE_REVERSALS {
            p.shaken = true;
            return Event::Shake;
        }
        Event::None
    }

    /// Call from a timer `HOLD_START_MS` after `down`.
    pub fn tick(&mut self, now: Ms) -> Event {
        match self.press.as_mut() {
            Some(p) if !p.dragging && !p.holding && now >= p.t0 + HOLD_START_MS => {
                p.holding = true;
                Event::HoldStart
            }
            _ => Event::None,
        }
    }

    /// Cursor velocity (px/s) over the last `THROW_SAMPLE_MS`, clamped.
    pub fn velocity(&self, now: Ms) -> (f64, f64) {
        self.trail.velocity(now, THROW_MAX_DIP_S * self.scale)
    }

    pub fn up(&mut self, now: Ms, x: f64, y: f64) -> Option<Release> {
        let p = self.press.take()?;
        if p.dragging {
            self.trail.push(now, x, y);
            let (vx, vy) = self.velocity(now);
            return Some(Release::Drop { vx, vy, shaken: p.shaken });
        }
        if p.holding {
            return Some(Release::HoldEnd);
        }
        self.clicks.push_back(now);
        while self.clicks.front().is_some_and(|&t| now.saturating_sub(t) > RAPID_WINDOW_MS) {
            self.clicks.pop_front();
        }
        let n = self.clicks.len();
        Some(Release::Click(if n >= DIZZY_CLICKS {
            self.clicks.clear();
            Click::Dizzy
        } else if n >= GIGGLE_CLICKS {
            Click::Giggle
        } else {
            Click::Pet
        }))
    }
}

/// The reaction to a click. Rapid clicking beats the double-click trick:
/// in a fast burst every second click arrives as a double-click.
pub fn click_anim(c: Click, double: bool) -> &'static str {
    match c {
        Click::Dizzy => "dizzy",
        Click::Giggle => "giggle",
        Click::Pet if double => "trick",
        Click::Pet => "petted",
    }
}

/// Smoothed horizontal speed for the dragging swing, with follow-through:
/// it eases toward the cursor speed and keeps swinging briefly after a stop.
#[derive(Default)]
pub struct Swing {
    pub v: f64,
}

impl Swing {
    pub fn update(&mut self, target_vx: f64, dt_ms: f64) {
        let k = 1.0 - (-dt_ms / LEAN_TAU_MS).exp();
        self.v += (target_vx - self.v) * k;
    }

    /// Lean level -2..=2: the body trails the motion, so moving right swings
    /// the legs left (negative).
    pub fn level(&self, scale: f64) -> i8 {
        let s = self.v.abs() / scale;
        let lvl = if s >= LEAN2_DIP_S { 2 } else if s >= LEAN1_DIP_S { 1 } else { 0 };
        if self.v > 0.0 { -lvl } else { lvl }
    }
}

/// Eye offset (±2 px each way) looking from the eyes toward a cursor at
/// (dx, dy) px away; `None` when it's farther than `radius` px.
pub fn gaze_toward(dx: f64, dy: f64, radius: f64) -> Option<(i32, i32)> {
    let d = dx.hypot(dy);
    if d > radius {
        return None;
    }
    if d < 1.0 {
        return Some((0, 0));
    }
    Some(((dx / d * 2.0).round() as i32, (dy / d * 2.0).round() as i32))
}

/// How long the eyes can go without looking: a cursor `distance` DIP away
/// can't get within their range any sooner, even at full speed.
pub fn cursor_poll_ms(distance: f64) -> u32 {
    let ms = (distance - EYES_RADIUS_DIP) / CURSOR_MAX_DIP_PER_S * 1000.0;
    (ms as u32).clamp(CURSOR_POLL_MS, CURSOR_POLL_SLOW_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_slows_down_only_when_the_cursor_cannot_arrive_sooner() {
        assert_eq!(cursor_poll_ms(0.0), CURSOR_POLL_MS);
        assert_eq!(cursor_poll_ms(EYES_RADIUS_DIP + 100.0), CURSOR_POLL_MS, "just outside the range");
        assert_eq!(cursor_poll_ms(EYES_RADIUS_DIP + 600.0), 100);
        assert_eq!(cursor_poll_ms(10_000.0), CURSOR_POLL_SLOW_MS);
    }

    #[test]
    fn gaze_points_at_the_cursor_within_range() {
        assert_eq!(gaze_toward(100.0, 0.0, 250.0), Some((2, 0)));
        assert_eq!(gaze_toward(-70.0, -70.0, 250.0), Some((-1, -1)));
        assert_eq!(gaze_toward(0.0, 120.0, 250.0), Some((0, 2)));
        assert_eq!(gaze_toward(300.0, 0.0, 250.0), None);
    }

    #[test]
    fn click_hold_and_drag_thresholds() {
        let mut h = Hand::new(1.5);
        h.down(0, 100.0, 100.0);
        assert_eq!(h.moved(10, 105.0, 100.0), Event::None, "4 DIP at 150 % = 6 px");
        assert_eq!(h.up(50, 105.0, 100.0), Some(Release::Click(Click::Pet)));

        h.down(1000, 100.0, 100.0);
        assert_eq!(h.tick(1000 + HOLD_START_MS - 1), Event::None);
        assert_eq!(h.tick(1000 + HOLD_START_MS), Event::HoldStart);
        assert_eq!(h.up(1600, 100.0, 100.0), Some(Release::HoldEnd));

        h.down(5000, 100.0, 100.0);
        assert_eq!(h.moved(5010, 107.0, 100.0), Event::DragStart);
        assert!(h.dragging());
        assert_eq!(h.tick(5000 + HOLD_START_MS), Event::None, "a drag never turns into a hold");
    }

    #[test]
    fn rapid_clicks_giggle_then_dizzy() {
        let mut h = Hand::new(1.0);
        let mut kinds = vec![];
        for i in 0..12 {
            h.down(i * 150, 0.0, 0.0);
            kinds.push(h.up(i * 150 + 50, 0.0, 0.0).unwrap());
        }
        assert_eq!(kinds[3], Release::Click(Click::Pet));
        assert_eq!(kinds[4], Release::Click(Click::Giggle));
        assert_eq!(kinds[11], Release::Click(Click::Dizzy));
        // The count starts over afterwards, and old clicks fall out of the window.
        h.down(10_000, 0.0, 0.0);
        assert_eq!(h.up(10_050, 0.0, 0.0), Some(Release::Click(Click::Pet)));
    }

    #[test]
    fn a_fast_burst_giggles_and_ends_dizzy_despite_double_clicks() {
        let mut h = Hand::new(1.0);
        let mut names = vec![];
        for i in 0..12 {
            h.down(i * 150, 0.0, 0.0);
            let Some(Release::Click(c)) = h.up(i * 150 + 50, 0.0, 0.0) else { panic!() };
            // Windows reports every second click of a burst as a double-click.
            names.push(click_anim(c, i % 2 == 1));
        }
        assert_eq!(&names[..4], ["petted", "trick", "petted", "trick"]);
        assert!(names[4..11].iter().all(|n| *n == "giggle"), "{names:?}");
        assert_eq!(names[11], "dizzy");
    }

    #[test]
    fn an_early_timer_waits_out_the_rest_of_the_hold() {
        let mut h = Hand::new(1.0);
        h.down(1000, 0.0, 0.0);
        // The 250 ms timer fired at 234 ms (measured on a real machine).
        assert_eq!(h.tick(1234), Event::None);
        assert_eq!(h.hold_due_in(1234), Some(16));
        assert_eq!(h.tick(1250), Event::HoldStart);
        assert_eq!(h.hold_due_in(1250), None, "already holding");
        h.up(1400, 0.0, 0.0);
        h.down(2000, 0.0, 0.0);
        h.moved(2010, 50.0, 0.0);
        assert_eq!(h.hold_due_in(2234), None, "a drag never becomes a hold");
    }

    #[test]
    fn throw_velocity_from_recent_samples() {
        let mut h = Hand::new(1.0);
        h.down(0, 0.0, 0.0);
        h.moved(10, 10.0, 0.0);
        for i in 1..=8 {
            h.moved(10 + i * 10, 10.0 + i as f64 * 20.0, -(i as f64) * 5.0); // 2000 px/s right, 500 up
        }
        match h.up(90, 170.0, -40.0).unwrap() {
            Release::Drop { vx, vy, shaken } => {
                assert!((vx - 2000.0).abs() < 1.0 && (vy + 500.0).abs() < 1.0, "{vx} {vy}");
                assert!(!shaken);
            }
            r => panic!("{r:?}"),
        }
        // Holding still before releasing: no throw.
        h.down(1000, 0.0, 0.0);
        h.moved(1010, 50.0, 0.0);
        h.moved(1020, 100.0, 0.0);
        let Some(Release::Drop { vx, .. }) = h.up(1500, 100.0, 0.0) else { panic!() };
        assert_eq!(vx, 0.0);
        // Clamped to the maximum.
        h.down(2000, 0.0, 0.0);
        h.moved(2001, 10.0, 0.0);
        h.moved(2009, 500.0, 0.0);
        assert!(h.velocity(2009).0 <= THROW_MAX_DIP_S + 1e-6);
    }

    #[test]
    fn shaking_is_detected_once() {
        let mut h = Hand::new(1.0);
        h.down(0, 0.0, 0.0);
        assert_eq!(h.moved(10, 10.0, 0.0), Event::DragStart);
        let xs = [40.0, 0.0, 40.0, 0.0, 40.0, 0.0, 40.0];
        let mut events = vec![];
        for (i, &x) in xs.iter().enumerate() {
            events.push(h.moved(20 + i as Ms * 60, x, 0.0));
        }
        assert_eq!(events.iter().filter(|&&e| e == Event::Shake).count(), 1, "{events:?}");
        let Some(Release::Drop { shaken, .. }) = h.up(500, 0.0, 0.0) else { panic!() };
        assert!(shaken);
        // Slow back-and-forth (outside the window) is not a shake.
        h.down(1000, 0.0, 0.0);
        h.moved(1010, 10.0, 0.0);
        let mut any = false;
        for (i, &x) in xs.iter().enumerate() {
            any |= h.moved(1020 + i as Ms * 400, x, 0.0) == Event::Shake;
        }
        assert!(!any);
    }

    #[test]
    fn swing_trails_and_follows_through() {
        let mut s = Swing::default();
        for _ in 0..20 {
            s.update(1500.0, 33.0);
        }
        assert_eq!(s.level(1.0), -2, "moving right swings the legs left");
        s.update(0.0, 33.0);
        assert!(s.level(1.0) < 0, "keeps swinging right after the cursor stops");
        for _ in 0..30 {
            s.update(0.0, 33.0);
        }
        assert_eq!(s.level(1.0), 0);
    }
}
