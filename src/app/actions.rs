//! Hotkey registration and dispatch of user commands.

use super::*;

// ---------------------------------------------------------------------------
// hotkeys
// ---------------------------------------------------------------------------

/// (Re)registers every hotkey from the config. While paused only the "essential" ones stay
/// registered, so the other Alt+... keys go back to the applications.
/// Returns descriptions of bindings that could not be registered.
pub(super) fn register_hotkeys() -> Vec<String> {
    let cfg = config::current();
    let (old, paused) = STATE.with(|s| {
        let s = s.borrow();
        (
            s.hotkeys.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            s.paused,
        )
    });
    for id in old {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
    let mut new = Vec::new();
    let mut failed = Vec::new();
    let mut refused = String::new();
    let mut next = 100;
    for (action, spec) in &cfg.bindings {
        let essential = matches!(
            action,
            Action::Pause | Action::Quit | Action::ReloadConfig | Action::EditConfig
        );
        if paused && !essential {
            continue;
        }
        match config::parse_hotkey(spec) {
            Ok((mods, vk)) => {
                let id = next;
                next += 1;
                // resize keys keep firing while held; everything else fires once per press
                let flags = if matches!(action, Action::Resize { .. }) {
                    mods
                } else {
                    mods | MOD_NOREPEAT
                };
                if unsafe { RegisterHotKey(None, id, flags, vk) }.is_ok() {
                    new.push((id, *action));
                } else {
                    refused.push_str(&format!("{}\t{}\n", action_name(*action), spec));
                    failed.push(format!(
                        "{} = {} (already used by another program?)",
                        action_name(*action),
                        spec
                    ));
                }
            }
            Err(e) => failed.push(e),
        }
    }
    STATE.with(|s| s.borrow_mut().hotkeys = new);
    // the settings window reads this to mark the combinations Windows refused
    let _ = std::fs::write(config::hotkey_failures_path(), refused);
    for f in &failed {
        log(format!("hotkey not registered: {f}"));
    }
    failed
}

/// Pause / resume asked for by the settings window, which shows the state itself.
pub(crate) fn toggle_pause_quietly() {
    toggle_pause(false);
}

/// `announce`: show a notification when pausing (hotkey and tray menu).
pub(super) fn toggle_pause(announce: bool) {
    end_divider(false);
    let paused = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.paused = !s.paused;
        if s.paused {
            s.anims.clear();
            s.live_resize.clear();
            s.live_borders.clear();
            cancel_anim_timer(&mut s);
        }
        s.paused
    });
    log(if paused { "paused" } else { "resumed" });
    register_hotkeys();
    tray::update();
    if paused && announce {
        tray::notify_paused();
    }
    if paused {
        clear_border();
        sync_dividers();
        STATE.with(|s| restore_hidden_fullscreen(&mut s.borrow_mut()));
    }
    if !paused {
        STATE.with(|s| s.borrow_mut().clear_layouts());
        retile();
        refresh_border();
    }
}

/// Opens the settings launcher (also used by the tray menu).
pub(crate) fn show_launcher() {
    launcher::show();
}

/// Quits tile-rs (used by the launcher's "Quit" button).
pub(crate) fn dispatch_quit() {
    unsafe {
        PostQuitMessage(0);
    }
}

pub(crate) fn reload_config() {
    reload(true);
}

/// Reload triggered by the settings window's autosave: refused hotkeys are shown there, so no
/// message box pops up after every edit.
pub(crate) fn reload_config_quiet() {
    reload(false);
}

pub(super) fn reload(announce_failures: bool) {
    end_divider(false);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.anims.clear();
        cancel_anim_timer(&mut s);
    });
    STATE.with(|s| s.borrow_mut().clear_placement_history());
    match config::load() {
        Ok(c) => {
            overlay::set_indicator_size(c.workspace_indicator_size);
            config::set(c);
            overlay::set_indicator_accent(config::accent_color_value());
            overlay::set_border_color(config::border_color_value());
            let failed = register_hotkeys();
            STATE.with(|s| s.borrow_mut().invalidate_layout_constraints());
            retile();
            tray::update();
            clear_border(); // the colour (or the on/off switch) may have changed
            refresh_border();
            sync_indicator();
            apply_focus_follows_timer();
            let minimum = config::min_tile();
            log(format!(
                "config reloaded: minimum={}x{}; learned size constraints cleared",
                minimum.0, minimum.1
            ));
            if announce_failures && !failed.is_empty() {
                notify(
                    "tile-rs: some hotkeys were not registered",
                    &failed.join("\n"),
                );
            }
        }
        Err(e) => {
            log(format!("config error: {e}"));
            notify("tile-rs: config error (the old settings stay active)", &e);
        }
    }
}

