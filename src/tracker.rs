//! SessionTracker and the aggregated display state (PLAN.md §5, corrected
//! against Phase 0 findings). Pure, with an injected clock (`now` in ms on
//! any monotonic scale).

use crate::anim::{ToolAnim, anim_for_tool, bash_flavour};
use crate::goto::{self, Host, Target};
use crate::ipc::IpcMsg;
use std::collections::HashMap;

pub type Ms = u64;

const MIN: Ms = 60_000;
/// Esc-interrupts send no event, so a silent Thinking session decays to Idle.
pub const THINKING_STALE: Ms = 3 * MIN;
pub const TOOL_STALE: Ms = 10 * MIN;
/// "Your turn" after `Stop` fades after this long.
pub const YOUR_TURN_FOR: Ms = 5 * MIN;
pub const SUBAGENT_STALE: Ms = 2 * MIN;
pub const SESSION_DROP: Ms = 6 * 60 * MIN;
pub const SLEEPY_AFTER: Ms = 5 * MIN;
/// A prompt whose tool has finished within this long gets a thank-you heart.
pub const FAST_APPROVAL: Ms = 5_000;
/// How long Clawd reads a subagent's report, unless it moves on sooner.
pub const REPORT_FOR: Ms = 8_000;
const MIN_ANSWERED_AFTER_S: u32 = 8;
/// A notification this soon after its prompt shows that the client repeats
/// open prompts in time for the "answered" rule.
const REPEATS_WITHIN: Ms = 10_000;
/// A session that waits on the user and whose process isn't known (so it
/// can't be checked) stops calling after this long without any event.
pub const UNCHECKED_WAIT_FOR: Ms = 60 * MIN;
/// Anything sending more sessions than this is not Claude Code.
pub const MAX_SESSIONS: usize = 64;

/// Which program a session lives in: they differ in what they report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    Cli = 0,
    Desktop = 1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Thinking,
    Tool(String),
    Compacting,
}

/// Ordered by priority (higher wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    /// After `Stop`: gentle, expires after `YOUR_TURN_FOR`.
    YourTurn,
    /// `Notification(idle_prompt)` or Claude asking a question (`AskUserQuestion`).
    WaitingForInput,
    Permission,
    /// A permission prompt that's being ignored. Only ever a display state:
    /// sessions hold `Permission`, and `display` escalates it.
    Impatient,
}

#[derive(Clone, Debug)]
struct Pending {
    kind: Attention,
    since: Ms,
    /// `Notification(permission_prompt)` arrived: the prompt was still open
    /// ~6 s in. Approving one sends no event, so without this a long command
    /// that was approved at once would look like an ignored prompt.
    unanswered: bool,
    /// Agent id that raised it ("" = main agent); only its events clear a permission.
    from: String,
    /// The tool it's about ("" = unknown, any tool finishing clears it).
    /// Parallel tools finish while another one's prompt is still open.
    tool: String,
}

impl Pending {
    /// Approving sends no event, so "the tool finished soon after asking" is
    /// the only sign of it. A prompt known only from its late `Notification`
    /// names no tool, and its start time is ~6 s off anyway.
    fn approved_fast(&self, now: Ms) -> bool {
        self.kind == Attention::Permission && !self.tool.is_empty() && now.saturating_sub(self.since) < FAST_APPROVAL
    }

    /// When this prompt counts as answered. Answering one sends no event, but
    /// a prompt still open is repeated by `Notification(permission_prompt)`
    /// ~6 s in, so a prompt nobody repeated has been dealt with. If the
    /// notification is only late, it brings the prompt back.
    fn answered_from(&self, after: Ms) -> Option<Ms> {
        (self.kind == Attention::Permission && !self.unanswered).then_some(self.since + after)
    }

    /// Stuck until the user answers: no timer ends these, only an event, a
    /// click on Clawd, or the session turning out to be gone.
    fn blocks(&self) -> bool {
        matches!(self.kind, Attention::Permission | Attention::WaitingForInput)
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub project: String,
    pub activity: Activity,
    attention: Option<Pending>,
    last_event: Ms,
    /// agent id → last event time.
    subagents: HashMap<String, Ms>,
    /// `PreToolUse(Agent)` seen; show the mini-clawd before its first event arrives.
    expecting_subagent: Option<Ms>,
    /// The Claude Code process running it, once the hook has said so.
    host: Option<Host>,
    /// Its name and its id in the desktop app, for sessions that live there.
    title: Option<String>,
    desktop_id: Option<String>,
    /// What the running shell command is, from the hook.
    command_kind: Option<String>,
    /// A subagent just reported back: Clawd reads until then. Nothing says
    /// when Claude has finished reading, so it's a guess with a time limit.
    report_until: Option<Ms>,
}

impl Session {
    fn new(project: String, now: Ms) -> Self {
        Session {
            project,
            activity: Activity::Idle,
            attention: None,
            last_event: now,
            subagents: HashMap::new(),
            expecting_subagent: None,
            host: None,
            title: None,
            desktop_id: None,
            command_kind: None,
            report_until: None,
        }
    }
    pub fn attention(&self) -> Option<Attention> {
        self.attention.as_ref().map(|p| p.kind)
    }
    fn name(&self) -> &str {
        match &self.title {
            Some(title) => title,
            None if self.project.is_empty() => "session",
            None => &self.project,
        }
    }
    /// What it's waiting on the user for, if it is.
    fn waiting(&self) -> Option<&'static str> {
        self.attention().map(|a| match a {
            Attention::Permission | Attention::Impatient => "needs permission",
            Attention::WaitingForInput => "waiting for you",
            Attention::YourTurn => "your turn",
        })
    }
    /// Lower = listed first.
    fn urgency(&self) -> u8 {
        match (self.attention(), &self.activity) {
            (Some(Attention::Permission | Attention::Impatient), _) => 0,
            (Some(Attention::WaitingForInput), _) => 1,
            (Some(Attention::YourTurn), _) => 2,
            (None, Activity::Idle) => 4,
            (None, _) => 3,
        }
    }
    fn set_attention(&mut self, kind: Attention, now: Ms, from: &str, tool: &str) {
        // Never downgrade an open permission request to something gentler.
        if self.attention.as_ref().is_some_and(|p| p.kind > kind) {
            return;
        }
        self.attention = Some(Pending { kind, since: now, unanswered: false, from: from.into(), tool: tool.into() });
    }
    fn client(&self) -> Client {
        if self.desktop_id.is_some() { Client::Desktop } else { Client::Cli }
    }
    /// A `Notification(permission_prompt)` arriving now repeats a prompt
    /// that was asked a moment ago: proof that this client says so when a
    /// prompt is still open.
    fn repeated_in_time(&self, now: Ms) -> bool {
        self.attention.as_ref().is_some_and(|p| p.kind == Attention::Permission && !p.unanswered && now.saturating_sub(p.since) <= REPEATS_WITHIN)
    }
    /// When a session nobody can check on stops calling for the user.
    fn unchecked_wait_ends(&self) -> Option<Ms> {
        (self.host.is_none() && self.attention.as_ref().is_some_and(Pending::blocks)).then_some(self.last_event + UNCHECKED_WAIT_FOR)
    }
    fn mark_unanswered(&mut self) {
        if let Some(p) = self.attention.as_mut().filter(|p| p.kind == Attention::Permission) {
            p.unanswered = true;
        }
    }
    /// When this session's open prompt counts as ignored, if it has one.
    fn ignored_from(&self, after: Ms) -> Option<Ms> {
        self.attention.as_ref().filter(|p| p.kind == Attention::Permission && p.unanswered).map(|p| p.since + after)
    }
    /// `tool` run by agent `from` finished: whatever it was waiting on is over.
    fn clear_attention_from(&mut self, from: &str, tool: &str) -> Option<Pending> {
        self.attention.take_if(|p| p.from == from && (p.tool.is_empty() || p.tool == tool))
    }
    fn subagent_count(&self) -> usize {
        self.subagents.len() + usize::from(self.expecting_subagent.is_some() && self.subagents.is_empty())
    }
}

