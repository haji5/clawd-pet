//! Throwing and falling (PLAN.md §6b): gravity, air drag, bounces off walls
//! and the ceiling, one-way floors (the taskbar edge; window title bars in
//! Phase 3c), sliding to a stop. Pure, fixed timestep. Units: physical
//! pixels and seconds; `scale` = DPI/96 turns the DIP tunables into pixels.
//! The body is tracked by its feet point (bottom centre).

use crate::feel::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Floor {
    pub x0: f64,
    pub x1: f64,
    pub y: f64,
    /// The window this is the top edge of; 0 = the ground (taskbar edge).
    pub owner: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct World {
    pub floors: Vec<Floor>,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    /// Body half-width and height (px) for wall and ceiling contact.
    pub half_w: f64,
    pub height: f64,
    pub scale: f64,
}

impl World {
    /// The surface feet at (x, y) are standing on, topmost-listed first.
    pub fn floor_at(&self, x: f64, y: f64) -> Option<&Floor> {
        self.floors.iter().find(|f| x >= f.x0 && x <= f.x1 && (f.y - y).abs() <= 0.5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Air,
    /// Feet on the floor mid-bounce, squashed, about to spring back up.
    Contact,
    Slide,
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Wall,
    Ceiling,
    /// Hit a floor at this downward speed (px/s) and will bounce after a
    /// moment of contact.
    Bounce(f64),
    /// Left the floor again after a bounce.
    Rebound,
    /// Came down to stay, with this impact speed (px/s).
    Landed(f64),
    /// Stopped sliding.
    Rest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sim {
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub state: State,
    /// An aimed hop: no air drag, and it stops dead where it lands.
    pub hop: bool,
    /// While in `Contact`: seconds left and in total, the speed it will
    /// leave the floor with (negative = up) and the impact that started it.
    contact_left: f64,
    contact_total: f64,
    rebound: f64,
    impact: f64,
}

/// How hard a floor hit was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact {
    Light,
    Medium,
    Hard,
    Slam,
}

impl Impact {
    pub fn of(speed: f64, scale: f64) -> Impact {
        let v = speed / scale;
        if v >= SLAM_IMPACT_DIP_S {
            Impact::Slam
        } else if v >= HARD_LANDING_DIP_S {
            Impact::Hard
        } else if v >= MEDIUM_IMPACT_DIP_S {
            Impact::Medium
        } else {
            Impact::Light
        }
    }
}

/// How long a bounce at `speed` (DIP/s) stays on the floor, in seconds.
fn contact_secs(speed: f64) -> f64 {
    let least = BOUNCE_MIN_DIP_S / RESTITUTION_FLOOR;
    let k = ((speed - least) / (CONTACT_FULL_DIP_S - least)).clamp(0.0, 1.0);
    (CONTACT_MIN_MS + (CONTACT_MAX_MS - CONTACT_MIN_MS) * k) / 1000.0
}

/// The pose for this instant of a flight or bounce (`State::Air` and
/// `State::Contact`), named after its animation. Chosen every physics tick,
/// so the body always reads the motion: long in the direction of travel when
/// fast, tucked at the top of an arc, flattened according to the hit.
pub fn body_pose(sim: &Sim, scale: f64) -> &'static str {
    if sim.state == State::Contact {
        let k = 1.0 - sim.contact_left / sim.contact_total.max(1e-6);
        return match Impact::of(sim.impact, scale) {
            Impact::Slam if k < 0.55 => "hit_slam",
            Impact::Slam if k < 0.8 => "hit_squash",
            Impact::Hard if k < 0.5 => "hit_splat",
            Impact::Hard if k < 0.78 => "hit_squash",
            Impact::Medium if k < 0.7 => "hit_squash",
            Impact::Light if k < 0.6 => "hit_light",
            _ => "hit_recoil",
        };
    }
    let v = sim.vy / scale;
    if v <= -RISE_FAST_DIP_S {
        "rise_2"
    } else if v <= -RISE_SLOW_DIP_S {
        "rise_1"
    } else if v >= FALL_PANIC_DIP_S {
        "fall_3"
    } else if v >= FALL_FAST_DIP_S {
        "fall_2"
    } else if v >= FALL_SLOW_DIP_S {
        "fall_1"
    } else {
        "air"
    }
}

/// The reaction once Clawd has come to stay: the last touch sets how big the
/// squash is, the hardest hit of the whole flight how shaken it is.
pub fn landing_anim(last_impact: f64, peak_impact: f64, shaken: bool, scale: f64) -> &'static str {
    if shaken {
        return "dazed";
    }
    match Impact::of(peak_impact, scale) {
        Impact::Slam => "hard_land",
        Impact::Hard => "rattled",
        _ if last_impact / scale >= SOFT_LANDING_DIP_S => "land",
        _ => "soft_land",
    }
}

/// Pitching forward while skidding along a floor, if fast enough.
pub fn skid_pose(vx: f64, scale: f64) -> Option<&'static str> {
    let v = vx.abs() / scale;
    match (v >= SKID_FAST_DIP_S, v >= SKID_SLOW_DIP_S, vx > 0.0) {
        (true, _, true) => Some("ride_r2"),
        (true, _, false) => Some("ride_l2"),
        (_, true, true) => Some("ride_r1"),
        (_, true, false) => Some("ride_l1"),
        _ => None,
    }
}

