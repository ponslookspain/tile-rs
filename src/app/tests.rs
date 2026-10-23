use super::*;

#[test]
fn repeated_client_repositioning_has_a_budget_per_target_and_window() {
    let mut retries = HashMap::new();
    let target = RECT {
        left: 10,
        top: 10,
        right: 500,
        bottom: 500,
    };
    for _ in 0..3 {
        assert!(allow_geometry_retry(&mut retries, 1, target));
    }
    assert!(!allow_geometry_retry(&mut retries, 1, target));
    assert!(allow_geometry_retry(&mut retries, 2, target));
    assert!(allow_geometry_retry(
        &mut retries,
        1,
        RECT {
            right: 600,
            ..target
        }
    ));
}

#[test]
fn frame_rate_limits_bound_visual_work_without_busy_polling() {
    assert_eq!(frame_interval_ms(240, 60), 17);
    assert_eq!(frame_interval_ms(144, 30), 34);
    assert_eq!(frame_interval_ms(60, 240), 17);
    assert_eq!(frame_interval_ms(240, 240), 5);
    assert_eq!(frame_interval_ms(0, 60), 1000);
}

#[test]
fn slower_monitor_skips_ticks_of_a_faster_timer_but_never_lags_by_more_than_a_tick() {
    let ms = Duration::from_millis;
    // A 144 Hz timer (7 ms ticks) drives a 60 Hz window (17 ms frames).
    assert!(!frame_due(ms(7), 17, 7));
    assert!(frame_due(ms(14), 17, 7)); // the nearest tick, not the one after
    assert!(frame_due(ms(21), 17, 7));
    // A window on the fastest monitor takes every tick.
    assert!(frame_due(ms(7), 7, 7));
    // With the timer at the window's own rate nothing is skipped, even with some jitter.
    assert!(frame_due(ms(16), 17, 17));
    assert!(frame_due(ms(9), 17, 17));
}

#[test]
fn unknown_destroy_does_not_schedule_a_desktop_scan() {
    STATE.with(|state| *state.borrow_mut() = State::default());
    unsafe {
        on_event(
            HWINEVENTHOOK(std::ptr::null_mut()),
            EVENT_OBJECT_DESTROY,
            HWND(987654usize as *mut _),
            0,
            0,
            0,
            0,
        );
    }
    STATE.with(|state| {
        let s = state.borrow();
        assert!(!s.windows_changed);
        assert_eq!(s.timer, 0);
    });
}

#[test]
fn rejected_height_does_not_lock_width_and_vice_versa() {
    assert_eq!(
        confirmed_minimum((0, 0), (2200, 600), (2200, 650), (200, 650)),
        (0, 650)
    );
    assert_eq!(
        confirmed_minimum((0, 0), (400, 1100), (480, 1100), (480, 200)),
        (480, 0)
    );
    assert_eq!(
        confirmed_minimum((480, 650), (2200, 800), (2201, 802), (200, 200)),
        (480, 650)
    );
    assert_eq!(
        confirmed_minimum((480, 650), (400, 600), (500, 700), (500, 700)),
        (500, 700)
    );
}

#[test]
fn pending_placement_cannot_be_learned_as_a_minimum_size() {
    let mut state = State::default();
    let target = RECT {
        left: 10,
        top: 10,
        right: 310,
        bottom: 210,
    };
    let now = Instant::now();
    state.expected.insert(11, (target, now));
    assert!(state.placement_pending(11, &target, now));
    assert!(!state.placement_pending(12, &target, now));
    assert!(!state.placement_pending(
        11,
        &RECT {
            right: 410,
            ..target
        },
        now
    ));
    assert!(!state.placement_pending(11, &target, now + Duration::from_millis(VERIFY_MS.into())));
}

#[test]
fn reloading_settings_allows_smaller_tiles_after_a_large_minimum_was_learned() {
    let area = RECT {
        left: 0,
        top: 0,
        right: 3440,
        bottom: 1440,
    };
    let gaps = Gaps {
        inner: 10,
        outer: 10,
    };
    let windows = [11, 12];
    let ratios = vec![0.2];
    let mut state = State::default();
    state.min_size.insert(11, (1200, 500));
    state.min_size_reflow = true;
    state.expected.insert(11, (area, Instant::now()));
    state.ratios[0].insert(0, ratios.clone());
    state.last[0] = windows.to_vec();
    let before = place_fitted(
        Layout::Spiral,
        area,
        &windows,
        &ratios,
        &state.min_size,
        (350, 200),
        gaps,
    )
    .0;
    assert!(before[0].right - before[0].left >= 1200);
    state.invalidate_layout_constraints();
    let after = place_fitted(
        Layout::Spiral,
        area,
        &windows,
        &ratios,
        &state.min_size,
        (350, 200),
        gaps,
    )
    .0;
    assert!(after[0].right - after[0].left < 1200);
    assert!(after[0].right - after[0].left >= 350);
    assert!(state.min_size.is_empty() && state.expected.is_empty());
    assert!(!state.min_size_reflow);
    assert!(state.last[0].is_empty());
    assert_eq!(state.ratios[0][&0], ratios);
}

