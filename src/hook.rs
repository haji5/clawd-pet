//! `--hook` mode: Claude Code runs this on every subscribed event.
//!
//! Rules (see PLAN.md §1): never print anything, always exit 0, never block.
//! Only the event name, tool name, session id, cwd basename and a short
//! detail string ever leave this process (PLAN.md §1 has the full list).

use crate::flavour;
use crate::goto;
use crate::ipc::{self, IpcMsg};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

const STDIN_CAP: usize = 4 * 1024 * 1024;

/// Only the fields we need. Everything else in the payload (prompts, file
/// contents, tool output) is read past without being stored.
#[derive(Debug, Default)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub notification_type: Option<String>,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub trigger: Option<String>,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
    pub is_interrupt: Option<bool>,
    /// The chat's name in the desktop app.
    pub session_title: Option<String>,
    /// `tool_input.command`: a shell tool's command line. It is classified
    /// here and goes no further.
    pub command: Option<String>,
}

/// A JSON value of which only a string or a boolean is of any use. Anything
/// else, including a type Claude Code changes one day, is read past, so one
/// odd field never costs the whole event.
enum Loose {
    Text(String),
    Flag(bool),
    Other,
}

impl Loose {
    fn text(self) -> Option<String> {
        if let Loose::Text(s) = self { Some(s) } else { None }
    }
    fn flag(self) -> Option<bool> {
        if let Loose::Flag(b) = self { Some(b) } else { None }
    }
}

/// Reads any value as a `Loose`. With `command`, an object gives its
/// `command` string instead (`tool_input`), without storing the rest.
struct LooseVisitor {
    command: bool,
}

impl<'de> Visitor<'de> for LooseVisitor {
    type Value = Loose;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_str<E>(self, v: &str) -> Result<Loose, E> {
        Ok(Loose::Text(v.to_owned()))
    }
    fn visit_bool<E>(self, v: bool) -> Result<Loose, E> {
        Ok(Loose::Flag(v))
    }
    fn visit_i64<E>(self, _: i64) -> Result<Loose, E> {
        Ok(Loose::Other)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Loose, E> {
        Ok(Loose::Other)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Loose, E> {
        Ok(Loose::Other)
    }
    fn visit_unit<E>(self) -> Result<Loose, E> {
        Ok(Loose::Other)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Loose, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Loose::Other)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Loose, A::Error> {
        let mut found = Loose::Other;
        while let Some(key) = map.next_key::<String>()? {
            if self.command && key == "command" {
                found = map.next_value::<Loose>()?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(if matches!(found, Loose::Text(_)) { found } else { Loose::Other })
    }
}

impl<'de> Deserialize<'de> for Loose {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Loose, D::Error> {
        d.deserialize_any(LooseVisitor { command: false })
    }
}

/// `tool_input`, reduced to its `command`.
struct Command(Option<String>);

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Command, D::Error> {
        d.deserialize_any(LooseVisitor { command: true }).map(|c| Command(c.text()))
    }
}

/// Fills a `HookInput` field by field as the payload is read, so whatever
/// came before a parse error is still there afterwards.
struct Fill<'a>(&'a mut HookInput);

impl<'de> DeserializeSeed<'de> for Fill<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Fill<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a hook payload object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let input = self.0;
        while let Some(key) = map.next_key::<String>()? {
            if key == "tool_input" {
                input.command = map.next_value::<Command>()?.0;
                continue;
            }
            let text: &mut Option<String> = match key.as_str() {
                "hook_event_name" => {
                    input.hook_event_name = map.next_value::<Loose>()?.text().unwrap_or_default();
                    continue;
                }
                "session_id" => {
                    input.session_id = map.next_value::<Loose>()?.text().unwrap_or_default();
                    continue;
                }
                "is_interrupt" => {
                    input.is_interrupt = map.next_value::<Loose>()?.flag();
                    continue;
                }
                "cwd" => &mut input.cwd,
                "tool_name" => &mut input.tool_name,
                "notification_type" => &mut input.notification_type,
                "source" => &mut input.source,
                "reason" => &mut input.reason,
                "trigger" => &mut input.trigger,
                "agent_type" => &mut input.agent_type,
                "agent_id" => &mut input.agent_id,
                "session_title" => &mut input.session_title,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
            };
            *text = map.next_value::<Loose>()?.text();
        }
        Ok(())
    }
}

