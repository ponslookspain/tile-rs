//! Focus navigation, workspace switching and restoring parked windows.

use super::*;

pub(super) fn focus_hwnd(h: isize) {
    unsafe {
        let _ = SetForegroundWindow(HWND(h as *mut _));
    }
    // our own SetForegroundWindow produces a foreground event that the hook skips
    // (WINEVENT_SKIPOWNPROCESS), so update the border here as well
    STATE.with(|s| update_border(&mut s.borrow_mut(), h));
    sync_border();
}

pub(super) fn focus_first_or_desktop() {
    let first = STATE.with(|s| {
        let s = s.borrow();
        let mi = active_mon(&s);
        s.last[mi].first().copied()
    });
    match first {
        Some(h) => focus_hwnd(h),
        // empty workspace: take focus away from the (now hidden) old window
        None => focus_hwnd(unsafe { GetShellWindow() }.0 as isize),
    }
}

/// Returns whether a window was focused or swapped.
pub(super) fn focus_or_swap(dir: Dir, swap: bool) -> bool {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let mut do_retile = false;
    let mut to_focus: Option<isize> = None;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mi = s.mi(fg);
        if s.paused || s.last[mi].is_empty() || s.rects[mi].len() != s.last[mi].len() {
            return;
        }
        let Some(cur) = s.last[mi].iter().position(|h| *h == fg) else {
            // foreground window is not tiled here: just jump to the first tiled one
            if !swap {
                to_focus = Some(s.last[mi][0]);
            }
            return;
        };
        let to = if is_monocle(&s, mi, s.cur[mi]) {
            // all tiles coincide in monocle: left/up and right/down cycle through the windows
            if swap {
                return;
            }
            let n = s.last[mi].len() as i32;
            let step = if matches!(dir, Dir::Left | Dir::Up) {
                -1
            } else {
                1
            };
            (cur as i32 + step).rem_euclid(n) as usize
        } else {
            let Some(to) = neighbour(&s.rects[mi], cur, dir) else {
                return;
            };
            to
        };
        if swap {
            let (a, b) = (s.last[mi][cur], s.last[mi][to]);
            let (ia, ib) = (
                s.order[mi].iter().position(|h| *h == a),
                s.order[mi].iter().position(|h| *h == b),
            );
            if let (Some(ia), Some(ib)) = (ia, ib) {
                s.order[mi].swap(ia, ib);
            }
            s.last[mi].clear(); // force a re-layout with the new order
            do_retile = true;
        } else {
            to_focus = Some(s.last[mi][to]);
        }
    });
    let acted = to_focus.is_some_and(|h| h != fg) || do_retile;
    if let Some(h) = to_focus {
        focus_hwnd(h);
    }
    if do_retile {
        retile();
    }
    acted
}

/// Switch the workspace of the monitor whose indicator was clicked.
pub(super) fn switch_ws_on_monitor(mi: usize, n: u8) {
    switch_ws(mi, n, None);
}

/// Switch the workspace on the active monitor (keyboard action).
pub(super) fn switch_ws_active(n: u8) {
    let mi = STATE.with(|s| active_mon(&s.borrow()));
    switch_ws(mi, n, None);
}

/// The current workspace of `mi` offset by `step`, wrapped into `0..WS_COUNT`.
pub(super) fn relative_ws(mi: usize, step: i8) -> u8 {
    let cur = STATE.with(|s| s.borrow().cur[mi]) as i8;
    (cur + step).rem_euclid(WS_COUNT as i8) as u8
}

/// Switch the workspace of the monitor the user is on, `step` away (wrapping).
pub(super) fn switch_ws_relative(step: i8) {
    let mi = STATE.with(|s| active_mon(&s.borrow()));
    let n = relative_ws(mi, step);
    switch_ws(mi, n, None);
}

/// Move the focused window to the workspace `step` away (wrapping) and follow it there.
pub(super) fn move_ws_relative(step: i8) {
    let mi = STATE.with(|s| {
        let s = s.borrow();
        let h = unsafe { GetForegroundWindow() }.0 as isize;
        s.mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
            .min(MON - 1)
    });
    let n = relative_ws(mi, step);
    move_to_ws(n);
}

/// Commit a workspace change before native placement and focus effects run.
pub(super) fn switch_workspace_state(s: &mut State, mi: usize, n: u8, now: Instant) -> bool {
    if mi >= MON || s.paused || s.dragging.is_some() || n == s.cur[mi] || n >= WS_COUNT {
        return false;
    }
    s.prev_ws[mi] = Some(s.cur[mi]);
    s.slide = Some((mi, if n > s.cur[mi] { 1 } else { -1 }, now));
    s.cur[mi] = n;
    s.no_anim = true;
    let affected: HashSet<isize> = s.order[mi].iter().copied().collect();
    s.anims.retain(|a| !affected.contains(&a.hwnd));
    s.last[mi].clear();
    s.last_switch = Some(now);
    true
}

