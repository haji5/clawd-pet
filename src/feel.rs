//! Every tunable for interaction and physics (PLAN.md §6b), in one place so
//! the feel can be tuned with the user. Distances are DIPs (1/96 inch) and
//! get scaled by DPI; times are ms.

use crate::tracker::Ms;

// ---- pointer ----
/// Movement before a press becomes a drag.
pub const DRAG_THRESHOLD_DIP: f64 = 4.0;
/// A press held this long without moving starts squishing.
pub const HOLD_START_MS: Ms = 250;
/// Clicks within this window count together.
pub const RAPID_WINDOW_MS: Ms = 2000;
/// Clicks within the window that start giggling / make Clawd dizzy.
pub const GIGGLE_CLICKS: usize = 5;
pub const DIZZY_CLICKS: usize = 12;

// ---- dragging ----
/// Direction reversals within `SHAKE_WINDOW_MS` that count as a shake, and
/// how far the cursor must travel back for a reversal to count.
pub const SHAKE_REVERSALS: usize = 4;
pub const SHAKE_WINDOW_MS: Ms = 800;
pub const SHAKE_MIN_DIP: f64 = 16.0;
/// How quickly the swing follows the cursor speed (time constant); the
/// slower it decays, the longer the follow-through after the cursor stops.
pub const LEAN_TAU_MS: f64 = 140.0;
/// Horizontal cursor speeds for lean level 1 and 2.
pub const LEAN1_DIP_S: f64 = 250.0;
pub const LEAN2_DIP_S: f64 = 900.0;
/// How often the dragging pose is updated while held (follow-through).
pub const DRAG_TICK_MS: u32 = 33;

// ---- throwing ----
/// Throw velocity = cursor movement over this window before release.
pub const THROW_SAMPLE_MS: Ms = 80;
/// Releases slower than this just drop.
pub const THROW_MIN_DIP_S: f64 = 300.0;
pub const THROW_MAX_DIP_S: f64 = 4500.0;

// ---- physics ----
/// Fixed simulation step and how often the window integrates.
pub const PHYS_DT: f64 = 1.0 / 120.0;
pub const PHYS_TICK_MS: u32 = 16;
pub const GRAVITY_DIP_S2: f64 = 3200.0;
/// Fraction of velocity lost to air per second.
pub const AIR_DRAG: f64 = 0.35;
pub const RESTITUTION_FLOOR: f64 = 0.35;
pub const RESTITUTION_WALL: f64 = 0.5;
pub const RESTITUTION_CEILING: f64 = 0.3;
/// Horizontal speed kept when bouncing off the floor.
pub const FLOOR_GRIP: f64 = 0.7;
/// Below this vertical speed a floor hit lands instead of bouncing.
pub const BOUNCE_MIN_DIP_S: f64 = 260.0;
/// Sliding deceleration after landing.
pub const SLIDE_FRICTION_DIP_S2: f64 = 2600.0;
/// Impact speeds that set how hard a floor hit reads (a fall of h DIPs hits
/// at about sqrt(6400 h)). Below `BOUNCE_MIN / RESTITUTION_FLOOR` (743, about
/// 86 DIPs) there's no bounce at all.
/// Touching down this softly is only a little dip.
pub const SOFT_LANDING_DIP_S: f64 = 420.0;
/// A bounce below this just bends the knees (under ~155 DIPs of fall).
pub const MEDIUM_IMPACT_DIP_S: f64 = 1000.0;
/// From here it flattens right out and is rattled afterwards (~350 DIPs).
pub const HARD_LANDING_DIP_S: f64 = 1500.0;
/// From here it pancakes and ends up seeing stars (~700 DIPs, so a drop from
/// the top of a 1080p screen gets there).
pub const SLAM_IMPACT_DIP_S: f64 = 2100.0;
/// How long a bounce keeps the feet on the floor, from the gentlest bounce
/// to `CONTACT_FULL_DIP_S`: the time the squash has to be seen.
pub const CONTACT_MIN_MS: f64 = 45.0;
pub const CONTACT_MAX_MS: f64 = 140.0;
pub const CONTACT_FULL_DIP_S: f64 = 2800.0;
/// Vertical speeds that pick the pose in the air: rising, then falling.
pub const RISE_SLOW_DIP_S: f64 = 200.0;
pub const RISE_FAST_DIP_S: f64 = 550.0;
pub const FALL_SLOW_DIP_S: f64 = 450.0;
pub const FALL_FAST_DIP_S: f64 = 1100.0;
/// Falling this fast: legs a blur, sweat flying.
pub const FALL_PANIC_DIP_S: f64 = 1900.0;
/// How long a knock against a wall or the ceiling shows.
pub const WALL_HIT_MS: Ms = 70;
/// Faster than this in the air, Clawd tumbles (90° steps), but only when
/// thrown sideways: a straight drop stays upright, stretching as it falls.
pub const TUMBLE_DIP_S: f64 = 1300.0;
pub const TUMBLE_VX_DIP_S: f64 = 500.0;
pub const TUMBLE_STEP_MS: Ms = 70;
/// Skidding after a landing: the top pitches forward above these speeds,
/// once the landing squash has had this long.
pub const SKID_SLOW_DIP_S: f64 = 200.0;
pub const SKID_FAST_DIP_S: f64 = 700.0;
pub const SKID_AFTER_LANDING_MS: Ms = 110;

