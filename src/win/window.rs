//! The pet window: a `WS_EX_LAYERED` popup that never takes focus, its
//! message loop, mouse interaction (PLAN.md §6b), throwing physics,
//! placement and the context menu.
//!
//! Everything runs on the UI thread, and every entry into the state goes
//! through `enter`: one `&mut App` at a time, however Windows nests its
//! calls (see `with_app`). Nothing ticks while nothing changes:
//! one one-shot timer wakes the window at the next moment the picture or
//! the state can change (from the tracker and the animator), plus the 1 s
//! debounced config save and, with `--art`, a 500 ms file check. Short-lived
//! timers run only while needed: physics while airborne or sliding, the
//! swing while dragging, leaning while the perch window moves, the 150 ms
//! fade, and a 15 Hz cursor poll for the eyes while awake and visible.
//! Perching and fullscreen hiding (PLAN.md §6c) are driven by WinEvent hooks.

use super::autostart;
use super::chime;
use super::foreground::{self, Hooks};
use super::http;
use super::launch;
use super::monitor::{self, Monitor};
use super::process::{self, ExitWatch, Finder, WM_APP_EXITED};
use super::render::Surface;
use super::tip::{self, Tip};
use super::tray::{self, Tray, WM_APP_TRAY};
use super::wide;
use super::windows;
use crate::anim::{Director, Look, Shot};
use crate::compose::{CANVAS_H, CANVAS_W, Canvas};
use crate::config::{self, Config, MAX_SIZE, MIN_SIZE, Placement};
use crate::debug::Stats;
use crate::feel;
use crate::goto::{self, Host};
use crate::install;
use crate::interaction::{self, Click, Hand, Release, Swing};
use crate::perch::{self, Fit, Foreground, Ride, RideEvent, Screen, WindowInfo};
use crate::mood::{self, Clock, Mood};
use crate::physics::{self, Floor, Sim, Stepper, World};
use crate::ipc::{self, IpcMsg, Queue};
use crate::placement;
use crate::rng::Rng;
use crate::sound;
use crate::sprite::{self, Sheet};
use crate::tracker::{Attention, Mode, Ms, Tracker};
use crate::update::{self, Verdict, Version};
use crate::walk::Stroll;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
};
use windows_sys::core::GUID;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
use windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows_sys::Win32::UI::Controls::{NMTTDISPINFOW, TTN_GETDISPINFOW};
use windows_sys::Win32::UI::Input::Ime::ImmDisableIME;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Hand Windows back the pages Clawd isn't using, when it has nothing to do:
/// after starting, falling asleep, hiding. They're nearly all shared DLL
/// pages, so this mostly lowers the number Task Manager shows, not memory
/// actually in use (ROADMAP.md, Phase 6). Pages return by themselves when needed.
fn trim_working_set() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

const CLASS: &str = "ClawdPetWindow";
const WM_APP_EVENT: u32 = 0x8000 + 2; // WM_APP + 2: IPC queue has messages
/// Never posted: put in `LATER` when a WinEvent arrived while the state was
/// in use, to look at the foreground and the perch window again afterwards.
const WM_APP_RECHECK: u32 = 0x8000 + 3;
/// Never posted: a `WM_POWERBROADCAST` about the display, read in `wndproc`
/// (wParam 1 = on, 0 = off).
const WM_APP_DISPLAY: u32 = 0x8000 + 4;
/// Posted by an update worker: its news is in `Updater::news`.
const WM_APP_UPDATE: u32 = 0x8000 + 5;
/// windows-sys has it in `SystemServices`, a large feature for one GUID.
const GUID_CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
/// After the cursor was last over the tray icon, its tooltip text is kept
/// current this long (the tooltip can stay up with the cursor resting).
const TRAY_HOVER_MS: Ms = 10_000;
const TIMER_SAVE: usize = 1;
/// One-shot: the next frame or state change (tracker + animator).
const TIMER_TICK: usize = 2;
/// `--art`: check the sprite file for changes.
const TIMER_ART: usize = 3;
/// One-shot after a press: start squishing if it's still held.
const TIMER_HOLD: usize = 4;
/// While airborne or sliding.
const TIMER_PHYS: usize = 5;
/// While dragging: swing and follow-through.
const TIMER_DRAG: usize = 6;
/// While awake and visible: eyes follow the cursor.
const TIMER_CURSOR: usize = 7;
/// While the perch window moves and the lean settles afterwards.
const TIMER_RIDE: usize = 8;
/// During the fade-out for a fullscreen app.
const TIMER_FADE: usize = 9;
/// During a stroll along the ground.
const TIMER_WALK: usize = 10;
/// While a permission prompt is being ignored: the tray icon pulses.
const TIMER_PULSE: usize = 11;
/// One-shot: look at the Claude desktop app again (is its window up, is it
/// still running), a moment after something about it changed.
const TIMER_APP: usize = 12;
/// Windows come and go in bursts: look once they've settled.
const APP_SETTLE_MS: u32 = 150;
/// One-shot, after an update: sweep the old exe away again (a hook may have
/// been running it at the first try).
const TIMER_SWEEP: usize = 13;
const SWEEP_RETRY_MS: u32 = 30_000;
const SWEEP_TRIES: u8 = 3;
/// Far off every monitor, for `--offscreen`.
const PARKED_AT: (i32, i32) = (-20000, -20000);
/// How long hover outlives a mouse-leave.
const HOVER_GRACE_MS: Ms = 300;
/// The tooltip over Clawd has room for every session; the tray's doesn't.
const TIP_CHARS: usize = 600;
/// Canvas row of the eyes (for the gaze direction).
const EYES_Y: i32 = 33;
/// Not in the windows-sys features we use (it lives with the common controls).
const WM_MOUSELEAVE: u32 = 0x02A3;

// Menu command ids.
const CMD_SHOW: usize = 1;
const CMD_SIZE_BASE: usize = 10; // + 1..=6
const CMD_PLACE_TOP: usize = 21;
const CMD_PLACE_ATTENTION: usize = 22;
const CMD_HOME: usize = 30;
const CMD_PERCH: usize = 40;
const CMD_FOLLOW: usize = 41;
const CMD_EYES: usize = 42;
const CMD_HIDE_FULLSCREEN: usize = 43;
const CMD_ATTENTION_FULLSCREEN: usize = 44;
const CMD_WALK: usize = 45;
const CMD_TIME_OF_DAY: usize = 46;
const CMD_SOUND: usize = 47;
const CMD_FOLLOW_APP: usize = 48;
const CMD_DO_NOT_DISTURB: usize = 50;
const CMD_LAUNCH_WITH_CLAUDE: usize = 51;
const CMD_START_WITH_WINDOWS: usize = 52;
const CMD_INSTALL_HOOKS: usize = 53;
const CMD_UNINSTALL_HOOKS: usize = 54;
const CMD_CHECK_UPDATES: usize = 55;
const CMD_QUIT: usize = 99;
const CMD_GOTO_BASE: usize = 100; // + the item's index in `App::goto_jumps`

/// Standing on a window.
struct Perched {
    hwnd: HWND,
    ride: Ride,
    /// `TIMER_RIDE` is running.
    riding: bool,
    /// Removed when Clawd leaves.
    _hooks: Hooks,
}

/// Follow the active window: waiting for it to settle, then crouching.
#[derive(Clone, Copy)]
enum Hop {
    Waiting { hwnd: HWND, at: Ms },
    Crouching { hwnd: HWND, at: Ms },
}

impl Hop {
    fn at(self) -> Ms {
        match self {
            Hop::Waiting { at, .. } | Hop::Crouching { at, .. } => at,
        }
    }
}

/// Where a "Go to" leads.
struct Jump {
    window: HWND,
    /// Opens the exact chat, for a session in the desktop app.
    link: Option<String>,
}

impl Jump {
    fn find(finder: &Finder, t: &goto::Target) -> Option<Jump> {
        Some(Jump { window: finder.window_for(t.host?)?, link: t.link() })
    }

    /// Only ever after the user's own click.
    fn go(&self) {
        windows::bring_forward(self.window);
        if let Some(link) = &self.link {
            process::open_link(link);
        }
    }
}

/// Thrown or dropped: the simulation and its clock.
struct Flight {
    sim: Sim,
    world: World,
    last: Ms,
    /// Steps `sim` and says where to draw it, between steps.
    stepper: Stepper,
    rot: u8,
    next_turn: Ms,
    shaken: bool,
    /// Fell off its perch: flails until it first touches something.
    whoa: bool,
    /// A knock against a wall or the ceiling: this pose until that time.
    knock: Option<(&'static str, Ms)>,
    /// The hardest hit of this flight (px/s).
    peak: f64,
    landed_at: Option<Ms>,
    /// Last skid direction (+1 right, -1 left): rocks back when it stops.
    skid: i8,
}

impl Flight {
    /// What the body should look like right now; `None` hands over to the
    /// landing reaction.
    fn pose(&self, now: Ms) -> Option<&'static str> {
        let s = self.world.scale;
        match self.sim.state {
            physics::State::Air => {
                if let Some((name, until)) = self.knock
                    && now < until
                {
                    return Some(name);
                }
                if self.whoa {
                    Some("whoa")
                } else if self.rot != 0 {
                    Some("air") // tumbling: a stretch means nothing when rotated
                } else {
                    Some(physics::body_pose(&self.sim, s))
                }
            }
            physics::State::Contact => Some(physics::body_pose(&self.sim, s)),
            physics::State::Slide if self.landed_at.is_some_and(|t| now >= t + feel::SKID_AFTER_LANDING_MS) => {
                physics::skid_pose(self.sim.vx, s)
            }
            physics::State::Slide | physics::State::Rest => None,
        }
    }
}

/// The Claude desktop app, once a session has shown which process it is.
struct ClaudeApp {
    host: Host,
    /// No window: closed to the tray (the process is still there).
    away: bool,
    _hooks: Hooks,
}

/// How Clawd is held up. One of these at a time, which is the point: a
/// flight on a perch, or a stroll while hanging, can't be written down.
/// Being carried is `Standing` with the hand pressed (`Hand` knows).
enum Motion {
    /// On the ground, at rest or in the user's hand.
    Standing,
    /// On a window's title bar, riding along with it.
    Perched(Perched),
    /// Thrown, dropped or hopping: the simulation runs.
    Flying(Flight),
    /// Hanging on a screen edge, half off-screen (`left`: which edge).
    Hanging { left: bool },
    /// Walking along the ground.
    Strolling(Stroll),
}

/// Whether the window is on screen. `App::visible` is what the user chose;
/// this is what's true, which also depends on fullscreen apps.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Up,
    /// Still up, fading out for a fullscreen app since then.
    Fading { since: Ms },
    Hidden,
}

impl Shown {
    fn up(self) -> bool {
        self == Shown::Up
    }
}

/// Peeking, tiny, from the corner of a fullscreen app.
struct Peek {
    /// Where to go back to.
    home: (i32, i32),
    /// Lost its footing meanwhile: falls when the peek ends.
    fell: bool,
}

/// Where "Check for updates" is (PLAN.md §7, "Updating"). Nothing about it
/// runs unless the user chose the menu item, or a test sent `__update`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Updating {
    No,
    Checking,
    /// The question is open.
    Asking,
    /// Downloading, checking and putting the new exe in place.
    Downloading,
}

/// `--send __update`: the menu's flow with no boxes.
#[derive(Clone, Copy, PartialEq)]
enum UpdateTest {
    /// `--tool check`: log the verdict and stop.
    CheckOnly,
    /// Answer Yes by itself, and restart with `--quiet`.
    Install,
}

/// What an update worker thread brings back.
enum UpdateNews {
    Checked(Result<Verdict, String>),
    /// The new exe is in place (its version), or why not.
    Placed(Result<Version, String>),
}

struct Updater {
    state: Updating,
    /// Filled by the worker, which then posts `WM_APP_UPDATE`. That message
    /// is a plain wake-up: `LATER` may keep it and deliver it later.
    news: Arc<Mutex<Option<UpdateNews>>>,
    /// What this Clawd counts as (`update::setup`).
    current: Version,
    /// The exe to replace, read at the click, before anything is renamed.
    exe: Option<PathBuf>,
    test: Option<UpdateTest>,
    /// The new exe is in place: what `run` starts once the window has gone.
    restart: Option<(PathBuf, Vec<String>)>,
    /// After an update: the exes to sweep beside again, and how many times.
    sweep: Vec<PathBuf>,
    sweeps_left: u8,
}

/// What a handler asks for that can only run once the state is free again
/// (`enter` runs these after the handler returns). Each one runs a modal
/// loop or hands the focus away, and Windows keeps delivering messages
/// while it does: they must find the state unborrowed.
enum After {
    Menu,
    Message { text: String, error: bool },
    Go(Jump),
    /// "Update now?", then `update_answered`.
    Ask { text: String, release: update::Release },
    Quit,
}

impl After {
    fn result(result: Result<String, String>) -> After {
        match result {
            Ok(text) => After::Message { text, error: false },
            Err(text) => After::Message { text, error: true },
        }
    }

    fn run(self, hwnd: HWND) {
        match self {
            After::Menu => show_menu(hwnd),
            // No owner: it would disable the pet window while it's open.
            After::Message { text, error } => super::message_box(&text, error),
            After::Go(jump) => jump.go(),
            After::Ask { text, release } => {
                let yes = super::ask(&text);
                enter(|a| a.update_answered(yes, release));
            }
            After::Quit => unsafe {
                DestroyWindow(hwnd);
            },
        }
    }
}

pub struct Options {
    /// The SessionStart that launched the pet (Phase 4).
    pub seed: Option<IpcMsg>,
    /// Only the tray icon (automated checks).
    pub hidden: bool,
    /// Cycle through every animation.
    pub demo: bool,
    /// Load and hot-reload art from this sprites.txt.
    pub art: Option<PathBuf>,
    /// `scripts/perf.ps1`: the real window, timers and drawing, parked off
    /// every monitor so nothing appears on the screen.
    pub offscreen: bool,
    /// Started by "Check for updates" in place of this version.
    pub updated_from: Option<String>,
    /// The pet that started this one: wait for it to end first.
    pub wait_pid: Option<u32>,
    /// No box about the update (the test command).
    pub quiet: bool,
}

struct ArtWatch {
    path: PathBuf,
    mtime: Option<SystemTime>,
    error: Option<String>,
}

