//! Runs the real binary in `--hook` mode the way Claude Code does (piped
//! stdin/stdout/stderr) and checks the hard rules: no output, exit code 0,
//! and one UDP datagram for a valid payload.

use std::io::Write;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// An APPDATA whose config turns "Launch with Claude" off: without it, the
/// `SessionStart` fixtures would launch a real pet on the screen.
fn appdata_without_launch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("appdata-no-launch");
        std::fs::create_dir_all(dir.join("clawd-pet")).unwrap();
        std::fs::write(dir.join("clawd-pet").join("config.json"), r#"{"launchWithClaude":false}"#).unwrap();
        dir
    })
}

fn run_hook(stdin: &[u8], port: u16) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clawd-pet"))
        .arg("--hook")
        .env("CLAWD_PET_PORT", port.to_string())
        .env("APPDATA", appdata_without_launch())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn listener() -> (UdpSocket, u16) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let port = sock.local_addr().unwrap().port();
    (sock, port)
}

fn assert_silent(out: &std::process::Output) {
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
    assert!(out.stderr.is_empty(), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn fixtures_are_silent_and_send_one_datagram() {
    let (sock, port) = listener();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hooks");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let out = run_hook(&std::fs::read(&path).unwrap(), port);
        assert_silent(&out);
        let mut buf = [0u8; 1024];
        let n = sock.recv(&mut buf).unwrap_or_else(|e| panic!("{path:?}: no datagram: {e}"));
        let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(v["v"], 1);
        assert!(n <= 512);
        // The hook reports its parent (here: this test) only where it's asked to.
        let e = v["e"].as_str().unwrap();
        if matches!(e, "SessionStart" | "UserPromptSubmit") {
            assert_eq!(v["pp"], std::process::id(), "{path:?}");
        } else {
            assert!(v.get("pp").is_none(), "{path:?}: {v}");
        }
    }
}

/// What the hook sends when Claude Code's environment has that desktop id.
fn prompt_datagram_with_desktop_id(id: &str) -> serde_json::Value {
    let (sock, port) = listener();
    let mut child = Command::new(env!("CARGO_BIN_EXE_clawd-pet"))
        .arg("--hook")
        .env("CLAWD_PET_PORT", port.to_string())
        .env("APPDATA", appdata_without_launch())
        .env("CLAUDE_CODE_HOST_SESSION_ID", id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = br#"{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"secret prompt","session_title":"My chat"}"#;
    child.stdin.take().unwrap().write_all(payload).unwrap();
    assert_silent(&child.wait_with_output().unwrap());
    let mut buf = [0u8; 1024];
    let n = sock.recv(&mut buf).expect("datagram");
    assert!(!String::from_utf8_lossy(&buf[..n]).contains("secret"));
    serde_json::from_slice(&buf[..n]).unwrap()
}

#[test]
fn a_desktop_session_sends_its_id_and_name() {
    let v = prompt_datagram_with_desktop_id("local_2abc-07");
    assert_eq!((v["ds"].as_str(), v["ti"].as_str()), (Some("local_2abc-07"), Some("My chat")));
    let v = prompt_datagram_with_desktop_id("something else");
    assert!(v.get("ds").is_none(), "{v}");
}

#[test]
fn port_argument_beats_the_environment_and_stays_silent() {
    let (sock, port) = listener();
    let mut child = Command::new(env!("CARGO_BIN_EXE_clawd-pet"))
        .args(["--hook", "--port", &port.to_string()])
        .env("CLAWD_PET_PORT", "1")
        .env("APPDATA", appdata_without_launch())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"Stop","session_id":"s"}"#).unwrap();
    assert_silent(&child.wait_with_output().unwrap());
    let mut buf = [0u8; 1024];
    let n = sock.recv(&mut buf).expect("datagram on the --port port");
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!(v["e"], "Stop");
}

#[test]
fn send_tool_emits_one_datagram() {
    let (sock, port) = listener();
    let out = Command::new(env!("CARGO_BIN_EXE_clawd-pet"))
        .args(["--send", "PreToolUse", "--tool", "Read", "--project", "demo"])
        .env("CLAWD_PET_PORT", port.to_string())
        .output()
        .unwrap();
    assert_silent(&out);
    let mut buf = [0u8; 1024];
    let n = sock.recv(&mut buf).expect("datagram from --send");
    let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
    assert_eq!((v["e"].as_str(), v["t"].as_str(), v["p"].as_str()), (Some("PreToolUse"), Some("Read"), Some("demo")));
}

#[test]
fn garbage_empty_and_huge_stdin_are_silent() {
    let (sock, port) = listener();
    sock.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let huge = vec![b'x'; 6 * 1024 * 1024];
    for input in [&b"not json {{{"[..], &b""[..], &huge[..]] {
        assert_silent(&run_hook(input, port));
    }
    let mut buf = [0u8; 64];
    assert!(sock.recv(&mut buf).is_err(), "no datagram expected for invalid input");
}
