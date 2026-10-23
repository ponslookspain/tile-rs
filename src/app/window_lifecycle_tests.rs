//! Isolated regression fixtures: no desktop enumeration or user-window placement.

use super::*;
use windows::Win32::Foundation::LRESULT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    GWLP_WNDPROC, SW_HIDE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

fn rect(width: i32, height: i32) -> RECT {
    RECT {
        left: 10,
        top: 20,
        right: 10 + width,
        bottom: 20 + height,
    }
}

/// Holds the shared test-config lock for as long as the changed config is in effect.
struct ConfigGuard(
    config::Config,
    #[allow(dead_code)] std::sync::MutexGuard<'static, ()>,
);

impl ConfigGuard {
    fn new(change: impl FnOnce(&mut config::Config)) -> Self {
        let lock = config::test_lock();
        let old = config::current();
        let mut test = old.clone();
        test.border = false;
        test.workspace_indicator = false;
        test.animations = false;
        test.rules = vec![config::Rule {
            process: None,
            class: None,
            title: Some("tile-rs lifecycle fixture".into()),
            action: Some("tile".into()),
            workspace: None,
        }];
        change(&mut test);
        config::set(test);
        Self(old, lock)
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        config::set(self.0.clone());
    }
}

struct StateGuard(Option<State>);

impl StateGuard {
    fn new() -> Self {
        Self(Some(
            STATE.with(|state| std::mem::take(&mut *state.borrow_mut())),
        ))
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        let previous = self.0.take().unwrap();
        let state = STATE.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), previous));
        for timer in [
            state.timer,
            state.drag_timer,
            state.ffm_timer,
            state.frame_fallback,
        ] {
            if timer != 0 {
                unsafe {
                    let _ = KillTimer(None, timer);
                }
            }
        }
    }
}

struct Fixture(HWND);

