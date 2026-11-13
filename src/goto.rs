//! "Go to the session that needs you" (PLAN.md §8 item 5b): which window
//! hosts a session. Pure; the process list and windows come from `win/`.
//!
//! The rule is to never send the user somewhere wrong. A pid is only trusted
//! together with the creation time it had, and when in doubt there's no
//! answer, so the menu item is greyed out.

/// A process, identified so that a reused pid can't pass for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Host {
    pub pid: u32,
    /// Creation time (`FILETIME` ticks).
    pub created: u64,
}

/// One entry of the process list.
pub struct Proc {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
}

/// A top-level window, in z-order (topmost first) in the lists used here.
#[derive(Clone, Copy, Debug, Default)]
pub struct TopWindow {
    pub id: usize,
    pub pid: u32,
    /// Has an owner: a dialog or popup of some other window.
    pub owned: bool,
    pub tool: bool,
    /// Hidden by the app itself. Not the cloaking that puts a window on
    /// another virtual desktop: that one is a fine place to go to.
    pub cloaked_by_app: bool,
    /// No area, and not because it's minimized.
    pub empty: bool,
}

impl TopWindow {
    pub fn is_app_window(&self) -> bool {
        !self.owned && !self.tool && !self.cloaked_by_app && !self.empty
    }
}

/// Starting a program from Explorer makes it the parent, and it owns every
/// folder window. Never a host, so the walk ends there.
const SHELL: &str = "explorer.exe";

/// The host and its ancestors, nearest first; empty if `host` is not that
/// process any more. `created` reads a process's creation time.
///
/// Windows keeps a dead parent's pid in the child's entry, and that pid can
/// be reused, so every step has to be older than the process before it.
pub fn chain(host: Host, procs: &[Proc], created: impl Fn(u32) -> Option<u64>) -> Vec<u32> {
    if created(host.pid) != Some(host.created) {
        return Vec::new();
    }
    let find = |pid: u32| procs.iter().find(|p| p.pid == pid);
    let mut out = vec![host.pid];
    let (mut pid, mut born) = (host.pid, host.created);
    while let Some(parent) = find(pid).and_then(|p| find(p.parent)) {
        if parent.pid == 0 || out.contains(&parent.pid) || parent.name.eq_ignore_ascii_case(SHELL) {
            break;
        }
        match created(parent.pid) {
            Some(t) if t <= born => {
                out.push(parent.pid);
                (pid, born) = (parent.pid, t);
            }
            _ => break,
        }
    }
    out
}

/// The desktop app's own process, for a session that lives in it: the one
/// that owns its window, or with the window closed to the tray, the oldest
/// ancestor if that is the app by name. `None` rather than a guess: Clawd
/// hides and quits along with this process.
pub fn app_process(chain: &[u32], procs: &[Proc], windows: &[TopWindow]) -> Option<u32> {
    if let Some(id) = pick(chain, windows) {
        return windows.iter().find(|w| w.id == id).map(|w| w.pid);
    }
    let root = *chain.last()?;
    procs.iter().any(|p| p.pid == root && p.name.eq_ignore_ascii_case(DESKTOP_APP)).then_some(root)
}

const DESKTOP_APP: &str = "claude.exe";

/// The topmost app window of the nearest process in `chain` that has one.
pub fn pick(chain: &[u32], windows: &[TopWindow]) -> Option<usize> {
    chain.iter().find_map(|&pid| windows.iter().find(|w| w.pid == pid && w.is_app_window()).map(|w| w.id))
}

