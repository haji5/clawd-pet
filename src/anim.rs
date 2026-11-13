//! What Clawd plays, and when (PLAN.md §6 "Animator").
//!
//! - `anim_for_tool`: the tool → animation table.
//! - `Director`: picks the animation from the tracker's display state,
//!   one-shot reactions (Claude's and the user's), held interaction poses,
//!   hover, cursor gaze, idle fidgets and blinks, and says when
//!   the picture can next change, so the window sets one one-shot timer
//!   instead of ticking. Pure, with an injected clock.

use crate::mood::Mood;
use crate::rng::Rng;
use crate::sprite::{Anim, Sheet, Step};
use crate::tracker::{Attention, Display, Mode, Ms, Reaction, Work};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolAnim {
    Reading,
    Typing,
    Bash,
    /// Flavours of `Bash`, from what kind of command it's running.
    Git,
    Test,
    Build,
    Install,
    Web,
    Delegating,
    /// Bookkeeping tools (the desktop app's `mcp__ccd_*`, todo lists…) look like thinking.
    Thinking,
    /// Claude asks the user something and waits (`AskUserQuestion`, `ExitPlanMode`).
    AsksUser,
    Generic,
}

pub fn anim_for_tool(name: &str) -> ToolAnim {
    match name {
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => ToolAnim::Reading,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ToolAnim::Typing,
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" | "KillBash" | "Monitor" => ToolAnim::Bash,
        "WebFetch" | "WebSearch" => ToolAnim::Web,
        "Agent" | "Task" => ToolAnim::Delegating,
        "AskUserQuestion" | "ExitPlanMode" => ToolAnim::AsksUser,
        "TodoWrite" | "TodoRead" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" | "TaskStop" | "ToolSearch"
        | "Skill" | "EnterPlanMode" | "SubagentHandback" => ToolAnim::Thinking,
        n if n.starts_with("mcp__ccd_") => ToolAnim::Thinking,
        _ => ToolAnim::Generic,
    }
}

/// A shell tool, by the class of its command (`flavour::classify`).
pub fn bash_flavour(kind: Option<&str>) -> ToolAnim {
    match kind {
        Some("git") => ToolAnim::Git,
        Some("test") => ToolAnim::Test,
        Some("build") => ToolAnim::Build,
        Some("install") => ToolAnim::Install,
        _ => ToolAnim::Bash,
    }
}

/// Wears `hat_hard` instead of whatever the season calls for.
const BUILD_ANIM: &str = "bash_build";

/// The looping animation for a display mode (names in `assets/sprites.txt`).
pub fn base_anim(mode: Mode) -> &'static str {
    match mode {
        Mode::Attention(Attention::Impatient) => "impatient",
        Mode::Attention(Attention::Permission) => "attention",
        Mode::Attention(_) => "waiting",
        Mode::Working(Work::Thinking) => "thinking",
        Mode::Working(Work::Compacting) => "compacting",
        Mode::Working(Work::Report) => "reading_report",
        Mode::Working(Work::Tool(t)) => match t {
            ToolAnim::Reading => "reading",
            ToolAnim::Typing => "typing",
            ToolAnim::Bash => "bash",
            ToolAnim::Git => "bash_git",
            ToolAnim::Test => "bash_test",
            ToolAnim::Build => BUILD_ANIM,
            ToolAnim::Install => "bash_install",
            ToolAnim::Web => "web",
            ToolAnim::Delegating => "delegating",
            ToolAnim::Thinking | ToolAnim::AsksUser => "thinking",
            ToolAnim::Generic => "tool",
        },
        Mode::Idle => "idle",
        Mode::Sleepy => "sleepy",
        Mode::Asleep => "asleep",
    }
}

/// One-shot reactions that play to completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shot {
    Claude(Reaction),
    /// A user reaction, by animation name (`petted`, `giggle`, `trick_spin`…).
    User(String),
}

impl Shot {
    pub fn anim(&self) -> &str {
        match self {
            Shot::Claude(Reaction::Wake) => "wake",
            Shot::Claude(Reaction::Celebrate) => "celebrate",
            Shot::Claude(Reaction::Oops) => "oops",
            Shot::Claude(Reaction::BigOops) => "oops_big",
            Shot::Claude(Reaction::Heart) => "heart",
            Shot::Claude(Reaction::Spawn) => "spawn",
            Shot::Claude(Reaction::Report) => "report",
            Shot::User(name) => name,
        }
    }
    /// User interactions beat everything; Claude's reactions beat everything
    /// except a permission request.
    fn priority(&self) -> u8 {
        if matches!(self, Shot::User(_)) { 3 } else { 1 }
    }
}

fn base_priority(mode: Mode) -> u8 {
    if matches!(mode, Mode::Attention(Attention::Permission | Attention::Impatient)) { 2 } else { 0 }
}