impl Sim {
    pub fn launch(x: f64, y: f64, vx: f64, vy: f64) -> Self {
        Sim { x, y, vx, vy, state: State::Air, hop: false, contact_left: 0.0, contact_total: 0.0, rebound: 0.0, impact: 0.0 }
    }

    pub fn hop(x: f64, y: f64, vx: f64, vy: f64) -> Self {
        Sim { hop: true, ..Sim::launch(x, y, vx, vy) }
    }

    pub fn speed(&self) -> f64 {
        self.vx.hypot(self.vy)
    }

    fn floor_below(&self, w: &World) -> Option<f64> {
        w.floors.iter().filter(|f| self.x >= f.x0 && self.x <= f.x1 && f.y >= self.y - 0.5).map(|f| f.y).reduce(f64::min)
    }

    pub fn step(&mut self, w: &World, dt: f64) -> Vec<Event> {
        let mut ev = Vec::new();
        let s = w.scale;
        match self.state {
            State::Rest => return ev,
            State::Contact => {
                self.contact_left -= dt;
                self.x += self.vx * dt; // still skidding while squashed
                self.walls(w, &mut ev);
                let off_the_end = self.floor_below(w).is_none_or(|y| y > self.y + 0.5);
                if self.contact_left <= 0.0 || off_the_end {
                    self.vy = self.rebound;
                    self.state = State::Air;
                    ev.push(Event::Rebound);
                }
                return ev;
            }
            State::Slide => {
                let decel = SLIDE_FRICTION_DIP_S2 * s * dt;
                if self.vx.abs() <= decel {
                    self.vx = 0.0;
                    self.state = State::Rest;
                    ev.push(Event::Rest);
                } else {
                    self.vx -= decel * self.vx.signum();
                }
                self.x += self.vx * dt;
                self.walls(w, &mut ev);
                // Slid off the end of a platform (a title bar, Phase 3c).
                if self.floor_below(w).is_none_or(|y| y > self.y + 0.5) {
                    self.state = State::Air;
                }
                return ev;
            }
            State::Air => {}
        }
        let drag = if self.hop { 1.0 } else { (1.0 - AIR_DRAG * dt).max(0.0) };
        self.vx *= drag;
        self.vy = self.vy * drag + GRAVITY_DIP_S2 * s * dt;
        let prev_y = self.y;
        self.x += self.vx * dt;
        self.y += self.vy * dt;
        self.walls(w, &mut ev);
        if self.y - w.height < w.top && self.vy < 0.0 {
            self.y = w.top + w.height;
            self.vy = -self.vy * RESTITUTION_CEILING;
            ev.push(Event::Ceiling);
        }
        if self.vy > 0.0 {
            // One-way floors: only while falling, only when the feet cross the edge.
            let hit = w
                .floors
                .iter()
                .filter(|f| self.x >= f.x0 && self.x <= f.x1 && prev_y <= f.y + 0.01 && self.y >= f.y)
                .map(|f| f.y)
                .reduce(f64::min);
            if let Some(fy) = hit {
                self.y = fy;
                let impact = self.vy;
                if !self.hop && impact * RESTITUTION_FLOOR > BOUNCE_MIN_DIP_S * s {
                    // Not an instant flip: the body needs time on the floor
                    // for the squash to be seen.
                    self.rebound = -impact * RESTITUTION_FLOOR;
                    self.impact = impact;
                    self.contact_total = contact_secs(impact / s);
                    self.contact_left = self.contact_total;
                    self.vy = 0.0;
                    self.vx *= FLOOR_GRIP;
                    self.state = State::Contact;
                    ev.push(Event::Bounce(impact));
                } else {
                    self.vy = 0.0;
                    self.vx = if self.hop { 0.0 } else { self.vx * FLOOR_GRIP };
                    self.state = State::Slide;
                    ev.push(Event::Landed(impact));
                }
            }
        }
        // Safety net: never fall out of the world.
        if let Some(lowest) = w.floors.iter().map(|f| f.y).reduce(f64::max)
            && self.y > lowest
        {
            self.y = lowest;
            self.vy = 0.0;
            self.state = State::Slide;
            ev.push(Event::Landed(0.0));
        }
        ev
    }

