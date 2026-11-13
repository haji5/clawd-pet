//! Compact message sent from the hook process to the pet over localhost UDP.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_PORT: u16 = 47823;
pub const MAX_FIELD: usize = 64;

/// Wire format, ≤ 512 bytes. Field names are single letters to keep it small.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IpcMsg {
    pub v: u8,
    /// Hook event name, e.g. `PreToolUse`.
    pub e: String,
    /// Session id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub s: String,
    /// Tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// Notification type / source / reason (event-dependent detail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<String>,
    /// Project: basename of cwd.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<String>,
    /// Agent type, set when the event comes from inside a subagent.
    /// Empty `agent_type` (Claude Code's internal helper agents) is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<String>,
    /// Agent id (opaque hex) so several parallel subagents can be told apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g: Option<String>,
    /// Parent pid of the hook: the Claude Code process hosting this session.
    /// Only on `SessionStart` and `UserPromptSubmit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pp: Option<u32>,
    /// Chat name, from the desktop app. Same two events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ti: Option<String>,
    /// The desktop app's own session id (`local_…`). Same two events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ds: Option<String>,
    /// What kind of shell command it is (`git`, `test`, `build`, `install`).
    /// Only on `PreToolUse` of a shell tool, and never the command itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<String>,
    /// Unix seconds. Nothing reads it: events are taken in the order they
    /// arrive. Still sent, because pets built before 2026-11-09 refuse a
    /// message without it; new ones don't need it.
    #[serde(default)]
    pub ts: u64,
}

/// The wire format this build speaks. A pet ignores any other version
/// rather than guess at fields that may mean something else.
pub const VERSION: u8 = 1;

/// Truncate to at most `MAX_FIELD` chars on a char boundary.
pub fn trunc(s: &str) -> String {
    s.chars().take(MAX_FIELD).collect()
}

/// The port from `CLAWD_PET_PORT`, if set and valid.
pub fn env_port() -> Option<u16> {
    std::env::var("CLAWD_PET_PORT").ok().and_then(|p| p.parse().ok())
}

pub fn port() -> u16 {
    env_port().unwrap_or(DEFAULT_PORT)
}

/// Fire-and-forget to the pet on localhost. Errors are ignored: if the pet
/// isn't running the datagram is simply dropped.
pub fn send(msg: &IpcMsg, port: u16) {
    let Ok(bytes) = serde_json::to_vec(msg) else { return };
    if let Ok(sock) = UdpSocket::bind("127.0.0.1:0") {
        let _ = sock.send_to(&bytes, ("127.0.0.1", port));
    }
}

/// Bind the pet's listening socket (localhost only). Windows doesn't let
/// another socket share a port bound to a specific address like this, with
/// or without `SO_REUSEADDR` (checked 2026-11-09), so nothing more is
/// needed to keep the messages from being taken by another program.
pub fn bind(port: u16) -> std::io::Result<UdpSocket> {
    UdpSocket::bind(("127.0.0.1", port))
}

/// Room for the longest message: seven fields of `MAX_FIELD` four-byte
/// chars come to about 2 KB.
const DATAGRAM_MAX: usize = 4096;
/// A socket that keeps failing mustn't turn the thread into a busy loop.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(200);

/// Receive on a background thread and hand every valid message from a
/// loopback sender to `on_msg`. The thread lives until the process exits.
pub fn spawn_listener(sock: UdpSocket, mut on_msg: impl FnMut(IpcMsg) + Send + 'static) {
    std::thread::spawn(move || {
        let mut buf = [0u8; DATAGRAM_MAX];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf) else {
                std::thread::sleep(RETRY_AFTER);
                continue;
            };
            if !from.ip().is_loopback() {
                continue;
            }
            if let Ok(m) = serde_json::from_slice::<IpcMsg>(&buf[..n])
                && m.v == VERSION
            {
                on_msg(m);
            }
        }
    });
}

/// Bounded hand-off queue between the listener thread and the UI thread.
/// `push` returns true when the UI thread needs a wake-up: none is on its
/// way yet. So a burst of events posts one window message, not one each.
#[derive(Default)]
pub struct Queue {
    msgs: Mutex<VecDeque<IpcMsg>>,
    /// A wake-up has been sent and the UI thread hasn't drained since.
    waking: AtomicBool,
}