#[test]
fn stalled_window_does_not_block_individual_or_batch_placement() {
    use std::sync::mpsc;
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};

    // This hidden window deliberately stops pumping messages. It models a client
    // busy with an expensive resize, without touching any of the user's windows.
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || unsafe {
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("tile-rs placement regression"),
            WS_POPUP,
            0,
            0,
            1000,
            800,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        ready_tx.send(hwnd.0 as isize).unwrap();
        release_rx.recv().unwrap();
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&msg);
        }
        let mut actual = RECT::default();
        GetWindowRect(hwnd, &mut actual).unwrap();
        DestroyWindow(hwnd).unwrap();
        actual
    });
    let h = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Always release the client, even if a regression makes placement synchronous.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(900));
        release_tx.send(()).unwrap();
    });
    let hwnd = HWND(h as *mut _);
    let started = Instant::now();
    let accepted = apply_window_pos(
        hwnd,
        10,
        10,
        200,
        200,
        SWP_NOZORDER | SWP_NOACTIVATE,
        "test",
    );
    let individual_elapsed = started.elapsed();
    // placements are kept inside the monitor's work area: make the target independent of it
    let target = monitor::clamp_to_work(RECT {
        left: 30,
        top: 40,
        right: 330,
        bottom: 240,
    });
    let mut state = State::default();
    let started = Instant::now();
    place_batch(&mut state, &[(h, target)]);
    let batch_elapsed = started.elapsed();
    let actual = owner.join().unwrap();
    release.join().unwrap();
    eprintln!(
        "native placement submission: single={}us batch={}us (client stalled 900ms)",
        individual_elapsed.as_micros(),
        batch_elapsed.as_micros()
    );
    assert!(accepted);
    assert!(
        individual_elapsed < Duration::from_millis(450),
        "individual placement blocked: {individual_elapsed:?}"
    );
    assert!(
        batch_elapsed < Duration::from_millis(450),
        "batch placement blocked: {batch_elapsed:?}"
    );
    assert_eq!(actual, target);
    assert!(!state.min_size.contains_key(&h));
}

#[test]
fn cancelling_live_divider_restores_real_windows_and_discards_pending_motion() {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_POPUP, WS_VISIBLE,
    };
    let area = RECT {
        left: -20000,
        top: -20000,
        right: -18800,
        bottom: -19000,
    };
    let gap = Gaps {
        inner: 10,
        outer: 10,
    };
    let (tiles, regions) = layout_tiles(Layout::Spiral, area, 2, &[0.5], gap);
    let windows: Vec<_> = tiles
        .iter()
        .map(|r| unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs live resize regression"),
                WS_POPUP | WS_VISIBLE,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                None,
                None,
            )
            .unwrap()
            .0 as isize
        })
        .collect();
    let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gap.inner)[0];
    let origin = POINT {
        x: handle.rect.left,
        y: handle.rect.top,
    };
    let gesture = divider::Resize {
        handle,
        layout: Layout::Spiral,
        area,
        gaps: gap,
        windows: windows.clone(),
        workspace: 0,
        ratios: vec![0.5],
        tiles: tiles.clone(),
        preview: tiles.clone(),
        regions: regions.clone(),
        minimums: vec![(100, 100); 2],
        origin,
        last_pointer_axis: origin.x,
    };
    let (_, changed, _) = gesture.target(POINT {
        x: origin.x + 150,
        ..origin
    });
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        *s = State::default();
        s.last[0] = windows.clone();
        s.rects[0] = tiles.clone();
        s.regions[0] = regions;
        for h in &windows {
            s.ws.insert(*h, 0);
            s.mon.insert(*h, 0);
        }
        let targets: Vec<_> = windows.iter().copied().zip(changed).collect();
        place_batch(&mut s, &targets);
        s.live_resize.queue(&targets);
        s.divider = Some(gesture);
    });
    end_divider(false);
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        assert!(s.divider.is_none());
        assert!(s
            .live_resize
            .take_ready(Instant::now(), |_| Some(area))
            .is_empty());
        for (h, original) in windows.iter().zip(tiles) {
            let mut actual = RECT::default();
            unsafe {
                GetWindowRect(HWND(*h as *mut _), &mut actual).unwrap();
            }
            assert_eq!(actual, original);
        }
        if s.timer != 0 {
            unsafe {
                let _ = KillTimer(None, s.timer);
            }
        }
        cancel_anim_timer(&mut s);
        *s = State::default();
    });
    for h in windows {
        unsafe {
            DestroyWindow(HWND(h as *mut _)).unwrap();
        }
    }
}

