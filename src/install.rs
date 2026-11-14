//! Installing Clawd (PLAN.md §7): the exe in `%LOCALAPPDATA%\clawd-pet\` and
//! the hook entries in `~/.claude/settings.json`.
//!
//! `merge` and `remove` are pure; the file functions around them only ever
//! replace a settings file that parsed, after backing it up.

use serde_json::{Value, json};
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const EXE_NAME: &str = "clawd-pet.exe";

/// The events Clawd subscribes to (PLAN.md §5).
pub const EVENTS: [&str; 14] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "PreCompact",
    "PostCompact",
    "SessionEnd",
];

/// These events don't support a matcher.
const NO_MATCHER: [&str; 2] = ["UserPromptSubmit", "Stop"];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Merged {
    pub added: usize,
    /// Our entry was already there but pointed elsewhere (a moved exe, a new port).
    pub updated: usize,
}

impl Merged {
    fn changed(self) -> bool {
        self.added + self.updated > 0
    }
}

/// A `clawd-pet.exe` (in any folder) run with `--hook`.
fn is_ours(hook: &Value) -> bool {
    let name = hook.get("command").and_then(Value::as_str).and_then(|c| c.rsplit(['\\', '/']).next());
    let first_arg = hook.get("args").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str);
    name.is_some_and(|n| n.eq_ignore_ascii_case(EXE_NAME)) && first_arg == Some("--hook")
}

fn desired_hook(exe: &str, port: Option<u16>) -> Value {
    let mut args = vec!["--hook".to_string()];
    if let Some(p) = port {
        args.extend(["--port".to_string(), p.to_string()]);
    }
    json!({ "type": "command", "command": exe, "args": args, "async": true, "timeout": 5 })
}

/// Run `keep` on each of our hooks under one event. Hooks it rejects are
/// removed, and so is a matcher group that leaves empty; a group that was
/// empty to begin with isn't ours to touch.
fn edit_ours(groups: &mut Vec<Value>, mut keep: impl FnMut(&mut Value) -> bool) {
    groups.retain_mut(|group| {
        let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
        let before = list.len();
        list.retain_mut(|h| !is_ours(h) || keep(h));
        !list.is_empty() || list.len() == before
    });
}

/// Add our hook to every event, once. An entry of ours that differs (the exe
/// moved, the port changed) is corrected in place rather than duplicated.
pub fn merge(root: &mut Value, exe: &str, port: Option<u16>) -> Result<Merged, String> {
    let want = desired_hook(exe, port);
    let root = root.as_object_mut().ok_or("the top level isn't a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({})).as_object_mut().ok_or("\"hooks\" isn't a JSON object")?;
    let mut merged = Merged::default();
    for event in EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{event}\" isn't a JSON array"))?;
        let (mut found, mut updated) = (false, false);
        edit_ours(groups, |h| {
            if found {
                updated = true;
                return false; // a duplicate
            }
            found = true;
            if *h != want {
                *h = want.clone();
                updated = true;
            }
            true
        });
        if !found {
            let mut group = serde_json::Map::new();
            if !NO_MATCHER.contains(&event) {
                group.insert("matcher".into(), json!("*"));
            }
            group.insert("hooks".into(), json!([want]));
            groups.push(Value::Object(group));
            merged.added += 1;
        } else if updated {
            merged.updated += 1;
        }
    }
    Ok(merged)
}

/// Remove only our hooks, from every event. Returns how many events had one.
pub fn remove(root: &mut Value) -> usize {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else { return 0 };
    let mut events = 0;
    hooks.retain(|_, groups| {
        let Some(list) = groups.as_array_mut() else { return true };
        let mut removed = false;
        edit_ours(list, |_| {
            removed = true;
            false
        });
        events += removed as usize;
        !(removed && list.is_empty())
    });
    if events > 0 && hooks.is_empty() {
        // `retain`, not `remove`: with preserve_order, `remove` reorders keys.
        if let Some(root) = root.as_object_mut() {
            root.retain(|k, _| k != "hooks");
        }
    }
    events
}

// ---- files ----

pub fn settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".claude")))?;
    Some(dir.join("settings.json"))
}

pub fn installed_exe() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("clawd-pet").join(EXE_NAME))
}

/// The file's bytes and its parsed value; `None` if there is no file.
/// A blank file counts as an empty object. Anything else that doesn't parse
/// is an error, and the caller leaves the file alone.
fn read_settings(path: &Path) -> Result<Option<(Vec<u8>, Value)>, String> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read {}: {e}", path.display())),
    };
    let text = bytes.strip_prefix(BOM).unwrap_or(&bytes);
    let root = if text.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(text)
            .map_err(|e| format!("{} isn't valid JSON ({e}), so it was left unchanged. Fix it, then try again.", path.display()))?
    };
    Ok(Some((bytes, root)))
}

