//! Single-instance startup, native message loop and shutdown.

use super::*;

/// `interactive` = started by double-click (launcher): may show dialogs.
pub(super) fn start(interactive: bool) {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // single instance
    let _mutex = match unsafe { CreateMutexW(None, true, w!("Local\\tile-rs-single-instance")) } {
        Ok(handle) => handle,
        Err(error) => {
            log(format!("could not acquire manager singleton: {error}"));
            notify(
                "tile-rs",
                "Could not acquire the single-instance lock. See tile-rs.log.",
            );
            return;
        }
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        log("(a second instance tried to start and exited)");
        if interactive {
            msgbox(
                "tile-rs",
                "tile-rs is already running.\n\nLook for its icon in the system tray (click ^ near the clock).",
                false,
            );
        }
        return;
    }
    log_reset();

    // config (created on first run; settings added by newer versions are appended to old files)
    let first_run = config::ensure_default();
    if !first_run && config::migrate_file() {
        log("config: added settings introduced by this version (backup: config.toml.bak)");
    }
    match config::load() {
        Ok(c) => {
            overlay::set_indicator_size(c.workspace_indicator_size);
            config::set(c);
        }
        Err(e) => {
            log(format!("config error: {e}"));
            notify("tile-rs: config error (built-in defaults are used)", &e);
        }
    }

    if interactive && first_run {
        let yes = msgbox(
            "tile-rs - first start",
            "Start tile-rs automatically when you sign in to Windows?\n\n\
             (You can change this later in the launcher or the tray menu.)",
            true,
        );
        if yes {
            set_autostart(true);
        }
    }

    window::SKIP_ZOOMED.store(true, Ordering::Relaxed);
    remember_monitor_slots();
    recover_parked();
    // A panic must not strand windows of other workspaces off-screen: bring them back from the
    // persisted list (it needs no application state), then run the normal hook.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let restored = parked::restore_persisted();
        log(format!("panic: restored {restored} parked window(s)"));
        previous_hook(info);
    }));
    let mut event_hooks: Vec<HWINEVENTHOOK> = Vec::new();
    let mut hook_failures = Vec::new();
    unsafe {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let mut hook = |lo: u32, hi: u32| {
            let handle = SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, flags);
            if handle.0.is_null() {
                hook_failures.push(format!(
                    "{lo:#06x}-{hi:#06x}: Win32 error {}",
                    GetLastError().0
                ));
            } else {
                event_hooks.push(handle);
            }
        };
        hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE); // created / destroyed / shown / hidden
        hook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND);
        hook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED); // other virtual desktop
        hook(EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND); // drag / resize
        hook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE);
        hook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE);
        hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND);
        hook(EVENT_SYSTEM_MENUPOPUPSTART, EVENT_SYSTEM_MENUPOPUPEND); // right-click menus
    }
    if !hook_failures.is_empty() {
        log(format!(
            "WinEvent hook registration failed: {}",
            hook_failures.join("; ")
        ));
        notify(
            "tile-rs: some window events are unavailable",
            "Window tracking may be incomplete. Check tile-rs.log for the failed hooks.",
        );
    }
    let failed = register_hotkeys();
    tray::create();
    launcher::create();
    update::start_background_checks();
    if !overlay::create_border() {
        log("could not create the border overlay window");
    }
    overlay::set_border_color(config::border_color_value());
    overlay::set_indicator_accent(config::accent_color_value());
    overlay::set_indicator_click(switch_ws_on_monitor);
    overlay::set_indicator_menu(dot_menu);
    overlay::create_indicator(monitor::count());
    let anim_timer = create_anim_timer();
    STATE.with(|s| s.borrow_mut().anim_timer = anim_timer);
    apply_focus_follows_timer();
    if config::focus_follows_mouse() {
        log("focus follows the mouse pointer");
    }
    log("tile-rs running");
    if !failed.is_empty() {
        notify(
            "tile-rs: some hotkeys were not registered",
            &failed.join("\n"),
        );
    }
    retile();
    refresh_border();
    STATE.with(|s| schedule_trim(&mut s.borrow_mut()));
    if interactive {
        launcher::show();
    }

    // One thread, one message loop. The animation frames come from a high-resolution waitable
    // timer (not WM_TIMER, which is the lowest priority and stutters while the mouse moves);
    // the debounce / live / verify timers stay ordinary SetTimer messages.
    unsafe {
        let mut msg = MSG::default();
        let timers = [anim_timer];
        let handles = if anim_timer.is_invalid() {
            &timers[..0]
        } else {
            &timers[..]
        };
        loop {
            replay_events();
            let r = MsgWaitForMultipleObjectsEx(
                Some(handles),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if r == WAIT_FAILED {
                log("MsgWaitForMultipleObjectsEx failed; stopping the message loop");
                break;
            }
            if !handles.is_empty() && r == WAIT_OBJECT_0 {
                anim_tick();
            }
            let mut quit = false;
            let drain_started = Instant::now();
            let mut drained = 0;
            while drained < 64
                && drain_started.elapsed() < Duration::from_millis(4)
                && PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool()
            {
                drained += 1;
                if msg.message == WM_QUIT {
                    quit = true;
                    break;
                }
                if msg.message == WM_PENDING_EVENTS {
                    PENDING_WAKE_POSTED.with(|p| p.set(false));
                    replay_events();
                    continue;
                }
                // Only *thread* timers (created with a NULL hwnd, i.e. the debounce / live / verify
                // timer) are ours to handle here. A WM_TIMER that belongs to a window (the
                // indicator's hover animation) must go through DispatchMessage to its own window
                // procedure - swallowing it would leave that timer running forever.
                if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
                    handle_debounce_timer(msg.wParam.0);
                    continue;
                }
                if msg.message == WM_HOTKEY {
                    let id = msg.wParam.0 as i32;
                    let action = STATE.with(|s| {
                        s.borrow()
                            .hotkeys
                            .iter()
                            .find(|(i, _)| *i == id)
                            .map(|(_, a)| *a)
                    });
                    if let Some(a) = action {
                        dispatch(a);
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if drained >= 64 || drain_started.elapsed() >= Duration::from_millis(4) {
                    break;
                }
            }
            if quit {
                break;
            }
        }
    }
    end_divider(false);
    divider::sync(Vec::new());
    clear_border();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.anims.clear();
        cancel_anim_timer(&mut s);
    });
    bring_everything_back();
    tray::remove();
    launcher::destroy();
    overlay::destroy_indicator();
    overlay::destroy_border();
    let (ffm_timer, drag_timer) = STATE.with(|s| {
        let s = s.borrow();
        (s.ffm_timer, s.drag_timer)
    });
    if ffm_timer != 0 {
        unsafe {
            let _ = KillTimer(None, ffm_timer);
        }
    }
    if drag_timer != 0 {
        unsafe {
            let _ = KillTimer(None, drag_timer);
        }
    }
    unsafe {
        let _ = CloseHandle(anim_timer);
        for hook in event_hooks {
            let _ = UnhookWinEvent(hook);
        }
    }
    log("bye");
}