#[test]
fn delayed_or_clamped_geometry_does_not_cancel_a_live_divider() {
    let area = RECT {
        left: 0,
        top: 0,
        right: 1600,
        bottom: 1000,
    };
    let gaps = Gaps {
        inner: 10,
        outer: 10,
    };
    let (tiles, regions) = layout_tiles(Layout::Spiral, area, 2, &[0.5], gaps);
    let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gaps.inner)[0];
    let origin = POINT {
        x: handle.rect.left,
        y: handle.rect.top,
    };
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        *s = State::default();
        s.ws.extend([(11, 0), (12, 0)]);
        s.last[0] = vec![11, 12];
        s.rects[0] = tiles.clone();
        s.divider = Some(divider::Resize {
            handle,
            layout: Layout::Spiral,
            area,
            gaps,
            windows: vec![11, 12],
            workspace: 0,
            ratios: vec![0.5],
            tiles: tiles.clone(),
            preview: tiles,
            regions,
            minimums: vec![(100, 100); 2],
            origin,
            last_pointer_axis: origin.x,
        });
    });
    // Even a temporarily unavailable DWM rectangle is not an explicit user
    // cancellation. The old code rolled the whole layout back in this case.
    unsafe {
        on_event(
            HWINEVENTHOOK(std::ptr::null_mut()),
            EVENT_OBJECT_LOCATIONCHANGE,
            HWND(11usize as *mut _),
            0,
            0,
            0,
            0,
        );
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        assert!(s.divider.is_some());
        assert!(!s.reflow);
        *s = State::default();
    });
}

#[test]
fn moving_or_repeating_native_geometry_does_not_send_redundant_size_events() {
    use windows::Win32::Foundation::{LPARAM, LRESULT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, SetWindowLongPtrW, GWLP_WNDPROC, WM_SIZE,
        WS_POPUP,
    };
    thread_local! { static SIZES: Cell<u32> = const { Cell::new(0) }; }
    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_SIZE {
            SIZES.with(|sizes| sizes.set(sizes.get() + 1));
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
    unsafe {
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("tile-rs size notification regression"),
            WS_POPUP,
            -20000,
            -20000,
            400,
            300,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let original = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as *const () as isize);
        SIZES.with(|sizes| sizes.set(0));
        let moved = apply_window_pos(
            hwnd,
            -19990,
            -19990,
            400,
            300,
            SWP_NOZORDER | SWP_NOACTIVATE,
            "test-move",
        );
        let repeated = apply_window_pos(
            hwnd,
            -19990,
            -19990,
            400,
            300,
            SWP_NOZORDER | SWP_NOACTIVATE,
            "test-repeat",
        );
        let move_sizes = SIZES.with(|sizes| sizes.get());
        let resized = apply_window_pos(
            hwnd,
            -19990,
            -19990,
            500,
            300,
            SWP_NOZORDER | SWP_NOACTIVATE,
            "test-size",
        );
        let final_sizes = SIZES.with(|sizes| sizes.get());
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, original);
        DestroyWindow(hwnd).unwrap();
        assert!(moved && repeated && resized);
        assert_eq!(move_sizes, 0);
        assert_eq!(final_sizes, 1);
    }
}

#[test]
fn browser_can_grow_back_after_neighbour_rejects_height() {
    let area = RECT {
        left: 0,
        top: 0,
        right: 3440,
        bottom: 1440,
    };
    let gaps = Gaps {
        inner: 10,
        outer: 10,
    };
    let ratios = vec![0.35, 0.55];
    let (tiles, regions) = layout_tiles(Layout::Spiral, area, 3, &ratios, gaps);
    let right_width = tiles[1].right - tiles[1].left;
    let observed = confirmed_minimum((0, 0), (right_width, 600), (right_width, 650), (200, 650));
    let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gaps.inner)[0];
    let origin = POINT {
        x: handle.rect.left,
        y: handle.rect.top,
    };
    let gesture = divider::Resize {
        handle,
        layout: Layout::Spiral,
        area,
        gaps,
        windows: vec![1, 2, 3],
        workspace: 0,
        ratios,
        preview: tiles.clone(),
        tiles: tiles.clone(),
        regions,
        minimums: vec![
            (480, 320),
            (480.max(observed.0), 320.max(observed.1)),
            (480, 320),
        ],
        origin,
        last_pointer_axis: origin.x,
    };
    let (_, enlarged, _) = gesture.target(POINT {
        x: origin.x + 200,
        y: origin.y,
    });
    assert_eq!(enlarged[0].right - tiles[0].right, 200);
    let (_, shrunk, _) = gesture.target(POINT {
        x: origin.x - 200,
        y: origin.y,
    });
    assert_eq!(tiles[0].right - shrunk[0].right, 200);
}