fn write_backup(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let stamp = crate::hook::unix_now();
    // Above every number already used in this second, even by a backup that
    // has since been pruned away, so `backups` can order them by name.
    let first = backup_files(path).filter_map(|(_, order)| order).filter(|&(s, _)| s == stamp).map(|(_, n)| n + 1).max().unwrap_or(0);
    for n in first..first + 100 {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let candidate = path.with_file_name(format!("{name}{BACKUP_MARK}{stamp}{suffix}"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many backups"))
}

/// Every install and uninstall that changes the file leaves a backup. The
/// newest few are kept; older ones are removed, or they'd pile up for good.
const KEEP_BACKUPS: usize = 5;

/// Clawd's backups of the settings file at `path`, newest first. Backups
/// written in the same clock tick have the same modification time, so a tie
/// goes by the stamp and number in the name.
fn backups(path: &Path) -> Vec<PathBuf> {
    let mut found: Vec<_> = backup_files(path)
        .filter_map(|(path, order)| Some((fs::metadata(&path).ok()?.modified().ok()?, order, path)))
        .collect();
    found.sort_by(|a, b| b.cmp(a));
    found.into_iter().map(|(_, _, path)| path).collect()
}

/// Every backup of the settings file at `path`, with its `backup_order`.
fn backup_files(path: &Path) -> impl Iterator<Item = (PathBuf, Option<(u64, u32)>)> {
    let prefix = format!("{}{BACKUP_MARK}", path.file_name().unwrap_or_default().to_string_lossy());
    path.parent()
        .and_then(|dir| fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(move |e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix).then(|| (e.path(), backup_order(&name)))
        })
}

const BACKUP_MARK: &str = ".clawd-backup-";

/// The stamp and number `write_backup` put in a backup's name
/// (`….clawd-backup-<stamp>` or `…-<stamp>-<n>`), compared as numbers so
/// that `-10` comes after `-9`. `None` for a name it didn't write.
fn backup_order(file_name: &str) -> Option<(u64, u32)> {
    let rest = &file_name[file_name.rfind(BACKUP_MARK)? + BACKUP_MARK.len()..];
    let (stamp, n) = rest.split_once('-').unwrap_or((rest, "0"));
    Some((stamp.parse().ok()?, n.parse().ok()?))
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The indentation the file already uses: the whitespace that starts its
/// first indented line. Two spaces when that can't be told.
fn indent_of(text: &[u8]) -> &[u8] {
    text.split(|&b| b == b'\n')
        .find_map(|line| {
            let n = line.iter().take_while(|&&b| b == b' ' || b == b'\t').count();
            (n > 0 && line.get(n).is_some_and(|&b| b != b'\r')).then(|| &line[..n])
        })
        .filter(|indent| indent.len() <= 8)
        .unwrap_or(b"  ")
}

/// The settings as file bytes, in the style of the file they replace: its
/// indentation, line endings, final newline and byte order mark. Spacing
/// inside a line can't be kept (the file is written out again from its
/// parsed form); `uninstall_from` has a way around that.
fn render(root: &Value, original: Option<&[u8]>) -> Result<Vec<u8>, serde_json::Error> {
    use serde::Serialize;
    let mut json = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(original.map_or(b"  ", indent_of));
    root.serialize(&mut serde_json::Serializer::with_formatter(&mut json, formatter))?;
    if original.is_none_or(|b| b.ends_with(b"\n")) {
        json.push(b'\n');
    }
    let crlf = original.is_some_and(|b| b.windows(2).any(|w| w == b"\r\n"));
    let mut out = if original.is_some_and(|b| b.starts_with(BOM)) { BOM.to_vec() } else { Vec::new() };
    for b in json {
        if b == b'\n' && crlf {
            out.push(b'\r');
        }
        out.push(b);
    }
    Ok(out)
}

/// Back up the old file, then replace it atomically.
fn save(path: &Path, original: Option<&[u8]>, new: &[u8]) -> Result<Option<PathBuf>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("Could not write {}: {e}. It was left unchanged.", path.display());
    let backup = original.map(|bytes| write_backup(path, bytes)).transpose().map_err(|e| failed(&e))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| failed(&e))?;
    }
    // Through a symlink (dotfile managers): replace the real file, not the link.
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = real.with_extension("json.clawd-tmp");
    let written = fs::write(&tmp, new).and_then(|()| fs::rename(&tmp, &real));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(failed(&e));
    }
    for old in backups(path).into_iter().skip(KEEP_BACKUPS) {
        let _ = fs::remove_file(old);
    }
    Ok(backup)
}