// ---- edges ----
/// Dropped this close to a left/right screen edge, Clawd hangs on it.
pub const EDGE_HANG_DIP: f64 = 8.0;
/// Jump back from the edge when clicked: speeds.
pub const CLIMB_VX_DIP_S: f64 = 520.0;
pub const CLIMB_VY_DIP_S: f64 = -900.0;

// ---- perching on windows ----
/// Narrower windows can't be perched on.
pub const PERCH_MIN_WIDTH_DIP: f64 = 160.0;
/// Feet stay this far from a window's left edge and from its right edge
/// (the caption buttons).
pub const PERCH_INSET_LEFT_DIP: f64 = 12.0;
pub const PERCH_INSET_RIGHT_DIP: f64 = 150.0;
/// Bits of title bar shorter than this (between other windows) don't count.
pub const PERCH_MIN_SEGMENT_DIP: f64 = 24.0;
/// A window's bounds match a monitor within this many pixels = fullscreen.
pub const FULLSCREEN_TOLERANCE_PX: i32 = 2;
/// While the perch window moves: lean and follow-through updates.
pub const RIDE_TICK_MS: u32 = 33;
/// Knocked off: this many reversals of at least `RIDE_SHAKE_MIN_DIP` within
/// `RIDE_SHAKE_WINDOW_MS`.
pub const RIDE_SHAKE_REVERSALS: usize = 4;
pub const RIDE_SHAKE_WINDOW_MS: Ms = 1000;
pub const RIDE_SHAKE_MIN_DIP: f64 = 48.0;
/// Knocked off: share of the window's velocity kept, and the upward kick.
pub const KNOCK_KEEP: f64 = 0.5;
pub const KNOCK_VY_DIP_S: f64 = -600.0;

// ---- following the active window ----
/// An eligible window in the foreground for this long gets a visit.
pub const FOLLOW_SETTLE_MS: Ms = 1500;
/// At most one hop per this long.
pub const FOLLOW_EVERY_MS: Ms = 10_000;
/// The crouch before the jump.
pub const HOP_CROUCH_MS: Ms = 180;
/// How far the arc rises above the higher of its two ends.
pub const HOP_APEX_DIP: f64 = 70.0;

// ---- walking ----
/// How long Clawd stands around between strolls (random in this range).
pub const WALK_EVERY_MS: (Ms, Ms) = (30_000, 90_000);
/// How far one stroll goes (random in this range).
pub const WALK_DISTANCE_DIP: (u64, u64) = (60, 240);
pub const WALK_SPEED_DIP_S: f64 = 36.0;
/// How often the window moves during a stroll.
pub const WALK_TICK_MS: u32 = 33;

// ---- impatience ----
/// The tray icon swaps this often while a permission prompt is being ignored.
pub const TRAY_PULSE_MS: u32 = 600;

// ---- fullscreen ----
pub const FADE_MS: Ms = 150;
pub const FADE_TICK_MS: u32 = 16;
/// `size` of the tiny Clawd in the corner ("show attention during fullscreen").
pub const PEEK_SIZE: u32 = 1;

// ---- cursor awareness ----
/// Eyes follow the cursor within this distance.
pub const EYES_RADIUS_DIP: f64 = 250.0;
/// Cursor polling while awake and visible (15 Hz).
pub const CURSOR_POLL_MS: u32 = 66;
/// With the cursor far away, or resting, polling slows down to this at most.
pub const CURSOR_POLL_SLOW_MS: u32 = 264;
/// A cursor that hasn't moved relative to Clawd for this long is resting:
/// polled at `CURSOR_POLL_SLOW_MS` until it moves again.
pub const CURSOR_STILL_MS: Ms = 1500;
/// Fast enough that a real flick doesn't beat the next poll to the eyes.
pub const CURSOR_MAX_DIP_PER_S: f64 = 6000.0;
/// The poll timer is re-armed only when the wait changes by this much.
pub const CURSOR_REARM_STEP_MS: u32 = 16;
/// Hovering this long turns the wide eyes into a happy squint.
pub const HOVER_HAPPY_MS: Ms = 3000;