    fn walls(&mut self, w: &World, ev: &mut Vec<Event>) {
        if self.x - w.half_w < w.left {
            self.x = w.left + w.half_w;
            if self.vx < 0.0 {
                self.vx = -self.vx * RESTITUTION_WALL;
                ev.push(Event::Wall);
            }
        } else if self.x + w.half_w > w.right {
            self.x = w.right - w.half_w;
            if self.vx > 0.0 {
                self.vx = -self.vx * RESTITUTION_WALL;
                ev.push(Event::Wall);
            }
        }
    }
}

/// Which screen edge (if any) a body dropped at `x` should hang on:
/// `Some(true)` = left edge, `Some(false)` = right edge.
pub fn edge_hang(x: f64, w: &World) -> Option<bool> {
    let near = EDGE_HANG_DIP * w.scale;
    if x - w.half_w <= w.left + near {
        Some(true)
    } else if x + w.half_w >= w.right - near {
        Some(false)
    } else {
        None
    }
}

/// Runs a `Sim` in fixed steps from uneven real time, and says where to
/// draw it: between the last two steps, by the time left over. The timer
/// fires about every 15.6 ms and a step is 8.3, so a tick runs one step or
/// two; drawn where the last step left it, Clawd would move half as far
/// every eighth frame.
pub struct Stepper {
    acc: f64,
    prev: (f64, f64),
}

impl Stepper {
    pub fn new(sim: &Sim) -> Self {
        Stepper { acc: 0.0, prev: (sim.x, sim.y) }
    }