/// One-shot reactions the animator plays to completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    Wake,
    Celebrate,
    Oops,
    BigOops,
    /// A permission prompt was approved quickly.
    Heart,
    /// Claude started a subagent.
    Spawn,
    /// A subagent finished.
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Tool(ToolAnim),
    Thinking,
    Compacting,
    /// Going through what a subagent came back with.
    Report,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Attention(Attention),
    Working(Work),
    Idle,
    Sleepy,
    Asleep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub mode: Mode,
    /// Sessions that need the user (any attention kind).
    pub attention_sessions: usize,
    /// Active subagents across sessions (mini-clawd while > 0).
    pub subagents: usize,
}

impl Display {
    /// Do not disturb: Clawd naps through everything except a session
    /// that's stuck until the user answers. The gentle "your turn" after a
    /// finished task isn't one of those. It doesn't work itself up either.
    pub fn quiet(self) -> Display {
        match self.mode {
            Mode::Attention(Attention::Impatient) => Display { mode: Mode::Attention(Attention::Permission), ..self },
            Mode::Attention(Attention::Permission | Attention::WaitingForInput) => self,
            _ => Display { mode: Mode::Asleep, attention_sessions: 0, subagents: 0 },
        }
    }
}

pub struct Tracker {
    sessions: HashMap<String, Session>,
    last_claude_event: Ms,
    asleep_after: Ms,
    /// How long a prompt stays open before Clawd gets impatient (`None` = never).
    impatient_after: Option<Ms>,
    /// How long a prompt nobody repeated keeps its "!" (`None` = until its
    /// session's next event).
    answered_after: Option<Ms>,
    /// The sleepy and asleep delays, in percent of the normal ones.
    drowsy_pct: Ms,
    /// Whether each `Client` is known to repeat a prompt that's still open
    /// (`Notification(permission_prompt)`). The "answered" rule reads the
    /// absence of that notification, so it only holds where the notification
    /// is known to come: seen in the desktop app (docs/hook-findings.md),
    /// learned for the CLI the first time one arrives in time.
    repeats_prompts: [bool; 2],
}

impl Tracker {
    pub fn new(now: Ms, sleep_after_minutes: u32) -> Self {
        Tracker {
            sessions: HashMap::new(),
            last_claude_event: now,
            asleep_after: sleep_after_minutes.max(1) as Ms * MIN,
            impatient_after: Some(30_000),
            answered_after: Some(10_000),
            drowsy_pct: 100,
            repeats_prompts: [false, true],
        }
    }

    /// 0 turns impatience off.
    pub fn set_impatient_after(&mut self, seconds: u32) {
        self.impatient_after = (seconds > 0).then_some(seconds as Ms * 1000);
    }

    /// 0 keeps the "!" until the session's next event. Anything shorter than
    /// `MIN_ANSWERED_AFTER_S` would end before the notification can say otherwise.
    pub fn set_answered_after(&mut self, seconds: u32) {
        self.answered_after = (seconds > 0).then_some(seconds.max(MIN_ANSWERED_AFTER_S) as Ms * 1000);
    }

    /// Time of day: nights get sleepy sooner.
    pub fn set_drowsiness(&mut self, pct: u32) {
        self.drowsy_pct = pct.clamp(10, 100) as Ms;
    }

    /// Sleepy comes before asleep, however short `sleepAfterMinutes` is.
    fn sleepy_after(&self) -> Ms {
        SLEEPY_AFTER.min(self.asleep_after / 2) * self.drowsy_pct / 100
    }

    /// For checks that need a client known (or not) to repeat open prompts.
    #[cfg(test)]
    fn set_cli_repeats_prompts(&mut self, known: bool) {
        self.repeats_prompts[Client::Cli as usize] = known;
    }

    fn asleep_after(&self) -> Ms {
        self.asleep_after * self.drowsy_pct / 100
    }

    #[cfg(test)]
    pub fn sessions(&self) -> impl Iterator<Item = (&String, &Session)> {
        self.sessions.iter()
    }

    pub fn on_event(&mut self, m: &IpcMsg, now: Ms) -> Vec<Reaction> {
        self.expire(now);
        let mut out = Vec::new();
        // Only the events Clawd subscribes to: anything else on the port
        // isn't a Claude session and mustn't become one.
        if m.s.is_empty() || !crate::install::EVENTS.contains(&m.e.as_str()) {
            return out;
        }
        self.last_claude_event = now;
        if m.e == "SessionEnd" {
            self.sessions.remove(&m.s);
            return out;
        }
        if !self.sessions.contains_key(&m.s) && self.sessions.len() >= MAX_SESSIONS {
            self.drop_least_needed();
        }
        let project = m.p.clone().unwrap_or_default();
        let s = self.sessions.entry(m.s.clone()).or_insert_with(|| Session::new(project.clone(), now));
        s.last_event = now;
        if !project.is_empty() {
            s.project = project;
        }
        // A new chat has no name yet at its first prompt: keep what's known.
        if m.ti.is_some() {
            s.title = m.ti.clone();
        }
        if m.pp.is_some() {
            s.desktop_id = m.ds.clone(); // the hook just said who hosts it
        }
        let tool = m.t.as_deref().unwrap_or("");

        // Events from inside a subagent (Phase 0: they carry agent_id).
        if let Some(g) = m.g.as_deref() {
            if m.a.is_none() {
                return out; // internal helper agent (empty agent_type): ignore
            }
            match m.e.as_str() {
                "SubagentStop" => {
                    s.subagents.remove(g);
                    s.report_until = Some(now + REPORT_FOR);
                    out.push(Reaction::Report);
                }
                "PermissionRequest" => {
                    s.subagents.insert(g.into(), now);
                    s.set_attention(Attention::Permission, now, g, tool);
                }
                "PostToolUse" | "PostToolUseFailure" => {
                    s.subagents.insert(g.into(), now);
                    let answered = s.clear_attention_from(g, tool);
                    if m.e == "PostToolUse" && answered.is_some_and(|p| p.approved_fast(now)) {
                        out.push(Reaction::Heart);
                    }
                }
                "Notification" if m.n.as_deref() == Some("permission_prompt") => {
                    s.subagents.insert(g.into(), now);
                    if s.repeated_in_time(now) {
                        self.repeats_prompts[s.client() as usize] = true;
                    }
                    s.mark_unanswered();
                }
                _ => {
                    s.subagents.insert(g.into(), now);
                }
            }
            s.expecting_subagent = None;
            return out;
        }

        match m.e.as_str() {
            "SessionStart" => {
                if m.n.as_deref() != Some("compact") {
                    s.activity = Activity::Idle;
                    s.attention = None;
                    s.report_until = None;
                    out.push(Reaction::Wake);
                }
            }
            "UserPromptSubmit" => {
                s.activity = Activity::Thinking;
                s.attention = None;
            }
            "PreToolUse" => {
                s.activity = Activity::Tool(tool.into());
                s.command_kind = m.k.clone();
                match anim_for_tool(tool) {
                    ToolAnim::Delegating => {
                        s.expecting_subagent = Some(now);
                        out.push(Reaction::Spawn);
                    }
                    // Bookkeeping isn't moving on from the report.
                    ToolAnim::Thinking => {}
                    ToolAnim::AsksUser => {
                        s.report_until = None;
                        s.set_attention(Attention::WaitingForInput, now, "", tool)
                    }
                    _ => s.report_until = None,
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                s.activity = Activity::Thinking;
                let answered = s.clear_attention_from("", tool);
                if m.e == "PostToolUse" && answered.is_some_and(|p| p.approved_fast(now)) {
                    out.push(Reaction::Heart);
                }
                if m.e == "PostToolUseFailure" && m.n.as_deref() != Some("interrupt") {
                    out.push(Reaction::Oops);
                }
            }
            // Claude Code shows AskUserQuestion/ExitPlanMode through the
            // permission dialog: that's a question for you, not a permission.
            "PermissionRequest" if anim_for_tool(tool) == ToolAnim::AsksUser => {
                s.set_attention(Attention::WaitingForInput, now, "", tool)
            }
            "PermissionRequest" => s.set_attention(Attention::Permission, now, "", tool),
            "Notification" => match m.n.as_deref() {
                // Arrives ~6 s after an unanswered prompt, without a tool name.
                // It repeats what PermissionRequest already said; only use it
                // if nothing is pending (e.g. the pet started mid-prompt).
                Some("permission_prompt") => {
                    if s.repeated_in_time(now) {
                        self.repeats_prompts[s.client() as usize] = true;
                    }
                    if s.attention.is_none() {
                        s.set_attention(Attention::Permission, now, "", "");
                    }
                    s.mark_unanswered();
                }
                Some("idle_prompt") | Some("elicitation_dialog") | Some("agent_needs_input") => {
                    s.set_attention(Attention::WaitingForInput, now, "", "")
                }
                _ => {}
            },
            "Stop" | "StopFailure" => {
                s.activity = Activity::Idle;
                s.attention = Some(Pending { kind: Attention::YourTurn, since: now, unanswered: false, from: String::new(), tool: String::new() });
                s.expecting_subagent = None;
                s.report_until = None;
                out.push(if m.e == "Stop" { Reaction::Celebrate } else { Reaction::BigOops });
            }
            "PreCompact" => {
                s.activity = Activity::Compacting;
                s.report_until = None;
            }
            "PostCompact" => s.activity = Activity::Idle,
            "SubagentStart" | "SubagentStop" => {} // without agent id: nothing to track
            _ => {}
        }
        out
    }

    /// `None` when the process couldn't be read: an old answer is worse.
    pub fn set_host(&mut self, session: &str, host: Option<Host>) {
        if let Some(s) = self.sessions.get_mut(session) {
            s.host = host;
        }
    }

    /// Forget every session whose Claude Code process is gone (`alive` says
    /// whether a host still is that process). A closed terminal or a crash
    /// sends no `SessionEnd`, and a prompt that was open then would call for
    /// the user forever. Returns how many were dropped.
    pub fn drop_dead(&mut self, alive: impl Fn(Host) -> bool) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.host.is_none_or(&alive));
        before - self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Is a session stuck until the user answers? While one is, the window
    /// checks now and then that its process is still there.
    pub fn has_open_wait(&self) -> bool {
        self.sessions.values().any(|s| s.attention.as_ref().is_some_and(Pending::blocks))
    }

