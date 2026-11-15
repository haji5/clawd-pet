//! `%APPDATA%\clawd-pet\config.json` (PLAN.md §7). Missing or unknown keys
//! fall back to defaults; a file that doesn't parse is renamed to `.bad`.

use crate::placement::Position;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Placement {
    /// Above all normal windows.
    #[default]
    AlwaysOnTop,
    /// A normal window; jumps to the top only while Claude needs the user.
    AttentionOnly,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Behaviours {
    pub perch: bool,
    pub follow_active_window: bool,
    pub eyes_follow_cursor: bool,
    pub walk: bool,
    pub hide_fullscreen: bool,
    pub attention_in_fullscreen: bool,
    pub animate_while_asleep: bool,
    /// Moods, hats and the morning mug.
    pub time_of_day: bool,
    /// A soft chime when a session starts waiting on the user.
    pub attention_sound: bool,
    /// Hide while the Claude desktop app is closed to the tray, and quit
    /// when it quits.
    pub follow_claude_app: bool,
}

impl Default for Behaviours {
    fn default() -> Self {
        Behaviours {
            perch: true,
            follow_active_window: false,
            eyes_follow_cursor: true,
            walk: false,
            hide_fullscreen: true,
            attention_in_fullscreen: false,
            animate_while_asleep: false,
            time_of_day: true,
            attention_sound: false,
            follow_claude_app: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Size 1–6: window pixels per art pixel at 100 % DPI (× DPI/96, rounded).
    /// Replaced `scale` when the art went 2x; an old `scale` key is ignored.
    pub size: u32,
    pub position: Position,
    pub placement: Placement,
    pub launch_with_claude: bool,
    pub port: u16,
    pub behaviours: Behaviours,
    pub perch_deny_processes: Vec<String>,
    pub do_not_disturb: bool,
    pub sleep_after_minutes: u32,
    /// How long a permission prompt can be ignored before Clawd gets
    /// impatient. 0 = never.
    pub impatient_after_seconds: u32,
    /// How long the "!" lasts when nothing says the prompt is still open
    /// (answering one sends no event). 0 = until the session's next event.
    pub permission_answered_after_seconds: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            size: 2,
            position: Position::default(),
            placement: Placement::AlwaysOnTop,
            launch_with_claude: true,
            port: crate::ipc::DEFAULT_PORT,
            behaviours: Behaviours::default(),
            perch_deny_processes: vec!["steam.exe".into(), "EpicGamesLauncher.exe".into()],
            do_not_disturb: false,
            sleep_after_minutes: 10,
            impatient_after_seconds: 30,
            permission_answered_after_seconds: 10,
        }
    }
}

pub const MIN_SIZE: u32 = 1;
pub const MAX_SIZE: u32 = 6;

impl Config {
    /// Parse, then force values into valid ranges. A setting of the wrong
    /// type (`"port": "auto"`) only costs that setting; the error is for a
    /// file that isn't a JSON object at all.
    pub fn from_json(text: &str) -> Result<Config, serde_json::Error> {
        let mut c = match serde_json::from_str::<Config>(text) {
            Ok(c) => c,
            Err(e) => {
                let Ok(given @ Value::Object(_)) = serde_json::from_str::<Value>(text) else { return Err(e) };
                let mut good = serde_json::to_value(Config::default())?;
                keep_what_parses(&mut good, &mut Vec::new(), &given);
                serde_json::from_value(good)?
            }
        };
        c.size = c.size.clamp(MIN_SIZE, MAX_SIZE);
        Ok(c)
    }
}

fn at_path<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |v, key| v.get_mut(*key))
}

/// Copy every setting of `given` into `good` (a whole, valid config) that
/// leaves it valid, one setting at a time, going into nested objects.
fn keep_what_parses<'a>(good: &mut Value, path: &mut Vec<&'a str>, given: &'a Value) {
    let Value::Object(settings) = given else { return };
    for (key, value) in settings {
        path.push(key);
        match at_path(good, path) {
            None => {} // a key this version doesn't know
            Some(slot) if slot.is_object() && value.is_object() => keep_what_parses(good, path, value),
            Some(_) => {
                let mut trial = good.clone();
                if let Some(slot) = at_path(&mut trial, path) {
                    *slot = value.clone();
                }
                if serde_json::from_value::<Config>(trial.clone()).is_ok() {
                    *good = trial;
                }
            }
        }
        path.pop();
    }
}