fn install_into(path: &Path, exe: &str, port: Option<u16>) -> Result<(Merged, Option<PathBuf>), String> {
    let existing = read_settings(path)?;
    let (original, mut root) = match existing {
        Some((bytes, root)) => (Some(bytes), root),
        None => (None, json!({})),
    };
    let unchanged = |e: &dyn std::fmt::Display| format!("{}: {e}, so it was left unchanged.", path.display());
    let merged = merge(&mut root, exe, port).map_err(|e| unchanged(&e))?;
    if !merged.changed() {
        return Ok((merged, None));
    }
    let new = render(&root, original.as_deref()).map_err(|e| unchanged(&e))?;
    Ok((merged, save(path, original.as_deref(), &new)?))
}

fn uninstall_from(path: &Path) -> Result<(usize, Option<PathBuf>), String> {
    let Some((bytes, mut root)) = read_settings(path)? else { return Ok((0, None)) };
    let events = remove(&mut root);
    if events == 0 {
        return Ok((0, None));
    }
    // A backup that holds exactly these settings is the file from before
    // Clawd's hooks went in: put back its very bytes, however it was laid
    // out. Without one (settings changed since, backup deleted) the file is
    // written out again in its own style.
    let as_before = backups(path).into_iter().find_map(|backup| {
        let (old, settings) = read_settings(&backup).ok()??;
        (settings == root).then_some(old)
    });
    let new = match as_before {
        Some(old) => old,
        None => render(&root, Some(&bytes)).map_err(|e| format!("{}: {e}, so it was left unchanged.", path.display()))?,
    };
    Ok((events, save(path, Some(&bytes), &new)?))
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn differs(from: &Path, to: &Path) -> bool {
    if same_file(from, to) {
        return false;
    }
    !matches!((fs::read(from), fs::read(to)), (Ok(a), Ok(b)) if a == b)
}

/// A file beside `exe` named after it: `<name>.new`, `<name>.old-<pid>`.
fn beside(exe: &Path, suffix: &str) -> PathBuf {
    exe.with_file_name(format!("{}.{suffix}", exe.file_name().unwrap_or_default().to_string_lossy()))
}

/// Put `new`, a finished file beside `to`, in `to`'s place. The old exe may
/// be running (the pet, or a hook firing right now): it can't be overwritten
/// but it can be renamed, and `sweep_leftovers` removes it once it's free.
/// On failure `to` is as it was and `new` is gone.
fn swap_in(new: &Path, to: &Path) -> io::Result<()> {
    let old = beside(to, &format!("old-{}", std::process::id()));
    let moved_old = to.exists();
    let swapped = if moved_old { fs::rename(to, &old) } else { Ok(()) }.and_then(|()| {
        fs::rename(new, to).inspect_err(|_| {
            if moved_old {
                let _ = fs::rename(&old, to);
            }
        })
    });
    if let Err(e) = swapped {
        let _ = fs::remove_file(new);
        return Err(e);
    }
    sweep_leftovers(to);
    Ok(())
}

/// Remove what earlier swaps left beside `exe` (`<name>.old-*`, `<name>.new`)
/// and return how many are still there: an old exe that a process still
/// runs can't be deleted yet.
pub fn sweep_leftovers(exe: &Path) -> usize {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else { return 0 };
    let name = name.to_string_lossy().to_ascii_lowercase();
    let (old, new) = (format!("{name}.old-"), format!("{name}.new"));
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.starts_with(&old) || n == new
        })
        .filter(|e| fs::remove_file(e.path()).is_err())
        .count()
}

/// Copy the exe into place; false if it was already there.
fn place_exe(from: &Path, to: &Path) -> io::Result<bool> {
    if !differs(from, to) {
        return Ok(false);
    }
    let dir = to.parent().ok_or_else(|| io::Error::other("no install folder"))?;
    fs::create_dir_all(dir)?;
    let new = beside(to, "new");
    if let Err(e) = fs::copy(from, &new) {
        let _ = fs::remove_file(&new);
        return Err(e);
    }
    swap_in(&new, to)?;
    Ok(true)
}

/// Write `bytes` over the exe at `to` ("Check for updates"), the same way.
pub fn place_bytes(bytes: &[u8], to: &Path) -> io::Result<()> {
    let dir = to.parent().ok_or_else(|| io::Error::other("no folder"))?;
    fs::create_dir_all(dir)?;
    let new = beside(to, "new");
    if let Err(e) = fs::write(&new, bytes) {
        let _ = fs::remove_file(&new);
        return Err(e);
    }
    swap_in(&new, to)
}

/// What the first start of a new version did (`refresh_after_update`).
#[derive(Debug, Default)]
pub struct AfterUpdate {
    /// The hook entries in `settings.json` were out of date and were rewritten.
    pub hooks_updated: bool,
    /// Something that couldn't be done, as a sentence for the box.
    pub problem: Option<String>,
    /// Leftovers beside the exes that couldn't be removed yet.
    pub leftovers: usize,
    /// The exes to sweep beside again later.
    pub exes: Vec<PathBuf>,
}