/// A media window that went fullscreen, or a window that opened maximized, may not have been
/// noticed yet: nothing re-tiles when a floating window changes size. Pin them to the workspace
/// they are on *before* it changes, or the next layout pass would pin them to the new one - and
/// they would cover it.
fn pin_covering_before_switch(mi: usize, n: u8) {
    let (will_switch, manual) = STATE.with(|s| {
        let s = s.borrow();
        let will_switch = mi < MON && !s.paused && s.dragging.is_none() && n != s.cur[mi];
        (
            will_switch,
            s.manual_float.union(&s.capacity_float).copied().collect(),
        )
    });
    if will_switch && n < WS_COUNT {
        let c = collect(false, &manual);
        STATE.with(|s| track_covering_windows(&mut s.borrow_mut(), &c));
    }
}

pub(super) fn switch_ws(mi: usize, n: u8, focus: Option<isize>) {
    end_divider(false);
    pin_covering_before_switch(mi, n);
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        switch_workspace_state(&mut s, mi, n, Instant::now())
    });
    if !changed {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let affected = s
            .mon
            .iter()
            .filter_map(|(h, monitor)| (*monitor == mi).then_some(*h))
            .collect();
        s.clear_placement_history_for(&affected);
    });
    retile();
    match focus {
        Some(h) => focus_hwnd(h),
        None => focus_workspace(mi, n),
    }
    tray::update();
    log(format!("switched to workspace {}", n + 1));
}

/// Focuses the fullscreen window of workspace `n` on monitor `mi` if it has one, otherwise the
/// window the user last worked in there (a maximized one included), otherwise the first tiled one.
pub(super) fn focus_workspace(mi: usize, n: u8) {
    let fs = STATE.with(|s| {
        let s = s.borrow();
        s.fs_ws
            .iter()
            .find(|(h, w)| {
                let hw = HWND(**h as *mut _);
                **w == n
                    && s.fs_mon
                        .get(*h)
                        .copied()
                        .unwrap_or_else(|| monitor::of_window(hw).min(MON - 1))
                        == mi
                    && !unsafe { IsIconic(hw) }.as_bool()
            })
            .map(|(h, _)| *h)
    });
    let remembered = fs.or_else(|| STATE.with(|s| remembered_focus(&s.borrow(), mi, n)));
    match remembered {
        Some(h) => focus_hwnd(h),
        None => focus_first_or_desktop(),
    }
}

/// A window dropped onto workspace dot `n` of monitor `mi` (or sent there from the dot's menu)
/// moves to that workspace. Returns whether it was moved. A drop then follows the window to that
/// workspace; the dot's menu leaves the user where they are.
pub(super) fn send_to_workspace(h: isize, mi: usize, n: u8) -> bool {
    let hwnd = HWND(h as *mut _);
    let plan = STATE.with(|s| {
        let s = s.borrow();
        if s.paused
            || n >= WS_COUNT
            || mi >= MON
            || !s.ws.contains_key(&h)
            || s.fs_ws.contains_key(&h)
        {
            return None;
        }
        Some(s.mi(h))
    });
    let Some(from) = plan else {
        retile(); // whatever happened, the dropped window goes back to its tile
        return false;
    };
    // where the window really is: a drag can leave it mostly on another monitor than its own
    let actual = monitor::of_window(hwnd).min(MON - 1);
    if actual != mi {
        // Post the move; retile keeps the explicit target until native geometry catches up.
        let work = monitor::work(mi);
        let moved = unsafe {
            SetWindowPos(
                hwnd,
                None,
                work.left + 40,
                work.top + 40,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            )
        };
        if moved.is_err() {
            retile();
            return false;
        }
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if actual != mi {
            s.update_tracked(h, |window| window.pending_monitor = Some(mi));
        } else {
            s.update_tracked(h, |window| window.pending_monitor = None);
        }
        if from != mi {
            s.order[from].retain(|window| *window != h);
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
            s.mon.insert(h, mi);
        }
        s.ws.insert(h, n);
        s.last[from].clear();
        s.last[actual].clear();
        s.last[mi].clear();
    });
    retile();
    tour_event("send");
    log(format!(
        "sent a window to workspace {} on monitor {}",
        n + 1,
        mi + 1
    ));
    true
}

