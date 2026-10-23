//! Command-line parsing and read-only console output.

use super::*;

pub(super) fn console_out() -> Box<dyn Write> {
    if unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|handle| !handle.is_invalid()) {
        return Box::new(std::io::stdout());
    }
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    match std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        Ok(f) => Box::new(f),
        Err(_) => Box::new(std::io::stdout()),
    }
}

fn is_apply_update(command: Option<&std::ffi::OsStr>) -> bool {
    command == Some(std::ffi::OsStr::new("--apply-update"))
}

pub(crate) fn run() {
    let command = std::env::args_os().nth(1);
    if is_apply_update(command.as_deref()) {
        update::apply_update();
        return;
    }
    if std::env::var_os("TILE_RS_PROFILE").is_some() {
        prof::PROFILE.store(true, Ordering::Relaxed);
    }
    let cmd = command.map(|command| command.to_string_lossy().into_owned());
    match cmd.as_deref() {
        None => start(true),
        Some("run") | Some("--autostart") => start(false),
        Some("--launcher-ui") => launcher::run(),
        Some("diagnose") => {
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            window::SKIP_ZOOMED.store(true, Ordering::Relaxed);
            let arguments: Vec<_> = std::env::args().skip(2).collect();
            let include_titles = arguments.iter().any(|argument| argument == "--titles");
            let mut output = None;
            let mut index = 0;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--titles" => {}
                    "--output" if index + 1 < arguments.len() => {
                        index += 1;
                        output = Some(std::path::PathBuf::from(&arguments[index]));
                    }
                    _ => {
                        eprintln!("usage: tile-rs diagnose [--titles] [--output PATH]");
                        return;
                    }
                }
                index += 1;
            }
            if output.is_none() && tray::request_manager_diagnostics(include_titles) {
                return;
            }
            if let Ok(config) = config::load() {
                config::set(config);
            }
            if let Some(path) = output {
                if let Err(error) = write_diagnostics(&path, include_titles, false) {
                    eprintln!("could not write diagnostics: {error}");
                    std::process::exit(1);
                }
            } else {
                let report = diagnostic_report(include_titles, false);
                let mut out = console_out();
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            }
        }
        Some("--restore-windows") => {
            // Rescue command for after a hard kill (Task Manager, power loss): bring back windows
            // that were left parked off-screen, without starting the window manager.
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            recover_parked();
            let mut out = console_out();
            let _ = writeln!(
                out,
                "Windows parked off-screen by tile-rs were brought back."
            );
        }
        Some("--version") | Some("-V") => {
            let mut out = console_out();
            let _ = writeln!(out, "tile-rs {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") => {
            let mut out = console_out();
            let _ = writeln!(
                out,
                "tile-rs {} - spiral tiling window manager for Windows",
                env!("CARGO_PKG_VERSION")
            );
            let _ = writeln!(out, "\nUSAGE:\n  tile-rs.exe              start (asks about autostart on the first run)");
            let _ = writeln!(
                out,
                "  tile-rs.exe --autostart  start silently (used by the autostart entry)"
            );
            let _ = writeln!(out, "  tile-rs.exe list         show which windows would be tiled, and why others are not");
            let _ = writeln!(out, "  tile-rs.exe diagnose [--titles] [--output PATH]  export read-only window diagnostics");
            let _ = writeln!(
                out,
                "  tile-rs.exe tile         arrange the current windows once and exit"
            );
            let _ = writeln!(out, "  tile-rs.exe --restore-windows  bring back windows left off-screen after a crash or kill");
            let _ = writeln!(out, "  tile-rs.exe --version    print the version");
            let _ = writeln!(out, "\nConfig: {}", config::config_path().display());
        }
        Some("list") | Some("tile") => {
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            if let Ok(c) = config::load() {
                config::set(c);
            }
            let mut out = console_out();
            let c = collect(true, &HashSet::new());
            let mut grouped = vec![Vec::new(); MON];
            for w in &c.tileable {
                grouped[monitor::of_window(w.hwnd).min(MON - 1)].push(w);
            }
            if cmd.as_deref() == Some("list") {
                let _ = writeln!(out, "TILEABLE ({}):", c.tileable.len());
                for (mi, wins) in grouped.iter().enumerate() {
                    if wins.is_empty() {
                        continue;
                    }
                    let area = monitor_work(mi);
                    let _ = writeln!(
                        out,
                        "\nMONITOR {} ({}x{} at {}, {}) - {} tileable:",
                        mi + 1,
                        area.right - area.left,
                        area.bottom - area.top,
                        area.left,
                        area.top,
                        wins.len()
                    );
                    for w in wins {
                        let _ = writeln!(out, "  [{:>6}] {:<28} {}", w.pid, w.class, w.title);
                    }
                }
                if !c.fullscreen.is_empty() {
                    let _ = writeln!(out, "\nFULLSCREEN ({}):", c.fullscreen.len());
                    for w in &c.fullscreen {
                        let _ = writeln!(
                            out,
                            "  [{:>6}] {:<28} {}  <- {}",
                            w.pid,
                            w.class,
                            w.title,
                            window::FULLSCREEN_REASON
                        );
                    }
                }
                let _ = writeln!(out, "\nSKIPPED ({}):", c.skipped.len());
                for (w, r) in &c.skipped {
                    let _ = writeln!(
                        out,
                        "  [{:>6}] {:<28} {}  <- {}",
                        w.pid, w.class, w.title, r
                    );
                }
                for mi in 0..monitor::count().min(MON) {
                    let area = monitor_work(mi);
                    let _ = writeln!(
                        out,
                        "WORK AREA monitor {}: {}x{} at ({}, {})",
                        mi + 1,
                        area.right - area.left,
                        area.bottom - area.top,
                        area.left,
                        area.top
                    );
                }
            } else {
                for (mi, wins) in grouped.iter().enumerate() {
                    if wins.is_empty() {
                        continue;
                    }
                    let (rects, _) =
                        layout_tiles(config::layout(), monitor_work(mi), wins.len(), &[], gaps());
                    for (w, r) in wins.iter().zip(rects) {
                        let _ = writeln!(
                            out,
                            "{}: {} -> {}x{} at ({}, {})",
                            mi + 1,
                            w.title,
                            r.right - r.left,
                            r.bottom - r.top,
                            r.left,
                            r.top
                        );
                        place(w.hwnd, r);
                    }
                }
            }
        }
        Some(_) => {
            let mut out = console_out();
            let _ = writeln!(
                out,
                "usage: tile-rs [--autostart | list | tile | --version | --help]"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn native_update_mode_is_recognized_before_normal_startup() {
        assert!(is_apply_update(Some(OsStr::new("--apply-update"))));
        assert!(!is_apply_update(None));
        assert!(!is_apply_update(Some(OsStr::new("--launcher-ui"))));
        assert!(!is_apply_update(Some(OsStr::new("--apply-update-extra"))));
    }
}