/// The first start after "Check for updates" replaced this exe: sweep the
/// old one away, bring the installed copy level if this is another file,
/// and rewrite Clawd's hooks if they are installed and out of date. Hooks
/// that aren't installed are never added.
pub fn refresh_after_update() -> AfterUpdate {
    let mut done = AfterUpdate::default();
    let me = std::env::current_exe().ok();
    done.exes.extend(me.clone());
    if let (Some(me), Some(installed)) = (&me, installed_exe())
        && installed.exists()
        && !same_file(me, &installed)
    {
        if let Err(e) = ensure_installed() {
            done.problem = Some(format!("{e}."));
        }
        done.exes.push(installed);
    }
    let status = hook_status();
    if status.can_uninstall && status.can_install {
        match install_hooks() {
            Ok(_) => done.hooks_updated = true,
            Err(e) => done.problem = Some(e),
        }
    }
    done.leftovers = done.exes.iter().map(|e| sweep_leftovers(e)).sum();
    done
}

/// Makes sure the running exe is the installed copy, so hooks and the Run
/// key never point at a download that may move. Also returns whether it
/// had to copy.
pub fn ensure_installed() -> Result<(PathBuf, bool), String> {
    let to = installed_exe().ok_or("LOCALAPPDATA isn't set")?;
    let from = std::env::current_exe().map_err(|e| format!("Could not find this program's own path: {e}"))?;
    let copied = place_exe(&from, &to).map_err(|e| format!("Could not copy Clawd to {}: {e}", to.display()))?;
    Ok((to, copied))
}

/// `Some(port)` only when it isn't the default: hooks run without a shell,
/// so the port can only reach them through `args`.
fn configured_port() -> Option<u16> {
    let port = crate::config::default_path().as_deref().map(crate::config::load).unwrap_or_default().port;
    (port != crate::ipc::DEFAULT_PORT).then_some(port)
}

// ---- what the tray and the command line call ----

fn with_backup(text: String, backup: Option<PathBuf>) -> String {
    match backup {
        Some(b) => format!("{text}\n\nBackup of your settings:\n{}", b.display()),
        None => text,
    }
}

/// `--install`.
pub fn install_exe() -> Result<String, String> {
    let (exe, copied) = ensure_installed()?;
    let what = if copied { "Installed Clawd to" } else { "Clawd is already installed at" };
    Ok(format!("{what}\n{}", exe.display()))
}

/// `--install-hooks` and the tray item.
pub fn install_hooks() -> Result<String, String> {
    let (exe, copied) = ensure_installed()?;
    let path = settings_path().ok_or("USERPROFILE isn't set")?;
    let (m, backup) = install_into(&path, &exe.to_string_lossy(), configured_port())?;
    let text = if m.changed() {
        let what = if m.added > 0 { "Installed" } else { "Updated" };
        format!("{what} Clawd's Claude hooks ({} events). Running Claude Code sessions pick them up right away.", m.added + m.updated)
    } else if copied {
        format!("Updated Clawd at {}. Its Claude hooks were already installed.", exe.display())
    } else {
        "Clawd's Claude hooks are already installed.".to_string()
    };
    Ok(with_backup(text, backup))
}

/// `--uninstall-hooks` and the tray item.
pub fn uninstall_hooks() -> Result<String, String> {
    let path = settings_path().ok_or("USERPROFILE isn't set")?;
    let (events, backup) = uninstall_from(&path)?;
    let text = if events > 0 {
        format!("Removed Clawd's Claude hooks ({events} events).")
    } else {
        "Clawd's Claude hooks aren't installed, so there was nothing to remove.".to_string()
    };
    Ok(with_backup(text, backup))
}

/// Which of the two tray items make sense right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HookStatus {
    pub can_install: bool,
    pub can_uninstall: bool,
}

/// A dry run over the real file. An unreadable or corrupt file leaves
/// "install" available, so that clicking it explains the problem.
pub fn hook_status() -> HookStatus {
    let nothing = HookStatus { can_install: true, can_uninstall: false };
    let (Some(path), Some(exe)) = (settings_path(), installed_exe()) else { return nothing };
    let Ok(Some((_, root))) = read_settings(&path) else { return nothing };
    let out_of_date = merge(&mut root.clone(), &exe.to_string_lossy(), configured_port()).map_or(true, Merged::changed);
    let exe_is_stale = std::env::current_exe().is_ok_and(|me| differs(&me, &exe));
    HookStatus { can_install: out_of_date || exe_is_stale, can_uninstall: remove(&mut root.clone()) > 0 }
}

/// The files `hook_status` reads (the settings, both exes, the config for
/// the port), each with its modification time and length.
#[derive(PartialEq)]
pub struct StatusKey(Vec<(Option<PathBuf>, Option<FileStamp>)>);

/// When a file was last written, and its length.
type FileStamp = (SystemTime, u64);