    /// Make room: the session that matters least goes (idle before working
    /// before waiting, the quietest first).
    fn drop_least_needed(&mut self) {
        let least = self.sessions.iter().max_by_key(|(_, s)| (s.urgency(), std::cmp::Reverse(s.last_event))).map(|(id, _)| id.clone());
        if let Some(id) = least {
            self.sessions.remove(&id);
        }
    }

    /// Needs the user first, then by how recent.
    fn ranked(&self) -> Vec<&Session> {
        let mut all: Vec<&Session> = self.sessions.values().collect();
        all.sort_by_key(|s| (s.urgency(), std::cmp::Reverse(s.last_event)));
        all
    }

    /// Where the user can be taken: every session waiting on them, or the
    /// latest one when none is.
    pub fn goto_targets(&mut self, now: Ms) -> Vec<Target> {
        self.expire(now);
        let target = |s: &Session| Target { project: s.name().to_string(), waiting: s.waiting(), host: s.host, desktop_id: s.desktop_id.clone() };
        let waiting: Vec<Target> = self.ranked().into_iter().filter(|s| s.waiting().is_some()).take(goto::MAX_TARGETS).map(target).collect();
        if !waiting.is_empty() {
            return waiting;
        }
        self.sessions.values().max_by_key(|s| s.last_event).map(target).into_iter().collect()
    }

    /// Clicking Clawd acknowledges everything it's calling for (a denied
    /// permission sends no event, so this is how the "!" goes away).
    pub fn dismiss_attention(&mut self) {
        for s in self.sessions.values_mut() {
            s.attention = None;
        }
    }

    /// Apply the time-based rules. Call before reading the display.
    pub fn expire(&mut self, now: Ms) {
        let (answered_after, repeats) = (self.answered_after, self.repeats_prompts);
        self.sessions.retain(|_, s| {
            let quiet = now.saturating_sub(s.last_event);
            let stale = match s.activity {
                Activity::Thinking => quiet >= THINKING_STALE,
                Activity::Tool(_) | Activity::Compacting => quiet >= TOOL_STALE,
                Activity::Idle => false,
            };
            if stale {
                s.activity = Activity::Idle;
            }
            if s.attention.as_ref().is_some_and(|p| p.kind == Attention::YourTurn && now.saturating_sub(p.since) >= YOUR_TURN_FOR) {
                s.attention = None;
            }
            if let Some(after) = answered_after
                && repeats[s.client() as usize]
                && s.attention.as_ref().and_then(|p| p.answered_from(after)).is_some_and(|t| now >= t)
            {
                s.attention = None;
            }
            if s.unchecked_wait_ends().is_some_and(|t| now >= t) {
                s.attention = None;
            }
            s.subagents.retain(|_, &mut t| now.saturating_sub(t) < SUBAGENT_STALE);
            if s.expecting_subagent.is_some_and(|t| now.saturating_sub(t) >= SUBAGENT_STALE) {
                s.expecting_subagent = None;
            }
            if s.report_until.is_some_and(|t| now >= t) {
                s.report_until = None;
            }
            !(s.activity == Activity::Idle && s.attention.is_none() && quiet >= SESSION_DROP)
        });
    }

    pub fn display(&mut self, now: Ms) -> Display {
        self.expire(now);
        let attention_sessions = self.sessions.values().filter(|s| s.attention.is_some()).count();
        let subagents = self.sessions.values().map(Session::subagent_count).sum();
        let busiest = self.sessions.values().filter(|s| s.activity != Activity::Idle || s.report_until.is_some()).max_by_key(|s| s.last_event);
        // "Your turn" is gentle: while any session is working, the work wins
        // (user report 2026-11-07: an old session's "?" hid this one's thinking).
        let top_attention = self
            .sessions
            .values()
            .filter_map(Session::attention)
            .filter(|&a| a > Attention::YourTurn || busiest.is_none())
            .max();
        let ignored = self.impatient_after.is_some_and(|after| self.sessions.values().filter_map(|s| s.ignored_from(after)).any(|t| now >= t));
        let mode = if let Some(a) = top_attention {
            Mode::Attention(if a == Attention::Permission && ignored { Attention::Impatient } else { a })
        } else if let Some(s) = busiest {
            Mode::Working(match &s.activity {
                _ if s.report_until.is_some() => Work::Report,
                Activity::Tool(t) => match anim_for_tool(t) {
                    ToolAnim::Thinking | ToolAnim::AsksUser => Work::Thinking,
                    ToolAnim::Bash => Work::Tool(bash_flavour(s.command_kind.as_deref())),
                    a => Work::Tool(a),
                },
                Activity::Compacting => Work::Compacting,
                _ => Work::Thinking,
            })
        } else {
            let quiet = now.saturating_sub(self.last_claude_event);
            if quiet >= self.asleep_after() {
                Mode::Asleep
            } else if quiet >= self.sleepy_after() {
                Mode::Sleepy
            } else {
                Mode::Idle
            }
        };
        Display { mode, attention_sessions, subagents }
    }