impl Fixture {
    fn new() -> Self {
        Self(unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs lifecycle fixture"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                -25000,
                -25000,
                800,
                600,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        })
    }

    fn h(&self) -> isize {
        self.0 .0 as isize
    }

    fn native(&self) -> RECT {
        let mut current = RECT::default();
        unsafe {
            GetWindowRect(self.0, &mut current).unwrap();
        }
        current
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

#[test]
fn old_wide_frame_never_becomes_a_minimum_even_after_a_second() {
    for cycle in 0..30 {
        let now = Instant::now();
        let old = rect(3400, 1400);
        let target = rect(1700 - cycle, 1000);
        let mut request = PlacementRequest::new(cycle as u64, Some(old), target, now);
        assert!(!request.ready_to_query(old, now));
        assert!(!request.ready_to_query(old, now + Duration::from_millis(89)));
        assert!(!request.ready_to_query(old, now + Duration::from_millis(90)));
        assert!(request.ready_to_query(old, now + Duration::from_millis(1000)));
        // A normal system tracking minimum is not evidence that the previous full-width
        // frame was a client constraint, even though it remained unchanged for a second.
        assert_eq!(
            confirmed_minimum((0, 0), (1700 - cycle, 1000), (3400, 1400), (112, 27)),
            (0, 0)
        );
        assert_eq!(
            confirmed_minimum((0, 0), (1700 - cycle, 1000), (3400, 1400), (0, 0)),
            (0, 0)
        );
    }
}

#[test]
fn confirmed_tracking_limits_apply_only_to_rejected_axes() {
    assert_eq!(
        confirmed_minimum((0, 0), (1700, 600), (1700, 650), (300, 650)),
        (0, 650)
    );
    assert_eq!(
        confirmed_minimum((0, 0), (400, 1000), (640, 1000), (640, 300)),
        (640, 0)
    );
    // A reported limit larger than the actual settled frame cannot explain its size.
    assert_eq!(
        confirmed_minimum((0, 0), (400, 600), (500, 700), (800, 900)),
        (0, 0)
    );
    assert_eq!(
        confirmed_minimum((640, 650), (800, 800), (800, 800), (640, 650)),
        (640, 650)
    );
}

#[test]
fn changed_target_replaces_previous_placement_evidence() {
    let window = Fixture::new();
    let mut state = State::default();
    let old_frame = window.native();
    let first = rect(1700, 1000);
    let second = rect(1200, 700);
    note_placement_result(&mut state, window.0, first, Some(old_frame), true);
    let generation = state.placements[&window.h()].generation;
    let now = Instant::now() + Duration::from_secs(1);
    state.placements.get_mut(&window.h()).unwrap().sample = Some((old_frame, now));
    state.placements.get_mut(&window.h()).unwrap().queried = true;
    note_placement_result(&mut state, window.0, second, Some(old_frame), true);
    let current = &state.placements[&window.h()];
    assert!(current.generation > generation);
    assert_eq!(current.target, second);
    assert_eq!(current.before, Some(old_frame));
    assert!(current.sample.is_none() && !current.queried);
    assert_eq!(state.expected[&window.h()].0, second);
    let generation = current.generation;
    note_placement_result(&mut state, window.0, second, Some(old_frame), true);
    assert_eq!(state.placements[&window.h()].generation, generation);
}

#[test]
fn manually_floating_windows_never_move_on_fast_add_repeated_show() {
    for _ in 0..30 {
        let window = Fixture::new();
        let before = window.native();
        let mut state = State::default();
        state.manual_float.insert(window.h());
        assert!(!fast_add(&mut state, window.0));
        assert!(!state.ws.contains_key(&window.h()));
        assert!(state.anims.is_empty() && state.placements.is_empty());
        assert_eq!(window.native(), before);
    }
}

#[test]
fn minimize_and_hide_retain_workspace_without_consuming_a_tile() {
    let _config = ConfigGuard::new(|_| {});
    let window = Fixture::new();
    let h = window.h();
    let mut state = State::default();
    state.ws.insert(h, 2);
    state.mon.insert(h, 0);
    state.order[0].push(h);
    remember_identity(&mut state, window.0);
    let empty = HashSet::new();
    for _ in 0..30 {
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWMINNOACTIVE);
        }
        assert!(unsafe { IsIconic(window.0) }.as_bool());
        retain_assignments(&mut state, &empty, &empty, &empty);
        assert_eq!(state.ws.get(&h), Some(&2));
        assert_eq!(state.mon.get(&h), Some(&0));
        assert!(state.suspended.contains(&h));
        assert!(workspace_windows(&state, 0, 2).is_empty());
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        }
        retain_assignments(&mut state, &HashSet::from([h]), &empty, &empty);
        assert!(!state.suspended.contains(&h));
        assert_eq!(workspace_windows(&state, 0, 2), vec![h]);
        unsafe {
            let _ = ShowWindow(window.0, SW_HIDE);
        }
        retain_assignments(&mut state, &empty, &empty, &empty);
        assert_eq!(state.ws.get(&h), Some(&2));
        assert!(state.suspended.contains(&h));
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        }
    }
    assert_eq!(state.order[0], vec![h]);
    // An explicit float is not a temporary hidden state, even for a hidden live HWND.
    unsafe {
        let _ = ShowWindow(window.0, SW_HIDE);
    }
    retain_assignments(&mut state, &empty, &empty, &HashSet::from([h]));
    assert!(!state.ws.contains_key(&h));
}

#[test]
fn restored_suspended_window_requests_reconciliation_on_each_relevant_event() {
    let _config = ConfigGuard::new(|_| {});
    let _state = StateGuard::new();
    let window = Fixture::new();
    let h = window.h();
    for event in [
        EVENT_OBJECT_NAMECHANGE,
        EVENT_OBJECT_LOCATIONCHANGE,
        EVENT_SYSTEM_FOREGROUND,
    ] {
        STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.ws.insert(h, 0);
            state.mon.insert(h, 0);
            state.order[0] = vec![h];
            state.suspended.insert(h);
            state.windows_changed = false;
        });
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                event,
                window.0,
                0,
                0,
                0,
                0,
            );
        }
        STATE.with(|cell| {
            let state = cell.borrow();
            assert!(
                state.windows_changed,
                "event {event:#x} must schedule restored-window reconciliation"
            );
            assert_eq!(state.ws.get(&h), Some(&0));
        });
    }
}

#[test]
fn late_title_admits_a_ready_window_without_an_unrelated_show() {
    let _config = ConfigGuard::new(|_| {});
    let _state = StateGuard::new();
    let window = Fixture::new();
    unsafe {
        SetWindowTextW(window.0, w!("")).unwrap();
    }
    assert!(matches!(
        window::classify(window.0, &config::rules()),
        Verdict::Ignore
    ));
    unsafe {
        SetWindowTextW(window.0, w!("tile-rs lifecycle fixture")).unwrap();
        on_event(
            HWINEVENTHOOK(std::ptr::null_mut()),
            EVENT_OBJECT_NAMECHANGE,
            window.0,
            0,
            0,
            0,
            0,
        );
    }
    assert!(STATE.with(|cell| cell.borrow().windows_changed));
}