impl StatusKey {
    pub fn now() -> StatusKey {
        let files = [settings_path(), installed_exe(), std::env::current_exe().ok(), crate::config::default_path()];
        StatusKey(
            files
                .into_iter()
                .map(|p| {
                    let stamp = p.as_ref().and_then(|p| fs::metadata(p).ok()).and_then(|m| Some((m.modified().ok()?, m.len())));
                    (p, stamp)
                })
                .collect(),
        )
    }
}

/// `hook_status`, worked out again only when one of the files it reads has
/// changed since the last answer in `cache`. Opening the menu otherwise
/// re-read the settings and, for a pet not run from the installed copy,
/// both exes, every time.
pub fn hook_status_cached(cache: &mut Option<(StatusKey, HookStatus)>) -> HookStatus {
    let key = StatusKey::now();
    match cache {
        Some((k, status)) if *k == key => *status,
        _ => {
            let status = hook_status();
            *cache = Some((key, status));
            status
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\me\AppData\Local\clawd-pet\clawd-pet.exe";

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn hook_lists(root: &Value, event: &str) -> Vec<Value> {
        root["hooks"][event].as_array().unwrap().iter().flat_map(|g| g["hooks"].as_array().unwrap().clone()).collect()
    }

    fn ours_in(root: &Value, event: &str) -> Vec<Value> {
        hook_lists(root, event).into_iter().filter(is_ours).collect()
    }

    /// A folder of its own for one test; removed by the test when it passes.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clawd-install-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        names.sort();
        names
    }

    fn fixture(name: &str) -> Vec<u8> {
        fs::read(format!("{}/tests/fixtures/settings/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn recognises_only_our_hook() {
        let hook = |command: &str, args: Value| json!({ "type": "command", "command": command, "args": args });
        assert!(is_ours(&hook(EXE, json!(["--hook"]))));
        assert!(is_ours(&hook(EXE, json!(["--hook", "--port", "5000"]))));
        assert!(is_ours(&hook("C:/x/CLAWD-PET.EXE", json!(["--hook"]))));
        assert!(!is_ours(&hook(r"C:\x\not-clawd-pet.exe", json!(["--hook"]))));
        assert!(!is_ours(&hook(EXE, json!(["--demo"]))));
        assert!(!is_ours(&json!({ "type": "command", "command": EXE })));
        assert!(!is_ours(&json!({ "type": "prompt", "prompt": "clawd-pet.exe" })));
    }

    #[test]
    fn merge_writes_the_planned_shape() {
        let mut root = json!({});
        assert_eq!(merge(&mut root, EXE, None).unwrap(), Merged { added: 14, updated: 0 });
        let want = json!({ "type": "command", "command": EXE, "args": ["--hook"], "async": true, "timeout": 5 });
        for event in EVENTS {
            let groups = root["hooks"][event].as_array().unwrap();
            assert_eq!(groups.len(), 1, "{event}");
            assert_eq!(groups[0]["hooks"], json!([want.clone()]), "{event}");
            let matcher = groups[0].get("matcher");
            assert_eq!(matcher.is_none(), event == "UserPromptSubmit" || event == "Stop", "{event}");
            if let Some(m) = matcher {
                assert_eq!(m, "*");
            }
        }
    }

    #[test]
    fn merge_is_idempotent() {
        let mut root = json!({});
        merge(&mut root, EXE, Some(5000)).unwrap();
        let first = root.clone();
        assert_eq!(merge(&mut root, EXE, Some(5000)).unwrap(), Merged::default());
        assert_eq!(root, first);
        assert_eq!(ours_in(&root, "Stop")[0]["args"], json!(["--hook", "--port", "5000"]));
    }

    #[test]
    fn merge_corrects_a_moved_exe_or_new_port_in_place() {
        let mut root = json!({});
        merge(&mut root, r"C:\old\clawd-pet.exe", None).unwrap();
        assert_eq!(merge(&mut root, EXE, Some(5000)).unwrap(), Merged { added: 0, updated: 14 });
        for event in EVENTS {
            let ours = ours_in(&root, event);
            assert_eq!(ours.len(), 1, "{event}");
            assert_eq!(ours[0]["command"], EXE);
        }
    }

    #[test]
    fn merge_keeps_other_hooks_and_drops_our_duplicates() {
        let mut root = parse(
            r#"{"hooks":{"Stop":[
                {"hooks":[{"type":"command","command":"C:\\a\\clawd-pet.exe","args":["--hook"]},
                          {"type":"command","command":"notify.exe"}]},
                {"hooks":[{"type":"command","command":"C:\\b\\clawd-pet.exe","args":["--hook"]}]}
            ]}}"#,
        );
        merge(&mut root, EXE, None).unwrap();
        assert_eq!(ours_in(&root, "Stop").len(), 1);
        let groups = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "the group emptied by removing the duplicate is gone");
        assert_eq!(groups[0]["hooks"][1]["command"], "notify.exe");
    }

    #[test]
    fn merge_refuses_shapes_it_does_not_understand() {
        for bad in ["[]", r#"{"hooks":[]}"#, r#"{"hooks":{"Stop":{}}}"#] {
            assert!(merge(&mut parse(bad), EXE, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn remove_takes_only_our_entries() {
        let mut root = parse(
            r#"{"model":"x","hooks":{
                "Stop":[{"hooks":[{"type":"command","command":"notify.exe"},
                                  {"type":"command","command":"C:\\a\\clawd-pet.exe","args":["--hook"]}]}],
                "PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"C:\\a\\clawd-pet.exe","args":["--hook"]}]}],
                "PostToolUse":[]
            }}"#,
        );
        assert_eq!(remove(&mut root), 2);
        assert_eq!(hook_lists(&root, "Stop").len(), 1);
        assert_eq!(hook_lists(&root, "Stop")[0]["command"], "notify.exe");
        assert!(root["hooks"].get("PreToolUse").is_none(), "emptied by us");
        assert!(root["hooks"].get("PostToolUse").is_some(), "was already empty: not ours to touch");
        assert_eq!(remove(&mut root), 0);
    }

    #[test]
    fn remove_drops_hooks_only_when_we_emptied_it() {
        let mut root = json!({ "a": 1 });
        merge(&mut root, EXE, None).unwrap();
        assert_eq!(remove(&mut root), 14);
        assert_eq!(root, json!({ "a": 1 }));
        let mut already_empty = json!({ "hooks": {} });
        assert_eq!(remove(&mut already_empty), 0);
        assert_eq!(already_empty, json!({ "hooks": {} }));
    }

    #[test]
    fn install_then_uninstall_gives_back_the_same_bytes() {
        for name in ["empty_object.json", "unrelated_keys.json", "other_hooks.json", "crlf_no_final_newline.json"] {
            let dir = temp_dir(&format!("roundtrip-{name}"));
            let path = dir.join("settings.json");
            let original = fixture(name);
            fs::write(&path, &original).unwrap();

            let (merged, backup) = install_into(&path, EXE, None).unwrap();
            assert_eq!(merged.added, 14, "{name}");
            assert_eq!(fs::read(backup.unwrap()).unwrap(), original, "{name}: backup");
            let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(ours_in(&installed, "Stop").len(), 1, "{name}");
            assert!(!files_in(&dir).iter().any(|f| f.contains("clawd-tmp")), "{name}: temp file left");

            let (events, _) = uninstall_from(&path).unwrap();
            assert_eq!(events, 14, "{name}");
            assert_eq!(fs::read_to_string(&path).unwrap(), String::from_utf8(original).unwrap(), "{name}");
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Install then uninstall in a folder of its own; the bytes in between
    /// and at the end. `forget` deletes the backups before uninstalling.
    fn round_trip(name: &str, original: &[u8], forget: bool) -> (Vec<u8>, Vec<u8>) {
        let dir = temp_dir(name);
        let path = dir.join("settings.json");
        fs::write(&path, original).unwrap();
        assert_eq!(install_into(&path, EXE, None).unwrap().0.added, 14, "{name}");
        let installed = fs::read(&path).unwrap();
        if forget {
            backups(&path).iter().for_each(|b| fs::remove_file(b).unwrap());
        }
        assert_eq!(uninstall_from(&path).unwrap().0, 14, "{name}");
        let after = fs::read(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        (installed, after)
    }

    #[test]
    fn a_file_laid_out_its_own_way_comes_back_byte_for_byte() {
        let odd = b"{ \"theme\":\"dark\",\r\n\t\"list\": [1,2,3], \"n\": 1.50,\r\n\t\"nested\": {\"a\":{}}\r\n}";
        let (installed, after) = round_trip("odd-layout", odd, false);
        assert_eq!(String::from_utf8(after).unwrap(), String::from_utf8(odd.to_vec()).unwrap());
        let text = String::from_utf8(installed).unwrap();
        assert!(text.contains("\r\n\t\"hooks\": {\r\n\t\t\"SessionStart\""), "tabs and CRLF kept while installed: {text}");
    }

    #[test]
    fn without_a_backup_the_file_keeps_its_own_indentation() {
        let four = "{\n    \"theme\": \"dark\",\n    \"env\": {\n        \"A\": \"1\"\n    }\n}\n";
        let (installed, after) = round_trip("four-spaces", four.as_bytes(), true);
        assert_eq!(String::from_utf8(after).unwrap(), four);
        assert!(String::from_utf8(installed).unwrap().contains("\n    \"hooks\": {\n        \"SessionStart\": [\n            {"));
    }

    #[test]
    fn settings_changed_since_the_install_are_never_replaced_by_a_backup() {
        let dir = temp_dir("changed-since");
        let path = dir.join("settings.json");
        fs::write(&path, "{ \"theme\":\"dark\" }").unwrap();
        install_into(&path, EXE, None).unwrap();
        let mut root: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        root["theme"] = json!("light");
        fs::write(&path, serde_json::to_vec(&root).unwrap()).unwrap();
        uninstall_from(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\n  \"theme\": \"light\"\n}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_byte_order_mark_is_kept() {
        let (installed, after) = round_trip("bom", b"\xEF\xBB\xBF{\n  \"a\": 1\n}\n", true);
        assert!(installed.starts_with(BOM) && after.starts_with(BOM));
        assert_eq!(&after[3..], b"{\n  \"a\": 1\n}\n");
    }

    #[test]
    fn reads_the_indentation_a_file_uses() {
        assert_eq!(indent_of(b"{\n    \"a\": 1\n}"), b"    ");
        assert_eq!(indent_of(b"{\r\n\t\"a\": {\r\n\t\t\"b\": 1\r\n\t}\r\n}"), b"\t");
        for unknown in [&b"{}"[..], b"{\"a\":1}", b"{\n\"a\": 1\n}", b"{\n   \n}", b"{\n                \"a\": 1\n}"] {
            assert_eq!(indent_of(unknown), b"  ", "{}", String::from_utf8_lossy(unknown));
        }
    }

    #[test]
    fn old_backups_are_cleared_out() {
        let dir = temp_dir("prune");
        let path = dir.join("settings.json");
        fs::write(&path, "{}\n").unwrap();
        fs::write(dir.join("settings.json.bak"), "mine").unwrap();
        for _ in 0..KEEP_BACKUPS + 2 {
            install_into(&path, EXE, None).unwrap();
            uninstall_from(&path).unwrap();
        }
        let left = files_in(&dir);
        assert_eq!(left.iter().filter(|f| f.contains(".clawd-backup-")).count(), KEEP_BACKUPS, "{left:?}");
        assert!(left.contains(&"settings.json.bak".to_string()), "only Clawd's own backups go");
        // The newest is the one just written: the installed state.
        let newest: Value = serde_json::from_slice(&fs::read(&backups(&path)[0]).unwrap()).unwrap();
        assert_eq!(ours_in(&newest, "Stop").len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn other_settings_keep_their_order() {
        let mut root: Value = serde_json::from_slice(&fixture("unrelated_keys.json")).unwrap();
        let before: Vec<String> = root.as_object().unwrap().keys().cloned().collect();
        merge(&mut root, EXE, None).unwrap();
        let after: Vec<String> = root.as_object().unwrap().keys().cloned().collect();
        assert_eq!(after[..before.len()], before[..]);
    }

    #[test]
    fn a_file_with_dev_hooks_is_taken_over() {
        let dir = temp_dir("dev");
        let path = dir.join("settings.json");
        fs::write(&path, fixture("dev_hooks.json")).unwrap();
        let (merged, _) = install_into(&path, EXE, None).unwrap();
        assert_eq!(merged, Merged { added: 12, updated: 2 });
        let root: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(ours_in(&root, "SessionStart").len(), 1);
        assert_eq!(ours_in(&root, "SessionStart")[0]["command"], EXE);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn malformed_settings_are_refused_and_untouched() {
        let dir = temp_dir("malformed");
        let path = dir.join("settings.json");
        let bad = fixture("malformed.json");
        fs::write(&path, &bad).unwrap();
        let err = install_into(&path, EXE, None).unwrap_err();
        assert!(err.contains("settings.json") && err.contains("left unchanged"), "{err}");
        assert!(uninstall_from(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), bad);
        assert_eq!(files_in(&dir), ["settings.json"], "no backup or temp file");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_or_blank_settings_are_created() {
        let dir = temp_dir("missing");
        let path = dir.join(".claude").join("settings.json");
        assert_eq!(uninstall_from(&path).unwrap(), (0, None));
        assert!(!path.exists(), "uninstall doesn't create it");
        let (merged, backup) = install_into(&path, EXE, None).unwrap();
        assert_eq!((merged.added, backup), (14, None));
        assert!(fs::read_to_string(&path).unwrap().ends_with("}\n"));

        fs::write(&path, " \n").unwrap();
        assert_eq!(install_into(&path, EXE, None).unwrap().0.added, 14);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_change_means_no_backup() {
        let dir = temp_dir("nochange");
        let path = dir.join("settings.json");
        fs::write(&path, "{}\n").unwrap();
        install_into(&path, EXE, None).unwrap();
        let (merged, backup) = install_into(&path, EXE, None).unwrap();
        assert_eq!((merged, backup), (Merged::default(), None));
        assert_eq!(files_in(&dir).len(), 2, "the settings and the one backup");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backup_names_order_by_stamp_then_number() {
        assert_eq!(backup_order("settings.json.clawd-backup-1700000000"), Some((1_700_000_000, 0)));
        assert_eq!(backup_order("settings.json.clawd-backup-1700000000-9"), Some((1_700_000_000, 9)));
        assert!(backup_order("settings.json.clawd-backup-1700000000-10") > backup_order("settings.json.clawd-backup-1700000000-9"));
        assert!(backup_order("settings.json.clawd-backup-1700000001") > backup_order("settings.json.clawd-backup-1700000000-99"));
        for odd in ["settings.json.clawd-backup-", "settings.json.clawd-backup-x", "settings.json.clawd-backup-1-x", "settings.json"] {
            assert_eq!(backup_order(odd), None, "{odd}");
        }
    }

    #[test]
    fn a_pruned_number_is_not_used_again() {
        let dir = temp_dir("renumber");
        let path = dir.join("settings.json");
        let first = write_backup(&path, b"one").unwrap();
        let second = write_backup(&path, b"two").unwrap();
        fs::remove_file(&first).unwrap();
        let third = write_backup(&path, b"three").unwrap();
        let order = |p: &Path| backup_order(&p.file_name().unwrap().to_string_lossy()).unwrap();
        // In the same second, the third must still sort after the second.
        if order(&third).0 == order(&second).0 {
            assert!(order(&third) > order(&second), "{third:?} vs {second:?}");
        }
        assert_eq!(backups(&path)[0], third);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backups_never_overwrite_each_other() {
        let dir = temp_dir("backups");
        let path = dir.join("settings.json");
        let first = write_backup(&path, b"one").unwrap();
        let second = write_backup(&path, b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"one");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn places_the_exe_and_replaces_only_when_it_differs() {
        let dir = temp_dir("exe");
        let from = dir.join("download.exe");
        let to = dir.join("installed").join(EXE_NAME);
        fs::write(&from, b"v1").unwrap();
        assert!(place_exe(&from, &to).unwrap());
        assert_eq!(fs::read(&to).unwrap(), b"v1");
        assert!(!place_exe(&from, &to).unwrap(), "same bytes");
        assert!(!place_exe(&to, &to).unwrap(), "already running from there");

        fs::write(&from, b"v2").unwrap();
        assert!(place_exe(&from, &to).unwrap());
        assert_eq!(fs::read(&to).unwrap(), b"v2");
        assert_eq!(files_in(to.parent().unwrap()), [EXE_NAME], "no leftovers");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn places_bytes_where_there_was_no_file_or_an_old_one() {
        let dir = temp_dir("bytes");
        let to = dir.join("sub").join("clawd-pet (1).exe");
        place_bytes(b"first", &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"first");
        place_bytes(b"second", &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"second");
        assert_eq!(files_in(to.parent().unwrap()), ["clawd-pet (1).exe"], "no leftovers, whatever the exe is called");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_running_exe_is_swapped_out_and_swept_once_it_has_stopped() {
        let dir = temp_dir("running");
        let to = dir.join(EXE_NAME);
        // A real program, running from `to` while it's replaced.
        fs::copy(std::env::var_os("ComSpec").expect("ComSpec"), &to).unwrap();
        let mut running = std::process::Command::new(&to).args(["/c", "ping -n 30 127.0.0.1 >nul"]).spawn().unwrap();
        place_bytes(b"MZ new", &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"MZ new");
        let old = format!("{EXE_NAME}.old-{}", std::process::id());
        assert_eq!(files_in(&dir), [EXE_NAME.to_string(), old.clone()], "the running one is only renamed");
        assert_eq!(sweep_leftovers(&to), 1, "still running");
        running.kill().unwrap();
        running.wait().unwrap();
        assert_eq!(sweep_leftovers(&to), 0);
        assert_eq!(files_in(&dir), [EXE_NAME]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_sweep_takes_only_leftovers_of_that_exe() {
        let dir = temp_dir("sweep");
        for name in [
            "clawd-pet (1).exe",
            "clawd-pet (1).exe.new",
            "clawd-pet (1).exe.old-123",
            "Clawd-Pet (1).EXE.OLD-9",
            "clawd-pet.exe",
            "clawd-pet.exe.old-5",
            "clawd-pet (1).exe.newer",
            "notes.txt",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(sweep_leftovers(&dir.join("clawd-pet (1).exe")), 0);
        assert_eq!(files_in(&dir), ["clawd-pet (1).exe", "clawd-pet (1).exe.newer", "clawd-pet.exe", "clawd-pet.exe.old-5", "notes.txt"]);
        assert_eq!(sweep_leftovers(&dir.join("clawd-pet.exe")), 0);
        assert_eq!(files_in(&dir), ["clawd-pet (1).exe", "clawd-pet (1).exe.newer", "clawd-pet.exe", "notes.txt"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
