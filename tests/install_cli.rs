//! Runs the real binary's installer commands against a throwaway profile:
//! `USERPROFILE`, `LOCALAPPDATA` and `APPDATA` all point into a temp folder,
//! so the user's own `~/.claude/settings.json` is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Profile {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("profile-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        Profile(dir)
    }

    fn settings(&self) -> PathBuf {
        self.0.join(".claude").join("settings.json")
    }

    fn exe(&self) -> PathBuf {
        self.0.join("local").join("clawd-pet").join("clawd-pet.exe")
    }

    fn run(&self, flag: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_clawd-pet"))
            .args([flag, "--quiet"])
            .env("USERPROFILE", &self.0)
            .env("LOCALAPPDATA", self.0.join("local"))
            .env("APPDATA", self.0.join("roaming"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap()
    }
}

fn hooks(p: &Profile) -> serde_json::Value {
    serde_json::from_slice::<serde_json::Value>(&std::fs::read(p.settings()).unwrap()).unwrap()["hooks"].clone()
}

#[test]
fn install_use_uninstall_leaves_the_settings_as_they_were() {
    let p = Profile::new("roundtrip");
    let original = "{\n  \"theme\": \"dark\"\n}\n";
    std::fs::write(p.settings(), original).unwrap();

    assert_eq!(p.run("--install-hooks").status.code(), Some(0));
    assert!(p.exe().exists(), "hooks point at the installed copy, so it has to be there");
    let installed = hooks(&p);
    assert_eq!(installed.as_object().unwrap().len(), 14);
    assert_eq!(installed["Stop"][0]["hooks"][0]["command"], p.exe().to_str().unwrap());

    let again = p.run("--install-hooks");
    assert_eq!(again.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&again.stdout).contains("already installed"));

    assert_eq!(p.run("--uninstall-hooks").status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(p.settings()).unwrap(), original);
}

#[test]
fn a_custom_port_reaches_the_hook_arguments() {
    let p = Profile::new("port");
    std::fs::create_dir_all(p.0.join("roaming").join("clawd-pet")).unwrap();
    std::fs::write(p.0.join("roaming").join("clawd-pet").join("config.json"), r#"{"port":5151}"#).unwrap();
    assert_eq!(p.run("--install-hooks").status.code(), Some(0));
    assert_eq!(hooks(&p)["Stop"][0]["hooks"][0]["args"], serde_json::json!(["--hook", "--port", "5151"]));
}

#[test]
fn corrupt_settings_fail_loudly_and_stay_as_they_were() {
    let p = Profile::new("corrupt");
    std::fs::write(p.settings(), "{ nope").unwrap();
    let out = p.run("--install-hooks");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("settings.json"));
    assert_eq!(std::fs::read_to_string(p.settings()).unwrap(), "{ nope");
}

#[test]
fn install_copies_only_the_exe() {
    let p = Profile::new("exe");
    assert_eq!(p.run("--install").status.code(), Some(0));
    assert!(p.exe().exists());
    assert!(!p.settings().exists());
}