struct App {
    hwnd: HWND,
    cfg: Config,
    /// The config as last loaded or saved: a save writes what differs.
    cfg_synced: Config,
    cfg_path: Option<PathBuf>,
    sheet: Sheet,
    canvas: Canvas,
    /// What's on screen now; a redraw is skipped if the new canvas equals it.
    shown: Canvas,
    /// The look `canvas` or `shown` was last composed from.
    drawn_look: Option<Look>,
    /// Falling asleep gives memory back once, not on every refresh.
    was_asleep: bool,
    surface: Option<Surface>,
    /// Window pixels per canvas pixel (size × DPI/96, rounded).
    px: i32,
    dpi: u32,
    pos: (i32, i32),
    /// Where the window was last presented (moves are presents too).
    shown_pos: (i32, i32),
    visible: bool,
    hand: Hand,
    /// Cursor minus window position when the drag started.
    grab: (i32, i32),
    swing: Swing,
    last_drag_tick: Ms,
    motion: Motion,
    /// Monitors, refreshed whenever the display layout changes.
    screens: Vec<Screen>,
    presence: Shown,
    /// The foreground window is fullscreen.
    fullscreen: bool,
    peek: Option<Peek>,
    /// The global foreground hook.
    fg_hooks: Option<Hooks>,
    fg_hwnd: HWND,
    /// Moves of the foreground window's thread (F11), and that thread's id.
    fg_location: Option<(Hooks, u32)>,
    hop: Option<Hop>,
    last_hop: Option<Ms>,
    /// When to set off on the next stroll, while Clawd is free to.
    next_stroll: Option<Ms>,
    rng: Rng,
    hovering: bool,
    /// The cursor left: hover ends at this time unless it comes back (the
    /// perk hop moves pixels out from under a cursor resting on the edge).
    hover_off_at: Option<Ms>,
    /// The second click of a double-click is pending: its release does a trick.
    double: bool,
    /// This press grabbed Clawd out of the air: letting go without dragging
    /// lets it fall again.
    caught: bool,
    /// After the dizzy spell from rapid clicking, a grumpy puff.
    grumpy_at: Option<Ms>,
    cursor_timer: bool,
    /// The cursor timer's current period.
    cursor_wait: u32,
    /// The cursor's offset from the eyes at the last poll, and when that
    /// last changed (never stored anywhere else, never logged).
    cursor_seen: ((f64, f64), Ms),
    tray: Option<Tray>,
    /// The tray icon was hovered recently: its tooltip text is kept current
    /// until then (otherwise it only goes to Explorer on hover).
    tray_hover_until: Option<Ms>,
    /// The display is off (`GUID_CONSOLE_DISPLAY_STATE`): no frames.
    display_off: bool,
    power_notify: HPOWERNOTIFY,
    /// `TIMER_PULSE` is running.
    pulsing: bool,
    /// Claude needs the user; lifts the window in `attentionOnly` mode.
    attention: bool,
    /// While the menu is open: where each "Go to" item leads.
    goto_jumps: Vec<Option<Jump>>,
    /// `install::hook_status` as of the last menu, and what it read.
    hook_status: Option<(install::StatusKey, install::HookStatus)>,
    /// For `enter` to run once the current handler has returned.
    after: Vec<After>,
    /// One per process a session runs in, and the Claude app's: each posts
    /// `WM_APP_EXITED` when its process ends (`host_for`, `drop_dead_sessions`).
    watches: Vec<(Host, ExitWatch)>,
    claude_app: Option<ClaudeApp>,
    taskbar_created: u32,
    tracker: Tracker,
    director: Director,
    /// Last animation name, for the debug log and the demo tooltip.
    anim_name: String,
    demo: bool,
    art: Option<ArtWatch>,
    t0: Instant,
    queue: Arc<Queue>,
    /// Set when the UDP port couldn't be bound; shown in the tooltip.
    port_error: Option<String>,
    /// `--debug-log`: pretend it's this time (`--send __clock`).
    clock_override: Option<Clock>,
    /// What the last refresh showed, to notice someone starting to wait.
    last_mode: Mode,
    last_chime: Option<Ms>,
    /// Created on the first hover.
    tip: Option<Tip>,
    /// The text the tooltip is showing: it reads this after we answer it.
    tip_text: Vec<u16>,
    /// `--offscreen`: where the window really goes, relative to `pos`.
    park: (i32, i32),
    /// `--offscreen`: the mouse comes from posted messages, not the real cursor.
    fake_cursor: Option<(f64, f64)>,
    /// `--offscreen` / `--debug-log`: fullscreen isn't read from the real
    /// foreground window, so the user's own apps can't disturb a measurement.
    fake_fullscreen: Option<bool>,
    updater: Updater,
    stats: Stats,
}

static APP: AtomicPtr<App> = AtomicPtr::new(std::ptr::null_mut());
/// The UI thread is inside `with_app`. Only that thread ever asks.
static IN_APP: AtomicBool = AtomicBool::new(false);
/// Messages that arrived while the state was in use, for `enter` to handle
/// afterwards: (message, wParam, lParam).
static LATER: Mutex<Vec<(u32, WPARAM, LPARAM)>> = Mutex::new(Vec::new());
/// A context menu is open: there can only be one.
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// Run `f` on the state, unless a caller further up the stack is already
/// using it (then `None`).
///
/// That happens all the time: Windows calls the window procedure from
/// inside `SetWindowPos`, `ShowWindow`, `UpdateLayeredWindow` (a move onto a
/// monitor with another DPI sends `WM_DPICHANGED` before it returns),
/// `Shell_NotifyIconW` (it waits on Explorer, and broadcasts get delivered
/// meanwhile) and every modal loop. Handing each of those calls its own
/// `&mut App` would alias the one already in use: undefined behaviour, and
/// in practice a handler finding the state changed under it, or a surface
/// freed while it's being presented. So a nested call gets nothing here,
/// and `wndproc` puts what it brought in `LATER`.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    struct Free;
    impl Drop for Free {
        fn drop(&mut self) {
            IN_APP.store(false, Ordering::Relaxed);
        }
    }
    let app = APP.load(Ordering::Relaxed);
    if app.is_null() || IN_APP.swap(true, Ordering::Relaxed) {
        return None;
    }
    let _free = Free;
    // SAFETY: the Box leaked in `run`, freed only after the message loop has
    // ended, and `IN_APP` makes this the only reference to it.
    Some(f(unsafe { &mut *app }))
}

/// The way into the state for everything Windows calls: run `f`, then,
/// with the state free again, what `f` asked for (`After`) and whatever
/// arrived in the meantime (`LATER`).
fn enter<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let (result, after, hwnd) = with_app(|a| {
        let result = f(a);
        (result, std::mem::take(&mut a.after), a.hwnd)
    })?;
    for step in after {
        step.run(hwnd);
    }
    handle_kept(hwnd);
    Some(result)
}

/// Handle the messages that were kept in `LATER`, now that the state is
/// free. (Not generic, unlike `enter`, which it calls back into.)
fn handle_kept(hwnd: HWND) {
    loop {
        let later = std::mem::take(&mut *LATER.lock().unwrap_or_else(|e| e.into_inner()));
        if later.is_empty() {
            return;
        }
        for (msg, wp, lp) in later {
            enter(|a| a.on_message(hwnd, msg, wp, lp));
        }
    }
}

/// A message came in while the state was in use. The ones that carry news
/// are kept for afterwards (once each); the rest, timers and mouse moves,
/// come round again by themselves.
fn handle_later(msg: u32, wp: WPARAM, lp: LPARAM) {
    if APP.load(Ordering::Relaxed).is_null() {
        return;
    }
    let keep = match msg {
        // Their lParam is a plain number.
        WM_APP_EVENT | WM_APP_RECHECK | WM_APP_TRAY | WM_APP_DISPLAY | WM_APP_UPDATE | WM_APP_EXITED | WM_ENDSESSION | WM_DESTROY => (msg, wp, lp),
        // Their lParam points at something that won't be there later, and
        // the handlers only read wParam.
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => (msg, wp, 0),
        m if m >= 0xC000 => (msg, 0, 0), // registered: TaskbarCreated
        _ => return,
    };
    let mut later = LATER.lock().unwrap_or_else(|e| e.into_inner());
    if msg == WM_APP_DISPLAY {
        // Only the latest state counts (off, on, off must end up off).
        later.retain(|m| m.0 != WM_APP_DISPLAY);
    }
    if !later.contains(&keep) {
        later.push(keep);
    }
}

pub fn window_px(size: u32, dpi: u32) -> i32 {
    ((size as f64 * dpi as f64 / 96.0).round() as i32).max(1)
}

impl App {
    fn size(&self) -> (i32, i32) {
        (CANVAS_W as i32 * self.px, CANVAS_H as i32 * self.px)
    }

    /// Recompute the pixel scale for `dpi`; true if it changed.
    fn set_dpi(&mut self, dpi: u32) -> bool {
        self.dpi = dpi;
        let size = if self.peek.is_some() { feel::PEEK_SIZE } else { self.cfg.size };
        let px = window_px(size, dpi);
        self.hand.scale = dpi as f64 / 96.0;
        let changed = px != self.px || self.surface.is_none();
        self.px = px;
        if changed {
            let (w, h) = self.size();
            self.surface = Surface::new(w, h);
        }
        changed
    }

    fn now(&self) -> Ms {
        self.t0.elapsed().as_millis() as Ms
    }

    /// Compose the current look and push it to the window if it or the
    /// position changed (always with `force`, e.g. after the surface was
    /// recreated). Moving and redrawing happen in one `UpdateLayeredWindow`,
    /// and only the pixels that changed are converted and copied.
    ///
    /// Nothing while the window is hidden, unless forced: the only way back
    /// up (`update_presence`) redraws with `force` before showing it. A fade
    /// or the display being off still draws, so the picture is right when
    /// they end.
    fn redraw(&mut self, force: bool) {
        if !force && self.presence == Shown::Hidden {
            return;
        }
        let look = self.director.look(&self.sheet, self.now());
        // The same layers give the same pixels: a drag or a stroll only has
        // to move the window.
        let dirty = if force || look != self.drawn_look {
            match &look {
                Some(look) => self.canvas.draw(&self.sheet, look),
                None => self.canvas.clear(),
            }
            self.stats.composed += 1;
            self.drawn_look = look;
            if force { Some(self.canvas.whole()) } else { self.canvas.changed_since(&self.shown) }
        } else {
            None
        };
        if dirty.is_none() && self.pos == self.shown_pos {
            return;
        }
        let (x, y) = (self.pos.0 + self.park.0, self.pos.1 + self.park.1);
        let shown = match dirty {
            Some(d) => {
                std::mem::swap(&mut self.canvas, &mut self.shown);
                let (hwnd, px) = (self.hwnd, self.px as usize);
                self.surface.as_mut().map(|s| {
                    self.shown.write_bgra(px, d, s.buf());
                    self.stats.drawn += 1;
                    let scaled = |v: usize| (v * px) as i32;
                    s.present(hwnd, x, y, RECT { left: scaled(d.x0), top: scaled(d.y0), right: scaled(d.x1), bottom: scaled(d.y1) })
                })
            }
            None => {
                self.stats.moved += 1;
                self.surface.as_ref().map(|_| Surface::move_to(self.hwnd, x, y))
            }
        };
        if shown == Some(false) {
            crate::dlog!("UpdateLayeredWindow failed: {}", std::io::Error::last_os_error());
        }
        if shown.is_some() {
            self.shown_pos = self.pos;
        }
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.pos = (x, y);
        self.redraw(false);
    }

    fn scale(&self) -> f64 {
        self.dpi as f64 / 96.0
    }

    /// The feet point (bottom centre of the canvas) in screen pixels.
    fn feet(&self) -> (f64, f64) {
        ((self.pos.0 + crate::compose::FEET.0 * self.px) as f64, (self.pos.1 + CANVAS_H as i32 * self.px) as f64)
    }

    fn place_feet(&mut self, x: f64, y: f64) {
        let wx = x.round() as i32 - crate::compose::FEET.0 * self.px;
        let wy = y.round() as i32 - CANVAS_H as i32 * self.px;
        self.move_to(wx, wy);
    }

    /// Change how Clawd is held up; returns how it was. The timer of the
    /// state it leaves stops here, so none can be left running, and a
    /// perch's hooks go with its value.
    fn set_motion(&mut self, to: Motion) -> Motion {
        let was = std::mem::replace(&mut self.motion, to);
        let timer = match was {
            Motion::Flying(_) => Some(TIMER_PHYS),
            Motion::Strolling(_) => Some(TIMER_WALK),
            Motion::Perched(_) => Some(TIMER_RIDE),
            Motion::Standing | Motion::Hanging { .. } => None,
        };
        if let Some(id) = timer {
            unsafe { KillTimer(self.hwnd, id) };
        }
        was
    }

    fn flying(&self) -> bool {
        matches!(self.motion, Motion::Flying(_))
    }

    /// End a flight (caught, or the position is being set from outside).
    fn take_flight(&mut self) -> Option<Flight> {
        if !self.flying() {
            return None;
        }
        match self.set_motion(Motion::Standing) {
            Motion::Flying(f) => Some(f),
            _ => None,
        }
    }

    fn perch(&self) -> Option<&Perched> {
        if let Motion::Perched(p) = &self.motion { Some(p) } else { None }
    }

    /// `Some(true)` = hanging on the left edge.
    fn hang(&self) -> Option<bool> {
        if let Motion::Hanging { left } = self.motion { Some(left) } else { None }
    }

    fn strolling(&self) -> Option<Stroll> {
        if let Motion::Strolling(s) = self.motion { Some(s) } else { None }
    }

    /// Stop any flight, stroll or edge hang (the position is being set from
    /// outside). A perch is kept.
    fn stop_motion(&mut self) {
        if !matches!(self.motion, Motion::Perched(_)) {
            self.set_motion(Motion::Standing);
        }
        self.director.set_pose(None, self.now());
        self.director.set_rot(0);
        self.director.set_flip(false);
    }