#[test]
fn reentrant_destroy_event_is_replayed_after_the_layout_borrow() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        *state = State::default();
        state.ws.insert(11, 0);
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                EVENT_OBJECT_DESTROY,
                HWND(11usize as *mut _),
                0,
                0,
                0,
                0,
            );
        }
        assert!(state.ws.contains_key(&11));
        assert_eq!(PENDING_EVENTS.with(|events| events.borrow().len()), 1);
    });
    replay_events();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        assert!(!state.ws.contains_key(&11));
        if state.timer != 0 {
            unsafe {
                let _ = KillTimer(None, state.timer);
            }
        }
        *state = State::default();
    });
    assert!(PENDING_EVENTS.with(|events| events.borrow().is_empty()));
}

#[test]
fn automatic_tile_budget_grows_with_the_screen_and_is_capped() {
    // 1920x1080 work area, 480x320 comfortable tile: 1 + log2(13.5) = 4
    assert_eq!(auto_tile_budget(1920, 1040, 480, 320), 4);
    // a small screen still holds two, a huge one stops at eight
    assert_eq!(auto_tile_budget(800, 600, 480, 320), 2);
    assert_eq!(auto_tile_budget(7680, 4320, 480, 320), 8);
    // a degenerate area never panics
    assert_eq!(auto_tile_budget(0, 0, 0, 0), 2);
}

#[test]
fn slow_frames_suspend_animations_and_finish_running_ones() {
    let interval = Duration::from_millis(16);
    let mut s = State::default();
    s.anims.push(Anim {
        hwnd: 11,
        from: RECT::default(),
        to: RECT::default(),
        border: (0, 0, 0, 0),
        t0: Instant::now(),
        dur_ms: 500,
        last: None,
        frame_at: None,
        tangent: None,
    });
    // fast frames reset the streak
    note_frame_cost(&mut s, Duration::from_millis(40), interval);
    note_frame_cost(&mut s, Duration::from_millis(40), interval);
    note_frame_cost(&mut s, Duration::from_millis(5), interval);
    assert_eq!(s.slow_frames, 0);
    assert!(!animations_suspended(&s));
    for _ in 0..SLOW_FRAMES_LIMIT {
        note_frame_cost(&mut s, Duration::from_millis(40), interval);
    }
    assert!(animations_suspended(&s));
    assert_eq!(s.anims[0].dur_ms, 1);
}

#[test]
fn vsync_due_lands_before_the_next_refresh() {
    let freq = 10_000_000; // 10 MHz QPC: 1 tick = 100 ns
    let period = 166_667; // 60 Hz
    let lead = 20_000; // 2 ms
    let to_ticks = |due: i64| -due; // 100 ns units == ticks here
                                    // refresh was 5 ms ago: next one is 11.67 ms away, we fire 2 ms earlier
    let due = vsync_due_100ns(1_000_000, 950_000, period, freq, lead).unwrap();
    assert_eq!(to_ticks(due), 950_000 + period - 20_000 - 1_000_000);
    // a refresh timestamp in the future works the same way
    let future = vsync_due_100ns(
        1_000_000,
        1_000_000 + 3 * period + 50_000,
        period,
        freq,
        lead,
    )
    .unwrap();
    assert!(to_ticks(future) >= freq / 1000, "never sooner than 1 ms");
    assert!(to_ticks(future) <= period);
    // the frame is never scheduled in the past, even right before a refresh
    let tight = vsync_due_100ns(1_000_000, 1_000_000 + 5_000, period, freq, lead).unwrap();
    assert!(tight <= -(freq / 1000));
    assert_eq!(vsync_due_100ns(0, 0, 0, freq, lead), None);
}