/// Write over `disk` whatever differs between `before` and `after`, and add
/// what `disk` lacks. Everything else in `disk` stays as it is.
fn carry_changes(disk: &mut Value, before: &Value, after: &Value) {
    let (Value::Object(disk), Value::Object(after)) = (disk, after) else { return };
    for (key, new) in after {
        let old = before.get(key);
        match disk.get_mut(key) {
            None => {
                disk.insert(key.clone(), new.clone());
            }
            Some(slot) if slot.is_object() && new.is_object() => carry_changes(slot, old.unwrap_or(&Value::Null), new),
            Some(slot) => {
                if old != Some(new) {
                    *slot = new.clone();
                }
            }
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("clawd-pet").join("config.json"))
}

/// Load the config. Never fails: a missing file gives defaults; a bad one is
/// moved aside to `config.json.bad` and defaults are used.
pub fn load(path: &Path) -> Config {
    let Ok(text) = std::fs::read_to_string(path) else { return Config::default() };
    match Config::from_json(&text) {
        Ok(c) => c,
        Err(e) => {
            crate::dlog!("config {}: {e}; moving it to .bad", path.display());
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Config::default()
        }
    }
}

/// Write atomically (temp file + rename). Only what Clawd itself changed
/// goes over the file: the settings that differ between `synced` (the
/// config as last loaded or saved) and `now`. So an edit made by hand
/// while the pet runs isn't undone by its next save; it takes effect at
/// the next start. Keys this version doesn't know are kept too.
///
/// Not flushed before the rename, on purpose (unlike `install::save`): this
/// runs on the UI thread, up to once a second after a drag, where a flush
/// can stall a frame, and a `config.json` cut short by a power cut heals
/// itself (`load` moves a file that isn't a JSON object to `.bad` and uses
/// the defaults).
pub fn save(path: &Path, synced: &Config, now: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = serde_json::to_value(now)?;
    if let Ok(text) = std::fs::read_to_string(path)
        && let Ok(mut disk @ Value::Object(_)) = serde_json::from_str::<Value>(&text)
    {
        carry_changes(&mut disk, &serde_json::to_value(synced)?, &out);
        out = disk;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&out)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_missing_keys_and_unknown_are_ignored() {
        let c = Config::from_json(r#"{"size": 9, "scale": 2, "placement": "attentionOnly", "whatever": 1,
            "position": {"anchor": "topLeft", "dx": 5}}"#)
        .unwrap();
        assert_eq!(c.size, MAX_SIZE);
        assert_eq!(c.placement, Placement::AttentionOnly);
        assert_eq!(c.position.dx, 5);
        assert!(c.position.on_taskbar); // default kept
        assert!(c.launch_with_claude);
        assert_eq!(c.port, 47823);
    }

    #[test]
    fn roundtrip_uses_camel_case() {
        let j = serde_json::to_string(&Config::default()).unwrap();
        assert!(j.contains(r#""launchWithClaude":true"#) && j.contains(r#""anchor":"bottomRight""#), "{j}");
        assert_eq!(Config::from_json(&j).unwrap(), Config::default());
    }

    #[test]
    fn bad_file_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("clawd-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load(&p), Config::default());
        assert!(!p.exists() && dir.join("config.json.bad").exists());
        save(&p, &Config::default(), &Config { size: 4, ..Default::default() }).unwrap();
        assert_eq!(load(&p).size, 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_bad_setting_only_costs_that_setting() {
        let c = Config::from_json(
            r#"{"size": 3, "port": "auto", "sleepAfterMinutes": -5, "doNotDisturb": true,
                "behaviours": {"walk": true, "perch": "sometimes", "eyesFollowCursor": false},
                "position": {"anchor": "middle", "dx": 7}, "placement": 4, "perchDenyProcesses": ["a.exe"]}"#,
        )
        .unwrap();
        let d = Config::default();
        assert_eq!((c.size, c.do_not_disturb, &c.perch_deny_processes[..]), (3, true, &["a.exe".to_string()][..]));
        assert_eq!((c.port, c.sleep_after_minutes, c.placement), (d.port, d.sleep_after_minutes, d.placement), "the bad ones are the defaults");
        assert_eq!((c.behaviours.walk, c.behaviours.perch, c.behaviours.eyes_follow_cursor), (true, d.behaviours.perch, false));
        assert_eq!((c.position.dx, c.position.anchor), (7, d.position.anchor));
        // A whole section of the wrong type, and files that aren't a config at all.
        assert_eq!(Config::from_json(r#"{"behaviours": 5, "size": 4}"#).unwrap(), Config { size: 4, ..Config::default() });
        for bad in ["{ not json", "[1, 2]", "\"text\"", ""] {
            assert!(Config::from_json(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_file_with_a_bad_setting_is_not_thrown_away() {
        let dir = std::env::temp_dir().join(format!("clawd-cfg-lenient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        std::fs::write(&p, r#"{"size": 5, "port": 70000}"#).unwrap();
        assert_eq!(load(&p).size, 5);
        assert!(p.exists() && !dir.join("config.json.bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_keeps_what_was_edited_by_hand_meanwhile() {
        let dir = std::env::temp_dir().join(format!("clawd-cfg-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        let started = Config::default();
        save(&p, &started, &started).unwrap();
        // By hand, while the pet runs: a setting, a nested one, a key of some other version.
        let mut disk: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        disk["sleepAfterMinutes"] = 3.into();
        disk["behaviours"]["walk"] = true.into();
        disk["futureSetting"] = "kept".into();
        disk["size"] = 6.into();
        std::fs::write(&p, disk.to_string()).unwrap();
        // The pet then saves what it changed itself: a drag, a menu choice.
        let mut now = started.clone();
        now.position.dx = 300;
        now.behaviours.perch = false;
        now.size = 4;
        save(&p, &started, &now).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let c = load(&p);
        assert_eq!((c.sleep_after_minutes, c.behaviours.walk), (3, true), "hand edits survive");
        assert_eq!((c.position.dx, c.behaviours.perch), (300, false), "the pet's changes are written");
        assert_eq!(c.size, 4, "and win where both changed the same setting");
        assert!(text.contains("futureSetting"), "{text}");
        // Nothing to merge with: the whole config is written.
        std::fs::write(&p, "{ not json").unwrap();
        save(&p, &started, &now).unwrap();
        assert_eq!(load(&p), now);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