/// Right-click on a workspace dot: switch to it, or move the active window there.
pub(super) fn dot_menu(mi: usize, n: u8) {
    // read before the menu takes the foreground
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let (tracked, from_mi) = STATE.with(|s| {
        let s = s.borrow();
        (
            s.ws.contains_key(&fg) && !s.fs_ws.contains_key(&fg),
            s.mi(fg),
        )
    });
    let open = format!("Open workspace {}", n + 1);
    let items: Vec<&str> = if tracked {
        vec![&open, "Move the active window here"]
    } else {
        vec![&open]
    };
    match tray::pick(&items) {
        Some(0) => switch_ws_on_monitor(mi, n),
        Some(1) => {
            let _ = send_to_workspace(fg, mi, n);
            refocus_after_menu(fg, from_mi);
        }
        _ => refocus_after_menu(fg, from_mi),
    }
}

/// The menu took the foreground: give it back to `h`, or, when `h` just left the screen, to the
/// first window of its former monitor's (`from_mi`) visible workspace.
pub(super) fn refocus_after_menu(h: isize, from_mi: usize) {
    let target = STATE.with(|s| {
        let s = s.borrow();
        if !unsafe { IsWindow(Some(HWND(h as *mut _))) }.as_bool() {
            return None;
        }
        let mi = s.mi(h);
        if s.ws.get(&h).is_none_or(|ws| *ws == s.cur[mi]) {
            Some(h)
        } else {
            s.last[from_mi.min(MON - 1)].first().copied()
        }
    });
    if let Some(target) = target {
        focus_hwnd(target);
    }
}

pub(super) fn move_window_workspace(s: &mut State, h: isize, n: u8) -> bool {
    if s.paused || s.dragging.is_some() || n >= WS_COUNT {
        return false;
    }
    if let Some(w) = s.fs_ws.get(&h).copied() {
        if w == n {
            return false;
        }
        s.fs_ws.insert(h, n);
        return true;
    }
    if !s.ws.contains_key(&h) || s.ws[&h] == n {
        return false;
    }
    s.ws.insert(h, n);
    true
}

pub(super) fn move_to_ws(n: u8) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let (mi, cur) = STATE.with(|s| {
        let s = s.borrow();
        let mi = s
            .mon
            .get(&fg)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(fg as *mut _)))
            .min(MON - 1);
        (mi, s.cur[mi])
    });
    // Reassign the window to workspace `n`: a tiled window moves its `ws` entry, a fullscreen one
    // its `fs_ws` entry. Either way `retile` below makes the new workspace show the right set.
    let moved = STATE.with(|s| {
        let mut s = s.borrow_mut();
        move_window_workspace(&mut s, fg, n)
    });
    if !moved {
        return;
    }
    if n != cur {
        // follow the window: open the target workspace with the moved window focused
        switch_ws(mi, n, Some(fg));
    } else {
        retile();
        focus_hwnd(fg);
    }
    tour_event("send");
    log(format!("moved a window to workspace {}", n + 1));
}

/// Forget windows that are gone or no longer parked, and persist the list when it changed.
pub(super) fn sync_parked(s: &mut State) {
    let before = s.parked.len();
    // Parking is asynchronous, so the real position is not checked here: an entry lives until its
    // window is gone or belongs to a visible workspace again.
    let drop: Vec<isize> = s
        .parked
        .keys()
        .copied()
        .filter(|h| {
            !unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool()
                || s.ws.get(h).is_none_or(|ws| *ws == s.cur[s.mi(*h)])
        })
        .collect();
    for h in drop {
        s.parked.remove(&h);
    }
    if s.parked_dirty || s.parked.len() != before {
        parked::save(&s.parked);
        s.parked_dirty = false;
    }
}

/// Windows left parked off-screen (e.g. after a crash) are brought back into view: first from the
/// persisted list (to their old positions), then any remaining ones found at a parking spot.
pub(super) fn recover_parked() {
    let restored = parked::restore_persisted();
    if restored > 0 {
        log(format!(
            "recovered {restored} parked window(s) from the saved list"
        ));
    }
    let manual = STATE.with(|s| s.borrow().manual_float.clone());
    let c = collect(false, &manual);
    for w in &c.tileable {
        if parked::restore_unlisted(w.hwnd.0 as isize) {
            log("recovered a window that was parked off-screen");
        }
    }
}