    /// Put the window where the saved anchor says, on its monitor (or the
    /// primary one if that monitor is gone). Called at startup and whenever
    /// the display layout, DPI or taskbar changes.
    fn apply_anchor(&mut self) {
        self.screens = windows::screens();
        self.check_fullscreen();
        if self.peek.is_some() {
            if self.surface.is_none() {
                self.set_dpi(self.dpi);
                self.redraw(true);
            }
            return;
        }
        if self.perch().is_some() {
            // Stays on its window, at the new scale.
            let m = self.monitor_under_window();
            let rescaled = self.set_dpi(m.dpi);
            self.on_perch_moved();
            if rescaled {
                self.redraw(true);
            }
            return;
        }
        if !matches!(self.motion, Motion::Standing) {
            self.stop_motion();
        }
        let m = monitor::by_name_or_primary(&self.cfg.position.monitor);
        let rescaled = self.set_dpi(m.dpi);
        let (w, h) = self.size();
        self.pos = placement::resolve(&self.cfg.position, m.work, w, h, m.dpi);
        crate::dlog!("anchor {:?} on {} work={:?} dpi={} → {:?} px={}", self.cfg.position, m.name, m.work, m.dpi, self.pos, self.px);
        if rescaled {
            self.redraw(true);
        } else {
            self.move_to(self.pos.0, self.pos.1);
        }
    }

    fn monitor_under_window(&self) -> Monitor {
        let (w, h) = self.size();
        monitor::at_point(self.pos.0 + w / 2, self.pos.1 + h - 1)
    }

    fn remember_position(&mut self) {
        let m = self.monitor_under_window();
        let (w, h) = self.size();
        self.cfg.position = placement::anchor_for(self.pos.0, self.pos.1, w, h, m.work, m.dpi, &m.name);
        crate::dlog!("saved position {:?}", self.cfg.position);
        self.schedule_save();
    }

    fn schedule_save(&self) {
        unsafe { SetTimer(self.hwnd, TIMER_SAVE, 1000, None) };
    }

    fn save_now(&mut self) {
        unsafe { KillTimer(self.hwnd, TIMER_SAVE) };
        let Some(p) = &self.cfg_path else { return };
        match config::save(p, &self.cfg_synced, &self.cfg) {
            Ok(()) => self.cfg_synced = self.cfg.clone(),
            Err(e) => crate::dlog!("saving config failed: {e}"),
        }
    }