/// Claude-driven states stay up at least this long, so fast tools
/// (Read ~16 ms) don't flicker past.
pub const MIN_SHOW: Ms = 600;
const BLINK_MS: Ms = 140;
const BLINK_EVERY: (Ms, Ms) = (3_000, 8_000);
const FIDGET_EVERY: (Ms, Ms) = (20_000, 60_000);
const YAWN_EVERY: (Ms, Ms) = (10_000, 25_000);
const DEMO_MIN: Ms = 3_000;
/// More subagents than this still show as a stack of this many.
const MAX_CREW: usize = 3;

/// Which step of `a` is up `elapsed` ms after it started, and how many ms
/// until the next step (`None`: the picture won't change).
pub fn step_at(a: &Anim, elapsed: Ms, looping: bool) -> (usize, Option<Ms>) {
    let total = a.duration_ms();
    if a.steps.len() < 2 || total == 0 {
        return (0, None);
    }
    if !looping && elapsed >= total {
        return (a.steps.len() - 1, None);
    }
    let t = if looping { elapsed % total } else { elapsed };
    let mut end = 0;
    for (i, s) in a.steps.iter().enumerate() {
        end += s.hold as Ms * a.tick_ms();
        if t < end {
            return (i, Some(end - t));
        }
    }
    (a.steps.len() - 1, None)
}

/// What to draw: one step of the body animation plus overlays.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub step: Step,
    /// The mini-clawd beside Clawd while subagents run.
    pub mini: Option<Step>,
    /// How many of them, stacked.
    pub crew: usize,
    /// Sessions waiting on the user; a badge shows when ≥ 2.
    pub badge: usize,
    pub flip: bool,
    /// Quarter turns clockwise.
    pub rot: u8,
    /// A frame worn on top of the head.
    pub hat: Option<usize>,
}

struct Playing {
    name: String,
    since: Ms,
}

impl Playing {
    fn new(name: &str, since: Ms) -> Self {
        Playing { name: name.into(), since }
    }
}

struct Demo {
    names: Vec<String>,
    idx: usize,
    since: Ms,
}

/// Which layer of the director picked the current animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Demo,
    /// Held by the window while an interaction lasts (dragging, airborne, squish…).
    Pose,
    Shot { user: bool },
    Hover,
    Extra,
    Base,
}

pub struct Director {
    base: Playing,
    base_mode: Mode,
    pending: Option<Mode>,
    shot: Option<(Shot, Ms)>,
    /// Idle fidget or sleepy yawn currently playing.
    extra: Option<Playing>,
    next_extra: Option<Ms>,
    blink_at: Ms,
    pose: Option<Playing>,
    hover: Option<Ms>,
    /// Eyes toward the cursor (screen space), when no anim sets a gaze.
    gaze: Option<(i32, i32)>,
    flip: bool,
    rot: u8,
    subagents: usize,
    mini_since: Option<Ms>,
    badge: usize,
    animate_asleep: bool,
    demo: Option<Demo>,
    mood: Mood,
    rng: Rng,
}

impl Director {
    pub fn new(now: Ms, animate_asleep: bool, rng: Rng) -> Self {
        let mut d = Director {
            base: Playing::new("idle", now),
            base_mode: Mode::Idle,
            pending: None,
            shot: None,
            extra: None,
            next_extra: None,
            blink_at: 0,
            pose: None,
            hover: None,
            gaze: None,
            flip: false,
            rot: 0,
            subagents: 0,
            mini_since: None,
            badge: 0,
            animate_asleep,
            demo: None,
            mood: Mood::ORDINARY,
            rng,
        };
        d.blink_at = now + d.rng.range(BLINK_EVERY.0, BLINK_EVERY.1);
        d.schedule_extra(now);
        d
    }

    /// `--demo`: cycle through every animation in the sheet.
    pub fn start_demo(&mut self, sheet: &Sheet, now: Ms) {
        let names = sheet.anims.iter().map(|a| a.name.to_string()).filter(|n| n != "mini").collect();
        self.demo = Some(Demo { names, idx: 0, since: now });
    }

    pub fn set_mood(&mut self, mood: Mood) {
        self.mood = mood;
    }

    pub fn mode(&self) -> Mode {
        self.base_mode
    }

    pub fn set_display(&mut self, d: Display, now: Ms) {
        self.badge = d.attention_sessions;
        if d.subagents != self.subagents {
            self.mini_since = (d.subagents > 0).then_some(self.mini_since.unwrap_or(now));
            self.subagents = d.subagents;
        }
        if base_anim(d.mode) == base_anim(self.base_mode) {
            self.base_mode = d.mode;
            self.pending = None;
        } else if now >= self.base.since + MIN_SHOW {
            self.switch_base(d.mode, now);
        } else {
            self.pending = Some(d.mode); // latest wins
        }
    }