    /// Advance by `dt` seconds of real time. Returns what happened and the
    /// feet point to draw: exactly where the simulation is on a call with
    /// an event (no frame with the feet a pixel into a title bar) and at
    /// rest, else up to one step behind it, the same on every frame. The
    /// simulation itself steps exactly as it would by hand.
    pub fn advance(&mut self, sim: &mut Sim, world: &World, dt: f64) -> (Vec<Event>, (f64, f64)) {
        self.acc += dt;
        let mut events = Vec::new();
        while self.acc >= PHYS_DT {
            self.prev = (sim.x, sim.y);
            events.extend(sim.step(world, PHYS_DT));
            self.acc -= PHYS_DT;
        }
        if !events.is_empty() || sim.state == State::Rest {
            return (events, (sim.x, sim.y));
        }
        let t = self.acc / PHYS_DT;
        let (px, py) = self.prev;
        (events, (px + (sim.x - px) * t, py + (sim.y - py) * t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        World {
            floors: vec![Floor { x0: 0.0, x1: 1000.0, y: 800.0, owner: 0 }],
            left: 0.0,
            right: 1000.0,
            top: 0.0,
            half_w: 40.0,
            height: 60.0,
            scale: 1.0,
        }
    }

    fn run(s: &mut Sim, w: &World, secs: f64) -> Vec<Event> {
        let mut all = vec![];
        let n = (secs / PHYS_DT) as usize;
        for _ in 0..n {
            all.extend(s.step(w, PHYS_DT));
            if s.state == State::Rest {
                break;
            }
        }
        all
    }

    /// What the window's timer gives: about 15.6 ms, sometimes 31.
    const TICK: f64 = 0.015_6;

    #[test]
    fn a_flight_is_drawn_moving_evenly_between_steps() {
        let w = world();
        let mut s = Sim::hop(200.0, 100.0, 300.0, -500.0);
        let mut stepper = Stepper::new(&s);
        let mut last: Option<f64> = None;
        let mut raw_moves = vec![];
        while s.state == State::Air {
            let before = s.x;
            let (events, (x, _)) = stepper.advance(&mut s, &w, TICK);
            if !events.is_empty() {
                break;
            }
            raw_moves.push(((s.x - before) / (300.0 * PHYS_DT)).round() as i32);
            if let Some(prev) = last {
                assert!((x - prev - 300.0 * TICK).abs() < 1e-6, "{} vs {}", x - prev, 300.0 * TICK);
            }
            last = Some(x);
        }
        assert!(raw_moves.len() > 20, "long enough to see the pattern");
        assert!(raw_moves.contains(&1) && raw_moves.contains(&2), "the simulation itself runs one step or two: {raw_moves:?}");
    }

    #[test]
    fn on_a_bounce_or_landing_the_feet_are_drawn_on_the_floor() {
        let w = world();
        let mut s = Sim::launch(500.0, 300.0, 0.0, 0.0);
        let mut stepper = Stepper::new(&s);
        let mut touched = 0;
        for i in 0..400 {
            let (events, (x, y)) = stepper.advance(&mut s, &w, if i % 8 == 7 { 2.0 * TICK } else { TICK });
            if events.iter().any(|e| matches!(e, Event::Bounce(_) | Event::Landed(_))) {
                assert_eq!((x, y), (s.x, 800.0));
                touched += 1;
            }
            if s.state == State::Rest {
                assert_eq!((x, y), (s.x, s.y), "at rest: exactly where it is");
                break;
            }
        }
        assert!(touched >= 1);
        assert_eq!(s.state, State::Rest);
    }

    #[test]
    fn stepping_through_a_stepper_is_stepping_by_hand() {
        let w = world();
        let start = Sim::launch(300.0, 200.0, 1800.0, -900.0);
        let (mut a, mut b) = (start.clone(), start);
        let mut stepper = Stepper::new(&a);
        let mut by_stepper = vec![];
        let pieces = [TICK, TICK, 2.0 * TICK, 0.004, TICK, 0.011];
        for i in 0..2000 {
            by_stepper.extend(stepper.advance(&mut a, &w, pieces[i % pieces.len()]).0);
            if a.state == State::Rest {
                break;
            }
        }
        let by_hand = run(&mut b, &w, 30.0);
        assert_eq!(a.state, State::Rest);
        assert_eq!(by_stepper, by_hand);
        assert_eq!(a, b);
    }

    #[test]
    fn a_drop_falls_lands_and_rests() {
        let w = world();
        let mut s = Sim::launch(500.0, 300.0, 0.0, 0.0);
        let ev = run(&mut s, &w, 3.0);
        assert_eq!(s.state, State::Rest);
        assert_eq!(s.y, 800.0);
        assert!(ev.iter().any(|e| matches!(e, Event::Landed(_))));
        assert_eq!(ev.last(), Some(&Event::Rest));
    }

    #[test]
    fn bounces_lose_energy_and_hard_landings_are_fast() {
        let w = world();
        let mut s = Sim::launch(500.0, 100.0, 0.0, 0.0);
        let ev = run(&mut s, &w, 5.0);
        let bounces: Vec<f64> = ev.iter().filter_map(|e| if let Event::Bounce(v) = e { Some(*v) } else { None }).collect();
        assert!(!bounces.is_empty(), "a 700 px drop bounces");
        assert!(bounces.windows(2).all(|p| p[1] < p[0]), "{bounces:?}");
        assert!(bounces[0] > HARD_LANDING_DIP_S);
        assert_eq!(s.state, State::Rest);
    }

    #[test]
    fn walls_bounce_and_keep_the_body_inside() {
        let w = world();
        let mut s = Sim::launch(900.0, 700.0, 3000.0, -500.0);
        let ev = run(&mut s, &w, 6.0);
        assert!(ev.contains(&Event::Wall));
        assert!(s.x + w.half_w <= w.right && s.x - w.half_w >= w.left);
        assert_eq!(s.state, State::Rest);
    }

    #[test]
    fn floors_are_one_way_and_sliding_off_falls() {
        // A platform above the taskbar: jumping up through it doesn't stop.
        let mut w = world();
        w.floors.push(Floor { x0: 400.0, x1: 600.0, y: 500.0, owner: 7 });
        let mut s = Sim::launch(500.0, 800.0, 0.0, -1600.0);
        let mut landed_on = None;
        for _ in 0..600 {
            for e in s.step(&w, PHYS_DT) {
                if let Event::Landed(_) = e {
                    landed_on.get_or_insert(s.y);
                }
            }
        }
        assert_eq!(landed_on, Some(500.0), "passes up through, lands on top coming down");
        // Sliding off its end drops to the taskbar.
        assert_eq!(w.floor_at(590.0, 500.0).map(|f| f.owner), Some(7));
        assert_eq!(w.floor_at(590.0, 700.0), None);
        let mut s = Sim { state: State::Slide, ..Sim::launch(590.0, 500.0, 400.0, 0.0) };
        run(&mut s, &w, 4.0);
        assert_eq!(s.y, 800.0);
        assert_eq!(w.floor_at(s.x, s.y).map(|f| f.owner), Some(0));
    }

    /// Drop from `height` above the floor; the poses in order, without repeats,
    /// plus the final reaction.
    fn drop_story(height: f64) -> (Vec<&'static str>, &'static str) {
        let w = world();
        let mut s = Sim::launch(500.0, 800.0 - height, 0.0, 0.0);
        let (mut poses, mut last, mut peak): (Vec<&str>, f64, f64) = (vec![], 0.0, 0.0);
        for _ in 0..1200 {
            for e in s.step(&w, PHYS_DT) {
                match e {
                    Event::Bounce(v) => peak = peak.max(v),
                    Event::Landed(v) => {
                        last = v;
                        peak = peak.max(v);
                    }
                    _ => {}
                }
            }
            if matches!(s.state, State::Air | State::Contact) {
                let p = body_pose(&s, 1.0);
                if poses.last() != Some(&p) {
                    poses.push(p);
                }
            }
        }
        (poses, landing_anim(last, peak, false, 1.0))
    }

    #[test]
    fn every_height_reads_differently() {
        // A tiny drop: no bounce, just a dip.
        let (p, end) = drop_story(20.0);
        assert!(!p.iter().any(|n| n.starts_with("hit_")), "{p:?}");
        assert_eq!(end, "soft_land");
        // Just too low to bounce: a proper landing.
        assert_eq!(drop_story(70.0).1, "land");
        // A light bounce only bends the knees.
        let (p, _) = drop_story(120.0);
        assert!(p.contains(&"hit_light") && !p.contains(&"hit_squash"), "{p:?}");
        // Window height: squashes, doesn't splat.
        let (p, _) = drop_story(250.0);
        assert!(p.contains(&"hit_squash") && !p.contains(&"hit_splat"), "{p:?}");
        // From high up: splats, and is rattled at the end.
        let (p, end) = drop_story(600.0);
        assert!(p.contains(&"hit_splat") && p.contains(&"fall_2") && !p.contains(&"fall_3"), "{p:?}");
        assert_eq!(end, "rattled");
    }

    #[test]
    fn a_long_plummet_pancakes_and_sees_stars() {
        let mut w = world();
        w.floors[0].y = 1600.0;
        let mut s = Sim::launch(500.0, 300.0, 0.0, 0.0);
        let mut poses = vec![];
        for _ in 0..600 {
            s.step(&w, PHYS_DT);
            poses.push(body_pose(&s, 1.0));
            if s.state == State::Contact {
                break;
            }
        }
        assert!(poses.contains(&"fall_3"));
        assert_eq!(poses.last(), Some(&"hit_slam"));
        assert_eq!(landing_anim(300.0, 2500.0, false, 1.0), "hard_land");
        assert_eq!(landing_anim(300.0, 2500.0, true, 1.0), "dazed", "shaken comes first");
    }

    #[test]
    fn the_squash_comes_at_first_touch_not_after_the_bounces() {
        let w = world();
        let mut s = Sim::launch(500.0, 200.0, 0.0, 0.0);
        while s.state == State::Air {
            s.step(&w, PHYS_DT);
        }
        // On the floor, flat, not moving, and the bounce hasn't happened yet.
        assert_eq!((s.state, s.y, s.vy), (State::Contact, 800.0, 0.0));
        assert!(body_pose(&s, 1.0).starts_with("hit_"));
        let rebound = s.rebound;
        let mut ticks = 0;
        while s.state == State::Contact {
            s.step(&w, PHYS_DT);
            ticks += 1;
            assert_eq!(s.y, 800.0, "feet stay down");
        }
        assert_eq!(s.vy, rebound);
        assert!(ticks >= 5, "long enough to be seen: {ticks}");
        assert_eq!(body_pose(&s, 1.0), "rise_2", "straight out of the squash into a stretch");
    }

    #[test]
    fn harder_hits_stay_down_longer_and_scale_with_dpi() {
        assert!(contact_secs(2500.0) > contact_secs(1000.0));
        assert_eq!(Impact::of(1200.0, 1.0), Impact::Medium);
        assert_eq!(Impact::of(1800.0, 1.5), Impact::Medium, "same DIP speed at 150 %");
        assert_eq!(Impact::of(2400.0, 1.0), Impact::Slam);
    }

    #[test]
    fn sliding_off_during_contact_keeps_going() {
        let mut w = world();
        w.floors.push(Floor { x0: 400.0, x1: 600.0, y: 500.0, owner: 7 });
        let mut s = Sim::launch(598.0, 300.0, 300.0, 0.0);
        let mut left = false;
        for _ in 0..1200 {
            let before = s.state;
            s.step(&w, PHYS_DT);
            left |= before == State::Contact && s.state == State::Air;
        }
        assert!(left);
        assert_eq!(s.y, 800.0, "ends on the floor below");
    }

    #[test]
    fn a_skid_pitches_the_way_it_slides() {
        assert_eq!(skid_pose(1500.0, 1.0), Some("ride_r2"));
        assert_eq!(skid_pose(-300.0, 1.0), Some("ride_l1"));
        assert_eq!(skid_pose(100.0, 1.0), None);
    }

    #[test]
    fn dpi_scales_gravity() {
        let mut w = world();
        let mut a = Sim::launch(500.0, 300.0, 0.0, 0.0);
        a.step(&w, PHYS_DT);
        w.scale = 2.0;
        let mut b = Sim::launch(500.0, 300.0, 0.0, 0.0);
        b.step(&w, PHYS_DT);
        assert!((b.vy - 2.0 * a.vy).abs() < 1e-9);
    }

    #[test]
    fn edge_hang_near_the_sides() {
        let w = world();
        assert_eq!(edge_hang(45.0, &w), Some(true));
        assert_eq!(edge_hang(500.0, &w), None);
        assert_eq!(edge_hang(955.0, &w), Some(false));
    }
}