    fn apply_z_order(&self) {
        let topmost = self.cfg.placement == Placement::AlwaysOnTop || self.attention;
        if !topmost && let Some(p) = self.perch() {
            // Whatever covers the perch window covers Clawd too.
            windows::place_above(self.hwnd, p.hwnd);
            return;
        }
        // Behind the windows in use, but in sight while flying or being carried.
        let moving = self.flying() || self.hand.pressed();
        let after = if topmost {
            HWND_TOPMOST
        } else if moving {
            HWND_NOTOPMOST
        } else {
            HWND_BOTTOM
        };
        unsafe { SetWindowPos(self.hwnd, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
    }

    fn set_attention(&mut self, on: bool) {
        if self.attention != on {
            self.attention = on;
            self.apply_z_order();
            self.update_presence();
        }
    }

    // ---- perching on windows (PLAN.md §6c) ----

    /// Room Clawd needs above a window: the whole canvas, so the speech
    /// bubble stays on screen.
    fn clawd_h(&self) -> i32 {
        CANVAS_H as i32 * self.px
    }

    fn fit<'a>(&'a self, deny: &'a [String]) -> Fit<'a> {
        Fit { screens: &self.screens, deny, clawd_h: self.clawd_h(), scale: self.scale() }
    }

    /// Landed on a window's title bar: start riding it.
    fn perch_on(&mut self, hwnd: HWND) {
        let fit = self.fit(&self.cfg.perch_deny_processes);
        let info = windows::info(hwnd, self.hwnd, true).filter(|i| perch::eligible(i, &fit));
        let Some(info) = info else {
            crate::dlog!("perch target went away during the landing");
            self.drop(0.0, 0.0, false);
            return;
        };
        crate::dlog!("perched on {} ({}) at {:?}", info.class, info.process, info.bounds);
        let ride = Ride::new(hwnd as usize, self.feet().0, info.bounds);
        let hooks = foreground::perch(hwnd, Some(on_win_event));
        self.set_motion(Motion::Perched(Perched { hwnd, ride, riding: false, _hooks: hooks }));
        self.apply_z_order();
        self.on_perch_moved();
    }

    /// Stop riding (the hooks go with it). Returns the window left behind.
    fn leave_perch(&mut self) -> Option<perch::WinId> {
        self.perch()?;
        let Motion::Perched(p) = self.set_motion(Motion::Standing) else { return None };
        if self.director.pose().is_some_and(|n| n.starts_with("ride_")) {
            let now = self.now();
            self.director.set_pose(None, now);
        }
        self.apply_z_order();
        Some(p.ride.id)
    }

    /// The perch window moved, changed or needs rechecking: follow it, or
    /// fall if it can't be stood on any more. Returns the window as it was
    /// read, so a perch on the foreground window can also tell whether it
    /// went fullscreen without reading it again.
    fn on_perch_moved(&mut self) -> Option<WindowInfo> {
        let now = self.now();
        let scale = self.scale();
        let hwnd = self.perch().map(|p| p.hwnd)?;
        let fit = Fit { screens: &self.screens, deny: &[], clawd_h: self.clawd_h(), scale };
        // The deny-list can't change mid-ride: no need to look up the process.
        let info = windows::info(hwnd, self.hwnd, false);
        let Some(bounds) = info.as_ref().filter(|i| perch::eligible(i, &fit)).map(|i| i.bounds) else {
            self.fall_off("window can't be perched on any more", None);
            return info;
        };
        let Motion::Perched(p) = &mut self.motion else { return info };
        let event = p.ride.moved(now, bounds.l, bounds.t, scale);
        let (fx, fy) = p.ride.feet(bounds, scale);
        let velocity = p.ride.velocity(now, scale);
        if event == RideEvent::Moved && !p.riding {
            p.riding = true;
            self.swing = Swing::default();
            self.last_drag_tick = now;
            unsafe { SetTimer(self.hwnd, TIMER_RIDE, feel::RIDE_TICK_MS, None) };
        }
        if self.peek.is_none() {
            self.place_feet(fx, fy);
        }
        if event == RideEvent::KnockedOff {
            self.fall_off("shaken off", Some(velocity));
        }
        info
    }

    /// While the perch window moves: lean against the motion, then settle.
    fn on_ride_tick(&mut self) {
        let now = self.now();
        let dt = now.saturating_sub(self.last_drag_tick) as f64;
        self.last_drag_tick = now;
        let scale = self.scale();
        let Motion::Perched(p) = &mut self.motion else {
            unsafe { KillTimer(self.hwnd, TIMER_RIDE) };
            return;
        };
        let (vx, _) = p.ride.velocity(now, scale);
        self.swing.update(vx, dt);
        let level = self.swing.level(scale);
        if level == 0 && vx == 0.0 {
            p.riding = false;
            unsafe { KillTimer(self.hwnd, TIMER_RIDE) };
            crate::dlog!("ride settled, window at {:?}", self.pos);
        }
        let pose = match level {
            -2 => Some("ride_l2"),
            -1 => Some("ride_l1"),
            1 => Some("ride_r1"),
            2 => Some("ride_r2"),
            _ => None,
        };
        // Never over another pose (held, menu open…).
        let current = self.director.pose();
        if current != pose && current.is_none_or(|n| n.starts_with("ride_")) {
            self.director.set_pose(pose, now);
            self.refresh();
        }
    }

    /// The perch is gone (or Clawd got shaken off, with the window's velocity).
    fn fall_off(&mut self, why: &str, velocity: Option<(f64, f64)>) {
        let Some(id) = self.leave_perch() else { return };
        crate::dlog!("fell off: {why}");
        if let Some(peek) = self.peek.as_mut() {
            peek.fell = true;
            return;
        }
        let world = self.world(Some(id));
        let (fx, fy) = self.feet();
        let s = world.scale;
        let (vx, vy) = velocity.map_or((0.0, 0.0), |(vx, vy)| (vx * feel::KNOCK_KEEP, vy * feel::KNOCK_KEEP + feel::KNOCK_VY_DIP_S * s));
        self.launch_as(Sim::launch(fx, fy, vx, vy), world, false, true);
        self.refresh();
    }

    fn on_win_event(&mut self, hook: HWINEVENTHOOK, event: u32, hwnd: HWND) {
        if matches!(event, EVENT_OBJECT_SHOW | EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY) {
            self.check_claude_app_soon();
        }
        if event == EVENT_SYSTEM_FOREGROUND {
            self.on_foreground(hwnd);
            return;
        }
        if self.perch().is_some_and(|p| p.hwnd == hwnd) {
            match event {
                // Perched on the foreground window, a move arrives through
                // the perch's hooks and again through the foreground's
                // location hook. The perch's copy does both jobs; the
                // fullscreen check reads what the perch just read.
                EVENT_OBJECT_LOCATIONCHANGE if self.fg_location.as_ref().is_some_and(|l| l.0.has(hook)) => {}
                EVENT_OBJECT_LOCATIONCHANGE => {
                    let info = self.on_perch_moved();
                    if hwnd == self.fg_hwnd {
                        self.judge_fullscreen(info);
                    }
                }
                EVENT_SYSTEM_MINIMIZESTART => self.fall_off("minimized", None),
                EVENT_OBJECT_DESTROY => self.fall_off("closed", None),
                EVENT_OBJECT_HIDE => self.fall_off("hidden", None),
                EVENT_OBJECT_CLOAKED => self.fall_off("cloaked", None),
                _ => {}
            }
            return;
        }
        if event == EVENT_OBJECT_LOCATIONCHANGE && hwnd == self.fg_hwnd {
            self.check_fullscreen();
        }
    }

    fn on_foreground(&mut self, hwnd: HWND) {
        if hwnd.is_null() || hwnd == self.hwnd {
            return;
        }
        self.fg_hwnd = hwnd;
        if self.cfg.behaviours.hide_fullscreen {
            // Going fullscreen (F11) changes the bounds, not the foreground.
            let tid = foreground::thread_of(hwnd);
            if self.fg_location.as_ref().map(|l| l.1) != Some(tid) {
                self.fg_location = Some(foreground::location(hwnd, Some(on_win_event)));
            }
        }
        self.check_fullscreen();
        if self.perch().is_some() {
            self.on_perch_moved();
            self.apply_z_order();
        }
        self.plan_hop(hwnd);
        self.refresh();
    }

    /// A window event was missed while the state was in use: look at the
    /// foreground and the perch window as they are now.
    fn recheck_windows(&mut self) {
        let foreground = windows::foreground();
        if foreground.is_null() || foreground == self.hwnd {
            self.check_fullscreen();
            self.on_perch_moved();
            self.refresh();
        } else {
            self.on_foreground(foreground);
        }
    }

    // ---- following the active window ----

    fn cancel_hop(&mut self, now: Ms) {
        if matches!(self.hop.take(), Some(Hop::Crouching { .. })) && self.director.pose() == Some("hop_crouch") {
            self.director.set_pose(None, now);
        }
    }

    /// A new foreground window: if it can be perched on, visit it once it
    /// has been in front for a while (rate-limited).
    fn plan_hop(&mut self, hwnd: HWND) {
        let now = self.now();
        self.cancel_hop(now);
        let b = &self.cfg.behaviours;
        if !b.follow_active_window || !b.perch || self.perch().is_some_and(|p| p.hwnd == hwnd) {
            return;
        }
        let fit = self.fit(&self.cfg.perch_deny_processes);
        if windows::info(hwnd, self.hwnd, true).is_some_and(|i| perch::eligible(&i, &fit)) {
            let at = (now + feel::FOLLOW_SETTLE_MS).max(self.last_hop.map_or(0, |t| t + feel::FOLLOW_EVERY_MS));
            self.hop = Some(Hop::Waiting { hwnd, at });
        }
    }

    fn step_hop(&mut self, now: Ms) {
        let busy = self.hand.pressed()
            || self.flying()
            || self.hang().is_some()
            || !self.animating()
            || self.demo
            || self.peek.is_some()
            || matches!(self.director.mode(), Mode::Asleep);
        match self.hop {
            Some(Hop::Waiting { hwnd, at }) if now >= at => {
                if busy || windows::foreground() != hwnd {
                    self.hop = None;
                    return;
                }
                self.director.set_pose(Some("hop_crouch"), now);
                self.hop = Some(Hop::Crouching { hwnd, at: now + feel::HOP_CROUCH_MS });
            }
            Some(Hop::Crouching { hwnd, at }) if now >= at => {
                self.hop = None;
                self.director.set_pose(None, now);
                if !busy {
                    self.hop_to(hwnd, now);
                }
            }
            _ => {}
        }
    }

    /// Jump in an arc onto `hwnd`'s title bar (same monitor only).
    fn hop_to(&mut self, hwnd: HWND, now: Ms) {
        let mut world = self.world(None);
        let (fx, fy) = self.feet();
        let id = hwnd as usize;
        let Some(to) = perch::hop_target(&world.floors, id, fx, world.half_w) else {
            crate::dlog!("nowhere to hop to on that window");
            return;
        };
        world.floors.retain(|f| f.owner == id || f.owner == 0);
        let (vx, vy) = perch::hop_velocity((fx, fy), to, world.top + world.height, world.scale);
        crate::dlog!("hop from {fx},{fy} to {to:?}");
        self.last_hop = Some(now);
        self.leave_perch();
        self.launch(Sim::hop(fx, fy, vx, vy), world, false);
    }

    // ---- fullscreen apps ----

    fn check_fullscreen(&mut self) {
        let w = windows::info(self.fg_hwnd, self.hwnd, false);
        self.judge_fullscreen(w);
    }

    /// `check_fullscreen` with the foreground window already read.
    fn judge_fullscreen(&mut self, w: Option<WindowInfo>) {
        let Some(w) = w else { return };
        let fullscreen = match (self.fake_fullscreen, perch::classify_foreground(&w, &self.screens)) {
            (Some(fake), _) => fake,
            (None, Foreground::Fullscreen) => true,
            (None, Foreground::Normal) => false,
            (None, Foreground::Transient) => return,
        };
        if fullscreen != self.fullscreen {
            crate::dlog!("fullscreen={fullscreen} (foreground class {})", w.class);
            self.fullscreen = fullscreen;
            self.update_presence();
            self.refresh();
        }
    }

    /// Decide whether the window should be up: shown, faded out for a
    /// fullscreen app, or peeking from its corner.
    fn update_presence(&mut self) {
        let b = &self.cfg.behaviours;
        let hide = b.hide_fullscreen && self.fullscreen;
        // Closed to the tray, the Claude app takes Clawd with it.
        let away = b.follow_claude_app && self.claude_app.as_ref().is_some_and(|c| c.away);
        let peek = hide && !away && self.visible && self.attention && b.attention_in_fullscreen && !self.hand.pressed();
        let want = self.visible && !away && (!hide || peek);
        if !peek && self.peek.is_some() {
            if !want {
                // Before it grows back: the app is still fullscreen.
                unsafe { ShowWindow(self.hwnd, SW_HIDE) };
                self.end_fade();
                self.presence = Shown::Hidden;
            }
            self.leave_peek();
        }
        if peek && self.peek.is_none() {
            self.end_fade();
            self.enter_peek();
        }
        if want == self.presence.up() {
            return; // a fade that's under way runs on
        }
        if want {
            self.end_fade();
            self.presence = Shown::Up;
            self.redraw(true);
            unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
            self.apply_z_order();
        } else if self.visible {
            self.presence = Shown::Fading { since: self.now() };
            unsafe { SetTimer(self.hwnd, TIMER_FADE, feel::FADE_TICK_MS, None) };
        } else {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.presence = Shown::Hidden;
            trim_working_set();
        }
    }

    /// Stop a fade where it is. The window itself is still up, at full
    /// opacity again: the caller shows or hides it for good.
    fn end_fade(&mut self) {
        if matches!(self.presence, Shown::Fading { .. }) {
            unsafe { KillTimer(self.hwnd, TIMER_FADE) };
            if let Some(s) = self.surface.as_mut() {
                s.alpha = 255;
            }
            self.presence = Shown::Hidden;
        }
    }

    fn on_fade_tick(&mut self) {
        let Shown::Fading { since } = self.presence else {
            unsafe { KillTimer(self.hwnd, TIMER_FADE) };
            return;
        };
        let k = self.now().saturating_sub(since) as f64 / feel::FADE_MS as f64;
        if k >= 1.0 {
            self.end_fade();
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            trim_working_set();
            return;
        }
        let (hwnd, x, y) = (self.hwnd, self.pos.0 + self.park.0, self.pos.1 + self.park.1);
        if let Some(s) = self.surface.as_mut() {
            s.alpha = ((1.0 - k) * 255.0) as u8;
            s.present(hwnd, x, y, s.all());
        }
    }

    /// Tiny, in the bottom-right corner of the fullscreen app's monitor.
    fn enter_peek(&mut self) {
        let fell = self.take_flight().is_some();
        if fell {
            self.director.set_rot(0);
            self.director.set_pose(None, self.now());
        }
        let m = match windows::info(self.fg_hwnd, self.hwnd, false) {
            Some(w) => monitor::at_point((w.bounds.l + w.bounds.r) / 2, (w.bounds.t + w.bounds.b) / 2),
            None => monitor::primary(),
        };
        self.peek = Some(Peek { home: self.pos, fell });
        self.surface = None;
        self.set_dpi(m.dpi);
        let (w, h) = self.size();
        self.pos = (m.full.r - w, m.full.b - h);
        crate::dlog!("peeking at {:?}", self.pos);
        self.redraw(true);
    }

    fn leave_peek(&mut self) {
        let Some(Peek { home, fell }) = self.peek.take() else { return };
        let m = monitor::at_point(home.0, home.1);
        self.surface = None;
        self.pos = home;
        self.set_dpi(m.dpi);
        self.redraw(true);
        self.on_perch_moved();
        if fell {
            self.drop(0.0, 0.0, false);
        }
    }

    // ---- Claude state ----

    /// `--debug-log` only: drop Clawd at a screen point (`--send __drop --tool x,y`),
    /// to exercise landing and perching without a hand on the mouse.
    fn debug_drop(&mut self, at: Option<&str>) {
        let Some((x, y)) = at.and_then(|s| s.split_once(',')) else { return };
        let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>()) else { return };
        self.leave_perch();
        self.stop_motion();
        self.place_feet(x, y);
        self.drop(0.0, 0.0, false);
    }

    /// Moods only matter around Claude events: Clawd is asleep a few minutes
    /// after the last one, so there's nothing to keep up with in between.
    fn apply_mood(&mut self) {
        let clock = self.clock_override.unwrap_or_else(super::local_clock);
        let mood = if self.cfg.behaviours.time_of_day { mood::at(clock) } else { Mood::ORDINARY };
        self.tracker.set_drowsiness(mood.drowsy_pct);
        self.director.set_mood(mood);
    }

    fn tip_body(&self) -> String {
        self.tracker.tooltip_within(TIP_CHARS)
    }

    fn ensure_tip(&mut self) {
        if self.tip.is_none() {
            self.tip = Tip::new(self.hwnd);
        }
    }

    /// `--debug-log` only: the parts of the tooltip and the chime that can
    /// be checked without a mouse or a noise.
    fn debug_tip_and_chime(&mut self) {
        self.ensure_tip();
        let tools = self.tip.as_ref().map_or(-1, Tip::tools);
        crate::dlog!("tip tools={tools} text={:?}; chime available={}", self.tip_body(), chime::available());
    }

    /// Someone started waiting on the user. Quiet in Do not disturb, and
    /// during a game unless attention is allowed to show there too.
    fn ring_if_needed(&mut self, mode: Mode, now: Ms) {
        let rings = sound::rings(self.last_mode, mode);
        self.last_mode = mode;
        let b = &self.cfg.behaviours;
        if rings
            && b.attention_sound
            && !self.cfg.do_not_disturb
            && (!self.fullscreen || b.attention_in_fullscreen)
            && self.last_chime.is_none_or(|t| now >= t + sound::QUIET_FOR)
        {
            crate::dlog!("chime");
            self.last_chime = Some(now);
            chime::play();
        }
    }

    fn on_ipc(&mut self) {
        let now = self.now();
        self.apply_mood();
        for m in self.queue.drain() {
            if m.e == "__show" {
                self.set_visible(true);
                continue;
            }
            if m.e == "__drop" && crate::debug::enabled() {
                self.debug_drop(m.t.as_deref());
                continue;
            }
            if m.e == "__walk" && crate::debug::enabled() {
                if self.strolling().is_none() {
                    self.next_stroll = Some(0); // the refresh below sets off
                }
                continue;
            }
            if m.e == "__goto" && crate::debug::enabled() {
                self.debug_goto();
                continue;
            }
            if m.e == "__tip" && crate::debug::enabled() {
                self.debug_tip_and_chime();
                continue;
            }
            if m.e == "__menu" && crate::debug::enabled() {
                // A menu choice without the menu (`--tool <command id>`).
                if let Some(cmd) = m.t.as_deref().and_then(|t| t.parse().ok()) {
                    crate::dlog!("menu command {cmd}");
                    self.on_command(cmd);
                }
                continue;
            }
            if m.e == "__update" && crate::debug::enabled() {
                // The menu item's flow with no boxes; `--tool check` stops
                // after the check.
                let test = if m.t.as_deref() == Some("check") { UpdateTest::CheckOnly } else { UpdateTest::Install };
                self.start_update(Some(test));
                continue;
            }
            if m.e == "__menubuild" && crate::debug::enabled() {
                // What opening the menu costs, without opening it.
                let t = Instant::now();
                let (menu, watching) = self.build_menu();
                let took = t.elapsed();
                unsafe { DestroyMenu(menu) };
                self.menu_closed(watching, 0);
                crate::dlog!("menu built in {:.2} ms", took.as_secs_f64() * 1000.0);
                continue;
            }
            if m.e == "__stats" && crate::debug::enabled() {
                crate::dlog!("{}", self.stats.take());
                continue;
            }
            if m.e == "__fullscreen" && crate::debug::enabled() {
                self.fake_fullscreen = m.t.as_deref().map(|t| t == "1");
                self.check_fullscreen();
                continue;
            }
            if m.e == "__display" && crate::debug::enabled() {
                // `--tool 0`: the display went off, `1`: back on.
                self.set_display_off(m.t.as_deref() == Some("0"));
                continue;
            }
            if m.e == "__trayhover" && crate::debug::enabled() {
                // The cursor over the tray icon, without a cursor.
                self.on_tray_hover();
                continue;
            }
            if m.e == "__clock" && crate::debug::enabled() {
                self.clock_override = m.t.as_deref().and_then(Clock::parse);
                self.apply_mood();
                crate::dlog!("clock {:?}", self.clock_override);
                continue;
            }
            crate::dlog!("event {} s={} t={:?} n={:?} a={:?}", m.e, m.s, m.t, m.n, m.a);
            let reactions = self.tracker.on_event(&m, now);
            if let Some(pid) = m.pp {
                let host = self.host_for(pid);
                self.tracker.set_host(&m.s, host);
                if m.ds.is_some() {
                    self.watch_claude_app(pid);
                }
            }
            if m.e == "SessionEnd" {
                // The app ends every session when it quits.
                self.check_claude_app_soon();
            }
            if !self.cfg.do_not_disturb {
                for r in reactions {
                    self.director.react(&self.sheet, Shot::Claude(r), now);
                }
            }
        }
        self.refresh();
    }

    // ---- going to a session (PLAN.md §8 item 5b) ----

    /// `--debug-log` only: log where each "Go to" would lead, without going.
    fn debug_goto(&mut self) {
        let now = self.now();
        self.drop_dead_sessions();
        let finder = Finder::new();
        for t in self.tracker.goto_targets(now) {
            let jump = Jump::find(&finder, &t);
            crate::dlog!("goto {:?} -> {:?} link={:?}", t.menu_text(jump.is_some()), jump.as_ref().map(|j| windows::describe(j.window)), jump.and_then(|j| j.link));
        }
    }

    /// Left-click on the tray icon: the most urgent waiting session that
    /// still has a window. Does nothing when nobody's waiting.
    fn go_to_waiting(&mut self) {
        let now = self.now();
        if self.drop_dead_sessions() {
            self.refresh();
        }
        let targets = self.tracker.goto_targets(now);
        let finder = Finder::new();
        if let Some(jump) = targets.iter().filter(|t| t.waiting.is_some()).find_map(|t| Jump::find(&finder, t)) {
            self.after.push(After::Go(jump));
        }
    }

    /// Forget sessions whose Claude Code process has gone without a
    /// `SessionEnd` (a closed terminal, a crash): a working session would
    /// keep its pose for minutes, and a prompt that was open then would have
    /// Clawd call for the user, and pulse the tray, forever. The one reaper:
    /// `WM_APP_EXITED` calls it, and so does anything that is about to show
    /// the sessions (a wake-up can be a moment behind). Free: one zero wait
    /// per watch. True if any went: the caller refreshes.
    fn drop_dead_sessions(&mut self) -> bool {
        self.watches.retain(|(host, watch)| {
            let ended = watch.ended();
            if ended {
                crate::dlog!("process {} ended", host.pid);
            }
            !ended
        });
        let watches = &self.watches;
        let gone = self.tracker.drop_dead(|h| watches.iter().any(|(w, watch)| *w == h && !watch.ended()));
        if gone > 0 {
            crate::dlog!("{gone} session(s) dropped: their process is gone");
        }
        gone > 0
    }

    /// The host for a session's process (or the app's), watched from now on.
    /// `None` if it can't be opened or has already exited. Lets go of the
    /// watches nothing refers to any more first, so there are at most
    /// `MAX_SESSIONS + 1`, one per process.
    fn host_for(&mut self, pid: u32) -> Option<Host> {
        let app = self.claude_app.as_ref().map(|c| c.host);
        let used: Vec<Host> = self.tracker.hosts().chain(app).collect();
        self.watches.retain(|(h, _)| used.contains(h));
        if let Some((h, _)) = self.watches.iter().find(|(h, w)| h.pid == pid && !w.ended()) {
            return Some(*h);
        }
        let (host, watch) = process::watch(pid, self.hwnd)?;
        self.watches.push((host, watch));
        Some(host)
    }

    /// Is `host` still running? Only a watched process can say yes.
    fn alive(&self, host: Host) -> bool {
        self.watches.iter().any(|(h, w)| *h == host && !w.ended())
    }

    /// A watched process has ended (or may have: the window asks each watch).
    fn on_exited(&mut self) {
        let app_ended = self.claude_app.as_ref().is_some_and(|c| !self.alive(c.host));
        self.drop_dead_sessions();
        if app_ended {
            // Its sessions' processes go down with it, a few ms apart, in any
            // order: judge once they have been reaped too.
            self.check_claude_app_soon();
        }
        self.refresh();
    }

    // ---- the Claude desktop app ----

    /// A session in the desktop app just said which process runs it: find
    /// the app from there and watch its windows. Closing the app to the tray
    /// hides Clawd, and quitting it quits Clawd.
    fn watch_claude_app(&mut self, session_pid: u32) {
        if !self.cfg.behaviours.follow_claude_app {
            return;
        }
        if self.claude_app.as_ref().is_some_and(|c| self.alive(c.host)) {
            return;
        }
        let Some(session) = self.host_for(session_pid) else { return };
        let Some(pid) = Finder::new().app_process(session) else { return };
        let Some(host) = self.host_for(pid) else { return };
        crate::dlog!("watching the Claude app, pid {pid}");
        let hooks = foreground::app(pid, Some(on_win_event));
        self.claude_app = Some(ClaudeApp { host, away: false, _hooks: hooks });
        self.check_claude_app();
    }

    fn check_claude_app_soon(&mut self) {
        if self.claude_app.is_some() {
            unsafe { SetTimer(self.hwnd, TIMER_APP, APP_SETTLE_MS, None) };
        }
    }

    fn check_claude_app(&mut self) {
        let Some(host) = self.claude_app.as_ref().map(|c| c.host) else { return };
        if !self.alive(host) {
            crate::dlog!("the Claude app has quit");
            self.claude_app = None;
            self.drop_dead_sessions();
            // A session somewhere else (a terminal) still needs its pet.
            if self.tracker.is_empty() {
                self.after.push(After::Quit);
            } else {
                self.update_presence();
                self.refresh();
            }
            return;
        }
        let away = !windows::has_app_window(host.pid);
        let Some(c) = self.claude_app.as_mut() else { return };
        let changed = away != c.away;
        c.away = away;
        if changed {
            crate::dlog!("the Claude app is {}", if away { "closed to the tray" } else { "open" });
            self.update_presence();
            self.refresh();
        }
    }

    // ---- walking (PLAN.md §8) ----

    /// Idle on the ground, with nothing else going on. Hidden pets only
    /// stroll with `--debug-log`, for checks that don't cover the screen.
    fn free_to_stroll(&self, mode: Mode, now: Ms) -> bool {
        self.cfg.behaviours.walk
            && mode == Mode::Idle
            && self.cfg.position.on_taskbar
            && !self.demo
            && (self.presence.up() || crate::debug::enabled())
            && !self.display_off
            && matches!(self.motion, Motion::Standing | Motion::Strolling(_))
            && self.hop.is_none()
            && self.peek.is_none()
            && !self.hand.pressed()
            && !self.hovering
            && self.director.pose().is_none_or(|p| p == "walk")
            && !self.director.user_playing(&self.sheet, now)
    }

    fn step_stroll(&mut self, mode: Mode, now: Ms) {
        let free = self.free_to_stroll(mode, now);
        if self.strolling().is_some() {
            if !free {
                self.end_stroll(now);
            }
            return;
        }
        match self.next_stroll {
            _ if !free => self.next_stroll = None,
            None => self.next_stroll = Some(now + self.rng.range(feel::WALK_EVERY_MS.0, feel::WALK_EVERY_MS.1)),
            Some(at) if now >= at => {
                self.next_stroll = None;
                self.start_stroll(now);
            }
            Some(_) => {}
        }
    }

    fn start_stroll(&mut self, now: Ms) {
        let work = self.monitor_under_window().work;
        let half = (crate::compose::FEET.0 * self.px) as f64;
        let (x, _) = self.feet();
        let Some(s) = Stroll::plan(x, work.l as f64 + half, work.r as f64 - half, self.scale(), now, &mut self.rng) else { return };
        crate::dlog!("stroll {:.0} → {:.0}", s.from, s.to);
        self.set_motion(Motion::Strolling(s));
        self.director.set_pose(Some("walk"), now);
        unsafe { SetTimer(self.hwnd, TIMER_WALK, feel::WALK_TICK_MS, None) };
    }

    /// Stop where it is now, and stay there.
    fn end_stroll(&mut self, now: Ms) {
        if self.strolling().is_none() {
            return;
        }
        self.set_motion(Motion::Standing);
        if self.director.pose() == Some("walk") {
            self.director.set_pose(None, now);
        }
        crate::dlog!("stroll ended at {:.0}", self.feet().0);
        self.remember_position();
    }

    fn on_walk_tick(&mut self) {
        let now = self.now();
        let Some(s) = self.strolling() else { return };
        let (_, y) = self.feet();
        self.place_feet(s.x_at(now), y);
        if s.done(now) {
            self.end_stroll(now);
            self.refresh();
        }
    }

    fn set_tray_pulse(&mut self, on: bool) {
        if on == self.pulsing {
            return;
        }
        self.pulsing = on;
        crate::dlog!("tray pulse {on}");
        if on {
            unsafe { SetTimer(self.hwnd, TIMER_PULSE, feel::TRAY_PULSE_MS, None) };
            self.on_pulse_timer();
        } else {
            unsafe { KillTimer(self.hwnd, TIMER_PULSE) };
            if let Some(t) = self.tray.as_mut() {
                t.set_alert(false);
            }
        }
    }

    fn on_pulse_timer(&mut self) {
        if let Some(t) = self.tray.as_mut() {
            let on = !t.alert_on();
            t.set_alert(on);
        }
    }

    /// Frames are worth drawing: the window is up and the display is on.
    fn animating(&self) -> bool {
        self.presence.up() && !self.display_off
    }

    /// Send the tray icon's tooltip to Explorer if it changed. Only around
    /// a hover (`tray_hover_until`): Explorer took ~0.55 ms of CPU per push,
    /// and the text changes on nearly every Claude event (performance audit,
    /// 2026-11-13). Someone reaching the icon by keyboard sees the text from
    /// the last hover.
    fn update_tray_tip(&mut self) {
        let mut tip = if self.demo { format!("Demo: {}", self.anim_name) } else { self.tracker.tooltip() };
        if let Some(e) = self.art.as_ref().and_then(|a| a.error.as_ref()) {
            tip = format!("{e}\n{tip}");
        }
        if let Some(e) = &self.port_error {
            tip = format!("{e}\n{tip}");
        }
        if let Some(t) = self.tray.as_mut() {
            t.set_tip(&tip);
        }
    }

    /// The cursor is over the tray icon: its tooltip is about to show.
    fn on_tray_hover(&mut self) {
        self.tray_hover_until = Some(self.now() + TRAY_HOVER_MS);
        self.update_tray_tip();
    }

    /// The display went off or came back on. Off counts as hidden for the
    /// frame and cursor timers: nobody can see them (performance audit,
    /// 2026-11-13). The window itself stays where it is.
    fn set_display_off(&mut self, off: bool) {
        if off != self.display_off {
            crate::dlog!("display {}", if off { "off" } else { "on" });
            self.display_off = off;
            self.refresh();
        }
    }

    /// Recompute what Clawd shows, redraw if the picture changed, and arm
    /// the one-shot timer for the next change.
    fn refresh(&mut self) {
        self.stats.refreshes += 1;
        let now = self.now();
        // A crashed app's window can vanish without any event.
        if self.perch().is_some_and(|p| !windows::exists(p.hwnd)) {
            self.fall_off("window is gone", None);
        }
        self.step_hop(now);
        let d = self.tracker.display(now);
        let d = if self.cfg.do_not_disturb { d.quiet() } else { d };
        self.director.set_display(d, now);
        self.director.tick(&self.sheet, now);
        self.step_stroll(d.mode, now);
        if self.hover_off_at.is_some_and(|t| now >= t) {
            self.hover_off_at = None;
            self.director.set_hover(false, now);
        }
        if self.grumpy_at.is_some_and(|t| now >= t) {
            self.grumpy_at = None;
            self.director.user(&self.sheet, "grumpy", now);
            self.director.tick(&self.sheet, now);
        }
        self.update_cursor_timer();
        let name = self.director.current_name(now);
        if name != self.anim_name {
            crate::dlog!("display {:?} subagents={} attention={} → anim {name}", d.mode, d.subagents, d.attention_sessions);
            self.anim_name = name.to_string();
        }
        self.redraw(false);
        self.ring_if_needed(d.mode, now);
        self.set_attention(matches!(d.mode, Mode::Attention(_)));
        self.set_tray_pulse(d.mode == Mode::Attention(Attention::Impatient) && !self.fullscreen && !self.display_off);
        if self.tray_hover_until.is_some_and(|t| now < t) {
            self.update_tray_tip();
        }

        let next = [
            self.tracker.next_change(now),
            self.director.next_change(&self.sheet, now, self.animating()),
            self.grumpy_at,
            self.hover_off_at,
            self.hop.map(Hop::at),
            self.next_stroll,
        ];
        match next.into_iter().flatten().min() {
            Some(at) => unsafe {
                SetTimer(self.hwnd, TIMER_TICK, (at.saturating_sub(now)).clamp(10, u32::MAX as Ms) as u32, None);
            },
            None => unsafe {
                KillTimer(self.hwnd, TIMER_TICK);
            },
        }

        let asleep = matches!(d.mode, Mode::Asleep);
        if asleep && !self.was_asleep {
            trim_working_set();
        }
        self.was_asleep = asleep;
    }

    /// `--art`: reload the sprite file when it changes. A broken file keeps
    /// the old art and shows the error (with its line) in the tooltip.
    fn check_art(&mut self) {
        let Some(w) = self.art.as_mut() else { return };
        let mtime = std::fs::metadata(&w.path).and_then(|m| m.modified()).ok();
        if mtime == w.mtime {
            return;
        }
        w.mtime = mtime;
        match sprite::load(&w.path) {
            Ok(sheet) => {
                crate::dlog!("art reloaded from {}", w.path.display());
                w.error = None;
                self.sheet = sheet;
                self.drawn_look = None; // the same look now means other pixels
                if self.demo {
                    let now = self.now();
                    self.director.start_demo(&self.sheet, now);
                }
            }
            Err(e) => {
                crate::dlog!("art reload failed: {e}");
                w.error = Some(e);
            }
        }
        self.refresh();
    }

    fn set_visible(&mut self, v: bool) {
        self.visible = v;
        self.update_presence();
        self.refresh(); // frame timers run only while visible
    }

    // ---- mouse (PLAN.md §6b) ----

    fn cursor(&self) -> (f64, f64) {
        let mut c = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut c) };
        self.fake_cursor.unwrap_or((c.x as f64, c.y as f64))
    }

    fn on_press(&mut self, double: bool) {
        let now = self.now();
        if let Some(t) = &self.tip {
            t.set_active(false);
        }
        if self.peek.is_some() {
            // The tiny peek isn't for playing with: a click just acknowledges.
            self.tracker.dismiss_attention();
            self.refresh();
            return;
        }
        self.cancel_hop(now);
        self.end_stroll(now);
        let (cx, cy) = self.cursor();
        if let Some(f) = self.take_flight() {
            self.director.set_rot(0);
            if matches!(f.sim.state, physics::State::Air | physics::State::Contact) {
                // Caught: grabbed in this very frame, not when the drag starts.
                self.caught = true;
                self.director.set_pose(Some("dragged"), now);
                self.refresh();
            } else {
                self.director.set_pose(None, now); // the skid's lean
                self.settle(f); // already down, just still sliding
            }
        }
        self.double = double;
        self.hand.down(now, cx, cy);
        self.grab = (cx as i32 - self.pos.0, cy as i32 - self.pos.1);
        unsafe {
            SetCapture(self.hwnd);
            SetTimer(self.hwnd, TIMER_HOLD, feel::HOLD_START_MS as u32, None);
        }
    }

    fn on_mouse_move(&mut self) {
        let now = self.now();
        if !self.hovering {
            self.hovering = true;
            self.ensure_tip();
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe { TrackMouseEvent(&mut tme) };
            let came_back = self.hover_off_at.take().is_some();
            if !came_back && !self.hand.pressed() && !self.flying() {
                self.director.set_hover(true, now);
                if !self.director.user_playing(&self.sheet, now) {
                    self.director.user(&self.sheet, "perk", now);
                }
                self.refresh();
            }
        }
        if !self.hand.pressed() {
            return;
        }
        let (cx, cy) = self.cursor();
        match self.hand.moved(now, cx, cy) {
            interaction::Event::DragStart => {
                unsafe {
                    KillTimer(self.hwnd, TIMER_HOLD);
                    SetTimer(self.hwnd, TIMER_DRAG, feel::DRAG_TICK_MS, None);
                }
                // Carried from here on, whatever held it up before. A
                // flight means it lost its footing while being held.
                self.leave_perch();
                if matches!(self.set_motion(Motion::Standing), Motion::Flying(_)) {
                    self.director.set_rot(0);
                }
                self.director.set_flip(false);
                self.director.set_hover(false, now);
                self.swing = Swing::default();
                self.last_drag_tick = now;
                self.director.set_pose(Some("dragged"), now);
                self.pos = (cx as i32 - self.grab.0, cy as i32 - self.grab.1);
                self.refresh(); // dangling pose right away, drawn at the new spot
            }
            interaction::Event::Shake => {
                crate::dlog!("shaken");
                self.director.set_pose(Some(self.drag_pose()), now);
                self.refresh(); // seeing stars right away
            }
            _ => {}
        }
        if self.hand.dragging() {
            self.move_to(cx as i32 - self.grab.0, cy as i32 - self.grab.1);
        }
    }

    /// The dangling pose for the current lean; seeing stars once shaken.
    fn drag_pose(&self) -> &'static str {
        match (self.swing.level(self.scale()), self.hand.shaken()) {
            (-2, false) => "swing_l2",
            (-1, false) => "swing_l1",
            (1, false) => "swing_r1",
            (2, false) => "swing_r2",
            (_, false) => "dragged",
            (-2, true) => "swing_l2_dizzy",
            (-1, true) => "swing_l1_dizzy",
            (1, true) => "swing_r1_dizzy",
            (2, true) => "swing_r2_dizzy",
            (_, true) => "dragged_dizzy",
        }
    }

    /// Dragging: the body swings with the cursor speed and settles after it stops.
    fn on_drag_tick(&mut self) {
        let now = self.now();
        let dt = now.saturating_sub(self.last_drag_tick) as f64;
        self.last_drag_tick = now;
        let (vx, _) = self.hand.velocity(now);
        self.swing.update(vx, dt);
        let pose = self.drag_pose();
        if self.director.pose() != Some(pose) {
            self.director.set_pose(Some(pose), now);
            self.refresh();
        }
    }

    fn on_hold_timer(&mut self) {
        unsafe { KillTimer(self.hwnd, TIMER_HOLD) };
        let now = self.now();
        // USER timers fire up to a clock tick early: wait out the rest.
        if let Some(left) = self.hand.hold_due_in(now)
            && left > 0
        {
            unsafe { SetTimer(self.hwnd, TIMER_HOLD, left as u32, None) };
            return;
        }
        if self.hand.tick(now) == interaction::Event::HoldStart && self.hang().is_none() && !self.caught {
            self.director.set_pose(Some("squish"), now);
            self.refresh();
        }
    }

    fn on_release(&mut self) {
        let now = self.now();
        if let Some(t) = &self.tip {
            t.set_active(true);
        }
        let (cx, cy) = self.cursor();
        let Some(rel) = self.hand.up(now, cx, cy) else { return };
        if let (Release::Drop { vx, vy, .. }, Some((moves, ms))) = (&rel, self.hand.last_drag.take()) {
            // How often this mouse reports: what `THROW_SAMPLE_MS` is tuned against.
            crate::dlog!("drag ended: {moves} mouse moves in {ms} ms, thrown at ({:.0}, {:.0}) DIP/s", vx / self.hand.scale, vy / self.hand.scale);
        }
        unsafe {
            KillTimer(self.hwnd, TIMER_HOLD);
            KillTimer(self.hwnd, TIMER_DRAG);
            ReleaseCapture();
        }
        let double = std::mem::take(&mut self.double);
        if std::mem::take(&mut self.caught) && !matches!(rel, Release::Drop { .. }) {
            self.drop(0.0, 0.0, false);
            self.refresh();
            return;
        }
        match rel {
            // A long press doesn't squish on the edge, so it's just a slow click.
            Release::Click(_) | Release::HoldEnd if self.hang().is_some() => self.climb_back(),
            Release::Click(c) => {
                // Petting also acknowledges whatever Clawd is calling for
                // (a denied permission sends no event, so this clears the "!").
                self.tracker.dismiss_attention();
                self.director.user(&self.sheet, interaction::click_anim(c, double), now);
                if c == Click::Dizzy {
                    let len = self.sheet.anim("dizzy").map_or(0, |a| a.duration_ms());
                    self.grumpy_at = Some(now + len);
                }
            }
            Release::HoldEnd => {
                self.director.set_pose(None, now);
                self.director.user(&self.sheet, "spring", now);
            }
            Release::Drop { vx, vy, shaken } => self.drop(vx, vy, shaken),
        }
        self.refresh();
    }

    /// The world Clawd moves in: the work area of the monitor under it, with
    /// its bottom edge (the taskbar) as the floor and, unless perching is
    /// off, the title bars that can be seen. `exclude`: a window not to land
    /// on (the one it just fell off). Computed lazily, only when taking off.
    fn world(&self, exclude: Option<perch::WinId>) -> World {
        let (fx, fy) = self.feet();
        // On a right-hand edge the feet are on the first pixel past it,
        // which may belong to the next monitor.
        let x = fx as i32 - i32::from(self.hang() == Some(false));
        let m = monitor::at_point(x, fy as i32 - 1);
        let w = m.work;
        let scale = m.dpi as f64 / 96.0;
        let mut floors = vec![Floor { x0: w.l as f64, x1: w.r as f64, y: w.b as f64, owner: 0 }];
        if self.cfg.behaviours.perch {
            let fit = Fit { scale, ..self.fit(&self.cfg.perch_deny_processes) };
            floors.extend(perch::surfaces(&windows::list(self.hwnd), &Screen { full: m.full, work: m.work }, &fit, exclude));
        }
        crate::dlog!("world: {} floors on {}", floors.len(), m.name);
        World {
            floors,
            left: w.l as f64,
            right: w.r as f64,
            top: w.t as f64,
            half_w: (16 * self.px) as f64,
            height: (22 * self.px) as f64,
            scale,
        }
    }

    fn drop(&mut self, vx: f64, vy: f64, shaken: bool) {
        let now = self.now();
        let world = self.world(None);
        let (fx, fy) = self.feet();
        let thrown = vx.hypot(vy) >= feel::THROW_MIN_DIP_S * world.scale;
        crate::dlog!("drop at {fx},{fy} v=({vx:.0},{vy:.0}) thrown={thrown} shaken={shaken}");
        if !thrown && let Some(left) = physics::edge_hang(fx, &world) {
            // Grab the screen edge and peek, half off-screen.
            self.set_motion(Motion::Hanging { left });
            self.director.set_flip(!left);
            self.director.set_pose(Some("hang"), now);
            let x = if left { world.left } else { world.right };
            self.place_feet(x, fy);
            return;
        }
        let (vx, vy) = if thrown { (vx, vy) } else { (0.0, 0.0) };
        self.launch(Sim::launch(fx, fy, vx, vy), world, shaken);
    }

    fn launch(&mut self, sim: Sim, world: World, shaken: bool) {
        self.launch_as(sim, world, shaken, false);
    }

    fn launch_as(&mut self, sim: Sim, world: World, shaken: bool, whoa: bool) {
        let now = self.now();
        let stepper = Stepper::new(&sim);
        let f = Flight { sim, world, last: now, stepper, rot: 0, next_turn: now, shaken, whoa, knock: None, peak: 0.0, landed_at: None, skid: 0 };
        self.director.set_pose(f.pose(now), now);
        self.director.set_hover(false, now);
        self.set_motion(Motion::Flying(f));
        self.apply_z_order();
        unsafe { SetTimer(self.hwnd, TIMER_PHYS, feel::PHYS_TICK_MS, None) };
    }

    /// Came to rest: on a window's title bar that's a perch.
    fn settle(&mut self, f: Flight) {
        unsafe { KillTimer(self.hwnd, TIMER_PHYS) };
        self.director.set_rot(0);
        match f.world.floor_at(f.sim.x, f.sim.y).map(|fl| fl.owner) {
            Some(owner) if owner != 0 => self.perch_on(owner as HWND),
            // A perch isn't saved: after a restart Clawd is back on the taskbar.
            _ => {
                self.remember_position();
                self.apply_z_order();
            }
        }
    }

    /// Clicked while hanging on an edge: hop back onto the screen.
    fn climb_back(&mut self) {
        let Some(left) = self.hang() else { return };
        let world = self.world(None); // while still hanging: that finds the right monitor
        self.set_motion(Motion::Standing);
        let s = world.scale;
        let dir = if left { 1.0 } else { -1.0 };
        let x = if left { world.left + world.half_w } else { world.right - world.half_w };
        let (_, fy) = self.feet();
        self.director.set_flip(false);
        self.launch(Sim::launch(x, fy, dir * feel::CLIMB_VX_DIP_S * s, feel::CLIMB_VY_DIP_S * s), world, false);
    }

    fn on_phys_tick(&mut self) {
        let now = self.now();
        let Motion::Flying(f) = &mut self.motion else {
            unsafe { KillTimer(self.hwnd, TIMER_PHYS) };
            return;
        };
        let dt = (now.saturating_sub(f.last)).min(50) as f64 / 1000.0;
        f.last = now;
        let (events, (x, y)) = f.stepper.advance(&mut f.sim, &f.world, dt);
        let s = f.world.scale;
        // Tumble in quarter turns when thrown fast, in the direction of travel.
        if f.sim.state == physics::State::Air {
            if f.sim.speed() > feel::TUMBLE_DIP_S * s && f.sim.vx.abs() > feel::TUMBLE_VX_DIP_S * s {
                if now >= f.next_turn {
                    f.rot = (f.rot + if f.sim.vx >= 0.0 { 1 } else { 3 }) % 4;
                    f.next_turn = now + feel::TUMBLE_STEP_MS;
                }
            } else {
                f.rot = 0;
            }
        }
        let mut rest = false;
        let mut landing = None;
        for e in events {
            match e {
                physics::Event::Wall => f.knock = Some(("hit_wall", now + feel::WALL_HIT_MS)),
                physics::Event::Ceiling => f.knock = Some(("hit_ceiling", now + feel::WALL_HIT_MS)),
                physics::Event::Bounce(v) => {
                    crate::dlog!("bounce at {:.0} DIP/s", v / s);
                    f.peak = f.peak.max(v);
                    f.whoa = false;
                    f.rot = 0;
                    f.knock = None;
                }
                physics::Event::Landed(v) => {
                    f.peak = f.peak.max(v);
                    f.whoa = false;
                    f.rot = 0;
                    f.knock = None;
                    f.landed_at = Some(now);
                    landing = Some(physics::landing_anim(v, f.peak, f.shaken, s));
                    crate::dlog!("landed at {:.0} DIP/s, worst hit {:.0} → {:?}", v / s, f.peak / s, landing);
                }
                physics::Event::Rest => rest = true,
                _ => {}
            }
        }
        // The body follows the motion every tick.
        let pose = f.pose(now);
        if f.sim.state == physics::State::Slide && pose.is_some() {
            f.skid = if f.sim.vx >= 0.0 { 1 } else { -1 };
        }
        let rot = if f.sim.state == physics::State::Air { f.rot } else { 0 };
        if landing.is_some() {
            self.director.set_pose(None, now);
        }
        if let Some(name) = landing {
            self.director.user(&self.sheet, name, now);
        }
        if pose.is_some() || landing.is_none() {
            self.director.set_pose(pose, now);
        }
        self.director.set_rot(rot);
        self.place_feet(x, y);
        if rest && let Some(f) = self.take_flight() {
            let skid = f.skid;
            self.settle(f);
            if skid != 0 {
                self.director.user(&self.sheet, if skid > 0 { "skid_stop_r" } else { "skid_stop_l" }, now);
            }
        }
        // Most ticks only move Clawd and step the pose, which `place_feet`
        // has drawn. The rest of `refresh` (the tracker, the tick timer,
        // strolls and hops, which wait while flying) is only needed when
        // something else changed: a landing, coming to rest, a new pose.
        // `TIMER_TICK` keeps running meanwhile and refreshes on its own.
        if landing.is_some() || rest || self.director.current_name(now) != self.anim_name {
            self.refresh();
        }
    }

    fn on_mouse_leave(&mut self) {
        self.hovering = false;
        self.hover_off_at = Some(self.now() + HOVER_GRACE_MS);
        self.refresh();
    }

    fn on_wheel(&mut self) {
        let now = self.now();
        if !self.director.user_playing(&self.sheet, now) {
            self.director.user(&self.sheet, "tickle", now);
            self.refresh();
        }
    }

    /// Poll the cursor at 15 Hz only while it matters (awake, visible,
    /// "eyes follow cursor" on). No global mouse hook; positions are never
    /// stored or logged.
    fn update_cursor_timer(&mut self) {
        let want = self.animating()
            && self.cfg.behaviours.eyes_follow_cursor
            && !self.demo
            && !matches!(self.director.mode(), Mode::Asleep);
        if want != self.cursor_timer {
            self.cursor_timer = want;
            unsafe {
                if want {
                    self.cursor_wait = feel::CURSOR_POLL_MS;
                    self.cursor_seen = ((f64::NAN, f64::NAN), self.now());
                    SetTimer(self.hwnd, TIMER_CURSOR, self.cursor_wait, None);
                } else {
                    KillTimer(self.hwnd, TIMER_CURSOR);
                }
            }
            if !want {
                self.director.set_gaze(None);
            }
        }
    }

    fn on_cursor_timer(&mut self) {
        let now = self.now();
        let (cx, cy) = self.cursor();
        let ex = (self.pos.0 + crate::compose::FEET.0 * self.px) as f64;
        let ey = (self.pos.1 + EYES_Y * self.px) as f64;
        let scale = self.scale();
        let (dx, dy) = (cx - ex, cy - ey);
        let g = interaction::gaze_toward(dx, dy, feel::EYES_RADIUS_DIP * scale);
        if g != self.director.gaze() {
            self.director.set_gaze(g);
            self.redraw(false);
        }
        // Relative to Clawd: a walk or a ride moves the eyes' target too.
        if (dx, dy) != self.cursor_seen.0 {
            self.cursor_seen = ((dx, dy), now);
        }
        // A far-away cursor can't reach the eyes' range before the next
        // look, and a resting one is looked at less often.
        let wait = interaction::cursor_poll_ms(dx.hypot(dy) / scale, now - self.cursor_seen.1);
        if wait.abs_diff(self.cursor_wait) >= feel::CURSOR_REARM_STEP_MS {
            self.cursor_wait = wait;
            unsafe { SetTimer(self.hwnd, TIMER_CURSOR, wait, None) };
        }
    }

    // ---- menu ----

    /// Build the context menu for `show_menu`; also says whether Clawd is
    /// watching it (then `menu_closed` lets go of that pose).
    fn build_menu(&mut self) -> (HMENU, bool) {
        // Clawd watches the menu while it's open (the cursor timer keeps
        // running inside the menu's modal loop, so the eyes follow along).
        let watching = !self.flying() && self.hang().is_none() && !self.hand.pressed();
        let now = self.now();
        if watching {
            self.director.set_pose(Some("menu_look"), now);
        }
        if self.drop_dead_sessions() || watching {
            self.refresh();
        }
        // Resolved now, so a window that can't be found is greyed out.
        let targets = self.tracker.goto_targets(now);
        // A snapshot of every process (~15 ms) only when there's a host to
        // look for.
        let finder = targets.iter().any(|t| t.host.is_some()).then(Finder::new);
        self.goto_jumps = targets.iter().map(|t| finder.as_ref().and_then(|f| Jump::find(f, t))).collect();
        let menu = unsafe {
            let menu = CreatePopupMenu();
            let item = |m: HMENU, flags: u32, id: usize, text: &str| {
                let t = wide(text);
                AppendMenuW(m, flags, id, t.as_ptr());
            };
            let checked = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };
            let enabled = |b: bool| if b { MF_ENABLED } else { MF_GRAYED };
            for (i, (t, jump)) in targets.iter().zip(&self.goto_jumps).enumerate() {
                item(menu, MF_STRING | enabled(jump.is_some()), CMD_GOTO_BASE + i, &t.menu_text(jump.is_some()));
            }
            if !targets.is_empty() {
                item(menu, MF_SEPARATOR, 0, "");
            }
            item(menu, MF_STRING, CMD_SHOW, if self.visible { "Hide" } else { "Show" });
            let size = CreatePopupMenu();
            for s in MIN_SIZE..=MAX_SIZE {
                item(size, MF_STRING | checked(self.cfg.size == s), CMD_SIZE_BASE + s as usize, &format!("{s}×"));
            }
            item(menu, MF_POPUP, size as usize, "Size");
            let place = CreatePopupMenu();
            let p = self.cfg.placement;
            item(place, MF_STRING | checked(p == Placement::AlwaysOnTop), CMD_PLACE_TOP, "Always on top");
            item(place, MF_STRING | checked(p == Placement::AttentionOnly), CMD_PLACE_ATTENTION, "On top only when Claude needs me");
            item(menu, MF_POPUP, place as usize, "Placement");
            item(menu, MF_STRING, CMD_HOME, "Back to taskbar spot");
            item(menu, MF_SEPARATOR, 0, "");
            let b = &self.cfg.behaviours;
            let behaviours = CreatePopupMenu();
            item(behaviours, MF_STRING | checked(b.perch), CMD_PERCH, "Perch on windows");
            item(behaviours, MF_STRING | checked(b.follow_active_window), CMD_FOLLOW, "Follow the active window");
            item(behaviours, MF_STRING | checked(b.eyes_follow_cursor), CMD_EYES, "Eyes follow cursor");
            item(behaviours, MF_STRING | checked(b.walk), CMD_WALK, "Walk around");
            item(behaviours, MF_STRING | checked(b.hide_fullscreen), CMD_HIDE_FULLSCREEN, "Hide during fullscreen apps");
            item(behaviours, MF_STRING | checked(b.attention_in_fullscreen), CMD_ATTENTION_FULLSCREEN, "Show attention during fullscreen");
            item(behaviours, MF_STRING | checked(b.time_of_day), CMD_TIME_OF_DAY, "Time-of-day moods");
            item(behaviours, MF_STRING | checked(b.attention_sound), CMD_SOUND, "Sound when Claude needs me");
            item(behaviours, MF_STRING | checked(b.follow_claude_app), CMD_FOLLOW_APP, "Hide and quit with the Claude app");
            item(menu, MF_POPUP, behaviours as usize, "Behaviours");
            item(menu, MF_STRING | checked(self.cfg.do_not_disturb), CMD_DO_NOT_DISTURB, "Do not disturb");
            item(menu, MF_SEPARATOR, 0, "");
            let hooks = install::hook_status_cached(&mut self.hook_status);
            item(menu, MF_STRING | checked(self.cfg.launch_with_claude), CMD_LAUNCH_WITH_CLAUDE, "Launch with Claude");
            item(menu, MF_STRING | checked(autostart::enabled()), CMD_START_WITH_WINDOWS, "Start with Windows");
            item(menu, MF_STRING | enabled(hooks.can_install), CMD_INSTALL_HOOKS, "Install Claude hooks");
            item(menu, MF_STRING | enabled(hooks.can_uninstall), CMD_UNINSTALL_HOOKS, "Uninstall Claude hooks");
            let (text, free) = match self.updater.state {
                Updating::No => ("Check for updates…", true),
                Updating::Checking | Updating::Asking => ("Checking for updates…", false),
                Updating::Downloading => ("Downloading update…", false),
            };
            item(menu, MF_STRING | enabled(free), CMD_CHECK_UPDATES, text);
            item(menu, MF_SEPARATOR, 0, "");
            item(menu, MF_STRING, CMD_QUIT, "Quit");
            menu
        };
        (menu, watching)
    }

    /// The menu closed with `cmd` chosen (0 = nothing).
    fn menu_closed(&mut self, watching: bool, cmd: usize) {
        if watching && self.director.pose() == Some("menu_look") {
            let now = self.now();
            self.director.set_pose(None, now);
            self.refresh();
        }
        self.on_command(cmd);
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            CMD_SHOW => self.set_visible(!self.visible),
            c if (CMD_SIZE_BASE + MIN_SIZE as usize..=CMD_SIZE_BASE + MAX_SIZE as usize).contains(&c) => {
                self.stop_motion();
                self.cfg.size = (c - CMD_SIZE_BASE) as u32;
                // Keep the feet where they are: re-anchor from the current spot.
                if self.perch().is_none() {
                    self.remember_position();
                } else {
                    self.schedule_save();
                }
                self.surface = None;
                self.apply_anchor();
            }
            CMD_PLACE_TOP | CMD_PLACE_ATTENTION => {
                self.cfg.placement = if cmd == CMD_PLACE_TOP { Placement::AlwaysOnTop } else { Placement::AttentionOnly };
                self.apply_z_order();
                self.schedule_save();
            }
            CMD_HOME => {
                self.leave_perch();
                self.cfg.position = placement::Position::default();
                self.apply_anchor();
                self.schedule_save();
            }
            CMD_PERCH => {
                self.cfg.behaviours.perch = !self.cfg.behaviours.perch;
                if !self.cfg.behaviours.perch {
                    self.cancel_hop(self.now());
                    self.fall_off("perching turned off", None);
                }
                self.schedule_save();
            }
            CMD_FOLLOW => {
                self.cfg.behaviours.follow_active_window = !self.cfg.behaviours.follow_active_window;
                self.plan_hop(self.fg_hwnd);
                self.schedule_save();
                self.refresh();
            }
            CMD_EYES => {
                self.cfg.behaviours.eyes_follow_cursor = !self.cfg.behaviours.eyes_follow_cursor;
                self.schedule_save();
                self.refresh();
            }
            CMD_WALK => {
                self.cfg.behaviours.walk = !self.cfg.behaviours.walk;
                self.schedule_save();
                self.refresh();
            }
            CMD_HIDE_FULLSCREEN => {
                self.cfg.behaviours.hide_fullscreen = !self.cfg.behaviours.hide_fullscreen;
                if self.cfg.behaviours.hide_fullscreen {
                    self.on_foreground(self.fg_hwnd);
                } else {
                    self.fg_location = None;
                }
                self.update_presence();
                self.schedule_save();
                self.refresh();
            }
            CMD_ATTENTION_FULLSCREEN => {
                self.cfg.behaviours.attention_in_fullscreen = !self.cfg.behaviours.attention_in_fullscreen;
                self.update_presence();
                self.schedule_save();
                self.refresh();
            }
            CMD_TIME_OF_DAY => {
                self.cfg.behaviours.time_of_day = !self.cfg.behaviours.time_of_day;
                self.apply_mood();
                self.schedule_save();
                self.refresh();
            }
            CMD_FOLLOW_APP => {
                self.cfg.behaviours.follow_claude_app = !self.cfg.behaviours.follow_claude_app;
                if !self.cfg.behaviours.follow_claude_app {
                    self.claude_app = None; // found again at the next prompt if turned back on
                }
                self.update_presence();
                self.schedule_save();
                self.refresh();
            }
            CMD_SOUND => {
                self.cfg.behaviours.attention_sound = !self.cfg.behaviours.attention_sound;
                self.schedule_save();
            }
            CMD_DO_NOT_DISTURB => {
                self.cfg.do_not_disturb = !self.cfg.do_not_disturb;
                self.schedule_save();
                self.refresh();
            }
            CMD_LAUNCH_WITH_CLAUDE => {
                self.cfg.launch_with_claude = !self.cfg.launch_with_claude;
                self.schedule_save();
            }
            // Message boxes, going to another window and quitting all run
            // once this handler has returned (`After`).
            CMD_START_WITH_WINDOWS => {
                let done = if autostart::enabled() {
                    autostart::disable()
                } else {
                    install::ensure_installed().and_then(|(exe, _)| autostart::enable(&exe))
                };
                if let Err(text) = done {
                    self.after.push(After::Message { text, error: true });
                }
            }
            CMD_INSTALL_HOOKS => self.after.push(After::result(install::install_hooks())),
            CMD_UNINSTALL_HOOKS => self.after.push(After::result(install::uninstall_hooks())),
            CMD_CHECK_UPDATES => self.start_update(None),
            CMD_QUIT => self.after.push(After::Quit),
            c if (CMD_GOTO_BASE..CMD_GOTO_BASE + goto::MAX_TARGETS).contains(&c) => {
                // The window may have closed while the menu was open.
                if let Some(jump) = self.goto_jumps.get_mut(c - CMD_GOTO_BASE).and_then(Option::take)
                    && windows::exists(jump.window)
                {
                    self.after.push(After::Go(jump));
                }
            }
            _ => {}
        }
        self.goto_jumps.clear();
    }

    // ---- "Check for updates" (PLAN.md §7, "Updating") ----

    /// The user's click (or `__update`): ask GitHub on a worker thread.
    fn start_update(&mut self, test: Option<UpdateTest>) {
        if self.updater.state != Updating::No {
            crate::dlog!("update: already {:?}", self.updater.state);
            return;
        }
        self.updater.test = test;
        let (source, current) = match update::setup() {
            Ok(setup) => setup,
            Err(text) => return self.update_finished(&text, true),
        };
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => return self.update_finished(&format!("Could not find this program's own path: {e}."), true),
        };
        crate::dlog!("update: checking {} as {current}", source.api);
        self.updater.current = current;
        self.updater.exe = Some(exe);
        self.updater.state = Updating::Checking;
        self.spawn_update_worker(move || UpdateNews::Checked(update::check(&http::get, &source, current)));
    }

    /// Run `work` on its own thread, then wake the window with its news.
    /// Nothing on that thread may panic (`panic = "abort"`).
    fn spawn_update_worker(&mut self, work: impl FnOnce() -> UpdateNews + Send + 'static) {
        let news = self.updater.news.clone();
        let target = self.hwnd as usize; // HWND isn't Send; it's just a handle
        let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
            let result = work();
            *news.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            // Fails only when the window has gone (Clawd quit meanwhile):
            // nobody to tell, and nothing was written before `place_bytes`.
            unsafe { PostMessageW(target as HWND, WM_APP_UPDATE, 0, 0) };
        });
        if let Err(e) = spawned {
            self.update_finished(&format!("Could not start the update: {e}."), true);
        }
    }

    fn on_update_news(&mut self) {
        let Some(news) = self.updater.news.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        let current = self.updater.current;
        match news {
            UpdateNews::Checked(Err(text)) | UpdateNews::Placed(Err(text)) => self.update_finished(&text, true),
            UpdateNews::Checked(Ok(Verdict::UpToDate)) => {
                crate::dlog!("update: up to date ({current})");
                self.update_finished(&update::up_to_date_text(current), false);
            }
            UpdateNews::Checked(Ok(Verdict::Ahead(latest))) => {
                crate::dlog!("update: ahead of the latest release ({current} > {latest})");
                self.update_finished(&update::ahead_text(current, latest), false);
            }
            UpdateNews::Checked(Ok(Verdict::Newer(release))) => {
                crate::dlog!("update: {} is available (have {current})", release.version);
                match self.updater.test {
                    Some(UpdateTest::CheckOnly) => self.update_finished("", false),
                    Some(UpdateTest::Install) => self.update_answered(true, release),
                    None => {
                        self.updater.state = Updating::Asking;
                        self.after.push(After::Ask { text: update::ask_text(current, release.version), release });
                    }
                }
            }
            UpdateNews::Placed(Ok(new)) => self.restart_updated(new),
        }
    }

    /// The check or download ended without a restart: say how (no box for
    /// the test command).
    fn update_finished(&mut self, text: &str, error: bool) {
        if error {
            crate::dlog!("update: failed: {text}");
        }
        self.updater.state = Updating::No;
        if self.updater.test.is_none() {
            self.after.push(After::Message { text: text.to_string(), error });
        }
        // WinHTTP brought in several DLLs, and nothing needs them now.
        trim_working_set();
    }

    /// The answer to "Update now?". Yes: download, check, swap on a worker.
    fn update_answered(&mut self, yes: bool, release: update::Release) {
        if !yes {
            crate::dlog!("update: declined");
            self.updater.state = Updating::No;
            trim_working_set();
            return;
        }
        let Some(exe) = self.updater.exe.clone() else { return self.update_finished("Could not find this program's own path.", true) };
        crate::dlog!("update: downloading {}", release.url);
        self.updater.state = Updating::Downloading;
        self.spawn_update_worker(move || {
            let placed = update::download(&http::get, &release).and_then(|bytes| {
                install::place_bytes(&bytes, &exe).map_err(|e| format!("Could not replace {}: {e}. Clawd was left as it was.", exe.display()))
            });
            UpdateNews::Placed(placed.map(|()| release.version))
        });
    }

    /// The new exe is in place: close, and have `run` start it.
    fn restart_updated(&mut self, new: Version) {
        let Some(exe) = self.updater.exe.clone() else { return };
        crate::dlog!("update: {new} is in place, restarting");
        let mut args = vec!["--updated-from".to_string(), self.updater.current.to_string(), "--wait-pid".into(), std::process::id().to_string()];
        if crate::debug::enabled() {
            args.push("--debug-log".into());
        }
        // Not in the config, so it has to be passed on.
        if !self.visible {
            args.push("--hidden".into());
        }
        if self.updater.test.is_some() {
            args.push("--quiet".into());
        }
        self.updater.restart = Some((exe, args));
        self.after.push(After::Quit);
    }

    /// `TIMER_SWEEP`: the old exe once more, a few times at most.
    fn sweep_again(&mut self) {
        let left: usize = self.updater.sweep.iter().map(|exe| install::sweep_leftovers(exe)).sum();
        crate::dlog!("update: {left} leftover(s) still there");
        self.updater.sweeps_left = self.updater.sweeps_left.saturating_sub(1);
        if left > 0 && self.updater.sweeps_left > 0 {
            unsafe { SetTimer(self.hwnd, TIMER_SWEEP, SWEEP_RETRY_MS, None) };
        } else {
            self.updater.sweep.clear();
        }
    }

    fn on_dpi_changed(&mut self, dpi: u32) {
        if self.hand.dragging() {
            // Crossing onto a monitor with another DPI mid-drag: rescale
            // the grab point and continue the drag from here.
            let old = self.px;
            let new = window_px(self.cfg.size, dpi);
            self.grab = (self.grab.0 * new / old, self.grab.1 * new / old);
            let mut c = POINT { x: 0, y: 0 };
            unsafe { GetCursorPos(&mut c) };
            self.pos = (c.x - self.grab.0, c.y - self.grab.1);
            self.set_dpi(dpi);
            self.redraw(true);
        } else if self.peek.is_some() || matches!(self.motion, Motion::Flying(_) | Motion::Hanging { .. }) {
            self.set_dpi(dpi);
            self.redraw(true);
        } else if self.perch().is_some() {
            // Carried onto another monitor by its window.
            self.set_dpi(dpi);
            self.on_perch_moved();
            self.redraw(true);
        } else {
            self.apply_anchor();
        }
    }

    fn on_destroy(&mut self) {
        self.save_now();
        if self.power_notify != 0 {
            unsafe { UnregisterPowerSettingNotification(self.power_notify) };
            self.power_notify = 0;
        }
        self.tray = None; // removes the icon
        unsafe { PostQuitMessage(0) };
    }

    /// One window message. `Some` is the answer; `None` leaves it to
    /// `DefWindowProcW`. Also gets the messages `enter` kept for later.
    fn on_message(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.park != (0, 0) && matches!(msg, WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP) {
            self.fake_cursor = Some(((lp as i16) as f64, ((lp >> 16) as i16) as f64));
        }
        if msg == WM_TIMER && wp < self.stats.timers.len() {
            self.stats.timers[wp] += 1;
        }
        match msg {
            WM_MOUSEACTIVATE => return Some(MA_NOACTIVATE as LRESULT),
            WM_LBUTTONDOWN => self.on_press(false),
            WM_LBUTTONDBLCLK => self.on_press(true),
            WM_MOUSEMOVE => self.on_mouse_move(),
            WM_LBUTTONUP => self.on_release(),
            WM_MOUSELEAVE => self.on_mouse_leave(),
            WM_NOTIFY => {
                let info = lp as *mut NMTTDISPINFOW;
                if !info.is_null() && unsafe { (*info).hdr.code } == TTN_GETDISPINFOW {
                    let text = self.tip_body();
                    unsafe { tip::answer(info, &text, &mut self.tip_text) };
                    return Some(0);
                }
            }
            WM_MOUSEWHEEL => {
                self.on_wheel();
                return Some(0);
            }
            WM_CAPTURECHANGED => {
                if self.hand.pressed() && lp as HWND != hwnd && self.park == (0, 0) {
                    self.on_release();
                }
            }
            WM_RBUTTONUP => self.after.push(After::Menu),
            WM_APP_TRAY => match lp as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU => self.after.push(After::Menu),
                WM_LBUTTONUP if !self.visible => self.set_visible(true),
                WM_LBUTTONUP => self.go_to_waiting(),
                WM_MOUSEMOVE => self.on_tray_hover(),
                _ => {}
            },
            WM_APP_DISPLAY => self.set_display_off(wp == 0),
            WM_APP_EVENT => self.on_ipc(),
            WM_APP_UPDATE => self.on_update_news(),
            WM_APP_EXITED => self.on_exited(),
            WM_APP_RECHECK => self.recheck_windows(),
            WM_TIMER if wp == TIMER_SAVE => self.save_now(),
            WM_TIMER if wp == TIMER_TICK => {
                unsafe { KillTimer(hwnd, wp) };
                self.refresh();
            }
            WM_TIMER if wp == TIMER_ART => self.check_art(),
            WM_TIMER if wp == TIMER_HOLD => self.on_hold_timer(),
            WM_TIMER if wp == TIMER_PHYS => self.on_phys_tick(),
            WM_TIMER if wp == TIMER_DRAG => self.on_drag_tick(),
            WM_TIMER if wp == TIMER_CURSOR => self.on_cursor_timer(),
            WM_TIMER if wp == TIMER_RIDE => self.on_ride_tick(),
            WM_TIMER if wp == TIMER_FADE => self.on_fade_tick(),
            WM_TIMER if wp == TIMER_WALK => self.on_walk_tick(),
            WM_TIMER if wp == TIMER_PULSE => self.on_pulse_timer(),
            WM_TIMER if wp == TIMER_SWEEP => {
                unsafe { KillTimer(hwnd, wp) };
                self.sweep_again();
            }
            WM_TIMER if wp == TIMER_APP => {
                unsafe { KillTimer(hwnd, wp) };
                self.check_claude_app();
            }
            WM_DPICHANGED => {
                self.on_dpi_changed((wp & 0xFFFF) as u32);
                return Some(0);
            }
            WM_DISPLAYCHANGE => self.apply_anchor(),
            WM_SETTINGCHANGE if wp == SPI_SETWORKAREA as usize => self.apply_anchor(),
            WM_ENDSESSION if wp != 0 => self.save_now(),
            WM_DESTROY => self.on_destroy(),
            m if m == self.taskbar_created && m != 0 => {
                if let Some(t) = &self.tray {
                    t.add();
                }
            }
            _ => {}
        }
        None
    }
}