/// Hotkey: minimize the focused window (a fullscreen game, or a normal tiled window).
pub(super) fn minimize_focused() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let h = fg.0 as isize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        // the user chose to minimize it: do not restore it on the next workspace switch
        s.fs_hidden.remove(&h);
        HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
    });
    unsafe {
        let _ = ShowWindowAsync(fg, SW_MINIMIZE);
    }
    log("minimized the focused window");
}

/// Hotkey: close the focused window (like Alt+F4).
pub(super) fn close_focused() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(fg), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    log("closed the focused window");
}

/// Hotkey: float the focused window (leave it where it is) or, if it is already floating, tile it
/// again. The choice lasts for this session; a config reload does not clear it.
pub(super) fn toggle_float() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let h = fg.0 as isize;
    let floating = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let was_manual = s.manual_float.remove(&h);
        let was_capacity = s.capacity_float.remove(&h);
        s.update_tracked(h, |window| window.float_reason = None);
        if was_manual || was_capacity {
            false
        } else {
            s.manual_float.insert(h);
            remember_identity(&mut s, fg);
            true
        }
    });
    retile();
    refresh_border();
    if floating {
        log("floating the focused window");
    } else {
        log("tiling the focused window again");
    }
}

/// The monitor slot `step` away from `from`, wrapping; `None` with a single monitor.
pub(super) fn monitor_step(from: usize, step: i8) -> Option<usize> {
    let n = monitor::count().min(MON);
    (n >= 2).then(|| (from as i64 + i64::from(step)).rem_euclid(n as i64) as usize)
}

/// Hotkey: move keyboard focus to the next / previous monitor.
pub(super) fn focus_monitor(step: i8) {
    let target = STATE.with(|s| {
        let s = s.borrow();
        let from = active_mon(&s);
        monitor_step(from, step).map(|to| (to, s.last[to].first().copied()))
    });
    let Some((to, first)) = target else {
        return;
    };
    match first {
        Some(h) => focus_hwnd(h),
        None => {
            // nothing tiled there: park the pointer on that monitor so it becomes the active one
            let work = monitor::work(to);
            unsafe {
                let _ = SetCursorPos(
                    work.left + (work.right - work.left) / 2,
                    work.top + (work.bottom - work.top) / 2,
                );
            }
            focus_hwnd(unsafe { GetShellWindow() }.0 as isize);
        }
    }
    log(format!("focused monitor {}", to + 1));
}

/// Hotkey: send the focused tiled window to the next / previous monitor, into the workspace that
/// monitor is showing, and keep it focused.
pub(super) fn move_to_monitor(step: i8) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let plan = STATE.with(|s| {
        let s = s.borrow();
        if s.paused || s.dragging.is_some() || !s.ws.contains_key(&fg) || s.fs_ws.contains_key(&fg)
        {
            return None;
        }
        monitor_step(s.mi(fg), step).map(|to| (to, s.cur[to]))
    });
    let Some((to, workspace)) = plan else {
        return;
    };
    let work = monitor::work(to);
    // Keep the target assignment until the posted move is reflected by native geometry.
    let moved = unsafe {
        SetWindowPos(
            HWND(fg as *mut _),
            None,
            work.left + 40,
            work.top + 40,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
        )
    };
    if moved.is_err() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let from = s.mi(fg);
        s.ws.insert(fg, workspace);
        s.mon.insert(fg, to);
        s.update_tracked(fg, |window| window.pending_monitor = Some(to));
        s.last[to].clear();
        s.last[from].clear();
    });
    retile();
    focus_hwnd(fg);
    log(format!("moved a window to monitor {}", to + 1));
}

/// Hotkey: switch to the next layout (spiral -> master-stack -> grid -> columns -> spiral) and
/// remember it in `config.toml`, so the choice survives a restart like the launcher's picker.
pub(super) fn cycle_layout() {
    let next = match config::layout() {
        Layout::Spiral => Layout::MasterStack,
        Layout::MasterStack => Layout::Grid,
        Layout::Grid => Layout::Columns,
        Layout::Columns => Layout::Spiral,
    };
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = config::edit_top(&text, &[("layout", format!("\"{}\"", next.name()))]);
    if let Err(e) = config::save_atomic(&path, &new) {
        log(format!("could not save the layout to config.toml: {e}"));
        return;
    }
    reload_config();
    notify("tile-rs", &format!("Layout: {}", next.name()));
}

/// Hotkey: show the focused window alone on the whole screen (the other windows of the workspace
/// stay stacked underneath and `focus_*` cycles through them); press again to restore the layout.
pub(super) fn toggle_monocle() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let on = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mi = s.mi(fg);
        let ws = s.cur[mi];
        if s.paused || !s.last[mi].contains(&fg) {
            return None;
        }
        let on = s.monocle[mi].insert(ws);
        if !on {
            s.monocle[mi].remove(&ws);
        }
        s.last[mi].clear(); // force a re-layout
        Some(on)
    });
    let Some(on) = on else { return };
    retile();
    focus_hwnd(fg);
    notify("tile-rs", if on { "Monocle on" } else { "Monocle off" });
}

