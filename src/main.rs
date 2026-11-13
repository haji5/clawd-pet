// GUI subsystem: no console window ever flashes, in hook mode or pet mode.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod anim;
mod compose;
mod config;
mod debug;
mod export;
mod feel;
mod flavour;
mod goto;
mod hook;
mod install;
mod interaction;
mod ipc;
mod mood;
mod perch;
mod physics;
mod placement;
mod rng;
mod sound;
mod sprite;
mod tracker;
mod walk;
#[cfg(windows)]
mod win;

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// `--send <Event> [--tool T] [--session S] [--note N] [--project P] [--agent TYPE] [--agent-id ID] [--parent-pid N] [--title T] [--desktop-id ID] [--kind K]`:
/// send a fake hook event to the running pet (manual testing without Claude).
fn send_fake(args: &[String]) {
    let Some(event) = value(args, "--send") else { return };
    let owned = |f: &str| value(args, f).map(str::to_string);
    let msg = ipc::IpcMsg {
        v: 1,
        e: event.into(),
        s: owned("--session").unwrap_or_else(|| "manual".into()),
        t: owned("--tool"),
        n: owned("--note"),
        p: Some(owned("--project").unwrap_or_else(|| "manual".into())),
        a: owned("--agent"),
        g: owned("--agent-id").or_else(|| value(args, "--agent").map(|_| "manual-agent".into())),
        pp: value(args, "--parent-pid").and_then(|p| p.parse().ok()),
        ti: owned("--title"),
        ds: owned("--desktop-id"),
        k: owned("--kind"),
        ts: hook::unix_now(),
    };
    ipc::send(&msg, ipc::port());
}

/// The art for the export tools: `--art <sprites.txt>` or the embedded copy.
fn sheet_or_exit(art: Option<&std::path::Path>) -> sprite::Sheet {
    let Some(path) = art else { return sprite::builtin() };
    sprite::load(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    })
}

fn write_or_exit(out: &str, bytes: &[u8]) {
    if let Err(e) = std::fs::write(out, bytes) {
        eprintln!("{out}: {e}");
        std::process::exit(1);
    }
}

/// Installer commands end here. There's no console (GUI subsystem), so the
/// outcome goes to a message box, unless `--quiet`, and to the exit code.
fn finish(result: Result<String, String>, quiet: bool) -> ! {
    let (text, ok) = match &result {
        Ok(text) => (text, true),
        Err(text) => (text, false),
    };
    println!("{text}");
    #[cfg(windows)]
    if !quiet {
        win::message_box(text, !ok);
    }
    #[cfg(not(windows))]
    let _ = quiet;
    std::process::exit(if ok { 0 } else { 1 })
}

fn main() {
    // Lossy, not `args()`: that one panics on an argument that isn't valid
    // Unicode, and the hook must never print or fail.
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let has = |f: &str| args.iter().any(|a| a == f);

    if has("--hook") {
        hook::run(hook::Flags {
            #[cfg(debug_assertions)]
            log_raw: has("--log-raw"),
            port: value(&args, "--port").and_then(|p| p.parse().ok()),
        });
    }
    if has("--send") {
        send_fake(&args);
        return;
    }
    if has("--install") {
        finish(install::install_exe(), has("--quiet"));
    }
    if has("--install-hooks") {
        finish(install::install_hooks(), has("--quiet"));
    }
    if has("--uninstall-hooks") {
        finish(install::uninstall_hooks(), has("--quiet"));
    }
    if has("--debug-log") {
        debug::enable();
    }
    // `--art <sprites.txt>`: art from disk instead of the embedded copy
    // (the pet hot-reloads it on change).
    let art = value(&args, "--art").map(std::path::PathBuf::from);
    if let Some(out) = value(&args, "--export-sheet") {
        let scale = value(&args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(4);
        write_or_exit(out, &export::sheet_bmp(&sheet_or_exit(art.as_deref()), scale));
        return;
    }
    if let Some(out) = value(&args, "--export-ico") {
        write_or_exit(out, &export::ico(&sheet_or_exit(art.as_deref())));
        return;
    }
    // `--dump-windows <file>`: what perching and fullscreen hiding make of
    // the windows open right now, without showing the pet.
    #[cfg(windows)]
    if let Some(out) = value(&args, "--dump-windows") {
        let cfg = config::default_path().as_deref().map(config::load).unwrap_or_default();
        if let Err(e) = win::windows::dump(out, cfg.size, &cfg.perch_deny_processes) {
            eprintln!("{out}: {e}");
            std::process::exit(1);
        }
        return;
    }
    // The hook leaves the SessionStart that made it launch the pet, so the
    // pet knows about that session right away.
    let seed = hook::take_seed();
    // `--hidden`: start with only the tray icon (testing without covering the
    // screen). The variable does the same for a pet the hook launches.
    let hidden = has("--hidden") || std::env::var_os("CLAWD_PET_HIDDEN").is_some();
    #[cfg(windows)]
    win::window::run(win::window::Options { seed, hidden: hidden && !has("--offscreen"), demo: has("--demo"), art, offscreen: has("--offscreen") });
    #[cfg(not(windows))]
    let _ = (seed, hidden, art);
}