#[test]
fn workspace_cleanup_preserves_other_monitor_placement_evidence() {
    let mut state = State::default();
    let now = Instant::now();
    let target = rect(800, 600);
    for h in [11, 22] {
        state.placements.insert(
            h,
            PlacementRequest::new(h as u64, Some(rect(1600, 900)), target, now),
        );
        state.expected.insert(h, (target, now));
        state.geometry_retries.insert(h, (target, 1));
        state.placement_failures.insert(h, (target, 1));
        state.live_borders.insert(h, (1, 2, 3, 4));
    }
    state.clear_placement_history_for(&HashSet::from([11]));
    assert!(!state.placements.contains_key(&11) && !state.expected.contains_key(&11));
    assert!(
        !state.geometry_retries.contains_key(&11) && !state.placement_failures.contains_key(&11)
    );
    assert!(!state.live_borders.contains_key(&11));
    assert_eq!(state.placements[&22].target, target);
    assert_eq!(state.expected[&22].0, target);
    assert_eq!(state.geometry_retries[&22].1, 1);
    assert_eq!(state.placement_failures[&22].1, 1);
    assert_eq!(state.live_borders[&22], (1, 2, 3, 4));
}

#[test]
fn explicit_tile_limit_applies_even_when_automatic_overflow_is_disabled() {
    let _config = ConfigGuard::new(|config| {
        config.max_tiles = 1;
        config.auto_overflow = false;
    });
    let first = Fixture::new();
    let second = Fixture::new();
    let mi = monitor::of_window(second.0).min(MON - 1);
    let mut state = State::default();
    state.ws.insert(first.h(), 0);
    state.mon.insert(first.h(), mi);
    state.order[mi].push(first.h());
    state.last[mi].push(first.h());
    state.rects[mi].push(first.native());
    state.regions[mi].push(first.native());
    let before = second.native();
    assert!(!fast_add(&mut state, second.0));
    assert_eq!(workspace_for_new_window(&state, mi, 0, second.h()), None);
    assert_eq!(second.native(), before);
    assert!(!state.ws.contains_key(&second.h()));
}

unsafe extern "system" fn minimum_proc(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    if message == WM_GETMINMAXINFO {
        let info = &mut *(lp.0 as *mut MINMAXINFO);
        info.ptMinTrackSize = POINT { x: 640, y: 400 };
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, message, wp, lp)
}

#[test]
fn client_tracking_minimum_is_queried_and_confirms_only_rejected_width() {
    let window = Fixture::new();
    unsafe {
        SetWindowLongPtrW(window.0, GWLP_WNDPROC, minimum_proc as *const () as isize);
    }
    let native = window.native();
    let reported = query_minimum(window.0, &native, &native).unwrap();
    assert_eq!(reported, (640, 400));
    assert_eq!(
        confirmed_minimum((0, 0), (300, 600), (800, 600), reported),
        (640, 0)
    );
}

#[test]
fn minimum_query_does_not_wait_for_a_stalled_foreign_window_thread() {
    use std::sync::mpsc;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let window = Fixture::new();
        ready_tx.send((window.h(), window.native())).unwrap();
        let _ = release_rx.recv();
    });
    struct Release {
        sender: Option<mpsc::Sender<()>>,
        owner: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(());
            }
            if let Some(owner) = self.owner.take() {
                owner.join().unwrap();
            }
        }
    }
    let _release = Release {
        sender: Some(release_tx),
        owner: Some(owner),
    };
    let (h, native) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    assert_eq!(query_minimum(HWND(h as *mut _), &native, &native), None);
    assert!(
        started.elapsed() < Duration::from_millis(450),
        "minimum query blocked the manager"
    );
}