    /// The next moment the display can change without a new event, so the
    /// window can set one one-shot timer instead of polling. `None` = nothing
    /// will change until the next event (e.g. asleep).
    pub fn next_change(&self, now: Ms) -> Option<Ms> {
        let mut t: Option<Ms> = None;
        let mut at = |x: Ms| {
            if x > now {
                t = Some(t.map_or(x, |t| t.min(x)));
            }
        };
        for s in self.sessions.values() {
            match s.activity {
                Activity::Thinking => at(s.last_event + THINKING_STALE),
                Activity::Tool(_) | Activity::Compacting => at(s.last_event + TOOL_STALE),
                Activity::Idle => {}
            }
            if let Some(p) = &s.attention
                && p.kind == Attention::YourTurn
            {
                at(p.since + YOUR_TURN_FOR);
            }
            if let Some(x) = self.impatient_after.and_then(|after| s.ignored_from(after)) {
                at(x);
            }
            if self.repeats_prompts[s.client() as usize]
                && let Some(x) = self.answered_after.and_then(|after| s.attention.as_ref()?.answered_from(after))
            {
                at(x);
            }
            if let Some(x) = s.unchecked_wait_ends() {
                at(x);
            }
            for &seen in s.subagents.values() {
                at(seen + SUBAGENT_STALE);
            }
            if let Some(x) = s.expecting_subagent {
                at(x + SUBAGENT_STALE);
            }
            if let Some(x) = s.report_until {
                at(x);
            }
        }
        at(self.last_claude_event + self.sleepy_after());
        at(self.last_claude_event + self.asleep_after());
        t
    }

    /// Tray tooltip: the shell limits it to 127 chars.
    pub fn tooltip(&self) -> String {
        self.tooltip_within(127)
    }

    /// One line per session, at most `limit` chars in total.
    pub fn tooltip_within(&self, limit: usize) -> String {
        if self.sessions.is_empty() {
            return "Clawd: no Claude sessions".into();
        }
        let mut out = String::new();
        // Sessions that need the user come first, so they survive the cut.
        for s in self.ranked() {
            let state = match (s.waiting(), &s.activity) {
                (Some(w), _) => w.to_string(),
                (None, Activity::Thinking) => "thinking".to_string(),
                (None, Activity::Tool(t)) => format!("running {t}"),
                (None, Activity::Compacting) => "compacting".to_string(),
                (None, Activity::Idle) => "idle".to_string(),
            };
            let l = format!("{}: {state}", s.name());
            let extra = if out.is_empty() { l.chars().count() } else { l.chars().count() + 1 };
            if out.chars().count() + extra > limit {
                break;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&l);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::{HookInput, build_msg};

    fn fx(name: &str) -> IpcMsg {
        let path = format!("{}/tests/fixtures/hooks/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let input: HookInput = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        // Fixtures come from several real sessions; replay them as one.
        IpcMsg { s: "fixture".into(), ..build_msg(&input, 0).unwrap() }
    }
    fn ev(e: &str, s: &str) -> IpcMsg {
        IpcMsg { v: 1, e: e.into(), s: s.into(), p: Some(format!("proj-{s}")), ..Default::default() }
    }
    fn tool(e: &str, s: &str, t: &str) -> IpcMsg {
        IpcMsg { t: Some(t.into()), ..ev(e, s) }
    }
    fn mode(t: &mut Tracker, now: Ms) -> Mode {
        t.display(now).mode
    }
    /// A tracker whose CLI sessions are known to repeat open prompts, as the
    /// desktop app's are from the start.
    fn repeating() -> Tracker {
        let mut t = Tracker::new(0, 10);
        t.set_cli_repeats_prompts(true);
        t
    }

    #[test]
    fn fixture_session_walkthrough() {
        let mut t = Tracker::new(0, 10);
        assert_eq!(t.on_event(&fx("session_start_startup"), 1), vec![Reaction::Wake]);
        assert_eq!(mode(&mut t, 1), Mode::Idle);
        t.on_event(&fx("user_prompt_submit"), 2);
        assert_eq!(mode(&mut t, 2), Mode::Working(Work::Thinking));
        t.on_event(&fx("pre_tool_use_read"), 3);
        assert_eq!(mode(&mut t, 3), Mode::Working(Work::Tool(ToolAnim::Reading)));
        t.on_event(&fx("post_tool_use_read"), 4);
        assert_eq!(mode(&mut t, 4), Mode::Working(Work::Thinking));
        t.on_event(&fx("pre_tool_use_bash"), 5);
        assert_eq!(t.on_event(&fx("post_tool_use_failure_bash"), 6), vec![Reaction::Oops]);
        t.on_event(&fx("pre_tool_use_bash"), 7);
        t.on_event(&fx("permission_request_bash"), 8);
        assert_eq!(mode(&mut t, 8), Mode::Attention(Attention::Permission));
        assert!(t.tooltip().contains("my-app: needs permission"), "{}", t.tooltip());
        // Approved → that tool ran (the fixtures have no successful Bash).
        assert_eq!(t.on_event(&IpcMsg { t: Some("Bash".into()), ..fx("post_tool_use_read") }, 9), vec![Reaction::Heart]);
        assert_eq!(mode(&mut t, 9), Mode::Working(Work::Thinking));
        assert_eq!(t.on_event(&fx("stop"), 10), vec![Reaction::Celebrate]);
        assert_eq!(mode(&mut t, 10), Mode::Attention(Attention::YourTurn));
        // Internal helper SubagentStop after Stop changes nothing.
        t.on_event(&fx("subagent_stop_internal"), 11);
        assert_eq!(t.display(11).subagents, 0);
        assert_eq!(mode(&mut t, 11), Mode::Attention(Attention::YourTurn));
        t.on_event(&fx("pre_compact_manual"), 12);
        assert_eq!(mode(&mut t, 12), Mode::Working(Work::Compacting)); // work beats the gentle "your turn"
        t.on_event(&fx("user_prompt_submit"), 13);
        t.on_event(&fx("pre_compact_manual"), 14);
        assert_eq!(mode(&mut t, 14), Mode::Working(Work::Compacting));
        t.on_event(&fx("post_compact_manual"), 15);
        assert!(t.on_event(&fx("session_start_compact"), 15).is_empty()); // no wake after compact
        assert_eq!(mode(&mut t, 15), Mode::Idle);
        t.on_event(&fx("session_end_prompt_input_exit"), 16);
        assert_eq!(t.sessions().count(), 0);
    }

    const IMPATIENT: Mode = Mode::Attention(Attention::Impatient);
    const PERMISSION: Mode = Mode::Attention(Attention::Permission);

    fn open_prompt(t: &mut Tracker, at: Ms) {
        t.on_event(&tool("PermissionRequest", "a", "Bash"), at);
        t.on_event(&IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") }, at + 6_000);
    }

    #[test]
    fn an_ignored_prompt_makes_clawd_impatient_exactly_on_time() {
        let mut t = Tracker::new(0, 10);
        open_prompt(&mut t, 1_000);
        assert_eq!(mode(&mut t, 30_999), PERMISSION);
        assert_eq!(t.next_change(30_999), Some(31_000), "woken by a timer, not by polling");
        assert_eq!(mode(&mut t, 31_000), IMPATIENT);
        assert!(t.tooltip().contains("needs permission"), "{}", t.tooltip());
        t.on_event(&tool("PostToolUse", "a", "Bash"), 40_000);
        assert_eq!(mode(&mut t, 40_000), Mode::Working(Work::Thinking));
    }

    #[test]
    fn a_long_command_approved_at_once_never_looks_ignored() {
        let mut t = repeating();
        t.on_event(&tool("PreToolUse", "a", "Bash"), 1_000);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 1_000); // approved: no Notification
        assert_eq!(mode(&mut t, 300_000), Mode::Working(Work::Tool(ToolAnim::Bash)));
    }

    #[test]
    fn a_prompt_nobody_repeated_was_answered() {
        let mut t = repeating();
        t.on_event(&tool("PreToolUse", "a", "Bash"), 1_000);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 1_000);
        assert_eq!(mode(&mut t, 10_999), PERMISSION);
        assert_eq!(t.next_change(10_999), Some(11_000), "woken by a timer, not by polling");
        let d = t.display(11_000);
        assert_eq!((d.mode, d.attention_sessions), (Mode::Working(Work::Tool(ToolAnim::Bash)), 0));
        assert!(t.tooltip().contains("running Bash"), "{}", t.tooltip());
    }

    #[test]
    fn a_prompt_still_open_keeps_its_alarm_going() {
        let mut t = Tracker::new(0, 10);
        t.set_impatient_after(0);
        open_prompt(&mut t, 0);
        assert_eq!(mode(&mut t, 30 * MIN), PERMISSION);
    }

    #[test]
    fn a_late_notification_brings_the_prompt_back() {
        let mut t = repeating();
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        assert_ne!(mode(&mut t, 12_000), PERMISSION);
        t.on_event(&IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") }, 12_001);
        assert_eq!(mode(&mut t, 12_001), PERMISSION);
        assert_eq!(mode(&mut t, 12_001 + 20_000), PERMISSION, "it's known to be open now");
    }

    #[test]
    fn the_answered_rule_can_be_tuned_or_turned_off() {
        let mut t = repeating();
        t.set_answered_after(20);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        assert_eq!(mode(&mut t, 19_999), PERMISSION);
        assert_ne!(mode(&mut t, 20_000), PERMISSION);

        let mut t = repeating();
        t.set_answered_after(1);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        assert_eq!(mode(&mut t, 7_999), PERMISSION, "never before the notification could arrive");

        let mut t = Tracker::new(0, 10);
        t.set_answered_after(0);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        assert_eq!(mode(&mut t, 30 * MIN), PERMISSION);
    }

    fn notification(s: &str) -> IpcMsg {
        IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", s) }
    }
    /// A prompt in the desktop app (the hook says so with `pp` and `ds`).
    fn desktop_prompt(t: &mut Tracker, s: &str, at: Ms) {
        t.on_event(&IpcMsg { pp: Some(1), ds: Some("local_1".into()), ..ev("UserPromptSubmit", s) }, at);
        t.on_event(&tool("PreToolUse", s, "Bash"), at);
        t.on_event(&tool("PermissionRequest", s, "Bash"), at);
    }

    #[test]
    fn the_answered_rule_needs_a_client_known_to_repeat_open_prompts() {
        // The CLI hasn't been seen to: silence proves nothing, the "!" stays.
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PreToolUse", "a", "Bash"), 1_000);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 1_000);
        assert_eq!(mode(&mut t, 30 * MIN), PERMISSION);
        t.on_event(&tool("PostToolUse", "a", "Bash"), 31 * MIN);
        assert_eq!(mode(&mut t, 31 * MIN), Mode::Working(Work::Thinking));

        // The desktop app has: its prompts count as answered after 10 s.
        let mut t = Tracker::new(0, 10);
        desktop_prompt(&mut t, "d", 1_000);
        assert_eq!(mode(&mut t, 10_999), PERMISSION);
        assert_eq!(mode(&mut t, 11_000), Mode::Working(Work::Tool(ToolAnim::Bash)));
    }

    #[test]
    fn the_cli_earns_the_answered_rule_by_repeating_a_prompt_in_time() {
        let mut t = Tracker::new(0, 10);
        open_prompt(&mut t, 0); // repeated after 6 s
        t.on_event(&tool("PostToolUse", "a", "Bash"), 20_000);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 30_000);
        assert_eq!(mode(&mut t, 39_999), PERMISSION);
        assert_ne!(mode(&mut t, 40_000), PERMISSION, "now silence means it was answered");

        // A repeat that comes too late for the rule teaches nothing, and
        // neither does one for a prompt Clawd never saw asked.
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        t.on_event(&notification("a"), 60_000);
        t.on_event(&notification("b"), 61_000);
        t.on_event(&ev("SessionEnd", "b"), 62_000);
        t.on_event(&tool("PostToolUse", "a", "Bash"), 70_000);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 80_000);
        assert_eq!(mode(&mut t, 30 * MIN), PERMISSION);
    }

    #[test]
    fn a_session_whose_process_is_gone_stops_calling() {
        let mut t = Tracker::new(0, 10);
        assert!(!t.has_open_wait());
        open_prompt(&mut t, 0);
        t.set_host("a", Some(HOST));
        t.on_event(&ev("Stop", "b"), 1_000); // "your turn" is no open wait
        t.on_event(&ev("UserPromptSubmit", "c"), 2_000); // no host known: kept
        assert!(t.has_open_wait());
        assert_eq!(t.drop_dead(|h| h == HOST), 0, "still running");
        assert_eq!(mode(&mut t, 3 * 60 * MIN), IMPATIENT, "a prompt can wait for hours while its session lives");
        assert_eq!(t.drop_dead(|_| false), 1);
        assert!(!t.has_open_wait());
        assert!(!t.tooltip().contains("needs permission"), "{}", t.tooltip());
    }

    #[test]
    fn a_wait_that_cant_be_checked_gives_up_after_an_hour() {
        let mut t = Tracker::new(0, 10);
        open_prompt(&mut t, 0); // no host: nothing says whether it still runs
        t.on_event(&tool("PermissionRequest", "q", "AskUserQuestion"), 6_000);
        assert_eq!(mode(&mut t, 6_000 + UNCHECKED_WAIT_FOR - 1), IMPATIENT);
        assert_eq!(t.next_change(6_000 + UNCHECKED_WAIT_FOR - 1), Some(6_000 + UNCHECKED_WAIT_FOR));
        let d = t.display(6_000 + UNCHECKED_WAIT_FOR);
        assert_eq!((d.mode, d.attention_sessions), (Mode::Asleep, 0));
    }

    #[test]
    fn only_claude_events_make_a_session() {
        let mut t = Tracker::new(0, 10);
        for e in ["Bogus", "__show", "PermissionDenied", ""] {
            assert!(t.on_event(&ev(e, "x"), 1_000).is_empty());
        }
        assert_eq!(t.sessions().count(), 0);
        assert_eq!(mode(&mut t, SLEEPY_AFTER), Mode::Sleepy, "and they don't keep Clawd awake");
    }

    #[test]
    fn a_flood_of_sessions_pushes_out_the_idle_ones_first() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PermissionRequest", "waiting", "Bash"), 0);
        t.on_event(&ev("UserPromptSubmit", "working"), 1);
        for i in 0..2 * MAX_SESSIONS as u64 {
            t.on_event(&ev("SessionStart", &format!("s{i}")), 10 + i);
        }
        assert_eq!(t.sessions().count(), MAX_SESSIONS);
        let kept: Vec<&String> = t.sessions().map(|(id, _)| id).collect();
        assert!(kept.iter().any(|id| *id == "waiting") && kept.iter().any(|id| *id == "working"), "{kept:?}");
    }

