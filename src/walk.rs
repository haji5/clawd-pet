//! Strolling along the ground while idle (PLAN.md §8 item 3): where to go
//! and where Clawd is at a given moment. Pure; the window does the moving.

use crate::feel;
use crate::rng::Rng;
use crate::tracker::Ms;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroll {
    pub from: f64,
    pub to: f64,
    start: Ms,
    end: Ms,
}

impl Stroll {
    /// A random stroll from feet position `x`, staying within `left..=right`
    /// (physical pixels). Picks a side at random and turns around if the
    /// edge of the screen is too close that way. `None` if there's no room.
    pub fn plan(x: f64, left: f64, right: f64, scale: f64, now: Ms, rng: &mut Rng) -> Option<Stroll> {
        let (min, max) = feel::WALK_DISTANCE_DIP;
        let wanted = rng.range(min, max) as f64 * scale;
        let (room_left, room_right) = ((x - left).max(0.0), (right - x).max(0.0));
        let (first, other) = if rng.range(0, 2) == 0 { (-room_left, room_right) } else { (room_right, -room_left) };
        let signed_room = if first.abs() >= wanted || first.abs() >= other.abs() { first } else { other };
        let distance = wanted.min(signed_room.abs());
        if distance < min as f64 * scale {
            return None;
        }
        let ms = (distance / (feel::WALK_SPEED_DIP_S * scale) * 1000.0) as Ms;
        Some(Stroll { from: x, to: x + distance * signed_room.signum(), start: now, end: now + ms.max(1) })
    }

    /// Feet position at `now`: a steady pace that never overshoots.
    pub fn x_at(&self, now: Ms) -> f64 {
        let t = now.saturating_sub(self.start) as f64 / (self.end - self.start) as f64;
        self.from + (self.to - self.from) * t.min(1.0)
    }

    pub fn done(&self, now: Ms) -> bool {
        now >= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_on_screen_and_within_the_distance_limits() {
        let mut rng = Rng::new(7);
        let (min, max) = feel::WALK_DISTANCE_DIP;
        for start in [100.0, 300.0, 960.0, 1700.0, 1880.0] {
            for _ in 0..200 {
                let s = Stroll::plan(start, 40.0, 1880.0, 1.5, 0, &mut rng).unwrap();
                assert!((40.0..=1880.0).contains(&s.to), "{s:?}");
                let d = (s.to - s.from).abs();
                assert!(d >= min as f64 * 1.5 - 1e-9 && d <= max as f64 * 1.5, "{s:?}");
            }
        }
    }

    #[test]
    fn turns_around_at_a_screen_edge() {
        let mut rng = Rng::new(11);
        for _ in 0..200 {
            let at_left = Stroll::plan(40.0, 40.0, 1880.0, 1.0, 0, &mut rng).unwrap();
            assert!(at_left.to > at_left.from);
            let at_right = Stroll::plan(1880.0, 40.0, 1880.0, 1.0, 0, &mut rng).unwrap();
            assert!(at_right.to < at_right.from);
        }
    }

    #[test]
    fn no_room_means_no_stroll() {
        let mut rng = Rng::new(3);
        let (min, _) = feel::WALK_DISTANCE_DIP;
        let narrow = min as f64 / 2.0 - 1.0;
        assert_eq!(Stroll::plan(100.0, 100.0 - narrow, 100.0 + narrow, 1.0, 0, &mut rng), None);
    }

    #[test]
    fn walks_at_a_steady_pace_and_stops_exactly_at_the_target() {
        let mut rng = Rng::new(5);
        let s = Stroll::plan(500.0, 0.0, 1000.0, 1.0, 1000, &mut rng).unwrap();
        assert_eq!(s.x_at(1000), 500.0);
        assert!(!s.done(1000));
        let mid = s.x_at((s.start + s.end) / 2);
        assert!((mid - (s.from + s.to) / 2.0).abs() < 1.0, "{mid} in {s:?}");
        assert!(s.done(s.end));
        assert_eq!(s.x_at(s.end + 5000), s.to, "no overshoot");
    }
}