#[test]
fn a_stalled_client_remains_tiled_until_its_queued_resize_is_applied() {
    use std::sync::mpsc;
    let _config = ConfigGuard::new(|config| {
        config.animations = false;
        config.min_tile_width = 100;
        config.min_tile_height = 100;
    });
    let _state = StateGuard::new();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (pump_tx, pump_rx) = mpsc::channel::<RECT>();
    let (applied_tx, applied_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let window = Fixture::new();
        ready_tx.send(window.h()).unwrap();
        let Ok(target) = pump_rx.recv() else {
            return;
        };
        let started = Instant::now();
        let mut reported = false;
        while started.elapsed() < Duration::from_secs(2) {
            let mut message = MSG::default();
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
            }
            if !reported {
                if let Some(actual) = visible_rect(window.0) {
                    if same_rect(&actual, &target, 4) {
                        applied_tx.send(actual).unwrap();
                        reported = true;
                    }
                }
            }
            if stop_rx.try_recv().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    struct Release {
        pump: Option<mpsc::Sender<RECT>>,
        stop: Option<mpsc::Sender<()>>,
        owner: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.pump.take(); // Disconnect a worker still waiting for its first pump request.
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(owner) = self.owner.take() {
                owner.join().unwrap();
            }
        }
    }
    let release = Release {
        pump: Some(pump_tx),
        stop: Some(stop_tx),
        owner: Some(owner),
    };
    let h = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let hwnd = HWND(h as *mut _);
    let target = RECT {
        left: -25000,
        top: -25000,
        right: -24600,
        bottom: -24700,
    };
    let mi = monitor::of_window(hwnd).min(MON - 1);
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.ws.insert(h, 0);
        state.mon.insert(h, mi);
        state.order[mi] = vec![h];
        state.last[mi] = vec![h];
        state.rects[mi] = vec![target];
        move_window(&mut state, hwnd, target, false);
        let now = Instant::now();
        let actual = visible_rect(hwnd).expect("fixture must expose its unchanged DWM frame");
        assert!(actual.right - actual.left > target.right - target.left);
        state
            .expected
            .insert(h, (target, now - Duration::from_millis(180)));
        let request = state.placements.get_mut(&h).unwrap();
        request.submitted = now - Duration::from_millis(180);
        request.sample = Some((actual, now - Duration::from_millis(90)));
    });
    let checked = Instant::now();
    assert!(!enforce_layout());
    assert!(checked.elapsed() < Duration::from_millis(450));
    STATE.with(|cell| {
        let state = cell.borrow();
        assert!(
            !state.placements[&h].queried,
            "timed-out query must retain the responsiveness grace period"
        );
        assert!(!state.min_size.contains_key(&h));
        assert!(!state.capacity_float.contains(&h));
        assert_eq!(state.ws.get(&h), Some(&0));
        assert!(!state.geometry_retries.contains_key(&h));
    });
    std::thread::sleep(Duration::from_millis(900));
    release.pump.as_ref().unwrap().send(target).unwrap();
    let applied = applied_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(same_rect(&applied, &target, 4));
    assert!(!enforce_layout());
    STATE.with(|cell| {
        let state = cell.borrow();
        assert_eq!(state.ws.get(&h), Some(&0));
        assert!(state.last[mi].contains(&h));
        assert!(!state.min_size.contains_key(&h));
        assert!(!state.capacity_float.contains(&h));
    });
}

#[test]
fn new_float_policy_releases_hidden_assignments_without_restoring_user_minimized_windows() {
    let _config = ConfigGuard::new(|config| {
        config.rules[0].action = Some("float".into());
        config.fullscreen_windows = FullscreenWindows::Minimize;
    });
    let hidden = Fixture::new();
    let minimized = Fixture::new();
    unsafe {
        let _ = ShowWindow(hidden.0, SW_HIDE);
        let _ = ShowWindow(minimized.0, SW_SHOWMINNOACTIVE);
    }
    let mut state = State::default();
    for (window, ws) in [(&hidden, 2), (&minimized, 3)] {
        state.ws.insert(window.h(), ws);
        state.mon.insert(window.h(), 0);
        assert!(window::policy_excludes(window.0, &config::rules()));
    }
    let empty = HashSet::new();
    retain_assignments(&mut state, &empty, &empty, &empty);
    assert!(state.ws.is_empty() && state.mon.is_empty() && state.suspended.is_empty());

    let manager_minimized = Fixture::new();
    let user_minimized = Fixture::new();
    for window in [&manager_minimized, &user_minimized] {
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWMINNOACTIVE);
        }
        assert!(unsafe { IsIconic(window.0) }.as_bool());
        state.fs_ws.insert(window.h(), 4);
        state.fs_mon.insert(window.h(), 0);
    }
    state.fs_hidden.insert(manager_minimized.h());
    state.follow = Some((0, 4, manager_minimized.h()));
    reconcile_fullscreen(&mut state, &[]);
    assert!(state.fs_ws.is_empty() && state.fs_mon.is_empty() && state.fs_hidden.is_empty());
    assert!(state.follow.is_none());
    // ShowWindowAsync can finish after the reconciliation returns. Pump only this fixture.
    let started = Instant::now();
    while unsafe { IsIconic(manager_minimized.0) }.as_bool()
        && started.elapsed() < Duration::from_millis(500)
    {
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, Some(manager_minimized.0), 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!unsafe { IsIconic(manager_minimized.0) }.as_bool());
    assert!(unsafe { IsIconic(user_minimized.0) }.as_bool());
}