#[test]
fn frame_batches_place_every_window_and_report_each_result() {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_POPUP, WS_VISIBLE,
    };
    let make = |x: i32| unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("tile-rs frame batch"),
            WS_POPUP | WS_VISIBLE,
            x,
            -20000,
            300,
            200,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    };
    let (a, b) = (make(-20000), make(-19000));
    let ops = [
        (
            a.0 as isize,
            RECT {
                left: -19900,
                top: -19900,
                right: -19500,
                bottom: -19600,
            },
            (0, 0, 0, 0),
        ),
        (
            b.0 as isize,
            RECT {
                left: -19000,
                top: -19900,
                right: -18800,
                bottom: -19800,
            },
            (0, 0, 0, 0),
        ),
        (0, RECT::default(), (0, 0, 0, 0)), // not a window: only its own result is false
    ];
    let results = apply_frames(&ops);
    assert_eq!(results.len(), 3);
    // posted requests are applied once the owner thread pumps messages (this one does)
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut ok = false;
    while Instant::now() < deadline && !ok {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
        }
        let mut ra = RECT::default();
        let mut rb = RECT::default();
        unsafe {
            GetWindowRect(a, &mut ra).unwrap();
            GetWindowRect(b, &mut rb).unwrap();
        }
        ok = ra.left == -19900 && ra.right == -19500 && rb.right == -18800 && rb.bottom == -19800;
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        let _ = DestroyWindow(a);
        let _ = DestroyWindow(b);
    }
    assert!(ok, "batched frame was not applied to both windows");
}

#[test]
fn frame_timer_has_a_cancellable_fallback_without_a_waitable_handle() {
    let mut state = State::default();
    arm_anim_timer(&mut state);
    assert!(state.frame_armed);
    assert_ne!(state.frame_fallback, 0);
    cancel_anim_timer(&mut state);
    assert!(!state.frame_armed);
    assert_eq!(state.frame_fallback, 0);
}

#[test]
fn animation_cannot_run_after_workspace_switch_pause_or_manual_drag() {
    let mut state = State::default();
    state.ws.insert(11, 0);
    assert!(state.animation_current(11));
    state.cur[0] = 1;
    assert!(!state.animation_current(11));
    state.cur[0] = 0;
    state.paused = true;
    assert!(!state.animation_current(11));
    state.paused = false;
    state.dragging = Some(11);
    assert!(!state.animation_current(11));
    assert!(!state.animation_current(12));
}

#[test]
fn expected_geometry_does_not_suppress_another_window_or_user_resize() {
    let mut state = State::default();
    let now = Instant::now();
    let target = RECT {
        left: 0,
        top: 0,
        right: 500,
        bottom: 400,
    };
    state.expected.insert(11, (target, now));
    assert!(state.expected_frame(11, &target, now));
    assert!(!state.expected_frame(12, &target, now));
    let resized = RECT {
        right: 700,
        ..target
    };
    assert!(!state.expected_frame(11, &resized, now));
    assert!(!state.expected_frame(11, &target, now + Duration::from_secs(2)));
}

#[test]
fn expected_location_event_still_corrects_a_stale_border() {
    let _config_lock = config::test_lock();
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_POPUP, WS_VISIBLE,
    };
    config::set(config::parse("border_width = 2").unwrap());
    unsafe {
        // Keep both test windows outside the desktop.
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("tile-rs border regression"),
            WS_POPUP | WS_VISIBLE,
            -20000,
            -20000,
            300,
            200,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let actual = visible_rect(hwnd).unwrap();
        let h = hwnd.0 as isize;
        assert!(overlay::create_border());
        let stale = RECT {
            left: actual.left - 30,
            ..actual
        };
        place_border(&stale, h);
        let before = overlay::border_rect().unwrap();
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            *s = State::default();
            s.border_on = Some(h);
            s.expected.insert(h, (actual, Instant::now()));
        });
        on_event(
            HWINEVENTHOOK::default(),
            EVENT_OBJECT_LOCATIONCHANGE,
            hwnd,
            0,
            0,
            0,
            0,
        );
        let after = overlay::border_rect().unwrap();
        let reconciled = STATE.with(|state| state.borrow().enforce);
        overlay::destroy_border();
        DestroyWindow(hwnd).unwrap();
        STATE.with(|state| *state.borrow_mut() = State::default());
        assert_ne!(before.left, after.left);
        assert_eq!(after.left, actual.left - 2);
        assert_eq!(after.top, actual.top - 2);
        assert_eq!(after.right, actual.right + 2);
        assert_eq!(after.bottom, actual.bottom + 2);
        assert!(
            !reconciled,
            "own placements must still skip layout reconciliation"
        );
    }
}