pub const QUEUE_CAP: usize = 64;

impl Queue {
    pub fn push(&self, m: IpcMsg) -> bool {
        {
            let mut q = self.msgs.lock().unwrap_or_else(|e| e.into_inner());
            if q.len() >= QUEUE_CAP {
                q.pop_front();
            }
            q.push_back(m);
        }
        !self.waking.swap(true, Ordering::SeqCst)
    }

    /// The wake-up couldn't be delivered, so the next push has to try
    /// again: otherwise the queue would never be looked at any more.
    pub fn wake_failed(&self) {
        self.waking.store(false, Ordering::SeqCst);
    }

    pub fn drain(&self) -> Vec<IpcMsg> {
        // Cleared first: a message pushed while this runs asks for a wake-up
        // of its own, which at worst finds the queue already empty.
        self.waking.store(false, Ordering::SeqCst);
        self.msgs.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trunc_respects_chars() {
        let s = "é".repeat(100);
        assert_eq!(trunc(&s).chars().count(), MAX_FIELD);
    }

    #[test]
    fn queue_is_bounded_and_coalesces_wakeups() {
        let q = Queue::default();
        let m = |i: u64| IpcMsg { ts: i, ..Default::default() };
        assert!(q.push(m(0)));
        assert!(!q.push(m(1)));
        for i in 2..100 {
            q.push(m(i));
        }
        let all = q.drain();
        assert_eq!(all.len(), QUEUE_CAP);
        assert_eq!(all.last().unwrap().ts, 99, "oldest dropped, newest kept");
        assert!(q.push(m(0)), "empty again after drain");
    }

    #[test]
    fn a_wake_up_that_failed_is_asked_for_again() {
        let q = Queue::default();
        assert!(q.push(IpcMsg::default()));
        assert!(!q.push(IpcMsg::default()), "one is on its way");
        q.wake_failed();
        assert!(q.push(IpcMsg::default()), "it never arrived: ask again");
        assert_eq!(q.drain().len(), 3);
    }

    #[test]
    fn the_listener_ignores_other_wire_versions_and_what_isnt_a_message() {
        let sock = bind(0).unwrap();
        let port = sock.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_listener(sock, move |m| {
            let _ = tx.send(m);
        });
        let from = UdpSocket::bind("127.0.0.1:0").unwrap();
        for junk in [&b"nonsense"[..], br#"{"v":2,"e":"Stop","s":"x"}"#, &[b'x'; 3000][..]] {
            from.send_to(junk, ("127.0.0.1", port)).unwrap();
        }
        let good = IpcMsg { v: VERSION, e: "Stop".into(), s: "ok".into(), ..Default::default() };
        send(&good, port);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), good, "only the good one comes through");
    }

    #[test]
    fn a_message_without_a_timestamp_is_still_a_message() {
        let m: IpcMsg = serde_json::from_str(r#"{"v":1,"e":"Stop","s":"abc"}"#).unwrap();
        assert_eq!((m.e.as_str(), m.ts), ("Stop", 0));
    }

    #[test]
    fn the_longest_message_fits_a_datagram() {
        let wide = "\u{1F980}".repeat(MAX_FIELD); // four bytes each
        let f = || Some(trunc(&wide));
        let m = IpcMsg { v: 1, e: trunc(&wide), s: trunc(&wide), t: f(), n: f(), p: f(), a: f(), g: f(), pp: Some(u32::MAX), ti: f(), ds: f(), k: f(), ts: u64::MAX };
        assert!(serde_json::to_vec(&m).unwrap().len() <= DATAGRAM_MAX);
    }

    #[test]
    fn listener_receives_from_loopback() {
        let sock = bind(0).unwrap();
        let port = sock.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_listener(sock, move |m| {
            let _ = tx.send(m);
        });
        let m = IpcMsg { v: 1, e: "Stop".into(), s: "x".into(), ..Default::default() };
        send(&m, port);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), m);
    }

    #[test]
    fn roundtrip_skips_empty() {
        let m = IpcMsg { v: 1, e: "Stop".into(), s: "abc".into(), ts: 5, ..Default::default() };
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(j, r#"{"v":1,"e":"Stop","s":"abc","ts":5}"#);
        assert_eq!(serde_json::from_str::<IpcMsg>(&j).unwrap(), m);
    }
}