#[test]
fn prospective_workspace_capacity_uses_the_real_monitor_window_order() {
    let _config = ConfigGuard::new(|config| {
        config.layout = Layout::Spiral;
        config.min_tile_width = 100;
        config.min_tile_height = 100;
        config.inner_gap = 0;
        config.outer_gap = 0;
        config.max_tiles = 0;
    });
    let area = monitor_work(0);
    let height = area.bottom - area.top;
    assert!(height >= 200 && area.right - area.left >= 200);
    let (a, b, c) = (11, 22, 33);
    let mut state = State::default();
    state.order[0] = vec![a, b, c];
    for (h, ws) in [(a, 0), (b, 1), (c, 1)] {
        state.ws.insert(h, ws);
        state.mon.insert(h, 0);
    }
    state.min_size.insert(a, (100, height - 50));
    assert!(workspace_accepts(&state, 0, 1, a));
    let (actual, _) = fitted(area, &[a, b, c], &[], &state.min_size, false);
    assert!(valid_layout(0, &[a, b, c], &actual, &state.min_size));
    let (appended, _) = fitted(area, &[b, c, a], &[], &state.min_size, false);
    assert!(!valid_layout(0, &[b, c, a], &appended, &state.min_size));
}

#[test]
fn fullscreen_admission_stays_on_current_workspace_when_all_nine_are_occupied() {
    let mut state = State::default();
    for ws in 0..WS_COUNT {
        let h = 100 + isize::from(ws);
        state.fs_ws.insert(h, ws);
        state.fs_mon.insert(h, 0);
    }
    for current in 0..WS_COUNT {
        assert_eq!(next_free_workspace(&state, 0, current), current);
    }
}

#[test]
fn create_event_clears_stale_state_even_when_process_and_thread_identity_match() {
    let _config = ConfigGuard::new(|_| {});
    let _state = StateGuard::new();
    let window = Fixture::new();
    let h = window.h();
    let before = window.native();
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.ws.insert(h, 3);
        state.mon.insert(h, 0);
        state.order[0] = vec![h];
        state.last[0] = vec![h];
        state.rects[0] = vec![before];
        state.regions[0] = vec![before];
        state.manual_float.insert(h);
        state.min_size.insert(h, (1000, 900));
        state.follow = Some((0, 3, h));
        remember_identity(&mut state, window.0);
        assert_eq!(
            state.tracked.get(&h).and_then(|window| window.identity),
            native_identity(window.0)
        );
        note_placement_result(&mut state, window.0, rect(400, 300), Some(before), true);
    });
    unsafe {
        on_event(
            HWINEVENTHOOK(std::ptr::null_mut()),
            EVENT_OBJECT_CREATE,
            window.0,
            0,
            0,
            0,
            0,
        );
    }
    STATE.with(|cell| {
        let state = cell.borrow();
        assert!(!state.ws.contains_key(&h) && !state.mon.contains_key(&h));
        assert!(!state.manual_float.contains(&h) && !state.min_size.contains_key(&h));
        assert!(state
            .tracked
            .get(&h)
            .is_none_or(|window| window.identity.is_none()));
        assert!(state.follow.is_none());
        assert!(!state.expected.contains_key(&h) && !state.placements.contains_key(&h));
        assert!(!state.order[0].contains(&h) && !state.last[0].contains(&h));
        assert!(state.windows_changed);
    });
    assert_eq!(window.native(), before);
}