#[test]
fn border_leaves_a_window_that_is_hidden_or_gone() {
    let _config_lock = config::test_lock();
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ShowWindow, SW_HIDE, WS_POPUP, WS_VISIBLE,
    };
    config::set(config::parse("border_width = 2").unwrap());
    unsafe {
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("tile-rs hidden border regression"),
            WS_POPUP | WS_VISIBLE,
            -20000,
            -20000,
            300,
            200,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let h = hwnd.0 as isize;
        let actual = visible_rect(hwnd).unwrap();
        assert!(overlay::create_border());
        assert!(border_window_shown(hwnd));
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            *s = State::default();
            s.border_on = Some(h);
        });
        // hiding the window clears the border at once (the app may never destroy it)
        let _ = ShowWindow(hwnd, SW_HIDE);
        on_event(
            HWINEVENTHOOK::default(),
            EVENT_OBJECT_HIDE,
            hwnd,
            0,
            0,
            0,
            0,
        );
        let cleared = STATE.with(|state| state.borrow().border_on.is_none());
        // a stale placement request on a hidden window must not bring the border back
        place_border(&actual, h);
        let visible = overlay::border_visible();
        overlay::destroy_border();
        DestroyWindow(hwnd).unwrap();
        STATE.with(|state| *state.borrow_mut() = State::default());
        assert!(
            cleared,
            "border target must be dropped when its window hides"
        );
        assert!(!visible, "border must stay hidden for a hidden window");
        assert!(
            !border_window_shown(hwnd),
            "a destroyed window has no border"
        );
    }
}

#[test]
fn repeating_an_animation_target_preserves_its_start_time() {
    let _config_lock = config::test_lock();
    config::set(config::parse("animations = true").unwrap());
    let mut state = State::default();
    let target = monitor::clamp_to_work(RECT {
        left: 0,
        top: 0,
        right: 500,
        bottom: 400,
    });
    let started = Instant::now() - Duration::from_millis(40);
    state.anims.push(Anim {
        hwnd: 11,
        from: RECT::default(),
        to: target,
        border: (0, 0, 0, 0),
        t0: started,
        dur_ms: 140,
        last: None,
        frame_at: None,
        tangent: None,
    });
    move_window(&mut state, HWND(11usize as *mut _), target, true);
    assert_eq!(state.anims.len(), 1);
    assert_eq!(state.anims[0].t0, started);
}

#[test]
fn destroying_a_window_clears_its_expected_geometry_and_pending_motion() {
    let mut state = State::default();
    state.ws.insert(11, 0);
    state.expected.insert(11, (RECT::default(), Instant::now()));
    state.dragging = Some(11);
    state.resizing = true;
    state.live_borders.insert(12, (7, 0, 7, 7));
    state.live_resize.queue(&[(
        12,
        RECT {
            right: 100,
            bottom: 100,
            ..Default::default()
        },
    )]);
    assert!(forget_window(&mut state, 11));
    assert!(!state.ws.contains_key(&11));
    assert!(!state.expected.contains_key(&11));
    assert!(state.dragging.is_none());
    assert!(!state.resizing);
    assert!(state.live_borders.is_empty());
    assert!(state
        .live_resize
        .take_ready(Instant::now(), |_| Some(RECT::default()))
        .is_empty());
    assert!(state.reflow);
}

fn test_monitor(device: &str, left: i32, right: i32, primary: bool) -> monitor::Monitor {
    let rect = RECT {
        left,
        top: 0,
        right,
        bottom: 100,
    };
    monitor::Monitor {
        handle: windows::Win32::Graphics::Gdi::HMONITOR(std::ptr::null_mut()),
        device: device.into(),
        work: rect,
        full: rect,
        primary,
    }
}

#[test]
fn monitor_slot_map_follows_device_names_when_enumeration_reorders() {
    let old = [
        test_monitor("DISPLAY-A", 0, 100, true),
        test_monitor("DISPLAY-B", 100, 200, false),
    ];
    let new = [
        test_monitor("DISPLAY-B", 100, 200, false),
        test_monitor("DISPLAY-A", 0, 100, true),
    ];
    let identities = std::array::from_fn(|i| old.get(i).map(|m| m.device.clone()));
    let (mapped, exact) = monitor_slot_map(&old, &new, &identities);
    assert_eq!(&mapped[..2], &[1, 0]);
    assert!(exact[0] && exact[1]);
}

#[test]
fn monitor_slot_map_uses_nearest_survivor_when_a_display_disappears() {
    let old = [
        test_monitor("DISPLAY-A", 0, 100, true),
        test_monitor("DISPLAY-B", 100, 200, false),
    ];
    let new = [test_monitor("DISPLAY-A", 0, 100, true)];
    let identities = std::array::from_fn(|i| old.get(i).map(|m| m.device.clone()));
    let (mapped, exact) = monitor_slot_map(&old, &new, &identities);
    assert_eq!(mapped[0], 0);
    assert_eq!(mapped[1], 0);
    assert!(exact[0]);
    assert!(!exact[1]);
}

