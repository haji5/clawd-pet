//! "Check for updates" end to end (PLAN.md §7, "Updating"): a hidden debug
//! pet is pointed at a server in this test (`CLAWD_PET_UPDATE_URL`), takes
//! the "release" it serves, replaces its own exe and the installed copy,
//! and restarts. Everything lives in a temp profile on a spare port.
//!
//! It starts real processes, so it runs on request:
//! `cargo test --test update_cli -- --ignored`

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

// The pure update module, for its SHA-256.
#[allow(dead_code)]
#[path = "../src/update.rs"]
mod update;

const DEADLINE: Duration = Duration::from_secs(40);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Serves the release JSON at `/api` and `exe` at `/dl/v99.0.0/clawd-pet.exe`,
/// with `digest` as the expected hash, until the test ends.
fn serve(exe: Vec<u8>, digest: [u8; 32]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let json = serde_json::json!({
        "tag_name": "v99.0.0",
        "assets": [{
            "name": "clawd-pet.exe",
            "size": exe.len(),
            "digest": format!("sha256:{}", hex(&digest)),
            "browser_download_url": format!("http://127.0.0.1:{port}/dl/v99.0.0/clawd-pet.exe"),
        }],
    })
    .to_string()
    .into_bytes();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let request = String::from_utf8_lossy(&request);
            let body: Option<&[u8]> = if request.starts_with("GET /api ") {
                Some(&json)
            } else if request.starts_with("GET /dl/v99.0.0/clawd-pet.exe ") {
                Some(&exe)
            } else {
                None
            };
            let head = match body {
                Some(b) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            };
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.unwrap_or_default());
        }
    });
    port
}

struct Profile {
    dir: PathBuf,
    pet_port: u16,
    server_port: u16,
}

impl Profile {
    fn new(name: &str, server_port: u16) -> Profile {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("update-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["app", "local/clawd-pet", "roaming", "temp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let me = env!("CARGO_BIN_EXE_clawd-pet");
        std::fs::copy(me, dir.join("app").join("clawd-pet.exe")).unwrap();
        std::fs::copy(me, dir.join("local").join("clawd-pet").join("clawd-pet.exe")).unwrap();
        let pet_port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        Profile { dir, pet_port, server_port }
    }

    fn app_exe(&self) -> PathBuf {
        self.dir.join("app").join("clawd-pet.exe")
    }

    fn installed_exe(&self) -> PathBuf {
        self.dir.join("local").join("clawd-pet").join("clawd-pet.exe")
    }

    fn command(&self, exe: &Path) -> Command {
        let mut c = Command::new(exe);
        c.env("CLAWD_PET_PORT", self.pet_port.to_string())
            .env("CLAWD_PET_HIDDEN", "1")
            .env("CLAWD_PET_UPDATE_URL", format!("http://127.0.0.1:{}/api", self.server_port))
            .env("CLAWD_PET_UPDATE_AS", "0.0.1")
            .env("USERPROFILE", &self.dir)
            .env("APPDATA", self.dir.join("roaming"))
            .env("LOCALAPPDATA", self.dir.join("local"))
            .env("TEMP", self.dir.join("temp"))
            .env("TMP", self.dir.join("temp"))
            .env_remove("CLAUDE_CONFIG_DIR");
        c
    }

    fn start_pet(&self) -> Child {
        let pet = self.command(&self.app_exe()).args(["--hidden", "--debug-log"]).spawn().unwrap();
        self.wait_for_stats(0);
        pet
    }

    /// `--send` from the build's own exe, so the pet's file is never held.
    fn send(&self, args: &[&str]) {
        let status = self.command(Path::new(env!("CARGO_BIN_EXE_clawd-pet"))).arg("--send").args(args).status().unwrap();
        assert!(status.success());
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("temp").join("clawd-pet.log")).unwrap_or_default()
    }

    fn wait_for(&self, what: &str) -> usize {
        let start = Instant::now();
        loop {
            if let Some(at) = self.log().find(what) {
                return at;
            }
            assert!(start.elapsed() < DEADLINE, "no {what:?} in the log:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A pet answers `__stats` with a line after byte `after` of the log.
    fn wait_for_stats(&self, after: usize) {
        let start = Instant::now();
        loop {
            self.send(&["__stats"]);
            std::thread::sleep(Duration::from_millis(200));
            if self.log().get(after..).is_some_and(|rest| rest.contains("stats msgs=")) {
                return;
            }
            assert!(start.elapsed() < DEADLINE, "no pet answers:\n{}", self.log());
        }
    }

    fn leftovers(&self) -> Vec<String> {
        [self.dir.join("app"), self.dir.join("local").join("clawd-pet")]
            .iter()
            .flat_map(|d| std::fs::read_dir(d).unwrap().flatten())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "clawd-pet.exe")
            .collect()
    }

    /// Quit the pet and wait until no process runs the app exe any more.
    fn quit_pet(&self) {
        self.send(&["__menu", "--tool", "99"]);
        wait_until("the pet didn't quit", || exe_is_free(&self.app_exe()));
    }
}

/// No process runs this exe: it can be opened for writing. (A running exe
/// can be renamed, so renaming says nothing.)
fn exe_is_free(exe: &Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(exe).is_ok()
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < DEADLINE, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The exe with bytes on the end: still a program that runs, but another file.
fn newer_exe() -> Vec<u8> {
    let mut exe = std::fs::read(env!("CARGO_BIN_EXE_clawd-pet")).unwrap();
    exe.extend_from_slice(b"clawd-pet v99.0.");
    exe
}

#[test]
#[ignore = "starts a hidden pet; run with --ignored"]
fn an_update_replaces_both_exes_and_restarts() {
    let served = newer_exe();
    let port = serve(served.clone(), update::sha256(&served));
    let p = Profile::new("ok", port);
    let mut first = p.start_pet();

    p.send(&["__update"]);
    let at = p.wait_for("updated from 0.0.1 to");
    let log = p.log();
    for step in ["update: checking", "update: 99.0.0 is available", "update: downloading", "update: 99.0.0 is in place, restarting"] {
        assert!(log.contains(step), "{step:?} missing:\n{log}");
    }

    assert_eq!(std::fs::read(p.app_exe()).unwrap(), served, "the exe that ran");
    assert_eq!(std::fs::read(p.installed_exe()).unwrap(), served, "the installed copy");
    wait_until("the first pet is still running", || first.try_wait().unwrap().is_some());
    wait_until(&format!("leftovers: {:?}", p.leftovers()), || p.leftovers().is_empty());
    p.wait_for_stats(at);
    assert!(!p.log()[at..].contains("already running"), "the new pet lost to nobody:\n{}", p.log());

    p.quit_pet();
}

#[test]
#[ignore = "starts a hidden pet; run with --ignored"]
fn a_download_with_the_wrong_hash_changes_nothing() {
    let served = newer_exe();
    let port = serve(served, update::sha256(b"something else"));
    let p = Profile::new("bad-hash", port);
    let before = std::fs::read(p.app_exe()).unwrap();
    let mut first = p.start_pet();

    p.send(&["__update"]);
    p.wait_for("update: failed: The download didn't match the release's checksum");
    assert_eq!(std::fs::read(p.app_exe()).unwrap(), before);
    assert_eq!(std::fs::read(p.installed_exe()).unwrap(), before);
    assert!(p.leftovers().is_empty(), "{:?}", p.leftovers());
    assert!(first.try_wait().unwrap().is_none(), "the pet carries on");
    assert!(!p.log().contains("updated from"));

    p.quit_pet();
    wait_until("the pet didn't exit", || first.try_wait().unwrap().is_some());
}