    #[test]
    fn a_short_sleep_setting_is_honoured() {
        let mut t = Tracker::new(0, 2);
        assert_eq!(mode(&mut t, MIN - 1), Mode::Idle);
        assert_eq!(mode(&mut t, MIN), Mode::Sleepy);
        assert_eq!(mode(&mut t, 2 * MIN), Mode::Asleep);
        let mut t = Tracker::new(0, 30);
        assert_eq!(mode(&mut t, SLEEPY_AFTER), Mode::Sleepy, "sleepy still comes after five minutes");
        assert_eq!(mode(&mut t, 30 * MIN), Mode::Asleep);
    }

    #[test]
    fn a_question_is_never_assumed_answered() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PermissionRequest", "a", "AskUserQuestion"), 0);
        assert_eq!(mode(&mut t, 300_000), Mode::Attention(Attention::WaitingForInput));
    }

    #[test]
    fn a_question_never_makes_clawd_impatient() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PermissionRequest", "a", "AskUserQuestion"), 0);
        t.on_event(&IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") }, 6_000);
        assert_eq!(mode(&mut t, 300_000), Mode::Attention(Attention::WaitingForInput));
    }

    #[test]
    fn impatience_can_be_tuned_or_turned_off() {
        let mut t = Tracker::new(0, 10);
        t.set_impatient_after(10);
        open_prompt(&mut t, 0);
        assert_eq!(mode(&mut t, 9_999), PERMISSION);
        assert_eq!(mode(&mut t, 10_000), IMPATIENT);
        t.set_impatient_after(0);
        assert_eq!(mode(&mut t, 300_000), PERMISSION);
    }

    #[test]
    fn clicking_clawd_calms_it_and_do_not_disturb_keeps_it_calm() {
        let mut t = Tracker::new(0, 10);
        open_prompt(&mut t, 0);
        let d = t.display(60_000);
        assert_eq!(d.mode, IMPATIENT);
        assert_eq!(d.quiet().mode, PERMISSION);
        t.dismiss_attention();
        assert_ne!(mode(&mut t, 60_000), IMPATIENT);
    }

    #[test]
    fn sessions_that_need_you_are_listed_first_and_survive_the_cut() {
        let mut t = Tracker::new(0, 10);
        let named = |e: &str, s: &str| IpcMsg { p: Some(s.into()), ..ev(e, s) };
        t.on_event(&IpcMsg { t: Some("Bash".into()), ..named("PermissionRequest", "needs-you") }, 1);
        for (i, s) in ["busy-with-a-long-project-name-a", "busy-with-a-long-project-name-b", "busy-with-a-long-project-name-c", "busy-with-a-long-project-name-d"].iter().enumerate() {
            t.on_event(&named("UserPromptSubmit", s), 2 + i as Ms);
        }
        let tip = t.tooltip();
        assert!(tip.starts_with("needs-you: needs permission"), "{tip}");
        assert!(tip.chars().count() <= 127, "{tip}");
        assert!(tip.lines().nth(1).unwrap().contains("-d: "), "newest first among the rest: {tip}");
    }

    #[test]
    fn a_quick_approval_earns_a_heart_and_a_slow_one_doesnt() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 1_000);
        assert_eq!(t.on_event(&tool("PostToolUse", "a", "Bash"), 1_000 + FAST_APPROVAL - 1), vec![Reaction::Heart]);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 20_000);
        assert!(t.on_event(&tool("PostToolUse", "a", "Bash"), 20_000 + FAST_APPROVAL).is_empty());
    }

    #[test]
    fn only_a_real_approval_earns_a_heart() {
        let mut t = Tracker::new(0, 10);
        // Failed after approval: an oops, not both.
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 0);
        assert_eq!(t.on_event(&tool("PostToolUseFailure", "a", "Bash"), 1), vec![Reaction::Oops]);
        // Another tool finishing while the prompt is open.
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 2);
        assert!(t.on_event(&tool("PostToolUse", "a", "Grep"), 3).is_empty());
        // Denied: no event until the next prompt.
        assert!(t.on_event(&ev("UserPromptSubmit", "a"), 4).is_empty());
        // Tools that never asked.
        t.on_event(&tool("PreToolUse", "a", "Bash"), 5);
        assert!(t.on_event(&tool("PostToolUse", "a", "Bash"), 6).is_empty());
        // Answering a question isn't approving anything.
        t.on_event(&tool("PermissionRequest", "a", "AskUserQuestion"), 7);
        assert!(t.on_event(&tool("PostToolUse", "a", "AskUserQuestion"), 8).is_empty());
        // Known only from the late notification, so the wait isn't known.
        t.on_event(&IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") }, 9);
        assert!(t.on_event(&tool("PostToolUse", "a", "Bash"), 10).is_empty());
        // Clicking Clawd already dismissed it.
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 11);
        t.dismiss_attention();
        assert!(t.on_event(&tool("PostToolUse", "a", "Bash"), 12).is_empty());
    }

    #[test]
    fn approving_a_subagent_earns_a_heart_too() {
        let mut t = Tracker::new(0, 10);
        let sub = |e: &str, g: &str| IpcMsg { a: Some("general-purpose".into()), g: Some(g.into()), ..tool(e, "a", "Bash") };
        t.on_event(&sub("PermissionRequest", "x"), 0);
        assert!(t.on_event(&sub("PostToolUse", "y"), 1).is_empty(), "a different subagent");
        assert_eq!(t.on_event(&sub("PostToolUse", "x"), 2), vec![Reaction::Heart]);
    }

    #[test]
    fn a_shell_command_shows_its_flavour_until_the_next_tool() {
        let mut t = Tracker::new(0, 10);
        let bash = |kind: Option<&str>| IpcMsg { k: kind.map(Into::into), ..tool("PreToolUse", "a", "Bash") };
        t.on_event(&bash(Some("test")), 0);
        assert_eq!(mode(&mut t, 0), Mode::Working(Work::Tool(ToolAnim::Test)));
        t.on_event(&bash(None), 1);
        assert_eq!(mode(&mut t, 1), Mode::Working(Work::Tool(ToolAnim::Bash)));
        t.on_event(&bash(Some("nonsense from a newer hook")), 2);
        assert_eq!(mode(&mut t, 2), Mode::Working(Work::Tool(ToolAnim::Bash)));
        t.on_event(&bash(Some("build")), 3);
        t.on_event(&tool("PreToolUse", "a", "Read"), 4);
        assert_eq!(mode(&mut t, 4), Mode::Working(Work::Tool(ToolAnim::Reading)));
    }

    #[test]
    fn a_drowsy_hour_gets_sleepy_and_asleep_sooner() {
        let mut t = Tracker::new(0, 10);
        t.set_drowsiness(40);
        assert_eq!(t.next_change(0), Some(2 * MIN));
        assert_eq!(mode(&mut t, 2 * MIN - 1), Mode::Idle);
        assert_eq!(mode(&mut t, 2 * MIN), Mode::Sleepy);
        assert_eq!(t.next_change(2 * MIN), Some(4 * MIN));
        assert_eq!(mode(&mut t, 4 * MIN), Mode::Asleep);
        // Back to an ordinary hour.
        t.set_drowsiness(100);
        assert_eq!(mode(&mut t, 4 * MIN), Mode::Idle);
    }

    #[test]
    fn the_longer_tooltip_fits_what_the_tray_cant() {
        let mut t = Tracker::new(0, 10);
        for i in 0..12 {
            t.on_event(&tool("PreToolUse", &format!("session-with-a-long-name-{i}"), "Bash"), i);
        }
        assert!(t.tooltip().lines().count() < 12);
        let long = t.tooltip_within(1_000);
        assert_eq!(long.lines().count(), 12, "{long}");
    }

    const HOST: Host = Host { pid: 42, created: 7 };

    #[test]
    fn go_to_lists_waiting_sessions_most_urgent_first() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("Stop", "done"), 1);
        t.on_event(&tool("PreToolUse", "asking", "AskUserQuestion"), 2);
        t.on_event(&tool("PermissionRequest", "old", "Bash"), 3);
        t.on_event(&tool("PermissionRequest", "new", "Bash"), 4);
        t.on_event(&ev("UserPromptSubmit", "busy"), 5);
        let got: Vec<_> = t.goto_targets(6).into_iter().map(|g| (g.project, g.waiting)).collect();
        let want = [
            ("proj-new", "needs permission"),
            ("proj-old", "needs permission"),
            ("proj-asking", "waiting for you"),
            ("proj-done", "your turn"),
        ]
        .map(|(p, w)| (p.to_string(), Some(w)));
        assert_eq!(got, want, "the busy session waits for nobody");
    }

    #[test]
    fn go_to_offers_the_latest_session_when_nothing_waits() {
        let mut t = Tracker::new(0, 10);
        assert!(t.goto_targets(0).is_empty());
        t.on_event(&tool("PreToolUse", "older", "Bash"), 1);
        t.on_event(&ev("SessionStart", "newest"), 2);
        let got = t.goto_targets(3);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].project.as_str(), got[0].waiting), ("proj-newest", None));
    }

    #[test]
    fn go_to_doesnt_offer_a_your_turn_that_has_faded() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("Stop", "a"), 0);
        assert_eq!(t.goto_targets(YOUR_TURN_FOR - 1)[0].waiting, Some("your turn"));
        assert_eq!(t.goto_targets(YOUR_TURN_FOR)[0].waiting, None);
    }

    #[test]
    fn go_to_fits_in_a_menu() {
        let mut t = Tracker::new(0, 10);
        for i in 0..30 {
            t.on_event(&tool("PermissionRequest", &format!("s{i}"), "Bash"), i);
        }
        let got = t.goto_targets(30);
        assert_eq!(got.len(), goto::MAX_TARGETS);
        assert_eq!(got[0].project, "proj-s29");
    }

    #[test]
    fn a_desktop_chat_goes_by_its_name_and_id() {
        let mut t = Tracker::new(0, 10);
        let prompt = |ti: Option<&str>, ds: Option<&str>| IpcMsg { pp: Some(1), ti: ti.map(Into::into), ds: ds.map(Into::into), ..ev("UserPromptSubmit", "a") };
        t.on_event(&prompt(None, Some("local_1")), 0);
        assert_eq!(t.goto_targets(0)[0].project, "proj-a", "no name yet");
        t.on_event(&prompt(Some("Fix the bug"), Some("local_1")), 1);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 2);
        let got = &t.goto_targets(2)[0];
        assert_eq!((got.project.as_str(), got.desktop_id.as_deref()), ("Fix the bug", Some("local_1")), "later events don't carry them");
        assert!(t.tooltip().starts_with("Fix the bug: needs permission"), "{}", t.tooltip());
        t.on_event(&prompt(None, None), 3);
        let got = &t.goto_targets(3)[0];
        assert_eq!((got.project.as_str(), got.desktop_id.as_deref()), ("Fix the bug", None), "reopened outside the app");
    }

    #[test]
    fn a_session_remembers_its_host_until_told_otherwise() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("UserPromptSubmit", "a"), 0);
        assert_eq!(t.goto_targets(0)[0].host, None, "not known yet");
        t.set_host("a", Some(HOST));
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 1);
        assert_eq!(t.goto_targets(1)[0].host, Some(HOST), "later events don't carry it");
        t.set_host("a", None);
        assert_eq!(t.goto_targets(1)[0].host, None);
        t.set_host("unknown session", Some(HOST));
        assert_eq!(t.goto_targets(1).len(), 1);
    }

    #[test]
    fn do_not_disturb_only_lets_real_waiting_through() {
        let quiet = |mode, sessions| Display { mode, attention_sessions: sessions, subagents: 2 }.quiet();
        let napping = Display { mode: Mode::Asleep, attention_sessions: 0, subagents: 0 };
        assert_eq!(quiet(Mode::Working(Work::Thinking), 0), napping);
        assert_eq!(quiet(Mode::Idle, 0), napping);
        assert_eq!(quiet(Mode::Attention(Attention::YourTurn), 1), napping);
        for a in [Attention::Permission, Attention::WaitingForInput] {
            assert_eq!(quiet(Mode::Attention(a), 1).mode, Mode::Attention(a));
            assert_eq!(quiet(Mode::Attention(a), 3).attention_sessions, 3);
        }
    }

    #[test]
    fn your_turn_expires_then_sleepy_then_asleep() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("UserPromptSubmit", "a"), 0);
        t.on_event(&ev("Stop", "a"), 1000);
        assert_eq!(t.next_change(1000), Some(1000 + YOUR_TURN_FOR));
        assert_eq!(mode(&mut t, 1000 + YOUR_TURN_FOR - 1), Mode::Attention(Attention::YourTurn));
        assert_eq!(mode(&mut t, 1000 + YOUR_TURN_FOR), Mode::Sleepy);
        assert_eq!(mode(&mut t, 1000 + 10 * MIN), Mode::Asleep);
        assert_eq!(t.next_change(1000 + 10 * MIN), None, "asleep: no timers at all");
    }

    #[test]
    fn esc_interrupt_decays_thinking_after_3_min_but_tools_after_10() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("UserPromptSubmit", "a"), 0);
        assert_eq!(t.next_change(0), Some(THINKING_STALE));
        assert_eq!(mode(&mut t, THINKING_STALE - 1), Mode::Working(Work::Thinking));
        assert_eq!(mode(&mut t, THINKING_STALE), Mode::Idle);
        t.on_event(&tool("PreToolUse", "a", "Bash"), 4 * MIN);
        assert_eq!(mode(&mut t, 13 * MIN), Mode::Working(Work::Tool(ToolAnim::Bash)));
        // 10 min without any event: the tool decays, and Clawd is asleep by then.
        assert_eq!(mode(&mut t, 4 * MIN + TOOL_STALE), Mode::Asleep);
    }

    #[test]
    fn your_turn_yields_to_work_in_another_session() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("Stop", "old"), 0);
        t.on_event(&ev("UserPromptSubmit", "new"), 1);
        let d = t.display(2);
        assert_eq!(d.mode, Mode::Working(Work::Thinking));
        assert_eq!(d.attention_sessions, 1, "still counted for the tooltip");
        t.on_event(&ev("Stop", "new"), 3);
        assert_eq!(mode(&mut t, 3), Mode::Attention(Attention::YourTurn));
        // A question or permission still beats work.
        t.on_event(&ev("UserPromptSubmit", "new"), 4);
        t.on_event(&tool("PreToolUse", "old", "AskUserQuestion"), 5);
        assert_eq!(mode(&mut t, 5), Mode::Attention(Attention::WaitingForInput));
    }

    #[test]
    fn an_open_prompt_stays_until_next_prompt_or_click() {
        let mut t = Tracker::new(0, 10);
        t.set_impatient_after(0);
        open_prompt(&mut t, 0);
        assert_eq!(mode(&mut t, 30 * MIN), Mode::Attention(Attention::Permission));
        t.dismiss_attention();
        assert_eq!(mode(&mut t, 30 * MIN), Mode::Asleep);
        open_prompt(&mut t, 31 * MIN);
        t.on_event(&ev("UserPromptSubmit", "a"), 32 * MIN);
        assert_eq!(mode(&mut t, 32 * MIN), Mode::Working(Work::Thinking));
    }

    #[test]
    fn a_parallel_tool_finishing_leaves_the_open_prompt_alone() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PreToolUse", "a", "Grep"), 0);
        t.on_event(&tool("PreToolUse", "a", "Bash"), 1);
        t.on_event(&tool("PermissionRequest", "a", "Bash"), 2);
        t.on_event(&tool("PostToolUse", "a", "Grep"), 3);
        assert_eq!(mode(&mut t, 3), Mode::Attention(Attention::Permission), "Bash is still waiting");
        t.on_event(&tool("PostToolUseFailure", "a", "Grep"), 4);
        assert_eq!(mode(&mut t, 4), Mode::Attention(Attention::Permission));
        t.on_event(&tool("PostToolUse", "a", "Bash"), 5);
        assert_eq!(mode(&mut t, 5), Mode::Working(Work::Thinking));
        // A prompt only known from its Notification names no tool: any tool ends it.
        let note = IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "b") };
        t.on_event(&note, 6);
        t.on_event(&tool("PostToolUse", "b", "Read"), 7);
        assert_eq!(t.sessions.get("b").unwrap().attention(), None);
    }

    #[test]
    fn permission_beats_everything_across_sessions() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PreToolUse", "a", "Edit"), 0);
        t.on_event(&ev("Stop", "b"), 1);
        t.on_event(&tool("PermissionRequest", "c", "Bash"), 2);
        let d = t.display(3);
        assert_eq!(d.mode, Mode::Attention(Attention::Permission));
        assert_eq!(d.attention_sessions, 2);
        // A gentler attention never downgrades a pending permission.
        t.on_event(&tool("PreToolUse", "c", "AskUserQuestion"), 4);
        assert_eq!(t.sessions.get("c").unwrap().attention(), Some(Attention::Permission));
    }

    #[test]
    fn subagents_tracked_by_agent_id() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&fx("pre_tool_use_agent"), 0);
        assert_eq!(t.display(0).subagents, 1, "expected before its first event");
        t.on_event(&fx("pre_tool_use_in_subagent"), 1);
        assert_eq!(t.display(1).subagents, 1);
        // Main activity is still the Agent tool, not the subagent's tool.
        assert_eq!(mode(&mut t, 1), Mode::Working(Work::Tool(ToolAnim::Delegating)));
        assert_eq!(t.display(1 + SUBAGENT_STALE).subagents, 0, "stale subagent dropped");
        t.on_event(&fx("pre_tool_use_in_subagent"), 2 * SUBAGENT_STALE);
        assert_eq!(t.display(2 * SUBAGENT_STALE).subagents, 1);
        // Both fixtures carry the same scrubbed agent id, so the stop removes it.
        assert_eq!(t.on_event(&fx("subagent_stop_general"), 2 * SUBAGENT_STALE + 1), vec![Reaction::Report]);
        assert_eq!(t.display(2 * SUBAGENT_STALE + 1).subagents, 0);
    }

    fn in_sub(e: &str, g: &str) -> IpcMsg {
        IpcMsg { a: Some("general-purpose".into()), g: Some(g.into()), ..ev(e, "a") }
    }
    const REPORT: Mode = Mode::Working(Work::Report);

    #[test]
    fn starting_a_subagent_is_marked_however_quickly_the_call_returns() {
        let mut t = Tracker::new(0, 10);
        assert_eq!(t.on_event(&tool("PreToolUse", "a", "Agent"), 0), vec![Reaction::Spawn]);
        assert_eq!(t.on_event(&tool("PreToolUse", "a", "Task"), 1), vec![Reaction::Spawn]);
        t.on_event(&tool("PostToolUse", "a", "Agent"), 2); // a background agent returns at once
        assert_eq!(mode(&mut t, 2), Mode::Working(Work::Thinking));
        assert_eq!(t.display(2).subagents, 1, "still shown running");
        let inside = IpcMsg { t: Some("Agent".into()), ..in_sub("PreToolUse", "x") };
        assert!(t.on_event(&inside, 3).is_empty(), "a subagent's own subagent isn't Claude's");
    }

    #[test]
    fn a_subagent_finishing_is_a_report_clawd_reads_for_a_while() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("UserPromptSubmit", "a"), 0);
        t.on_event(&in_sub("PreToolUse", "x"), 1);
        assert_eq!(t.on_event(&in_sub("SubagentStop", "x"), 100), vec![Reaction::Report]);
        assert_eq!(mode(&mut t, 100), REPORT);
        assert_eq!(t.next_change(100), Some(100 + REPORT_FOR));
        t.on_event(&tool("PostToolUse", "a", "Agent"), 101); // the foreground order
        assert_eq!(mode(&mut t, 100 + REPORT_FOR - 1), REPORT);
        assert_eq!(mode(&mut t, 100 + REPORT_FOR), Mode::Working(Work::Thinking));
    }

    #[test]
    fn real_work_ends_the_reading_but_bookkeeping_doesnt() {
        let reading = || {
            let mut t = Tracker::new(0, 10);
            t.on_event(&ev("UserPromptSubmit", "a"), 0);
            t.on_event(&in_sub("SubagentStop", "x"), 1);
            t
        };
        let mut t = reading();
        t.on_event(&tool("PreToolUse", "a", "mcp__ccd_turn__update_plan"), 2);
        assert_eq!(mode(&mut t, 2), REPORT);
        let mut t = reading();
        t.on_event(&tool("PreToolUse", "a", "Edit"), 2);
        assert_eq!(mode(&mut t, 2), Mode::Working(Work::Tool(ToolAnim::Typing)));
        let mut t = reading();
        t.on_event(&ev("Stop", "a"), 2);
        assert_eq!(mode(&mut t, 2), Mode::Attention(Attention::YourTurn));
        let mut t = reading();
        t.on_event(&ev("PreCompact", "a"), 2);
        assert_eq!(mode(&mut t, 2), Mode::Working(Work::Compacting));
    }

    #[test]
    fn a_report_after_the_turn_ended_still_shows_then_fades() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&ev("Stop", "a"), 0);
        t.on_event(&in_sub("SubagentStop", "x"), 1);
        assert_eq!(mode(&mut t, 1), REPORT, "a background subagent came back");
        assert_eq!(mode(&mut t, 1 + REPORT_FOR), Mode::Attention(Attention::YourTurn));
    }

    #[test]
    fn a_permission_still_beats_a_report_and_helpers_dont_report() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&in_sub("SubagentStop", "x"), 0);
        t.on_event(&tool("PermissionRequest", "b", "Bash"), 1);
        assert_eq!(mode(&mut t, 1), Mode::Attention(Attention::Permission));
        let mut t = Tracker::new(0, 10);
        let helper = IpcMsg { g: Some("h".into()), ..ev("SubagentStop", "a") }; // empty agent_type
        assert!(t.on_event(&helper, 0).is_empty());
        assert_eq!(mode(&mut t, 0), Mode::Idle);
    }

    #[test]
    fn subagent_permission_only_cleared_by_that_subagent() {
        let mut t = Tracker::new(0, 10);
        let sub = |e: &str, g: &str| IpcMsg { a: Some("general-purpose".into()), g: Some(g.into()), ..tool(e, "a", "Bash") };
        t.on_event(&sub("PermissionRequest", "x"), 0);
        t.on_event(&sub("PostToolUse", "y"), 1);
        assert_eq!(mode(&mut t, 1), Mode::Attention(Attention::Permission));
        t.on_event(&sub("PostToolUse", "x"), 2);
        assert_ne!(mode(&mut t, 2), Mode::Attention(Attention::Permission));
    }

    #[test]
    fn ask_user_question_waits_for_input() {
        let mut t = Tracker::new(0, 10);
        // Live order (2026-11-07): PreToolUse, PermissionRequest 12 ms later,
        // Notification(permission_prompt) 6 s later, PostToolUse on answer.
        t.on_event(&tool("PreToolUse", "a", "AskUserQuestion"), 0);
        assert_eq!(mode(&mut t, 0), Mode::Attention(Attention::WaitingForInput));
        t.on_event(&tool("PermissionRequest", "a", "AskUserQuestion"), 12);
        t.on_event(&IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") }, 6012);
        assert_eq!(mode(&mut t, 6012), Mode::Attention(Attention::WaitingForInput), "a question is not a permission");
        t.on_event(&tool("PostToolUse", "a", "AskUserQuestion"), 18000);
        assert_eq!(mode(&mut t, 18000), Mode::Working(Work::Thinking));
    }

    #[test]
    fn late_permission_notification_only_fills_a_gap() {
        let mut t = Tracker::new(0, 10);
        let note = IpcMsg { n: Some("permission_prompt".into()), ..ev("Notification", "a") };
        // Pet started mid-prompt: the Notification is the only signal.
        t.on_event(&note, 0);
        assert_eq!(mode(&mut t, 0), Mode::Attention(Attention::Permission));
    }

    #[test]
    fn desktop_quit_ends_every_session_and_old_idle_sessions_drop() {
        let mut t = Tracker::new(0, 10);
        for s in ["a", "b", "c"] {
            t.on_event(&ev("SessionStart", s), 0);
        }
        t.on_event(&ev("SessionEnd", "a"), 1);
        assert_eq!(t.sessions().count(), 2);
        t.expire(SESSION_DROP + 1);
        assert_eq!(t.sessions().count(), 0);
    }

    #[test]
    fn bookkeeping_tools_look_like_thinking_and_tooltip_fits() {
        let mut t = Tracker::new(0, 10);
        t.on_event(&tool("PreToolUse", "a", "mcp__ccd_turn__update_plan"), 0);
        assert_eq!(mode(&mut t, 0), Mode::Working(Work::Thinking));
        for i in 0..20 {
            t.on_event(&tool("PreToolUse", &format!("s{i}"), "Bash"), i);
        }
        let tip = t.tooltip();
        assert!(tip.chars().count() <= 127 && tip.starts_with("proj-s19: running Bash"), "{tip}");
        assert_eq!(Tracker::new(0, 10).tooltip(), "Clawd: no Claude sessions");
    }
}