fn prepare_clamped_divider(windows: &[Fixture; 2]) -> (RECT, Vec<RECT>, Vec<RECT>) {
    // Small enough for any CI display: Windows will not size a real window beyond the screen.
    let area = RECT {
        left: -25000,
        top: -25000,
        right: -23600,
        bottom: -24300,
    };
    let gaps = Gaps {
        inner: 10,
        outer: 10,
    };
    let handles: Vec<_> = windows.iter().map(Fixture::h).collect();
    let (tiles, regions) = place_fitted(
        Layout::Spiral,
        area,
        &handles,
        &[0.5],
        &HashMap::new(),
        (480, 320),
        gaps,
    );
    let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gaps.inner)[0];
    let origin = POINT {
        x: (handle.rect.left + handle.rect.right) / 2,
        y: (handle.rect.top + handle.rect.bottom) / 2,
    };
    let mut gesture = divider::Resize {
        handle,
        layout: Layout::Spiral,
        area,
        gaps,
        windows: handles.clone(),
        workspace: 0,
        ratios: vec![0.5],
        tiles: tiles.clone(),
        preview: tiles.clone(),
        regions: regions.clone(),
        minimums: vec![(480, 320); 2],
        origin,
        last_pointer_axis: origin.x,
    };
    let far = POINT {
        x: origin.x + 100000,
        ..origin
    };
    assert!(gesture.pointer_changed(far));
    let (_, preview, _) = gesture.target(far);
    assert_eq!(preview[1].right - preview[1].left, 480);
    assert_ne!(preview, tiles);
    gesture.preview = preview.clone();
    STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        for h in &handles {
            state.ws.insert(*h, 0);
            state.mon.insert(*h, 0);
        }
        state.order[0] = handles.clone();
        state.last[0] = handles.clone();
        state.rects[0] = tiles.clone();
        state.regions[0] = regions;
        let targets: Vec<_> = handles
            .iter()
            .copied()
            .zip(preview.iter().copied())
            .collect();
        place_batch(&mut state, &targets);
        state.divider = Some(gesture);
    });
    (area, tiles, preview)
}

#[test]
fn divider_capture_loss_keeps_the_last_clamped_geometry() {
    let _config = ConfigGuard::new(|config| {
        config.layout = Layout::Spiral;
        config.inner_gap = 10;
        config.outer_gap = 10;
        config.min_tile_width = 480;
        config.min_tile_height = 320;
    });
    let _state = StateGuard::new();
    let windows = [Fixture::new(), Fixture::new()];
    let (area, original, preview) = prepare_clamped_divider(&windows);
    settle_divider();
    STATE.with(|cell| {
        let state = cell.borrow();
        assert!(state.divider.is_none());
        assert_eq!(state.rects[0], preview);
        assert_ne!(state.rects[0], original);
        let ratios = &state.ratios[0][&0];
        let restored = place_fitted(
            Layout::Spiral,
            area,
            &state.last[0],
            ratios,
            &HashMap::new(),
            (480, 320),
            Gaps {
                inner: 10,
                outer: 10,
            },
        )
        .0;
        assert_eq!(
            restored, preview,
            "committed split must survive the next layout pass"
        );
    });
    for (window, target) in windows.iter().zip(preview) {
        assert_eq!(visible_rect(window.0), Some(target));
    }
}

#[test]
fn divider_ignores_unrelated_tool_window_show_and_create() {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowLongW, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
    let _config = ConfigGuard::new(|config| {
        config.layout = Layout::Spiral;
        config.inner_gap = 10;
        config.outer_gap = 10;
    });
    let _state = StateGuard::new();
    let windows = [Fixture::new(), Fixture::new()];
    let (_, _, preview) = prepare_clamped_divider(&windows);
    let tooltip = Fixture::new();
    unsafe {
        SetWindowLongW(tooltip.0, GWL_EXSTYLE, WS_EX_TOOLWINDOW.0 as i32);
    }
    for event in [EVENT_OBJECT_SHOW, EVENT_OBJECT_CREATE] {
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                event,
                tooltip.0,
                0,
                0,
                0,
                0,
            );
        }
        STATE.with(|cell| {
            let state = cell.borrow();
            assert_eq!(
                state
                    .divider
                    .as_ref()
                    .expect("unmanaged popup must not end the drag")
                    .preview,
                preview
            );
            assert!(!state.windows_changed);
            assert_eq!(
                state.timer, 0,
                "unmanaged popup must not schedule a later rollback"
            );
        });
    }
    for (window, target) in windows.iter().zip(preview) {
        assert_eq!(visible_rect(window.0), Some(target));
    }
}