/// The context menu, start to finish. The state is free while it's open:
/// its modal loop keeps delivering timers and Claude events, and they are
/// handled as usual (Clawd keeps moving behind an open menu).
fn show_menu(hwnd: HWND) {
    if MENU_OPEN.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Some((menu, watching)) = enter(|a| a.build_menu()) {
        let mut c = POINT { x: 0, y: 0 };
        let cmd = unsafe {
            GetCursorPos(&mut c);
            // Needed so the menu closes when clicking elsewhere (documented TrackPopupMenu quirk).
            let previous = windows::foreground();
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, c.x, c.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu); // also destroys the submenus
            // Clawd never keeps the focus: hand it back, unless the click
            // that closed the menu already gave it to another window, or
            // the choice was to go to one.
            let going = (cmd as usize) >= CMD_GOTO_BASE;
            if !going && windows::foreground() == hwnd && !previous.is_null() && previous != hwnd {
                SetForegroundWindow(previous);
            }
            cmd as usize
        };
        MENU_OPEN.store(false, Ordering::Relaxed);
        enter(|a| a.menu_closed(watching, cmd));
        // A menu pulls in a lot of DLLs that nothing touches afterwards.
        trim_working_set();
    }
    MENU_OPEN.store(false, Ordering::Relaxed);
}