    fn switch_base(&mut self, mode: Mode, now: Ms) {
        self.base = Playing::new(base_anim(mode), now);
        self.base_mode = mode;
        self.pending = None;
        self.extra = None;
        self.schedule_extra(now);
    }

    fn schedule_extra(&mut self, now: Ms) {
        self.next_extra = match self.base_mode {
            Mode::Idle => Some(now + self.rng.range(FIDGET_EVERY.0, FIDGET_EVERY.1)),
            Mode::Sleepy => Some(now + self.rng.range(YAWN_EVERY.0, YAWN_EVERY.1)),
            _ => None,
        };
    }

    pub fn react(&mut self, sheet: &Sheet, s: Shot, now: Ms) {
        if let Some((cur, since)) = &self.shot
            && sheet.anim(cur.anim()).is_some_and(|a| now < since + a.duration_ms())
        {
            // A Wake never interrupts a more interesting reaction, and Claude
            // never interrupts the user's.
            if (s == Shot::Claude(Reaction::Wake) && *cur != s) || cur.priority() > s.priority() {
                return;
            }
        }
        self.shot = Some((s, now));
        self.extra = None;
    }

    /// Play a user reaction: `name` itself or one of its variants
    /// (`petted`, `petted_2`… / `trick_*`), picked at random.
    pub fn user(&mut self, sheet: &Sheet, name: &str, now: Ms) {
        let variants: Vec<&str> = sheet
            .anims
            .iter()
            .map(|a| a.name)
            .filter(|n| *n == name || n.strip_prefix(name).is_some_and(|r| r.starts_with('_')))
            .collect();
        if variants.is_empty() {
            return;
        }
        let pick = variants[self.rng.range(0, variants.len() as u64) as usize].to_string();
        self.react(sheet, Shot::User(pick), now);
    }

    /// Is a user reaction (not a pose) still playing?
    pub fn user_playing(&self, sheet: &Sheet, now: Ms) -> bool {
        matches!(&self.shot, Some((s @ Shot::User(_), since)) if sheet.anim(s.anim()).is_some_and(|a| now < since + a.duration_ms()))
    }

    /// Hold an animation while an interaction lasts (`None` releases it).
    /// Setting the same name again keeps its clock running.
    pub fn set_pose(&mut self, name: Option<&str>, now: Ms) {
        match name {
            Some(n) if self.pose.as_ref().is_some_and(|p| p.name == n) => {}
            Some(n) => {
                self.pose = Some(Playing::new(n, now));
                self.extra = None;
            }
            None => self.pose = None,
        }
    }

    pub fn pose(&self) -> Option<&str> {
        self.pose.as_ref().map(|p| p.name.as_str())
    }

    pub fn set_hover(&mut self, on: bool, now: Ms) {
        if on && self.hover.is_none() {
            self.hover = Some(now);
            self.extra = None;
        } else if !on {
            self.hover = None;
        }
    }

    pub fn set_gaze(&mut self, g: Option<(i32, i32)>) {
        self.gaze = g;
    }
    pub fn gaze(&self) -> Option<(i32, i32)> {
        self.gaze
    }
    pub fn set_flip(&mut self, f: bool) {
        self.flip = f;
    }
    pub fn set_rot(&mut self, r: u8) {
        self.rot = r % 4;
    }

    /// Apply everything time-based: finished reactions, deferred state
    /// changes, fidgets, blinks, the demo cycle. Call before `look`.
    pub fn tick(&mut self, sheet: &Sheet, now: Ms) {
        let done = |name: &str, since: Ms| sheet.anim(name).is_none_or(|a| now >= since + a.duration_ms());
        if self.shot.as_ref().is_some_and(|(s, since)| done(s.anim(), *since)) {
            self.shot = None;
        }
        if self.extra.as_ref().is_some_and(|e| done(&e.name, e.since)) {
            self.extra = None;
        }
        if let Some(m) = self.pending
            && now >= self.base.since + MIN_SHOW
        {
            self.switch_base(m, now);
        }
        if let Some(at) = self.next_extra
            && now >= at
        {
            if self.shot.is_none() && self.extra.is_none() && self.pose.is_none() && self.hover.is_none() {
                let treat = self.mood.treat.filter(|&(name, pct)| sheet.anim(name).is_some() && self.rng.range(0, 100) < pct);
                let pick = match self.base_mode {
                    Mode::Idle | Mode::Sleepy if treat.is_some() => treat.map(|t| t.0),
                    Mode::Idle => {
                        let fidgets: Vec<&str> =
                            sheet.anims.iter().map(|a| a.name).filter(|n| n.starts_with("fidget")).collect();
                        (!fidgets.is_empty()).then(|| fidgets[self.rng.range(0, fidgets.len() as u64) as usize])
                    }
                    Mode::Sleepy => Some("yawn"),
                    _ => None,
                };
                if let Some(p) = pick {
                    self.extra = Some(Playing::new(p, now));
                }
            }
            self.schedule_extra(now);
        }
        if now >= self.blink_at + BLINK_MS {
            self.blink_at = now + self.rng.range(BLINK_EVERY.0, BLINK_EVERY.1);
        }
        if let Some(d) = self.demo.as_mut() {
            let len = d.names.get(d.idx).and_then(|n| sheet.anim(n)).map_or(0, Anim::duration_ms);
            if now >= d.since + len.max(DEMO_MIN) {
                d.idx = (d.idx + 1) % d.names.len().max(1);
                d.since = now;
            }
        }
    }

