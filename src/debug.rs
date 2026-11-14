//! Tiny opt-in debug log: `--debug-log` writes to `%TEMP%\clawd-pet.log`.
//! Off by default; when off, `dlog!` costs one relaxed atomic load.

use std::fs::{File, OpenOptions};
use std::io::{LineWriter, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The log file, opened on the first line and kept open: opening and closing
/// it for every line cost more than the event being logged (0.62–0.78 ms per
/// event at 20 events/s against 0.21–0.37 ms without the log). A
/// `LineWriter` hands each whole line to the file at once, so the measuring
/// scripts can read it while the pet runs, and a crash leaves whole lines.
static FILE: Mutex<Option<LineWriter<File>>> = Mutex::new(None);

pub fn write(msg: std::fmt::Arguments) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let line = format!("{}.{:03} {msg}\n", t.as_secs(), t.subsec_millis());
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        let path = std::env::temp_dir().join("clawd-pet.log");
        *file = OpenOptions::new().create(true).append(true).open(path).ok().map(LineWriter::new);
    }
    if let Some(f) = file.as_mut()
        && f.write_all(line.as_bytes()).is_err()
    {
        // Try opening it again on the next line.
        *file = None;
    }
}

/// What the UI thread actually did, for `scripts/perf.ps1` (`--send __stats`
/// logs it and starts over). Plain increments on one thread: nothing to
/// switch off.
#[derive(Default)]
pub struct Stats {
    /// Messages out of `GetMessage`.
    pub msgs: u32,
    /// WinEvent callbacks (these wake the thread but aren't messages).
    pub win_events: u32,
    pub refreshes: u32,
    /// Times the canvas was composed.
    pub composed: u32,
    /// `UpdateLayeredWindow` with new pixels, and with only a new position.
    pub drawn: u32,
    pub moved: u32,
    /// `WM_TIMER`, by timer id.
    pub timers: [u32; 16],
}

impl Stats {
    /// One line, and back to zero.
    pub fn take(&mut self) -> String {
        let timers: Vec<String> = self.timers.iter().enumerate().filter(|(_, n)| **n != 0).map(|(id, n)| format!("{id}:{n}")).collect();
        let line = format!(
            "stats msgs={} win_events={} refreshes={} composed={} drawn={} moved={} timers={}",
            self.msgs,
            self.win_events,
            self.refreshes,
            self.composed,
            self.drawn,
            self.moved,
            timers.join(",")
        );
        *self = Stats::default();
        line
    }
}

#[macro_export]
macro_rules! dlog {
    ($($t:tt)*) => {
        if $crate::debug::enabled() {
            $crate::debug::write(format_args!($($t)*));
        }
    };
}