/// Strict: for tests and fixtures, where a payload that doesn't parse is a mistake.
impl<'de> Deserialize<'de> for HookInput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<HookInput, D::Error> {
        let mut input = HookInput::default();
        Fill(&mut input).deserialize(d)?;
        Ok(input)
    }
}

impl HookInput {
    /// What the hook does with stdin. A payload that stops making sense
    /// part-way (cut off at `STDIN_CAP`, say, by a huge tool result) still
    /// counts for what came before, as long as that names the event: the
    /// pet would otherwise show that tool as running until it goes stale.
    pub fn parse(bytes: &[u8]) -> Option<HookInput> {
        let mut input = HookInput::default();
        let _ = Fill(&mut input).deserialize(&mut serde_json::Deserializer::from_slice(bytes));
        (!input.hook_event_name.is_empty()).then_some(input)
    }
}

/// Only enum-like values (`permission_prompt`, `startup`, `manual`…) may be
/// forwarded as detail. Free text such as `PermissionDenied.reason` (the
/// classifier's explanation) must never leave the hook.
fn enum_like(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn basename(path: &str) -> &str {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Pure: payload → IPC message. `None` if the payload has no event name.
pub fn build_msg(input: &HookInput, ts: u64) -> Option<IpcMsg> {
    if input.hook_event_name.is_empty() {
        return None;
    }
    let detail = match input.hook_event_name.as_str() {
        "Notification" => input.notification_type.as_deref(),
        "SessionStart" => input.source.as_deref(),
        "SessionEnd" => input.reason.as_deref(),
        "PreCompact" | "PostCompact" => input.trigger.as_deref(),
        "PostToolUseFailure" if input.is_interrupt == Some(true) => Some("interrupt"),
        _ => None,
    }
    .filter(|d| enum_like(d));
    let non_empty = |o: &Option<String>| o.as_deref().filter(|s| !s.is_empty()).map(ipc::trunc);
    Some(IpcMsg {
        v: 1,
        e: ipc::trunc(&input.hook_event_name),
        s: ipc::trunc(&input.session_id),
        t: non_empty(&input.tool_name),
        n: detail.map(ipc::trunc),
        p: input.cwd.as_deref().map(|c| ipc::trunc(basename(c))),
        a: non_empty(&input.agent_type),
        g: non_empty(&input.agent_id),
        pp: None,
        ti: input.session_title.as_deref().filter(|_| says_who_hosts(&input.hook_event_name)).and_then(clean_title),
        ds: None,
        k: command_kind(input).map(String::from),
        ts,
    })
}

/// The class of a shell command that's about to run, if it has one.
fn command_kind(input: &HookInput) -> Option<&'static str> {
    if input.hook_event_name != "PreToolUse" || !matches!(input.tool_name.as_deref(), Some("Bash" | "PowerShell")) {
        return None;
    }
    flavour::classify(input.command.as_deref()?)
}

/// One short line: a newline would split the tooltip, a tab means something
/// in a menu.
fn clean_title(title: &str) -> Option<String> {
    let line: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    Some(ipc::trunc(line.trim())).filter(|t| !t.is_empty())
}

/// The events that say which process hosts the session. The host doesn't
/// change during a turn, so the rest don't repeat it.
fn says_who_hosts(event: &str) -> bool {
    matches!(event, "SessionStart" | "UserPromptSubmit")
}

fn add_host(m: &mut IpcMsg) {
    if !says_who_hosts(&m.e) {
        return;
    }
    // Set by the desktop app for its sessions.
    m.ds = std::env::var("CLAUDE_CODE_HOST_SESSION_ID").ok().filter(|id| goto::is_desktop_id(id));
    #[cfg(windows)]
    {
        m.pp = crate::win::process::parent_of_this_process();
    }
}

/// Read all of stdin, storing at most `STDIN_CAP` bytes but always draining.
fn read_stdin() -> Option<Vec<u8>> {
    let mut stdin = std::io::stdin().lock();
    let mut out = Vec::with_capacity(4096);
    let mut buf = [0u8; 16 * 1024];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) => {
                if out.len() < STDIN_CAP {
                    let take = n.min(STDIN_CAP - out.len());
                    out.extend_from_slice(&buf[..take]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return if out.is_empty() { None } else { Some(out) },
        }
    }
}

/// A session started or came back. `SessionStart(compact)` is neither, and
/// shouldn't bring back a pet the user quit earlier in that session.
fn starts_a_session(m: &IpcMsg) -> bool {
    m.e == "SessionStart" && m.n.as_deref() != Some("compact")
}

/// The only time the hook reads the config. A plain read: moving a bad file
/// aside is the pet's job, and an unreadable config means the default (on).
#[cfg(windows)]
fn launch_enabled() -> bool {
    let Some(path) = crate::config::default_path() else { return false };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    crate::config::Config::from_json(&text).unwrap_or_default().launch_with_claude
}

fn launch_with_claude(m: &IpcMsg) {
    if !starts_a_session(m) {
        return;
    }
    // The mutex check first: it's far cheaper than the config, and a pet is
    // usually already there.
    #[cfg(windows)]
    if !crate::win::launch::pet_running()
        && launch_enabled()
        && let Ok(seed) = serde_json::to_string(m)
    {
        // In the new pet's environment, not on its command line: that one
        // shows in Task Manager and in process-creation logs for as long
        // as the pet runs, and the seed names the project and the chat.
        crate::win::launch::spawn_self_detached(&[], &[(SEED_VAR, &seed)]);
    }
}

/// The `SessionStart` that made the hook launch the pet, as wire JSON.
pub const SEED_VAR: &str = "CLAWD_PET_SEED";

/// The seed a hook left for this pet, taken out of the environment so that
/// nothing started from here (a "Go to" link) inherits it.
pub fn take_seed() -> Option<IpcMsg> {
    let seed = std::env::var(SEED_VAR).ok()?;
    // SAFETY: called first thing in pet mode, before any other thread exists.
    unsafe { std::env::remove_var(SEED_VAR) };
    serde_json::from_str(&seed).ok()
}

pub struct Flags {
    /// Debug builds only (Phase 0's payload logger): append raw payloads
    /// to `%TEMP%\clawd-hook-log.jsonl`. They contain prompts and tool
    /// input, so the code isn't in a release exe at all.
    #[cfg(debug_assertions)]
    pub log_raw: bool,
    /// `--port N`: the installer's way to pass a non-default port. Hooks run
    /// in exec form (no shell), so an env var can't go in the `command`.
    pub port: Option<u16>,
}

fn body(flags: &Flags) {
    #[cfg(debug_assertions)]
    let start = std::time::Instant::now();
    let raw = read_stdin();
    let parsed = raw.as_deref().and_then(HookInput::parse);
    let mut msg = parsed.as_ref().and_then(|p| build_msg(p, unix_now()));
    if let Some(m) = &mut msg {
        add_host(m);
        ipc::send(m, flags.port.unwrap_or_else(ipc::port));
        launch_with_claude(m);
    }
    #[cfg(debug_assertions)]
    if flags.log_raw {
        log_raw(raw.as_deref(), &msg, start.elapsed().as_micros() as u64);
    }
}

#[cfg(debug_assertions)]
fn log_raw(raw: Option<&[u8]>, msg: &Option<IpcMsg>, elapsed_us: u64) {
    let raw_val: serde_json::Value = match raw {
        None => serde_json::Value::Null,
        Some(b) => serde_json::from_slice(b).unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(b).into())),
    };
    let line = serde_json::json!({
        "logged_at_ms": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        "elapsed_us": elapsed_us,
        "pid": std::process::id(),
        "stdin_ok": raw.is_some(),
        "msg": msg,
        "raw": raw_val,
    });
    // One write_all per line: concurrent hook processes interleaved
    // their output when writeln! split the line into several writes.
    let path = std::env::temp_dir().join("clawd-hook-log.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// Entry point. Never returns, always exits 0.
pub fn run(flags: Flags) -> ! {
    // With panic = "abort", a panic would print to stderr and exit non-zero.
    // The hook replaces that with a silent exit(0); catch_unwind covers
    // unwinding builds (debug / tests).
    std::panic::set_hook(Box::new(|_| std::process::exit(0)));
    let _ = std::panic::catch_unwind(|| body(&flags));
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_compact_msg() {
        let input: HookInput = serde_json::from_str(
            r#"{"hook_event_name":"PreToolUse","session_id":"s1","cwd":"C:\\code\\my-app\\",
                "tool_name":"Bash","tool_input":{"command":"secret"}}"#,
        )
        .unwrap();
        let m = build_msg(&input, 7).unwrap();
        assert_eq!(m.e, "PreToolUse");
        assert_eq!(m.t.as_deref(), Some("Bash"));
        assert_eq!(m.p.as_deref(), Some("my-app"));
        let wire = serde_json::to_string(&m).unwrap();
        assert!(!wire.contains("secret"));
        assert!(wire.len() <= 512);
    }

    fn fixture(name: &str) -> IpcMsg {
        let path = format!("{}/tests/fixtures/hooks/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        build_msg(&serde_json::from_str(&text).unwrap(), 0).unwrap()
    }

    #[test]
    fn every_fixture_builds_a_small_msg() {
        let dir = format!("{}/tests/fixtures/hooks", env!("CARGO_MANIFEST_DIR"));
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let input: HookInput =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let wire = serde_json::to_string(&build_msg(&input, 1_800_000_000).unwrap()).unwrap();
            assert!(wire.len() <= 512, "{path:?}: {wire}");
            assert!(!wire.contains("example prompt") && !wire.contains("Denied by"), "{wire}");
            n += 1;
        }
        assert!(n >= 20);
    }

    #[test]
    fn detail_per_event() {
        assert_eq!(fixture("session_start_resume").n.as_deref(), Some("resume"));
        assert_eq!(fixture("session_start_compact").n.as_deref(), Some("compact"));
        assert_eq!(fixture("session_end_prompt_input_exit").n.as_deref(), Some("prompt_input_exit"));
        assert_eq!(fixture("pre_compact_manual").n.as_deref(), Some("manual"));
        assert_eq!(fixture("post_tool_use_failure_bash").n, None); // is_interrupt=false
        // Free-text classifier reason must not be forwarded.
        assert_eq!(fixture("permission_denied_auto_mode").n, None);
    }

    #[test]
    fn subagent_fields() {
        let m = fixture("pre_tool_use_in_subagent");
        assert_eq!(m.a.as_deref(), Some("general-purpose"));
        assert!(m.g.is_some());
        // Internal helper agents have an empty agent_type: dropped.
        let m = fixture("subagent_stop_internal");
        assert_eq!(m.a, None);
        assert!(m.g.is_some());
        assert_eq!(fixture("pre_tool_use_read").g, None);
    }

    #[test]
    fn only_a_real_session_start_launches_the_pet() {
        assert!(starts_a_session(&fixture("session_start_startup")));
        assert!(starts_a_session(&fixture("session_start_resume")));
        assert!(!starts_a_session(&fixture("session_start_compact")));
        assert!(!starts_a_session(&fixture("stop")));
    }

    #[test]
    fn the_chat_name_comes_along_on_the_two_host_events() {
        assert_eq!(fixture("user_prompt_submit_desktop").ti.as_deref(), Some("Example session"));
        assert_eq!(fixture("session_start_resume").ti.as_deref(), Some("Example session"));
        assert_eq!(fixture("user_prompt_submit").ti, None);
        let input = HookInput { hook_event_name: "Stop".into(), session_title: Some("x".into()), ..Default::default() };
        assert_eq!(build_msg(&input, 0).unwrap().ti, None);
    }

    #[test]
    fn a_chat_name_is_one_short_line() {
        assert_eq!(clean_title("  fix\tthe\nbug ").as_deref(), Some("fix the bug"));
        assert_eq!(clean_title(" \n ").as_deref(), None);
        assert_eq!(clean_title(&"x".repeat(200)).unwrap().chars().count(), ipc::MAX_FIELD);
    }

    #[test]
    fn only_two_events_look_up_the_host() {
        assert!(says_who_hosts("SessionStart") && says_who_hosts("UserPromptSubmit"));
        assert!(!says_who_hosts("PreToolUse") && !says_who_hosts("Stop"));
    }

    fn shell(event: &str, tool: &str, input: serde_json::Value) -> IpcMsg {
        let payload = serde_json::json!({"hook_event_name": event, "session_id": "s", "tool_name": tool, "tool_input": input});
        build_msg(&serde_json::from_value(payload).unwrap(), 0).unwrap()
    }

    #[test]
    fn a_shell_command_is_reduced_to_its_class() {
        let cmd = serde_json::json!({"command": "cargo test --package hunter2"});
        let m = shell("PreToolUse", "Bash", cmd.clone());
        assert_eq!(m.k.as_deref(), Some("test"));
        assert!(!serde_json::to_string(&m).unwrap().contains("hunter2"));
        assert_eq!(shell("PreToolUse", "PowerShell", cmd.clone()).k.as_deref(), Some("test"));
        assert_eq!(shell("PostToolUse", "Bash", cmd.clone()).k, None, "only when it starts");
        assert_eq!(shell("PreToolUse", "Write", cmd).k, None, "only shell tools");
        assert_eq!(shell("PreToolUse", "Bash", serde_json::json!({"command": "ls"})).k, None);
    }

    #[test]
    fn an_odd_tool_input_doesnt_lose_the_event() {
        for input in [serde_json::json!("text"), serde_json::json!(3), serde_json::json!({"command": ["a"]})] {
            let m = shell("PreToolUse", "Bash", input);
            assert_eq!((m.e.as_str(), m.k), ("PreToolUse", None));
        }
    }

    #[test]
    fn missing_event_is_none() {
        assert!(build_msg(&HookInput::default(), 0).is_none());
    }

    #[test]
    fn a_payload_cut_off_part_way_still_gives_its_event() {
        // As when a tool result is bigger than the hook's cap on stdin.
        let whole = r#"{"session_id":"s1","hook_event_name":"PostToolUse","tool_name":"Read","tool_response":"aaaaaaaaaaaa"}"#;
        let cut = &whole.as_bytes()[..whole.len() - 8];
        let m = build_msg(&HookInput::parse(cut).unwrap(), 0).unwrap();
        assert_eq!((m.e.as_str(), m.s.as_str(), m.t.as_deref()), ("PostToolUse", "s1", Some("Read")));
        // Cut before the event is named, or not a payload at all: nothing to send.
        for bad in [&whole.as_bytes()[..20], &b"not json {{{"[..], &b""[..], &b"[1,2]"[..], &b"\"text\""[..]] {
            assert!(HookInput::parse(bad).is_none(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_command_cut_off_part_way_is_not_classified() {
        let cut = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test --pack"#;
        assert_eq!(build_msg(&HookInput::parse(cut).unwrap(), 0).unwrap().k, None);
    }

    #[test]
    fn fields_of_an_unexpected_type_are_skipped_not_fatal() {
        let odd = br#"{"hook_event_name":"PostToolUseFailure","session_id":"s","cwd":null,"tool_name":["x"],
            "agent_type":7,"agent_id":{"a":1},"is_interrupt":"yes","session_title":false,"tool_input":[{"command":"git status"}]}"#;
        let input = HookInput::parse(odd).unwrap();
        assert_eq!((input.cwd, input.tool_name, input.agent_type, input.agent_id), (None, None, None, None));
        assert_eq!((input.is_interrupt, input.session_title, input.command), (None, None, None));
        assert_eq!(input.session_id, "s");
    }

    #[test]
    fn only_the_command_is_kept_from_a_tool_input() {
        let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash",
            "tool_input":{"description":"secret one","command":"git push","nested":{"command":"cargo test"},"timeout":5}}"#;
        let input = HookInput::parse(payload).unwrap();
        assert_eq!(input.command.as_deref(), Some("git push"));
        assert!(!format!("{input:?}").contains("secret"));
    }
}