    /// The animation on screen right now, its start time and which layer chose it.
    fn current(&self, now: Ms) -> (&str, Ms, Kind) {
        if let Some(d) = &self.demo {
            return (d.names.get(d.idx).map_or("idle", String::as_str), d.since, Kind::Demo);
        }
        if let Some(p) = &self.pose {
            return (&p.name, p.since, Kind::Pose);
        }
        if let Some((s, since)) = &self.shot
            && s.priority() > base_priority(self.base_mode)
        {
            return (s.anim(), *since, Kind::Shot { user: matches!(s, Shot::User(_)) });
        }
        if let Some(h) = self.hover {
            let happy = now >= h + crate::feel::HOVER_HAPPY_MS;
            return (if happy { "hover_happy" } else { "hover" }, h, Kind::Hover);
        }
        if let Some(e) = &self.extra {
            return (&e.name, e.since, Kind::Extra);
        }
        (&self.base.name, self.base.since, Kind::Base)
    }

    pub fn current_name(&self, now: Ms) -> &str {
        self.current(now).0
    }

    fn frozen(&self, kind: Kind) -> bool {
        kind == Kind::Base && self.base_mode == Mode::Asleep && !self.animate_asleep
    }

    fn looping(kind: Kind, a: &Anim) -> bool {
        match kind {
            Kind::Demo | Kind::Base | Kind::Hover => true,
            Kind::Pose => a.looping, // one-shot poses (squish) hold their last frame
            Kind::Shot { .. } | Kind::Extra => false,
        }
    }

    fn blinks(&self) -> bool {
        !matches!(self.base_mode, Mode::Sleepy | Mode::Asleep) || self.demo.is_some()
    }

    fn crew(&self, now: Ms) -> usize {
        let n = self.subagents.min(MAX_CREW);
        match self.current_name(now) {
            "spawn" => n.saturating_sub(1), // the new one is still flying there
            "delegating" => n.max(1),
            _ => n,
        }
    }

    pub fn look(&self, sheet: &Sheet, now: Ms) -> Option<Look> {
        let (name, since, kind) = self.current(now);
        let a = sheet.anim(name).or_else(|| sheet.anim("idle"))?;
        let i = if self.frozen(kind) { 0 } else { step_at(a, now.saturating_sub(since), Self::looping(kind, a)).0 };
        let mut step = a.steps[i].clone();
        // Claude's bubble stays visible during interactions.
        if step.glyph.is_none() && matches!(kind, Kind::Pose | Kind::Hover | Kind::Shot { user: true }) {
            step.glyph = sheet.anim(&self.base.name).and_then(|b| {
                let frozen = self.frozen(Kind::Base);
                let j = if frozen { 0 } else { step_at(b, now.saturating_sub(self.base.since), true).0 };
                b.steps[j].glyph
            });
        }
        if self.blinks() && now >= self.blink_at && now < self.blink_at + BLINK_MS && step.eyes == sheet.index("eye_normal") {
            step.eyes = sheet.index("eye_blink").or(step.eyes);
        }
        if step.look == (0, 0)
            && let Some(g) = self.gaze
        {
            // The cursor gaze is in screen space; `look` is in body space.
            step.look = if step.flip != self.flip { (-g.0, g.1) } else { g };
        }
        let crew = self.crew(now);
        let mini = (crew > 0).then(|| sheet.anim("mini")).flatten().map(|m| {
            let t = now.saturating_sub(self.mini_since.unwrap_or(since));
            m.steps[step_at(m, t, true).0].clone()
        });
        if kind == Kind::Base
            && self.mood.mug
            && matches!(self.base_mode, Mode::Idle | Mode::Sleepy)
            && let Some(mug) = sheet.index(if i % 2 == 0 { "mug_0" } else { "mug_1" })
        {
            step.fx.push(mug);
        }
        let hat = if name == BUILD_ANIM { sheet.index("hat_hard") } else { self.mood.hat.and_then(|h| sheet.index(h)) };
        let rot = (step.rot + self.rot) % 4;
        let flip = step.flip != self.flip;
        Some(Look { step, mini, crew, badge: self.badge, flip, rot, hat })
    }