/// Hotkey: focus the previously focused window, switching to its workspace if it is on another.
pub(super) fn focus_last() {
    let target = STATE.with(|s| {
        let s = s.borrow();
        let h = s.focus_prev?;
        let ws = s.ws.get(&h).copied().or_else(|| s.fs_ws.get(&h).copied())?;
        let mi = s
            .mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
            .min(MON - 1);
        Some((h, ws, mi, ws != s.cur[mi]))
    });
    let Some((h, ws, mi, other)) = target else {
        return;
    };
    if !unsafe { IsWindow(Some(HWND(h as *mut _))) }.as_bool() {
        return;
    }
    if other {
        switch_ws(mi, ws, Some(h));
    } else {
        focus_hwnd(h);
    }
}

/// Hotkey: go back to the workspace the active monitor showed before the current one.
pub(super) fn workspace_back() {
    let (mi, prev) = STATE.with(|s| {
        let s = s.borrow();
        let mi = active_mon(&s);
        (mi, s.prev_ws[mi])
    });
    if let Some(n) = prev {
        switch_ws(mi, n, None);
    }
}

/// Hotkey: add (`step` > 0) or remove a window in the master column of master-stack, and remember
/// the count in `config.toml`.
pub(super) fn change_master_count(step: i8) {
    if config::layout() != Layout::MasterStack {
        notify("tile-rs", "Master count applies to the master-stack layout");
        return;
    }
    let now = config::master_count() as i32;
    let next = (now + step as i32).clamp(1, config::MAX_MASTER_COUNT as i32);
    if next == now {
        return;
    }
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = config::edit_top(&text, &[("master_count", next.to_string())]);
    if let Err(e) = config::save_atomic(&path, &new) {
        log(format!("could not save master_count to config.toml: {e}"));
        return;
    }
    reload_config();
    notify("tile-rs", &format!("Master windows: {next}"));
}

/// Marker file: the settings window's first-run tour is open and watching.
pub(crate) fn tour_marker() -> std::path::PathBuf {
    config::config_path().with_file_name("tour-active")
}

/// The tour's progress file: one line per step the user has done ("focus", "swap", "send").
pub(crate) fn tour_progress_path() -> std::path::PathBuf {
    config::config_path().with_file_name("tour-progress.txt")
}

/// Notes a tour step for the settings window, but only while its tour is open. A marker older
/// than an hour was left behind by a launcher that did not exit cleanly and is ignored.
pub(super) fn tour_event(step: &str) {
    let fresh = std::fs::metadata(tour_marker())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(3600));
    if !fresh {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tour_progress_path())
    {
        let _ = writeln!(file, "{step}");
    }
}

pub(crate) fn dispatch(a: Action) {
    end_divider(false);
    match a {
        Action::Focus(d) => {
            if focus_or_swap(d, false) {
                tour_event("focus");
            }
        }
        Action::Swap(d) => {
            if focus_or_swap(d, true) {
                tour_event("swap");
            }
        }
        Action::Workspace(n) => switch_ws_active(n),
        Action::MoveTo(n) => move_to_ws(n),
        Action::WorkspaceNext => switch_ws_relative(1),
        Action::WorkspacePrev => switch_ws_relative(-1),
        Action::MoveToNext => move_ws_relative(1),
        Action::MoveToPrev => move_ws_relative(-1),
        Action::Resize { width, grow } => resize_focused(width, grow),
        Action::Minimize => minimize_focused(),
        Action::ToggleFloat => toggle_float(),
        Action::Close => close_focused(),
        Action::Pause => toggle_pause(true),
        Action::Quit => unsafe { PostQuitMessage(0) },
        Action::Retile => {
            // also resets the sizes of every workspace to equal halves
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.invalidate_layout_constraints();
                for mi in 0..MON {
                    s.ratios[mi].clear();
                }
                s.clear_layouts();
            });
            retile();
        }
        Action::CycleLayout => cycle_layout(),
        Action::ToggleMonocle => toggle_monocle(),
        Action::FocusLast => focus_last(),
        Action::WorkspaceBack => workspace_back(),
        Action::MasterCount(step) => change_master_count(step),
        Action::FocusMonitor(step) => focus_monitor(step),
        Action::MoveToMonitor(step) => move_to_monitor(step),
        Action::ReloadConfig => reload_config(),
        Action::EditConfig => {
            config::ensure_default();
            tray::open(
                "notepad.exe",
                &format!("\"{}\"", config::config_path().display()),
            );
        }
    }
}