/// The desktop app's id for a session: `local_` and letters, digits, dashes.
/// It ends up in a link handed to the shell, so nothing else gets through.
pub fn is_desktop_id(id: &str) -> bool {
    id.strip_prefix("local_").is_some_and(|rest| (1..=64).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// A menu has room for this many.
pub const MAX_TARGETS: usize = 9;

/// A session the user can be taken to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The chat's name, or the project when there's no name.
    pub project: String,
    /// What it's waiting for; `None` = it isn't waiting, just the latest.
    pub waiting: Option<&'static str>,
    pub host: Option<Host>,
    pub desktop_id: Option<String>,
}

impl Target {
    /// Opens this chat in the desktop app (the link its own taskbar jump
    /// list uses). An id it doesn't know just opens its Code page.
    pub fn link(&self) -> Option<String> {
        self.desktop_id.as_deref().filter(|id| is_desktop_id(id)).map(|id| format!("claude://code/continue?session={id}"))
    }

    /// Menu text. A single `&` would underline the next letter.
    pub fn menu_text(&self, found: bool) -> String {
        let project = self.project.replace('&', "&&");
        let waiting = self.waiting.map(|w| format!(" ({w})")).unwrap_or_default();
        let missing = if found { "" } else { ": window not found" };
        format!("Go to {project}{waiting}{missing}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: u32, name: &str) -> Proc {
        Proc { pid, parent, name: name.into() }
    }
    fn window(id: usize, pid: u32) -> TopWindow {
        TopWindow { id, pid, ..Default::default() }
    }
    /// Born in pid order: 10 is older than 20, as parents are.
    fn by_pid(pid: u32) -> Option<u64> {
        Some(pid as u64)
    }
    fn host(pid: u32) -> Host {
        Host { pid, created: pid as u64 }
    }

    /// Claude Code in a terminal: claude → shell → terminal → Explorer.
    fn terminal() -> Vec<Proc> {
        vec![proc(30, 20, "claude.exe"), proc(20, 10, "pwsh.exe"), proc(10, 5, "WindowsTerminal.exe"), proc(5, 1, "explorer.exe")]
    }

    #[test]
    fn walks_up_to_the_process_with_the_window() {
        assert_eq!(chain(host(30), &terminal(), by_pid), [30, 20, 10]);
        let windows = [window(7, 99), window(8, 10)];
        assert_eq!(pick(&chain(host(30), &terminal(), by_pid), &windows), Some(8));
    }

    #[test]
    fn explorer_windows_are_never_a_host() {
        let procs = [proc(20, 10, "claude.exe"), proc(10, 5, "cmd.exe"), proc(5, 1, "EXPLORER.EXE")];
        let c = chain(host(20), &procs, by_pid);
        assert_eq!(c, [20, 10]);
        assert_eq!(pick(&c, &[window(1, 5)]), None, "not a random folder window");
    }

    #[test]
    fn a_reused_pid_never_counts_as_the_host() {
        let reused = Host { pid: 30, created: 1 };
        assert!(chain(reused, &terminal(), by_pid).is_empty());
    }

    #[test]
    fn a_process_that_is_gone_is_no_host() {
        assert!(chain(host(30), &terminal(), |_| None).is_empty());
    }

    #[test]
    fn a_dead_parents_reused_pid_ends_the_walk() {
        // The terminal (10) is gone and an unrelated app was given its pid,
        // after the shell started.
        let procs = [proc(30, 20, "claude.exe"), proc(20, 10, "pwsh.exe"), proc(10, 5, "game.exe")];
        let created = |pid| if pid == 10 { Some(u64::MAX) } else { by_pid(pid) };
        let c = chain(host(30), &procs, created);
        assert_eq!(c, [30, 20]);
        assert_eq!(pick(&c, &[window(1, 10)]), None);
    }

    #[test]
    fn an_ancestor_that_cant_be_checked_ends_the_walk() {
        let created = |pid| if pid == 10 { None } else { by_pid(pid) };
        assert_eq!(chain(host(30), &terminal(), created), [30, 20]);
    }

    #[test]
    fn broken_process_lists_end_the_walk() {
        let same_age = |_| Some(5);
        let loop_of_two = [proc(1, 2, "a.exe"), proc(2, 1, "b.exe")];
        assert_eq!(chain(Host { pid: 1, created: 5 }, &loop_of_two, same_age), [1, 2]);
        let own_parent = [proc(1, 1, "a.exe")];
        assert_eq!(chain(Host { pid: 1, created: 5 }, &own_parent, same_age), [1]);
        let idle = [proc(4, 0, "System"), proc(0, 0, "[System Process]")];
        assert_eq!(chain(Host { pid: 4, created: 5 }, &idle, same_age), [4]);
        assert_eq!(chain(Host { pid: 9, created: 5 }, &[], same_age), [9], "not in the list");
    }

    #[test]
    fn the_host_itself_can_own_the_window() {
        assert_eq!(pick(&[30, 20], &[window(1, 20), window(2, 30)]), Some(2), "nearest first");
    }

    #[test]
    fn only_real_app_windows_count() {
        let w = |t: TopWindow| TopWindow { id: 1, pid: 10, ..t };
        for bad in [
            w(TopWindow { owned: true, ..Default::default() }),
            w(TopWindow { tool: true, ..Default::default() }),
            w(TopWindow { cloaked_by_app: true, ..Default::default() }),
            w(TopWindow { empty: true, ..Default::default() }),
        ] {
            assert_eq!(pick(&[10], &[bad]), None, "{bad:?}");
        }
        assert_eq!(pick(&[10], &[window(1, 10)]), Some(1));
    }

    #[test]
    fn the_topmost_of_several_windows_wins() {
        assert_eq!(pick(&[10], &[window(5, 10), window(6, 10)]), Some(5));
    }

    #[test]
    fn the_desktop_app_is_the_window_owner_or_the_root_named_claude() {
        let procs = [proc(30, 20, "claude.exe"), proc(20, 10, "Claude.exe"), proc(10, 5, "explorer.exe")];
        let c = chain(host(30), &procs, by_pid);
        assert_eq!(c, [30, 20]);
        assert_eq!(app_process(&c, &procs, &[window(1, 20)]), Some(20), "its window is open");
        assert_eq!(app_process(&c, &procs, &[]), Some(20), "closed to the tray: by name");
        // A CLI in a terminal with no window to be found: no guessing.
        assert_eq!(app_process(&chain(host(30), &terminal(), by_pid), &terminal(), &[]), None);
        assert_eq!(app_process(&[], &procs, &[]), None);
    }

    #[test]
    fn only_a_real_desktop_id_becomes_a_link() {
        let link = |id: &str| Target { project: "p".into(), waiting: None, host: None, desktop_id: Some(id.into()) }.link();
        assert_eq!(link("local_2abc-07").as_deref(), Some("claude://code/continue?session=local_2abc-07"));
        for bad in ["", "local_", "2abc", "local_a b", "local_a&q=x", "local_a/../b", "LOCAL_a", &format!("local_{}", "a".repeat(65))] {
            assert_eq!(link(bad), None, "{bad}");
        }
        assert_eq!(Target { project: "p".into(), waiting: None, host: None, desktop_id: None }.link(), None);
    }

    #[test]
    fn menu_text_says_what_is_going_on() {
        let t = |project: &str, waiting| Target { project: project.into(), waiting, host: None, desktop_id: None };
        assert_eq!(t("my-app", Some("needs permission")).menu_text(true), "Go to my-app (needs permission)");
        assert_eq!(t("my-app", None).menu_text(true), "Go to my-app");
        assert_eq!(t("my-app", None).menu_text(false), "Go to my-app: window not found");
        assert_eq!(t("R&D", None).menu_text(true), "Go to R&&D");
    }
}