    /// The next moment the picture or the chosen animation can change on
    /// its own. With `frames == false` (window hidden) only state changes
    /// count, not frame steps or blinks.
    pub fn next_change(&self, sheet: &Sheet, now: Ms, frames: bool) -> Option<Ms> {
        let mut t: Option<Ms> = None;
        let mut at = |x: Ms| {
            let x = x.max(now + 1);
            t = Some(t.map_or(x, |t| t.min(x)));
        };
        if self.pending.is_some() {
            at(self.base.since + MIN_SHOW);
        }
        if let Some((s, since)) = &self.shot {
            at(since + sheet.anim(s.anim()).map_or(0, Anim::duration_ms));
        }
        if let Some(e) = &self.extra {
            at(e.since + sheet.anim(&e.name).map_or(0, Anim::duration_ms));
        }
        if let Some(h) = self.hover
            && now < h + crate::feel::HOVER_HAPPY_MS
        {
            at(h + crate::feel::HOVER_HAPPY_MS);
        }
        if let Some(d) = &self.demo {
            let len = d.names.get(d.idx).and_then(|n| sheet.anim(n)).map_or(0, Anim::duration_ms);
            at(d.since + len.max(DEMO_MIN));
        }
        if !frames {
            return t;
        }
        if let Some(x) = self.next_extra {
            at(x);
        }
        let (name, since, kind) = self.current(now);
        if !self.frozen(kind)
            && let Some(a) = sheet.anim(name)
        {
            if let (_, Some(dt)) = step_at(a, now.saturating_sub(since), Self::looping(kind, a)) {
                at(now + dt);
            }
            if self.blinks() && a.steps.iter().any(|s| s.eyes.is_some() && s.eyes == sheet.index("eye_normal")) {
                at(if now < self.blink_at { self.blink_at } else { self.blink_at + BLINK_MS });
            }
            // Claude's bubble animates under a user pose too (e.g. thinking dots).
            if kind != Kind::Base
                && let Some(b) = sheet.anim(&self.base.name)
                && !self.frozen(Kind::Base)
                && b.steps.iter().any(|s| s.glyph.is_some())
                && let (_, Some(dt)) = step_at(b, now.saturating_sub(self.base.since), true)
            {
                at(now + dt);
            }
        }
        if self.crew(now) > 0
            && let Some(m) = sheet.anim("mini")
            && let (_, Some(dt)) = step_at(m, now.saturating_sub(self.mini_since.unwrap_or(now)), true)
        {
            at(now + dt);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprite::builtin;

    #[test]
    fn table() {
        assert_eq!(anim_for_tool("Grep"), ToolAnim::Reading);
        assert_eq!(anim_for_tool("Edit"), ToolAnim::Typing);
        assert_eq!(anim_for_tool("PowerShell"), ToolAnim::Bash);
        assert_eq!(anim_for_tool("Agent"), ToolAnim::Delegating);
        assert_eq!(anim_for_tool("mcp__ccd_turn__update_plan"), ToolAnim::Thinking);
        assert_eq!(anim_for_tool("mcp__github__create_issue"), ToolAnim::Generic);
        assert_eq!(anim_for_tool("AskUserQuestion"), ToolAnim::AsksUser);
    }

    fn disp(mode: Mode) -> Display {
        Display { mode, attention_sessions: 0, subagents: 0 }
    }
    const READ: Mode = Mode::Working(Work::Tool(ToolAnim::Reading));
    const THINK: Mode = Mode::Working(Work::Thinking);

    fn dir(now: Ms) -> Director {
        Director::new(now, false, Rng::new(7))
    }
    fn at(d: &mut Director, s: &Sheet, now: Ms) -> String {
        d.tick(s, now);
        d.current_name(now).to_string()
    }
    fn user(name: &str) -> Shot {
        Shot::User(name.into())
    }

    /// Every animation name the code (director or window) asks for.
    pub const CODE_ANIMS: &[&str] = &[
        "dragged", "swing_l1", "swing_l2", "swing_r1", "swing_r2", "dragged_dizzy", "swing_l1_dizzy", "swing_l2_dizzy",
        "swing_r1_dizzy", "swing_r2_dizzy", "air", "soft_land", "land", "rattled", "hard_land",
        "rise_1", "rise_2", "fall_1", "fall_2", "fall_3", "hit_light", "hit_squash", "hit_splat", "hit_slam", "hit_recoil",
        "hit_wall", "hit_ceiling", "skid_stop_l", "skid_stop_r", "dizzy", "dazed",
        "grumpy", "giggle", "petted", "squish", "spring", "perk", "hover", "hover_happy", "menu_look", "hang", "tickle",
        "yawn", "mini", "walk", "ride_l1", "ride_l2", "ride_r1", "ride_r2", "whoa", "hop_crouch",
    ];

    #[test]
    fn every_animation_the_code_names_exists() {
        let s = builtin();
        let modes = [
            Mode::Attention(Attention::Impatient),
            Mode::Attention(Attention::Permission),
            Mode::Attention(Attention::YourTurn),
            THINK,
            Mode::Working(Work::Compacting),
            Mode::Working(Work::Report),
            Mode::Idle,
            Mode::Sleepy,
            Mode::Asleep,
        ];
        let tools = [
            ToolAnim::Reading,
            ToolAnim::Typing,
            ToolAnim::Bash,
            ToolAnim::Git,
            ToolAnim::Test,
            ToolAnim::Build,
            ToolAnim::Install,
            ToolAnim::Web,
            ToolAnim::Delegating,
            ToolAnim::Generic,
        ];
        let mut names: Vec<&str> = modes.iter().map(|&m| base_anim(m)).collect();
        names.extend(tools.iter().map(|&t| base_anim(Mode::Working(Work::Tool(t)))));
        let shots = [Reaction::Wake, Reaction::Celebrate, Reaction::Oops, Reaction::BigOops, Reaction::Heart, Reaction::Spawn, Reaction::Report].map(Shot::Claude);
        let shot_names: Vec<String> = shots.iter().map(|s| s.anim().to_string()).collect();
        names.extend(shot_names.iter().map(String::as_str));
        names.extend(CODE_ANIMS);
        for n in names {
            assert!(s.anim(n).is_some(), "sprites.txt has no anim `{n}`");
        }
        for prefix in ["fidget", "trick_", "petted_"] {
            assert!(s.anims.iter().any(|a| a.name.starts_with(prefix)), "no `{prefix}*` anims");
        }
        for f in ["eye_normal", "eye_blink", "bubble", "badge", "num_2", "num_9", "num_more", "icon", "icon_alert", "hat_hard", "mug_0", "mug_1"] {
            assert!(s.frame(f).is_some(), "sprites.txt has no frame `{f}`");
        }
        for a in s.anims.iter().filter(|a| !a.looping) {
            assert!(a.duration_ms() <= 2_000, "one-shot `{}` is {} ms", a.name, a.duration_ms());
        }
    }

    #[test]
    fn bash_flavours() {
        assert_eq!(bash_flavour(Some("git")), ToolAnim::Git);
        assert_eq!(bash_flavour(Some("install")), ToolAnim::Install);
        assert_eq!(bash_flavour(Some("other")), ToolAnim::Bash);
        assert_eq!(bash_flavour(None), ToolAnim::Bash);
    }

    #[test]
    fn a_hard_hat_for_builds_otherwise_the_seasons_hat() {
        let s = builtin();
        let mut d = dir(0);
        assert_eq!(d.look(&s, 0).unwrap().hat, None);
        d.set_mood(Mood { hat: Some("hat_december"), ..Mood::ORDINARY });
        assert_eq!(d.look(&s, 0).unwrap().hat, s.index("hat_december"));
        d.set_display(disp(Mode::Working(Work::Tool(ToolAnim::Build))), 1000);
        d.tick(&s, 1000);
        assert_eq!(d.look(&s, 1000).unwrap().hat, s.index("hat_hard"));
    }

    #[test]
    fn the_mug_is_for_idling_only() {
        let s = builtin();
        let mug = s.index("mug_0").unwrap();
        let mut d = dir(0);
        assert!(!d.look(&s, 0).unwrap().step.fx.contains(&mug));
        d.set_mood(Mood { mug: true, ..Mood::ORDINARY });
        assert!(d.look(&s, 0).unwrap().step.fx.contains(&mug));
        d.set_display(disp(THINK), 1000);
        d.tick(&s, 1000);
        assert!(!d.look(&s, 1000).unwrap().step.fx.contains(&mug));
    }

    #[test]
    fn a_mood_swaps_some_fidgets_for_its_treat() {
        let s = builtin();
        let mut d = dir(0);
        d.set_mood(Mood { treat: Some(("heart", 100)), ..Mood::ORDINARY });
        let mut now = 0;
        let mut treated = 0;
        while now < 200_000 {
            now = d.next_change(&s, now, true).unwrap();
            d.tick(&s, now);
            let name = d.current_name(now);
            assert!(!name.starts_with("fidget"), "{name} at {now}");
            treated += usize::from(name == "heart");
        }
        assert!(treated > 0);
    }

    #[test]
    fn step_timing() {
        let s = builtin();
        let a = s.anim("celebrate").unwrap();
        let total = a.duration_ms();
        assert_eq!(step_at(a, 0, false).0, 0);
        assert_eq!(step_at(a, total, false), (a.steps.len() - 1, None));
        let (i, next) = step_at(a, total + 5, true);
        assert_eq!(i, 0);
        assert!(next.unwrap() > 0);
    }

    #[test]
    fn fast_tools_dont_flicker() {
        let s = builtin();
        let mut d = dir(0);
        d.set_display(disp(THINK), 1000);
        d.set_display(disp(READ), 1100); // < 600 ms after thinking started: deferred
        assert_eq!(at(&mut d, &s, 1100), "thinking");
        assert_eq!(d.next_change(&s, 1100, false), Some(1600));
        assert_eq!(at(&mut d, &s, 1600), "reading");
        // Read → Thinking → Read inside the window: reading just carries on.
        d.set_display(disp(THINK), 1700);
        d.set_display(disp(READ), 1750);
        assert_eq!(at(&mut d, &s, 2300), "reading");
    }

    #[test]
    fn reaction_priorities() {
        let s = builtin();
        let mut d = dir(0);
        d.set_display(disp(Mode::Attention(Attention::YourTurn)), 1000);
        d.react(&s, Shot::Claude(Reaction::Celebrate), 1000);
        assert_eq!(at(&mut d, &s, 1000), "celebrate");
        let end = 1000 + s.anim("celebrate").unwrap().duration_ms();
        assert_eq!(at(&mut d, &s, end), "waiting");
        // A permission beats Claude's reactions, petting beats everything.
        d.set_display(disp(Mode::Attention(Attention::Permission)), end + 1000);
        d.react(&s, Shot::Claude(Reaction::Oops), end + 1000);
        assert_eq!(at(&mut d, &s, end + 1000), "attention");
        d.react(&s, user("petted"), end + 1001);
        assert_eq!(at(&mut d, &s, end + 1001), "petted");
        // A wake doesn't cut the petting short.
        d.react(&s, Shot::Claude(Reaction::Wake), end + 1002);
        assert_eq!(at(&mut d, &s, end + 1002), "petted");
        d.set_pose(Some("dragged"), end + 1003);
        assert_eq!(at(&mut d, &s, end + 1003), "dragged");
    }

    #[test]
    fn the_heart_shows_once_the_prompt_is_gone_but_not_over_another_one() {
        let s = builtin();
        let heart = Shot::Claude(Reaction::Heart);
        let mut d = dir(0);
        d.set_display(disp(Mode::Attention(Attention::Permission)), 1000);
        // The approved tool finished: back to work, with a thank-you on top.
        d.set_display(disp(THINK), 3000);
        d.react(&s, heart.clone(), 3000);
        assert_eq!(at(&mut d, &s, 3000), "heart");
        assert_eq!(at(&mut d, &s, 3000 + s.anim("heart").unwrap().duration_ms()), "thinking");
        // Another session is still waiting: that "!" is more important.
        d.set_display(disp(Mode::Attention(Attention::Permission)), 10_000);
        d.react(&s, heart, 10_000);
        assert_eq!(at(&mut d, &s, 10_000), "attention");
    }

    #[test]
    fn user_variants_poses_hover_and_the_claude_bubble() {
        let s = builtin();
        let mut d = dir(0);
        let mut seen = std::collections::HashSet::new();
        for i in 0..40 {
            d.user(&s, "petted", 10_000 * i);
            seen.insert(at(&mut d, &s, 10_000 * i));
        }
        assert!(seen.len() >= 2 && seen.iter().all(|n| n.starts_with("petted")), "{seen:?}");
        let now = 500_000;
        at(&mut d, &s, now + 5_000); // let the last petting finish
        // Dragging while Claude asks for permission: dangling, but the "!" stays.
        d.set_display(disp(Mode::Attention(Attention::Permission)), now + 5_000);
        d.set_pose(Some("swing_l2"), now + 5_000);
        let l = d.look(&s, now + 5_000).unwrap();
        assert_eq!(d.current_name(now + 5_000), "swing_l2");
        assert_eq!(l.step.glyph, s.index("glyph_bang"));
        // The squish is a one-shot pose: it holds its last frame.
        d.set_pose(Some("squish"), now + 6_000);
        let last = s.anim("squish").unwrap().steps.last().unwrap().body;
        assert_eq!(d.look(&s, now + 9_000).unwrap().step.body, last);
        d.set_pose(None, now + 9_000);
        // Hover: wide eyes, happy after 3 s; gaze applies when the anim sets none.
        d.set_display(disp(Mode::Idle), now + 10_000);
        d.set_hover(true, now + 10_000);
        assert_eq!(at(&mut d, &s, now + 10_000), "hover");
        assert_eq!(at(&mut d, &s, now + 13_000), "hover_happy");
        d.set_gaze(Some((2, -1)));
        assert_eq!(d.look(&s, now + 13_000).unwrap().step.look, (2, -1));
        d.set_hover(false, now + 14_000);
        assert_eq!(at(&mut d, &s, now + 14_000), "idle");
        // Hanging on the right edge: flipped, and its own look is mirrored by the flip.
        d.set_flip(true);
        d.set_pose(Some("hang"), now + 15_000);
        let l = d.look(&s, now + 15_000).unwrap();
        assert!(l.flip);
        assert_eq!(l.step.look, (2, 0), "body-space look; compose mirrors it");
        d.set_rot(1);
        assert_eq!(d.look(&s, now + 15_000).unwrap().rot, 1);
    }

    #[test]
    fn idle_fidgets_and_blinks_but_asleep_is_static() {
        let s = builtin();
        let mut d = dir(0);
        d.tick(&s, 0);
        let next = d.next_change(&s, 0, true).unwrap();
        assert!(next <= BLINK_EVERY.1, "idle wakes up for the next breath or blink");
        // Run an idle minute: at some point a fidget plays, and a blink shows.
        let (mut fidgeted, mut blinked) = (false, false);
        let mut now = 0;
        while now < 65_000 {
            now = d.next_change(&s, now, true).unwrap();
            d.tick(&s, now);
            fidgeted |= d.current_name(now).starts_with("fidget");
            blinked |= d.look(&s, now).unwrap().step.eyes == s.index("eye_blink");
        }
        assert!(fidgeted && blinked);
        d.set_display(disp(Mode::Asleep), now);
        d.tick(&s, now);
        assert_eq!(d.current_name(now), "asleep");
        assert_eq!(d.next_change(&s, now, true), None, "asleep: no timers at all");
        // Hidden: only state changes wake the window, not frames.
        d.set_display(disp(READ), now + 1000);
        assert_eq!(d.current_name(now + 1000), "reading");
        assert_eq!(d.next_change(&s, now + 1000, false), None);
        assert!(d.next_change(&s, now + 1000, true).is_some());
    }

    #[test]
    fn the_crew_shows_how_many_are_out_and_the_new_one_arrives_by_air() {
        let s = builtin();
        let crew = |n: usize, shot: Option<Reaction>| {
            let mut d = dir(0);
            d.set_display(Display { mode: THINK, attention_sessions: 0, subagents: n }, 1000);
            if let Some(r) = shot {
                d.react(&s, Shot::Claude(r), 1000);
            }
            d.tick(&s, 1000);
            let l = d.look(&s, 1000).unwrap();
            (l.crew, l.mini.is_some())
        };
        assert_eq!(crew(0, None), (0, false));
        assert_eq!(crew(2, None), (2, true));
        assert_eq!(crew(9, None), (MAX_CREW, true));
        assert_eq!(crew(1, Some(Reaction::Spawn)), (0, false), "the one in the air isn't standing there yet");
        assert_eq!(crew(3, Some(Reaction::Spawn)), (2, true));
        assert_eq!(crew(1, Some(Reaction::Report)), (1, true), "the one coming back has already left the count");
    }

    #[test]
    fn spawn_plays_in_full_even_if_the_call_returns_at_once() {
        let s = builtin();
        let mut d = dir(0);
        d.set_display(disp(Mode::Working(Work::Tool(ToolAnim::Delegating))), 1000);
        d.react(&s, Shot::Claude(Reaction::Spawn), 1000);
        let len = s.anim("spawn").unwrap().duration_ms();
        d.set_display(disp(THINK), 1050); // a background agent's PostToolUse
        assert_eq!(at(&mut d, &s, 1000 + len - 1), "spawn");
        assert_eq!(at(&mut d, &s, 1000 + len), "thinking");
    }

    #[test]
    fn mini_clawd_while_subagents_run_and_demo_cycles() {
        let s = builtin();
        let mut d = dir(0);
        d.set_display(Display { mode: THINK, attention_sessions: 0, subagents: 1 }, 1000);
        d.tick(&s, 1000);
        assert!(d.look(&s, 1000).unwrap().mini.is_some());
        d.set_display(disp(THINK), 2000);
        assert!(d.look(&s, 2000).unwrap().mini.is_none());

        d.start_demo(&s, 3000);
        let first = at(&mut d, &s, 3000);
        let mut now = 3000;
        let mut seen = vec![first.clone()];
        for _ in 0..2000 {
            now = d.next_change(&s, now, false).unwrap();
            let n = at(&mut d, &s, now);
            if n == first {
                break;
            }
            if seen.last() != Some(&n) {
                seen.push(n);
            }
        }
        assert_eq!(seen.len(), s.anims.len() - 1, "demo shows every anim but `mini`: {seen:?}");
    }
}