/// On exit: put every window of every workspace into the current workspace and tile them.
pub(super) fn bring_everything_back() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        // bring every window to its own monitor's current workspace
        let ws: Vec<(isize, u8)> = s.ws.keys().map(|h| (*h, s.cur[s.mi(*h)])).collect();
        for (h, w) in ws {
            s.ws.insert(h, w);
        }
        let fs: Vec<(isize, u8)> = s
            .fs_ws
            .keys()
            .map(|h| {
                let mi = s
                    .fs_mon
                    .get(h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(*h as *mut _)).min(MON - 1));
                (*h, s.cur[mi])
            })
            .collect();
        for (h, w) in fs {
            s.fs_ws.insert(h, w); // so the fullscreen windows are not hidden again below
        }
        s.clear_layouts();
        s.paused = false;
        s.no_anim = true; // the process is about to exit: no gliding
        restore_hidden_fullscreen(&mut s);
    });
    retile();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.parked.clear();
        parked::save(&s.parked);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_switch_parks_inactive_windows_and_returns_them_in_stable_order() {
        let mut s = State::default();
        s.order[0] = vec![11, 22, 33, 44, 55];
        s.order[1] = vec![66];
        for (h, ws) in [(11, 0), (22, 1), (33, 0), (44, 0), (55, 1), (66, 0)] {
            s.ws.insert(h, ws);
        }
        s.suspended.insert(55);
        let present = HashSet::from([11, 22, 33, 55, 66]);
        assert_eq!(
            workspace_visibility(&s, 0, &present),
            (vec![22], vec![11, 33])
        );
        let now = Instant::now();
        assert!(switch_workspace_state(&mut s, 0, 1, now));
        assert_eq!(
            workspace_visibility(&s, 0, &present),
            (vec![11, 33], vec![22])
        );
        assert_eq!(workspace_visibility(&s, 1, &present), (vec![], vec![66]));
        assert!(s.no_anim);
        assert_eq!(s.last_switch, Some(now));
        assert!(switch_workspace_state(&mut s, 0, 0, now));
        assert_eq!(
            workspace_visibility(&s, 0, &present),
            (vec![22], vec![11, 33])
        );
    }

    #[test]
    fn workspace_back_and_forth_preserves_each_monitors_independent_history() {
        let mut s = State::default();
        let now = Instant::now();
        assert!(s.prev_ws[0].is_none());
        assert!(switch_workspace_state(&mut s, 0, 2, now));
        assert!(switch_workspace_state(&mut s, 1, 3, now));
        for (expected, previous) in [(0, 2), (2, 0), (0, 2), (2, 0)] {
            let back = s.prev_ws[0].unwrap();
            assert!(switch_workspace_state(&mut s, 0, back, now));
            assert_eq!(s.cur[0], expected);
            assert_eq!(s.prev_ws[0], Some(previous));
            assert_eq!((s.cur[1], s.prev_ws[1]), (3, Some(0)));
        }
        assert!(!switch_workspace_state(&mut s, 0, 2, now));
        assert_eq!(s.prev_ws[0], Some(0));
        assert!(!switch_workspace_state(&mut s, MON, 1, now));
        assert!(!switch_workspace_state(&mut s, 0, WS_COUNT, now));
        s.paused = true;
        assert!(!switch_workspace_state(&mut s, 0, 1, now));
        s.paused = false;
        s.dragging = Some(11);
        assert!(!switch_workspace_state(&mut s, 0, 1, now));
    }

    #[test]
    fn workspace_move_reassigns_only_the_requested_tiled_or_fullscreen_window() {
        let mut s = State::default();
        s.ws.extend([(11, 0), (22, 0)]);
        s.mon.extend([(11, 0), (22, 0)]);
        s.fs_ws.insert(33, 1);
        s.fs_mon.insert(33, 1);
        s.order[0] = vec![11, 22];
        assert!(move_window_workspace(&mut s, 11, 2));
        assert_eq!(s.ws[&11], 2);
        assert_eq!(s.ws[&22], 0);
        assert_eq!(s.cur[0], 0); // native wrapper decides whether to follow the moved window
        assert_eq!(s.order[0], vec![11, 22]);
        assert_eq!(
            workspace_visibility(&s, 0, &HashSet::from([11, 22])),
            (vec![11], vec![22])
        );
        assert!(move_window_workspace(&mut s, 33, 2));
        assert_eq!(s.fs_ws[&33], 2);
        assert_eq!(s.fs_mon[&33], 1);
        assert!(!s.ws.contains_key(&33));
        assert!(!move_window_workspace(&mut s, 11, 2));
        assert!(!move_window_workspace(&mut s, 99, 1));
        assert!(!move_window_workspace(&mut s, 11, WS_COUNT));
        s.paused = true;
        assert!(!move_window_workspace(&mut s, 11, 1));
        assert_eq!(s.ws[&11], 2);
    }
}