/// WinEvent callback (out-of-context, so it runs on this thread from the
/// message loop). Only whole-window events count; a thread's child objects
/// (buttons, carets…) report here too.
unsafe extern "system" fn on_win_event(hook: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    let whole_window = object == OBJID_WINDOW && child == CHILDID_SELF as i32 && !hwnd.is_null();
    let handled = enter(|a| {
        a.stats.win_events += 1;
        if whole_window {
            a.on_win_event(hook, event, hwnd);
        }
    });
    if handled.is_none() && whole_window {
        handle_later(WM_APP_RECHECK, 0, 0);
    }
}

/// What a `WM_POWERBROADCAST` says about the display: `Some(true)` when it
/// is on (or dimmed), `Some(false)` when off, `None` for anything else.
///
/// # Safety
/// `lp` must be the message's lParam, which for `PBT_POWERSETTINGCHANGE`
/// points at a `POWERBROADCAST_SETTING`.
unsafe fn display_on(wp: WPARAM, lp: LPARAM) -> Option<bool> {
    if wp != PBT_POWERSETTINGCHANGE as WPARAM || lp == 0 {
        return None;
    }
    let s = lp as *const POWERBROADCAST_SETTING;
    let display = |g: GUID| {
        let d = GUID_CONSOLE_DISPLAY_STATE;
        (g.data1, g.data2, g.data3, g.data4) == (d.data1, d.data2, d.data3, d.data4)
    };
    unsafe {
        if !display((*s).PowerSetting) || ((*s).DataLength as usize) < size_of::<u32>() {
            return None;
        }
        // 0 off, 1 on, 2 dimmed.
        Some(std::ptr::addr_of!((*s).Data).cast::<u32>().read_unaligned() != 0)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Its lParam is gone once this returns: read it now, and go on as a
    // plain `WM_APP_DISPLAY` that can wait in `LATER` if it has to.
    if msg == WM_POWERBROADCAST {
        if let Some(on) = unsafe { display_on(wp, lp) } {
            let wp = on as WPARAM;
            if enter(|a| a.on_message(hwnd, WM_APP_DISPLAY, wp, 0)).is_none() {
                handle_later(WM_APP_DISPLAY, wp, 0);
            }
        }
        return 1; // TRUE
    }
    match enter(|a| a.on_message(hwnd, msg, wp, lp)) {
        Some(Some(answer)) => answer,
        Some(None) => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        // Before the state exists, or nested inside a call that's using it.
        None => {
            handle_later(msg, wp, lp);
            match msg {
                WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
                WM_DPICHANGED => 0,
                _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
        }
    }
}

/// Pet mode. Returns when the user quits.
pub fn run(opts: Options) {
    let Options { seed, hidden, demo, art, offscreen, updated_from, wait_pid, quiet } = opts;

    // Started by an update: the old pet holds the UDP port (and held the
    // mutex) until its process ends.
    if let Some(pid) = wait_pid {
        let t = Instant::now();
        process::wait_for_exit(pid, 5000);
        crate::dlog!("waited {} ms for pid {pid} to end", t.elapsed().as_millis());
    }
    // Before the mutex: a pet that a hook launched meanwhile may win it, and
    // the housekeeping must happen all the same.
    let updated = updated_from.map(|old| {
        let done = install::refresh_after_update();
        let new = Version::current();
        crate::dlog!(
            "updated from {old} to {new} (hooks updated: {}, leftovers: {}, problem: {:?})",
            done.hooks_updated,
            done.leftovers,
            done.problem
        );
        let mut text = update::updated_text(&old, new, done.hooks_updated);
        if let Some(problem) = &done.problem {
            text = format!("{text}\n\n{problem}");
        }
        (text, done)
    });

    let cfg_path = config::default_path();
    let cfg = cfg_path.as_deref().map(config::load).unwrap_or_default();
    let port = ipc::env_port().unwrap_or(cfg.port);

    // Single instance. A second launch by hand asks the running pet to show
    // itself. Two sessions starting together can each launch one: the loser
    // hands over its SessionStart, so the winner hears about that session.
    let name = wide(&launch::mutex_name());
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        crate::dlog!("already running");
        ipc::send(&seed.unwrap_or_else(|| IpcMsg { v: 1, e: "__show".into(), ..Default::default() }), port);
        if let Some((text, done)) = &updated
            && !quiet
        {
            super::message_box(text, done.problem.is_some());
        }
        return;
    }

    let mut art_error = None;
    let sheet = match &art {
        Some(p) => sprite::load(p).unwrap_or_else(|e| {
            crate::dlog!("--art: {e}; using the embedded art");
            art_error = Some(e);
            sprite::builtin()
        }),
        None => sprite::builtin(),
    };
    let queue = Arc::new(Queue::default());
    let sock = ipc::bind(port);

    unsafe {
        // Clawd has no text input, so it doesn't need the text services
        // framework (msctf.dll and what that pulls in).
        ImmDisableIME(u32::MAX);
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let title = wide("Clawd");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            crate::dlog!("CreateWindowExW failed: {}", std::io::Error::last_os_error());
            return;
        }

        let mut tracker = Tracker::new(0, cfg.sleep_after_minutes);
        tracker.set_impatient_after(cfg.impatient_after_seconds);
        tracker.set_answered_after(cfg.permission_answered_after_seconds);
        let mut director = Director::new(0, cfg.behaviours.animate_while_asleep, Rng::from_clock());
        if demo {
            director.start_demo(&sheet, 0);
        }
        let app = Box::new(App {
            hwnd,
            cfg_synced: cfg.clone(),
            cfg,
            cfg_path,
            canvas: Canvas::new(CANVAS_W, CANVAS_H),
            shown: Canvas::new(CANVAS_W, CANVAS_H),
            drawn_look: None,
            was_asleep: false,
            surface: None,
            px: 0,
            dpi: 96,
            pos: (0, 0),
            shown_pos: (i32::MIN, i32::MIN),
            visible: false,
            hand: Hand::new(1.0),
            grab: (0, 0),
            swing: Swing::default(),
            last_drag_tick: 0,
            motion: Motion::Standing,
            screens: windows::screens(),
            presence: Shown::Hidden,
            fullscreen: false,
            peek: None,
            fg_hooks: None,
            fg_hwnd: std::ptr::null_mut(),
            fg_location: None,
            hop: None,
            last_hop: None,
            next_stroll: None,
            rng: Rng::from_clock(),
            hovering: false,
            hover_off_at: None,
            double: false,
            caught: false,
            grumpy_at: None,
            cursor_timer: false,
            cursor_wait: feel::CURSOR_POLL_MS,
            cursor_seen: ((f64::NAN, f64::NAN), 0),
            tray: None,
            tray_hover_until: None,
            display_off: false,
            power_notify: 0,
            pulsing: false,
            attention: false,
            goto_jumps: Vec::new(),
            hook_status: None,
            after: Vec::new(),
            watches: Vec::new(),
            claude_app: None,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            tracker,
            director,
            anim_name: String::new(),
            demo,
            art: art.map(|path| ArtWatch {
                mtime: std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
                path,
                error: art_error,
            }),
            t0: Instant::now(),
            queue: queue.clone(),
            port_error: None,
            clock_override: None,
            last_mode: Mode::Idle,
            last_chime: None,
            tip: None,
            tip_text: Vec::new(),
            park: if offscreen { PARKED_AT } else { (0, 0) },
            fake_cursor: None,
            fake_fullscreen: offscreen.then_some(false),
            updater: Updater {
                state: Updating::No,
                news: Arc::default(),
                current: Version::current(),
                exe: None,
                test: None,
                restart: None,
                sweep: Vec::new(),
                sweeps_left: 0,
            },
            stats: Stats::default(),
            sheet,
        });
        APP.store(Box::into_raw(app), Ordering::Relaxed);
        let port_error = match sock {
            Ok(sock) => {
                let target = hwnd as usize; // HWND isn't Send; it's just a handle
                ipc::spawn_listener(sock, move |m| {
                    // A wake-up that can't be posted is asked for again by
                    // the next message, or the queue would never be read.
                    if queue.push(m) && PostMessageW(target as HWND, WM_APP_EVENT, 0, 0) == 0 {
                        queue.wake_failed();
                    }
                });
                None
            }
            Err(e) => {
                crate::dlog!("binding 127.0.0.1:{port} failed: {e}");
                Some(format!("Port {port} is busy: Claude events can't reach Clawd. Change \"port\" in config.json, then reinstall the hooks."))
            }
        };
        enter(|a| {
            a.port_error = port_error;
            a.apply_mood();
            a.apply_anchor();
            a.redraw(true);
            // Before showing anything: launched during a game, it must not flash.
            a.fg_hooks = Some(foreground::foreground(Some(on_win_event)));
            // Display off/on. Windows answers at once with the current state;
            // that arrives nested in here and waits in `LATER`.
            a.power_notify = RegisterPowerSettingNotification(hwnd, &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE);
            if a.power_notify == 0 {
                crate::dlog!("RegisterPowerSettingNotification failed: {}", std::io::Error::last_os_error());
            }
            a.on_foreground(windows::foreground());
            a.set_visible(!hidden);
            // Tray icon: a small 16-px Clawd drawn for it (the 2x body doesn't
            // fit a 16-px icon); else the first idle step, cropped.
            let icon = match a.sheet.frame("icon") {
                Some(f) => tray::icon_from_frame(f),
                None => {
                    let mut c = Canvas::new(CANVAS_W, CANVAS_H);
                    if let Some(step) = a.sheet.anim("idle").map(|i| i.steps[0].clone()) {
                        c.draw(&a.sheet, &Look { step, mini: None, crew: 0, badge: 0, flip: false, rot: 0, hat: None });
                    }
                    tray::icon_from_frame(&c.to_frame("icon"))
                }
            };
            let alert = a.sheet.frame("icon_alert").map_or(std::ptr::null_mut(), tray::icon_from_frame);
            a.tray = Some(Tray::new(hwnd, icon, alert));
            if a.art.is_some() {
                SetTimer(hwnd, TIMER_ART, 500, None);
            }
            if let Some(m) = seed {
                a.queue.push(m);
                a.on_ipc();
            } else {
                a.refresh();
            }
            if let Some((text, done)) = updated {
                if !quiet {
                    a.after.push(After::Message { text, error: done.problem.is_some() });
                }
                if done.leftovers > 0 {
                    a.updater.sweep = done.exes;
                    a.updater.sweeps_left = SWEEP_TRIES;
                    SetTimer(hwnd, TIMER_SWEEP, SWEEP_RETRY_MS, None);
                }
            }
        });

        trim_working_set();
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            with_app(|a| a.stats.msgs += 1);
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // The loop has ended, so nothing is inside the state any more.
        let mut app = Box::from_raw(APP.swap(std::ptr::null_mut(), Ordering::Relaxed));
        let restart = app.updater.restart.take();
        drop(app);
        // An update is in place: start it. It waits for this process to end,
        // which also frees the port; the mutex goes now.
        if let Some((exe, args)) = restart {
            CloseHandle(mutex);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if !launch::spawn_detached(&exe, &args) {
                crate::dlog!("update: could not start {}: {}", exe.display(), std::io::Error::last_os_error());
            }
        }
    }
}