#[test]
fn session_latch_suppresses_parking_and_resets_after_cancellation() {
    SESSION_ENDING.with(|ending| ending.set(false));
    let mut s = State::default();
    assert!(!s.session_ending());
    SESSION_ENDING.with(|ending| ending.set(true));
    assert!(s.session_ending());
    assert!(s.session_ending);
    park(&mut s, HWND::default());
    assert!(s.parked.is_empty());
    SESSION_ENDING.with(|ending| ending.set(false));
    assert!(!s.session_ending());
}

#[test]
fn monitor_transfer_retains_target_until_native_geometry_acknowledges_it() {
    let mut s = State::default();
    let h = 42;
    s.ws.insert(h, 3);
    s.mon.insert(h, 1);
    s.update_tracked(h, |window| window.pending_monitor = Some(1));
    for _ in 0..4 {
        s.observe_monitor(h, 0);
        assert_eq!(s.mi(h), 1);
        assert_eq!(
            s.tracked.get(&h).and_then(|window| window.pending_monitor),
            Some(1)
        );
        assert_eq!(s.ws[&h], 3);
    }
    s.observe_monitor(h, 1);
    assert_eq!(s.mi(h), 1);
    assert!(s
        .tracked
        .get(&h)
        .is_none_or(|window| window.pending_monitor.is_none()));
    // Once acknowledged, an ordinary user move may change the monitor again.
    s.observe_monitor(h, 0);
    assert_eq!(s.mi(h), 0);
}

#[test]
fn closing_a_window_discards_its_pending_monitor_transfer() {
    let mut s = State::default();
    s.update_tracked(42, |window| window.pending_monitor = Some(1));
    assert!(drop_window_data(&mut s, 42));
    assert!(s.tracked.is_empty());
}

/// A per-window collection added to `State` must be cleaned when its window closes. This reads
/// the source so that forgetting `drop_window_data` for a new field fails here instead of leaking.
#[test]
fn every_per_window_collection_in_state_is_released_by_drop_window_data() {
    let state_source = include_str!("mod.rs");
    let body = state_source
        .split("struct State {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("State struct");
    let cleanup = include_str!("events.rs")
        .split("fn drop_window_data")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("drop_window_data");
    let per_window = ["HashMap<isize", "HashSet<isize", "Vec<isize"];
    let mut checked = 0;
    for line in body.lines().map(str::trim) {
        let Some((field, ty)) = line.split_once(": ") else {
            continue;
        };
        if field.contains(' ') || field.starts_with("//") {
            continue;
        }
        if per_window.iter().any(|keyed| ty.contains(keyed)) {
            checked += 1;
            assert!(
                cleanup.contains(&format!("s.{field}")),
                "State::{field} is keyed by window but drop_window_data never touches it"
            );
        }
    }
    assert!(checked >= 15, "found only {checked} per-window collections");
}

#[test]
fn closing_window_removes_every_cached_entry_and_reference() {
    let mut s = State::default();
    let closed = 11;
    let other = 22;
    let now = Instant::now();
    let rect = RECT {
        right: 400,
        bottom: 300,
        ..Default::default()
    };
    for h in [closed, other] {
        s.ws.insert(h, 0);
        s.mon.insert(h, 0);
        s.update_tracked(h, |window| window.pending_monitor = Some(1));
        s.min_size.insert(h, (200, 100));
        s.fs_ws.insert(h, 0);
        s.fs_mon.insert(h, 0);
        s.fs_hidden.insert(h);
        HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().insert(h));
        s.manual_float.insert(h);
        s.parked.insert(h, rect);
        s.capacity_float.insert(h);
        s.update_tracked(h, |window| window.float_reason = Some("fixture".into()));
        s.suspended.insert(h);
        s.update_tracked(h, |window| window.identity = Some((1, 2)));
        s.placement_failures.insert(h, (rect, 1));
        s.geometry_retries.insert(h, (rect, 1));
        s.expected.insert(h, (rect, now));
        s.placements
            .insert(h, PlacementRequest::new(1, Some(rect), rect, now));
        s.live_borders.insert(h, (1, 1, 1, 1));
        s.anims.push(Anim {
            hwnd: h,
            from: rect,
            to: rect,
            border: (0, 0, 0, 0),
            t0: now,
            dur_ms: 140,
            last: None,
            frame_at: None,
            tangent: None,
        });
        for mi in 0..MON {
            s.order[mi].push(h);
            s.last[mi].push(h);
            s.rects[mi].push(rect);
            s.regions[mi].push(rect);
        }
    }
    s.follow = Some((0, 0, closed));
    s.focus_cur = Some(closed);
    s.focus_prev = Some(closed);
    s.ffm_last = Some(closed);
    s.dragging = Some(closed);
    s.drag_preview = Some((0, closed));
    s.border_on = Some(closed);
    s.divider = Some(divider::Resize {
        handle: divider::Handle {
            monitor: 0,
            split: 0,
            vertical: true,
            rect,
        },
        layout: Layout::Spiral,
        area: rect,
        gaps: 8.into(),
        windows: vec![closed, other],
        workspace: 0,
        ratios: vec![0.5],
        tiles: vec![rect; 2],
        preview: vec![rect; 2],
        regions: vec![rect; 2],
        minimums: vec![(1, 1); 2],
        origin: POINT::default(),
        last_pointer_axis: 0,
    });
    assert!(forget_window(&mut s, closed));
    macro_rules! map_is_clean {
        ($($field:ident),+ $(,)?) => {$(
            assert!(!s.$field.contains_key(&closed), "{} retains closed HWND", stringify!($field));
            assert!(s.$field.contains_key(&other), "{} lost unrelated HWND", stringify!($field));
        )+};
    }
    map_is_clean!(
        ws,
        mon,
        tracked,
        min_size,
        fs_ws,
        fs_mon,
        parked,
        placement_failures,
        geometry_retries,
        expected,
        placements
    );
    for collection in [
        &s.fs_hidden,
        &s.manual_float,
        &s.capacity_float,
        &s.suspended,
    ] {
        assert!(!collection.contains(&closed));
        assert!(collection.contains(&other));
    }
    assert!(s.parked_dirty);
    assert!(!HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow().contains(&closed)));
    HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&other));
    for mi in 0..MON {
        assert_eq!(s.order[mi], vec![other]);
        assert!(!s.last[mi].contains(&closed));
    }
    assert_eq!(
        s.anims.iter().map(|a| a.hwnd).collect::<Vec<_>>(),
        vec![other]
    );
    assert!(s.live_borders.is_empty()); // ending a dragged window cancels the whole live gesture
    assert!(s.follow.is_none() && s.focus_cur.is_none() && s.focus_prev.is_none());
    assert!(s.ffm_last.is_none() && s.dragging.is_none() && s.drag_preview.is_none());
    assert!(s.border_on.is_none() && s.divider.is_none());
}

#[test]
fn closing_window_forgets_pending_and_sent_resize_without_losing_another_window() {
    let mut s = State::default();
    let now = Instant::now();
    let before = RECT::default();
    let target = RECT {
        right: 400,
        bottom: 300,
        ..Default::default()
    };
    s.ws.insert(11, 0);
    s.ws.insert(22, 0);
    s.live_resize.queue(&[(11, target), (22, target)]);
    assert_eq!(s.live_resize.take_ready(now, |_| Some(before)).len(), 2);
    s.live_resize.queue(&[(11, target), (22, target)]);
    assert!(forget_window(&mut s, 11));
    assert_eq!(
        s.live_resize
            .take_ready(now + Duration::from_millis(100), |_| Some(before)),
        vec![(22, target)]
    );
    let constrained = RECT {
        right: 600,
        bottom: 500,
        ..Default::default()
    };
    assert_eq!(
        s.live_resize
            .constrained_targets(now + Duration::from_millis(200), |_| Some(constrained)),
        vec![(22, target)]
    );
}

#[test]
fn optional_tracked_fields_preserve_siblings_and_discard_empty_entries() {
    let mut s = State::default();
    s.update_tracked(11, |window| {
        window.identity = Some((1, 2));
        window.pending_monitor = Some(1);
        window.float_reason = Some("fixture".into());
    });
    s.observe_monitor(11, 1);
    assert_eq!(s.tracked[&11].identity, Some((1, 2)));
    assert_eq!(s.tracked[&11].float_reason.as_deref(), Some("fixture"));
    assert!(s.tracked[&11].pending_monitor.is_none());
    s.invalidate_layout_constraints();
    assert_eq!(s.tracked[&11].identity, Some((1, 2)));
    assert!(s.tracked[&11].float_reason.is_none());
    s.update_tracked(11, |window| window.identity = None);
    assert!(!s.tracked.contains_key(&11));
    s.update_tracked(22, |window| window.pending_monitor = None);
    assert!(s.tracked.is_empty());
}

#[test]
fn a_lone_window_is_admitted_with_smart_gaps() {
    let _config_lock = config::test_lock();
    config::set(config::parse("smart_gaps = true\nmax_tiles = 0\nauto_overflow = true").unwrap());
    let mut s = State::default();
    // the lone window fills the work area without gaps; validation must use the same gaps
    assert!(workspace_accepts(&s, 0, 0, 1));
    s.ws.insert(1, 0);
    s.mon.insert(1, 0);
    s.order[0].push(1);
    assert!(
        workspace_accepts(&s, 0, 0, 2),
        "a second window joins with normal gaps"
    );
    config::set(config::parse("").unwrap());
}
