//! Deferred WinEvent replay, window lifecycle cleanup and debounce timers.

use super::*;

pub(super) fn replay_events() {
    let budget = PENDING_EVENTS.with(|pending| pending.borrow().len().min(64));
    let started = Instant::now();
    for _ in 0..budget {
        if started.elapsed() >= Duration::from_millis(4) {
            break;
        }
        let Some((event, hwnd, object, child, thread, time)) =
            PENDING_EVENTS.with(|pending| pending.borrow_mut().pop_front())
        else {
            break;
        };
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                event,
                hwnd,
                object,
                child,
                thread,
                time,
            );
        }
    }
    wake_pending_events();
}

pub(super) fn wake_pending_events() {
    if PENDING_EVENTS.with(|q| q.borrow().is_empty())
        || PENDING_WAKE_POSTED.with(|p| p.replace(true))
    {
        return;
    }
    let posted = unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
            windows::Win32::System::Threading::GetCurrentThreadId(),
            WM_PENDING_EVENTS,
            WPARAM(0),
            LPARAM(0),
        )
    };
    if posted.is_err() {
        PENDING_WAKE_POSTED.with(|p| p.set(false));
    }
}

/// Drop every piece of cached state for a destroyed HWND immediately. HWND values are reusable;
/// waiting for the debounced EnumWindows pass can accidentally transfer stale workspace/order
/// state to a newly created window that receives the same numeric handle.
/// Removes `h` from every per-window collection of [`State`] and reports whether it was in any of
/// them. This is the single place that knows those collections: a new per-window map must be
/// added here, and it is then forgotten (and counted as tracked) everywhere.
pub(super) fn drop_window_data(s: &mut State, h: isize) -> bool {
    let mut tracked = false;
    tracked |= s.live_borders.remove(&h).is_some();
    tracked |= s.ws.remove(&h).is_some();
    tracked |= s.mon.remove(&h).is_some();
    tracked |= s.fs_ws.remove(&h).is_some();
    tracked |= s.fs_mon.remove(&h).is_some();
    tracked |= s.fs_hidden.remove(&h);
    HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
    tracked |= s.manual_float.remove(&h);
    tracked |= s.capacity_float.remove(&h);
    tracked |= s.float_reasons.remove(&h).is_some();
    tracked |= s.suspended.remove(&h);
    tracked |= s.identities.remove(&h).is_some();
    tracked |= s.min_size.remove(&h).is_some();
    tracked |= s.placement_failures.remove(&h).is_some();
    tracked |= s.geometry_retries.remove(&h).is_some();
    tracked |= s.expected.remove(&h).is_some();
    tracked |= s.placements.remove(&h).is_some();
    if s.follow.is_some_and(|(_, _, window)| window == h) {
        s.follow = None;
        tracked = true;
    }
    let animations = s.anims.len();
    s.anims.retain(|a| a.hwnd != h);
    tracked |= s.anims.len() != animations;
    for mi in 0..MON {
        let ordered = s.order[mi].len();
        s.order[mi].retain(|window| *window != h);
        tracked |= s.order[mi].len() != ordered;
        if let Some(i) = s.last[mi].iter().position(|window| *window == h) {
            tracked = true;
            s.last[mi].remove(i);
            if i < s.rects[mi].len() {
                s.rects[mi].remove(i);
            }
            if i < s.regions[mi].len() {
                s.regions[mi].remove(i);
            }
        }
    }
    tracked |= s.dragging == Some(h);
    tracked |= s.border_on == Some(h);
    tracked |= s.drag_preview.is_some_and(|(_, preview)| preview == h);
    tracked
}

pub(super) fn forget_window(s: &mut State, h: isize) -> bool {
    // `live_resize` has no membership query; forgetting an unknown window is a no-op
    s.live_resize.forget(&[h]);
    if !drop_window_data(s, h) {
        return false;
    }
    if s.dragging == Some(h) {
        s.live_resize.clear();
        s.live_borders.clear();
        s.drop_dot = None;
        overlay::set_drop_target(None);
        overlay::raise_indicators(false);
        s.dragging = None;
        s.drag_started = None;
        s.resizing = false;
        s.live = false;
        if s.drag_timer != 0 {
            unsafe {
                let _ = KillTimer(None, s.drag_timer);
            }
            s.drag_timer = 0;
        }
    }
    if s.drag_preview.is_some_and(|(_, preview)| preview == h) {
        s.drag_preview = None;
    }
    if s.border_on == Some(h) {
        s.border_on = None;
        overlay::hide_border();
    }
    s.clear_layouts();
    s.reflow = true;
    true
}

pub(super) fn native_identity(hwnd: HWND) -> Option<(u32, u32)> {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0 && thread != 0).then_some((pid, thread))
}

pub(super) fn remember_identity(s: &mut State, hwnd: HWND) {
    if let Some(identity) = native_identity(hwnd) {
        s.identities.insert(hwnd.0 as isize, identity);
    }
}

pub(super) fn temporarily_unavailable(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() || IsZoomed(hwnd).as_bool()
        {
            return true;
        }
        let mut cloaked: u32 = 0;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

pub(super) fn retain_assignments(
    s: &mut State,
    present: &HashSet<isize>,
    fullscreen: &HashSet<isize>,
    excluded: &HashSet<isize>,
) {
    let rules = config::rules();
    let retained: HashSet<_> =
        s.ws.keys()
            .copied()
            .filter(|h| {
                present.contains(h)
                    || (!excluded.contains(h)
                        && !fullscreen.contains(h)
                        && temporarily_unavailable(HWND(*h as *mut _))
                        && !window::policy_excludes(HWND(*h as *mut _), &rules))
            })
            .collect();
    s.ws.retain(|h, _| retained.contains(h));
    s.mon.retain(|h, _| retained.contains(h));
    s.suspended = retained.difference(present).copied().collect();
    if s.follow
        .is_some_and(|(_, _, h)| !s.ws.contains_key(&h) && !s.fs_ws.contains_key(&h))
    {
        s.follow = None;
    }
}

/// Remove cached identity before a missed destroy can transfer state to another process.
pub(super) fn prune_identities(s: &mut State) {
    let stale: Vec<_> = s
        .identities
        .iter()
        .filter_map(|(h, identity)| {
            (native_identity(HWND(*h as *mut _)) != Some(*identity)).then_some(*h)
        })
        .collect();
    for h in stale {
        forget_window(s, h);
    }
}

pub(super) fn float_for_capacity(s: &mut State, h: isize, reason: &str) {
    let mi = s
        .mon
        .get(&h)
        .copied()
        .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)).min(MON - 1));
    s.ws.remove(&h);
    s.mon.remove(&h);
    s.suspended.remove(&h);
    s.capacity_float.insert(h);
    s.float_reasons.insert(h, reason.to_owned());
    s.anims.retain(|animation| animation.hwnd != h);
    s.placements.remove(&h);
    s.expected.remove(&h);
    s.geometry_retries.remove(&h);
    if s.follow.is_some_and(|(_, _, window)| window == h) {
        s.follow = None;
    }
    s.live_resize.forget(&[h]);
    for order in &mut s.order {
        order.retain(|window| *window != h);
    }
    let hwnd = HWND(h as *mut _);
    if is_parked(hwnd) {
        let area = monitor_work(mi);
        let _ = apply_window_pos(
            hwnd,
            area.left + 20,
            area.top + 20,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            "capacity-float",
        );
    }
    remember_identity(s, hwnd);
    log(format!("floating: hwnd={h:#x} reason={reason}"));
}

/// Recheck old workspaces too: a newly confirmed minimum can invalidate an existing layout.
pub(super) fn reconcile_capacity(s: &mut State) {
    for mi in 0..monitor::count().min(MON) {
        for ws in 0..WS_COUNT {
            loop {
                let windows = workspace_windows(s, mi, ws);
                if windows.is_empty() {
                    break;
                }
                let ratios = s.ratios[mi].get(&ws).map(Vec::as_slice).unwrap_or(&[]);
                let monocle = is_monocle(s, mi, ws);
                let (tiles, _) = fitted(monitor_work(mi), &windows, ratios, &s.min_size, monocle);
                if windows.len() <= tile_budget(mi)
                    && valid_layout(mi, &windows, &tiles, &s.min_size)
                {
                    break;
                }
                let h = *windows.last().unwrap();
                s.ws.remove(&h);
                let destination = config::auto_overflow()
                    .then(|| {
                        (1..WS_COUNT)
                            .map(|offset| (ws + offset) % WS_COUNT)
                            .find(|candidate| workspace_accepts(s, mi, *candidate, h))
                    })
                    .flatten();
                if let Some(destination) = destination {
                    s.ws.insert(h, destination);
                    if ws == s.cur[mi] {
                        s.follow = Some((mi, destination, h));
                    }
                    log(format!("overflow: hwnd={h:#x} monitor={} workspace={} -> {} reason=layout constraints", mi + 1, ws + 1, destination + 1));
                } else {
                    float_for_capacity(s, h, "no workspace satisfies window/tile minimums");
                }
            }
        }
    }
}

pub(super) unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if SESSION_ENDING.with(Cell::get) {
        return;
    }
    // Only whole-window events (OBJID_WINDOW == 0, no child element). Popup menus report
    // OBJID_CLIENT and would be filtered out here, but they matter: our border would otherwise be
    // left sitting above the menu.
    let menu = event == EVENT_SYSTEM_MENUPOPUPSTART || event == EVENT_SYSTEM_MENUPOPUPEND;
    if !menu && (id_object != 0 || id_child != 0 || hwnd.is_invalid()) {
        return;
    }
    // Foreign window APIs can pump WinEvents while a layout borrows STATE. Preserve their order
    // and replay after that transaction instead of panicking across the Win32 callback boundary.
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        PENDING_EVENTS.with(|pending| {
            let mut pending = pending.borrow_mut();
            let previous = pending.len();
            let key = (event == EVENT_OBJECT_LOCATIONCHANGE).then_some(hwnd.0 as isize);
            pending.push((event, hwnd, id_object, id_child, _thread, _time), key);
            if prof::on() {
                prof::EVENTS_QUEUED.fetch_add(1, Ordering::Relaxed);
                prof::EVENT_QUEUE_PEAK.fetch_max(
                    pending.len().min(u32::MAX as usize) as u32,
                    Ordering::Relaxed,
                );
                if pending.len() == previous {
                    prof::EVENTS_COALESCED.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        wake_pending_events();
        return;
    }
    let h = hwnd.0 as isize;
    let unrelated_divider_event = STATE.with(|state| {
        let s = state.borrow();
        s.divider
            .as_ref()
            .is_some_and(|gesture| !gesture.windows.contains(&h))
            && matches!(
                event,
                EVENT_OBJECT_CREATE
                    | EVENT_OBJECT_SHOW
                    | EVENT_OBJECT_UNCLOAKED
                    | EVENT_SYSTEM_MINIMIZEEND
            )
            && !matches!(
                window::classify_with_manual(hwnd, &config::rules(), &s.manual_float),
                Verdict::Tileable(_) | Verdict::Fullscreen(_)
            )
    });
    if unrelated_divider_event {
        return; // responsive client popups/tooltips do not change the tiled window set
    }
    let cancel_divider = STATE.with(|state| {
        let s = state.borrow();
        let Some(gesture) = s.divider.as_ref() else {
            return false;
        };
        match event {
            EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_CLOAKED
            | EVENT_SYSTEM_MINIMIZESTART => gesture.windows.contains(&h),
            EVENT_OBJECT_SHOW
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZEEND
            | EVENT_SYSTEM_MOVESIZESTART
            | EVENT_SYSTEM_MENUPOPUPSTART => true,
            EVENT_SYSTEM_FOREGROUND => !gesture.windows.contains(&h),
            // A live client may clamp its size or report a delayed DWM frame.
            // Only explicit lifecycle/input changes cancel ownership of the gesture.
            EVENT_OBJECT_LOCATIONCHANGE => false,
            _ => false,
        }
    });
    if cancel_divider {
        if event == EVENT_SYSTEM_FOREGROUND || event == EVENT_SYSTEM_MENUPOPUPSTART {
            settle_divider();
        } else {
            end_divider(false);
        }
    }
    let mut resync = false;
    // Focus moved within the current workspace: the border target was just set to the event's
    // window, so it only has to be repositioned (no re-derivation from `GetForegroundWindow`,
    // which can still race with the click).
    let mut reposition = false;
    // Did this event ask for any actual work? Only then is the debounce timer armed: an event we
    // ignore (e.g. an unrelated window moving) must not cause a whole re-collect 40 ms later.
    let mut dirty = false;
    // A dragged window dropped onto a workspace dot: (window, monitor, workspace).
    let mut dot_drop: Option<(isize, usize, u8)> = None;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if event == EVENT_OBJECT_CREATE && s.identities.contains_key(&h) {
            // CREATE is a lifecycle boundary even when the old HWND was reused by
            // the same process/thread and its DESTROY notification was missed.
            forget_window(&mut s, h);
            s.windows_changed = true;
            dirty = true;
        }
        // A window that is hidden, cloaked, minimized or destroyed has nothing left to outline.
        // Many apps hide a window long before (or instead of) destroying it, so do not wait for
        // the debounced re-tile to notice.
        // (A replayed HIDE for a window that is visible again must not clear it.)
        if s.border_on == Some(h)
            && (event == EVENT_OBJECT_DESTROY
                || (matches!(
                    event,
                    EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED | EVENT_SYSTEM_MINIMIZESTART
                ) && !border_window_shown(hwnd)))
        {
            s.border_on = None;
            overlay::hide_border();
        }
        match event {
            EVENT_SYSTEM_MENUPOPUPSTART => {
                // a popup menu (e.g. a right-click menu) is drawn above the windows but below our
                // border overlay, so hide the border while the menu is up
                s.border_on = None;
                overlay::hide_border();
                return;
            }
            EVENT_SYSTEM_MENUPOPUPEND => {
                // the menu is gone: let the caller re-evaluate and re-show the border
                resync = true;
                return;
            }
            EVENT_SYSTEM_MOVESIZESTART => {
                // let the user drag freely; we decide what to do while / when they let go
                if !s.paused {
                    let mi = s.mi(h);
                    if let Some(k) = s.last[mi].iter().position(|x| *x == h) {
                        if k < s.rects[mi].len() {
                            s.drop_dot = None;
                            s.dragging = Some(h);
                            overlay::raise_indicators(true);
                            s.placement_failures.remove(&h);
                            s.geometry_retries.remove(&h);
                            s.drag_started = Some(Instant::now());
                            if s.drag_timer != 0 {
                                let _ = KillTimer(None, s.drag_timer);
                            }
                            s.drag_timer = SetTimer(None, 0, DRAG_WATCHDOG_MS, None);
                            let affected: HashSet<isize> = s.last[mi].iter().copied().collect();
                            s.anims.retain(|a| !affected.contains(&a.hwnd));
                            s.resizing = false;
                            s.live = false;
                            s.drag_preview = None;
                            // the overlay would otherwise chase the window (and its "whole monitor"
                            // guard makes it blink while the frame is in transition), so hide it
                            // for the duration of the drag and let the re-tile restore it
                            s.border_on = None;
                            overlay::hide_border();
                            divider::sync(Vec::new());
                            s.live_resize.clear();
                            s.live_borders.clear();
                            let windows = s.last[mi].clone();
                            capture_live_borders(&mut s, &windows);
                            // Remember the window's *actual* size, not the tile's: an app with a
                            // minimum size (or one that resized itself) can be larger than its
                            // tile, and comparing a plain move against the tile size would then be
                            // mistaken for a resize, which corrupts the split ratios.
                            s.drag_size = visible_rect(hwnd)
                                .map(|v| (v.right - v.left, v.bottom - v.top))
                                .unwrap_or_else(|| {
                                    let t = s.rects[mi][k];
                                    (t.right - t.left, t.bottom - t.top)
                                });
                        }
                    }
                }
                return;
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                if s.dragging == Some(h) {
                    s.dragging = None;
                    s.placement_failures.remove(&h);
                    s.geometry_retries.remove(&h);
                    s.drag_started = None;
                    if s.drag_timer != 0 {
                        let _ = KillTimer(None, s.drag_timer);
                        s.drag_timer = 0;
                    }
                    let old_mi = s.mi(h);
                    let new_mi = monitor::of_window(hwnd).min(MON - 1);
                    let affected = s.last[old_mi].clone();
                    s.live_resize.forget(&affected);
                    overlay::set_drop_target(None);
                    overlay::raise_indicators(false);
                    let on_dot = s.drop_dot.take().filter(|_| !s.resizing);
                    if let Some((dot_mi, n)) = on_dot {
                        // dropped onto a workspace dot: the window goes to that workspace (see
                        // `send_to_workspace`); undo the swap preview and let it snap back first
                        if let Some((pi, ph)) = s.drag_preview.take() {
                            if pi < s.rects[old_mi].len() {
                                let back = s.rects[old_mi][pi];
                                move_window(&mut s, HWND(ph as *mut _), back, true);
                            }
                        }
                        s.resizing = false;
                        s.live = false;
                        dot_drop = Some((h, dot_mi, n));
                    } else if old_mi != new_mi {
                        // A manual cross-monitor drag is an explicit move to the destination
                        // monitor's currently visible workspace. Do not interpret its final size
                        // as a resize of a split on the source monitor.
                        s.order[old_mi].retain(|window| *window != h);
                        if !s.order[new_mi].contains(&h) {
                            s.order[new_mi].push(h);
                        }
                        s.mon.insert(h, new_mi);
                        let destination_workspace = s.cur[new_mi];
                        s.ws.insert(h, destination_workspace);
                        s.last[old_mi].clear();
                        s.last[new_mi].clear();
                        s.resizing = false;
                        s.live = false;
                        s.drag_preview = None;
                    } else if s.resizing {
                        let mi = old_mi;
                        // final size: take the last position of the dragged edges
                        if let (Some(k), Some(vis)) =
                            (s.last[mi].iter().position(|x| *x == h), visible_rect(hwnd))
                        {
                            let cur = s.cur[mi];
                            let (tiles, regions) = (s.rects[mi].clone(), s.regions[mi].clone());
                            let mut rs = s.ratios[mi].remove(&cur).unwrap_or_default();
                            apply_resize_for(
                                config::layout(),
                                &mut rs,
                                &tiles,
                                &regions,
                                k,
                                vis,
                                config::inner_gap(),
                            );
                            s.ratios[mi].insert(cur, rs);
                            let (tiles, regions) = layout_tiles(
                                config::layout(),
                                monitor_work(mi),
                                s.last[mi].len(),
                                &s.ratios[mi][&cur],
                                gaps(),
                            );
                            let targets: Vec<_> = s.last[mi]
                                .iter()
                                .copied()
                                .zip(tiles.iter().copied())
                                .collect();
                            place_batch(&mut s, &targets);
                            s.rects[mi] = tiles;
                            s.regions[mi] = regions;
                        }
                        s.resizing = false;
                        s.live = false;
                    } else {
                        let mi = old_mi;
                        // dropped onto another tile? -> swap the two windows; otherwise snap back
                        let mut p = POINT::default();
                        let _ = GetCursorPos(&mut p);
                        let hit = s.rects[mi].iter().position(|r| {
                            p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
                        });
                        let swap_at = hit.filter(|i| *i < s.last[mi].len() && s.last[mi][*i] != h);
                        // the live preview already slid the hovered window into our slot: it stays
                        // there if it is the one we swap with, otherwise it goes back
                        if let Some((pi, ph)) = s.drag_preview.take() {
                            if swap_at != Some(pi) && pi < s.rects[mi].len() {
                                let back = s.rects[mi][pi];
                                move_window(&mut s, HWND(ph as *mut _), back, true);
                            }
                        }
                        if let Some(i) = swap_at {
                            let other = s.last[mi][i];
                            let (ia, ib) = (
                                s.order[mi].iter().position(|x| *x == h),
                                s.order[mi].iter().position(|x| *x == other),
                            );
                            if let (Some(ia), Some(ib)) = (ia, ib) {
                                s.order[mi].swap(ia, ib);
                                tour_event("swap");
                            }
                        }
                    }
                    s.reflow = true;
                    for h in &affected {
                        s.live_borders.remove(h);
                    }
                    dirty = true;
                } else {
                    return;
                }
            }
            EVENT_OBJECT_LOCATIONCHANGE => {
                if s.paused {
                    return;
                }
                if s.suspended.contains(&h) && !temporarily_unavailable(hwnd) {
                    s.windows_changed = true;
                    dirty = true;
                }
                let actual = visible_rect(hwnd);
                // Even our own accepted placement can settle after the last animation
                // tick. Update the border before suppressing layout reconciliation.
                if s.border_on == Some(h) {
                    match actual {
                        Some(vis) => place_border(&vis, h),
                        None => overlay::hide_border(),
                    }
                }
                if s.divider
                    .as_ref()
                    .is_some_and(|gesture| gesture.windows.contains(&h))
                {
                    // The gesture owns these geometry changes. Verification and
                    // adaptive fitting resume after release, not between frames.
                    return;
                }
                if !dirty
                    && s.dragging.is_none()
                    && actual
                        .as_ref()
                        .is_some_and(|r| s.expected_frame(h, r, Instant::now()))
                {
                    return;
                }
                if let Some(d) = s.dragging {
                    // during a drag only the dragged window matters: is it being resized?
                    if d == h {
                        if let Some(vis) = actual {
                            let (w, hh) = (vis.right - vis.left, vis.bottom - vis.top);
                            if !s.resizing
                                && ((w - s.drag_size.0).abs() > RESIZE_PX
                                    || (hh - s.drag_size.1).abs() > RESIZE_PX)
                            {
                                s.resizing = true;
                            }
                        }
                        if s.resizing {
                            // a resize is not a drop: undo any live swap preview
                            s.drop_dot = None;
                            overlay::set_drop_target(None);
                            let mi = s.mi(h);
                            if let Some((pi, ph)) = s.drag_preview.take() {
                                if pi < s.rects[mi].len() {
                                    let back = s.rects[mi][pi];
                                    move_window(&mut s, HWND(ph as *mut _), back, true);
                                }
                            }
                            s.live = true;
                            // Coalesce input to the next frame without resetting its deadline.
                            arm_anim_timer(&mut s);
                        } else {
                            // the raise at drag start can lose to the dragged window being
                            // brought forward on the first grab: make sure the dots stay on top
                            overlay::raise_indicators(true);
                            // over a workspace dot: highlight it instead of previewing a swap
                            let mut p = POINT::default();
                            let dot = if GetCursorPos(&mut p).is_ok() {
                                overlay::dot_at_screen(p.x, p.y)
                            } else {
                                None
                            };
                            overlay::set_drop_target(dot);
                            s.drop_dot = dot;
                            if dot.is_some() {
                                let mi = s.mi(h);
                                if let Some((pi, ph)) = s.drag_preview.take() {
                                    if pi < s.rects[mi].len() {
                                        let back = s.rects[mi][pi];
                                        move_window(&mut s, HWND(ph as *mut _), back, true);
                                    }
                                }
                            } else {
                                update_drag_preview(&mut s, h);
                            }
                        }
                    }
                    return;
                }
                // a window moved/resized outside of a drag (and not because of us)
                let parked =
                    s.ws.get(&h).is_some_and(|ws| *ws != s.cur[s.mi(h)]) && is_parked(hwnd);
                if parked {
                    return;
                }
                if s.last[s.mi(h)].contains(&h) {
                    let mi = s.mi(h);
                    let exhausted = s
                        .geometry_retries
                        .get(&h)
                        .is_some_and(|(target, attempts)| {
                            *attempts >= 3
                                && s.last[mi]
                                    .iter()
                                    .position(|window| *window == h)
                                    .and_then(|i| s.rects[mi].get(i))
                                    .is_some_and(|r| same_rect(r, target, 0))
                        });
                    if exhausted {
                        return;
                    }
                    s.enforce = true; // a tiled window moved on its own: pull it back
                    dirty = true;
                } else if s.suspended.contains(&h)
                    || s.fs_ws.contains_key(&h)
                    || h == GetForegroundWindow().0 as isize
                {
                    // a tracked fullscreen window changed size (a game left fullscreen), or the
                    // foreground window did: reconcile the window set
                    s.windows_changed = true;
                    dirty = true;
                }
            }
            EVENT_OBJECT_SHOW => {
                s.windows_changed = true;
                dirty = true;
                // a window just appeared: put it into its tile right away, without waiting
                if !s.paused && s.dragging.is_none() && fast_add(&mut s, hwnd) {
                    // some apps move/resize themselves right after showing: check again shortly
                    s.enforce = true;
                    s.timer = SetTimer(None, s.timer, VERIFY_MS, None);
                    return;
                }
            }
            EVENT_OBJECT_NAMECHANGE => {
                if s.paused {
                    return;
                }
                let ignored = s.manual_float.union(&s.capacity_float).copied().collect();
                let verdict = window::classify_with_manual(hwnd, &config::rules(), &ignored);
                let changed = match verdict {
                    Verdict::Tileable(window) => {
                        let changed = !s.ws.contains_key(&h)
                            || s.suspended.contains(&h)
                            || s.fs_ws.contains_key(&h)
                            || window.rule_ws.is_some_and(|ws| s.ws.get(&h) != Some(&ws));
                        if let Some(ws) = window.rule_ws.filter(|_| s.ws.contains_key(&h)) {
                            s.ws.insert(h, ws);
                        }
                        changed
                    }
                    Verdict::Fullscreen(_) => !s.fs_ws.contains_key(&h),
                    _ => {
                        (s.ws.contains_key(&h) || s.fs_ws.contains_key(&h))
                            && !temporarily_unavailable(hwnd)
                    }
                };
                if !changed {
                    return;
                }
                s.windows_changed = true;
                dirty = true;
            }
            EVENT_SYSTEM_FOREGROUND => {
                update_border(&mut s, h);
                if s.suspended.contains(&h) && !temporarily_unavailable(hwnd) {
                    s.windows_changed = true;
                    dirty = true;
                }
                let recently = s
                    .last_switch
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
                // a window of another workspace got focus (taskbar / Alt+Tab), tiled or fullscreen
                let mi = s
                    .mon
                    .get(&h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
                    .min(MON - 1);
                let ws = s.ws.get(&h).copied().or_else(|| s.fs_ws.get(&h).copied());
                match ws {
                    Some(w) if w != s.cur[mi] && !s.paused && !recently => {
                        s.follow = Some((mi, w, h));
                        s.windows_changed = true;
                        dirty = true;
                    }
                    _ => {
                        // focus change inside the current workspace (a click on a tile): no re-tile
                        // is needed, but the border overlay must move to the newly focused window
                        reposition = true;
                        if ws.is_none() && !s.paused {
                            let ignored =
                                s.manual_float.union(&s.capacity_float).copied().collect();
                            if matches!(
                                window::classify_with_manual(hwnd, &config::rules(), &ignored),
                                Verdict::Tileable(_) | Verdict::Fullscreen(_)
                            ) {
                                s.windows_changed = true;
                                dirty = true;
                            }
                        }
                        if !dirty {
                            return;
                        }
                    }
                }
            }
            EVENT_OBJECT_CREATE if !IsWindowVisible(hwnd).as_bool() => {}
            EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED | EVENT_SYSTEM_MINIMIZESTART
                if !s.ws.contains_key(&h) && !s.fs_ws.contains_key(&h) => {}
            EVENT_OBJECT_CREATE
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_CLOAKED
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND => {
                s.windows_changed = true;
                dirty = true;
            }
            EVENT_OBJECT_DESTROY if forget_window(&mut s, h) => {
                s.windows_changed = true;
                dirty = true;
            }
            _ => {}
        }
        // (Re)arm the debounce timer only when this event actually changed something. Bursts still
        // collapse into a single re-tile (SetTimer resets the countdown), but events we ignore no
        // longer schedule a full re-collect of every window.
        if dirty {
            let now = Instant::now();
            let since = *s.dirty_since.get_or_insert(now);
            // Stop moving the deadline after a sustained burst. A noisy client
            // cannot keep resetting another window's reconciliation indefinitely.
            if s.timer == 0 || now.saturating_duration_since(since) < Duration::from_millis(120) {
                s.timer = SetTimer(None, s.timer, DEBOUNCE_MS, None);
            }
        }
    });
    if let Some((window, mi, n)) = dot_drop {
        send_to_workspace(window, mi, n);
    }
    if reposition {
        // the borrow above is released: move the border onto the window that just got focus
        sync_border();
    }
    if reposition || dirty {
        sync_dividers();
    }
    if resync {
        // the menu just closed: re-evaluate the border from the real foreground window
        refresh_border();
    }
}

/// Starts or stops the focus-follows-mouse poll to match the current setting (on start-up and after
/// every config reload).
pub(super) fn apply_focus_follows_timer() {
    let want = config::focus_follows_mouse();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let on = s.ffm_timer != 0;
        if want && !on {
            s.ffm_timer = unsafe { SetTimer(None, 0, FFM_MS, None) };
        } else if !want && on {
            unsafe {
                let _ = KillTimer(None, s.ffm_timer);
            }
            s.ffm_timer = 0;
        }
        if !want {
            s.ffm_last = None;
        }
    });
}

/// Focus-follows-mouse poll: focus the tiled window under the pointer, if it is not focused yet.
/// Runs on its own timer, only while the `focus_follows_mouse` setting is enabled.
pub(super) fn focus_follows_tick() {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let under = unsafe { WindowFromPoint(pt) };
    if under.is_invalid() {
        return;
    }
    let root = unsafe { GetAncestor(under, GA_ROOT) };
    if root.is_invalid() {
        return;
    }
    let h = root.0 as isize;
    let (is_tiled, dragging, last) = STATE.with(|s| {
        let s = s.borrow();
        let mi = s.mi(h);
        (
            s.ws.get(&h).is_some_and(|w| *w == s.cur[mi]) && !s.manual_float.contains(&h),
            s.dragging.is_some() || s.divider.is_some(),
            s.ffm_last,
        )
    });
    if dragging {
        return;
    }
    if !is_tiled {
        // the pointer is off the tiled area (desktop, taskbar, a floating window): let the next
        // tile the pointer enters take focus again
        if last.is_some() {
            STATE.with(|s| s.borrow_mut().ffm_last = None);
        }
        return;
    }
    if last == Some(h) {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg != root {
        // SetForegroundWindow is refused unless our process may set the foreground; attaching to
        // the current foreground thread lifts that restriction (the usual FFM trick).
        unsafe {
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let target_thread = GetWindowThreadProcessId(root, None);
            let attach = fg_thread != 0 && target_thread != 0 && fg_thread != target_thread;
            if attach {
                let _ = AttachThreadInput(fg_thread, target_thread, true);
            }
            let _ = SetForegroundWindow(root);
            if attach {
                let _ = AttachThreadInput(fg_thread, target_thread, false);
            }
        }
    }
    STATE.with(|s| s.borrow_mut().ffm_last = Some(h));
}

/// Handles the thread timers: the debounce / live / verify timer and the focus-follows-mouse poll.
pub(super) fn handle_debounce_timer(wparam: usize) {
    let frame = STATE.with(|s| s.borrow().frame_fallback);
    if frame != 0 && frame == wparam {
        anim_tick();
        return;
    }
    let trim = STATE.with(|s| s.borrow().trim_timer);
    if trim != 0 && wparam == trim {
        unsafe {
            let _ = KillTimer(None, trim);
        }
        STATE.with(|s| s.borrow_mut().trim_timer = 0);
        trim_working_set();
        return;
    }
    let (timer, ffm, drag_timer, dragging) = STATE.with(|s| {
        let s = s.borrow();
        (s.timer, s.ffm_timer, s.drag_timer, s.dragging)
    });
    if ffm != 0 && wparam == ffm {
        focus_follows_tick();
        return;
    }
    if drag_timer != 0 && wparam == drag_timer {
        let Some(h) = dragging else {
            unsafe {
                let _ = KillTimer(None, drag_timer);
            }
            STATE.with(|s| s.borrow_mut().drag_timer = 0);
            return;
        };
        let hwnd = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                forget_window(&mut s, h);
                s.windows_changed = true;
                s.drag_timer = 0;
                s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            });
            unsafe {
                let _ = KillTimer(None, drag_timer);
            }
            return;
        }
        let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        match unsafe { GetGUIThreadInfo(thread, &mut info) } {
            Ok(()) if info.flags.contains(GUI_INMOVESIZE) => return,
            Ok(()) => {
                // A missed end notification is safe to synthesize only after Windows confirms
                // the owning GUI thread has left its modal move/size loop.
                unsafe {
                    on_event(
                        HWINEVENTHOOK(std::ptr::null_mut()),
                        EVENT_SYSTEM_MOVESIZEEND,
                        hwnd,
                        0,
                        0,
                        thread,
                        0,
                    );
                }
                return;
            }
            Err(error) => {
                let expired = STATE.with(|s| {
                    s.borrow()
                        .drag_started
                        .is_some_and(|started| started.elapsed() >= Duration::from_secs(15))
                });
                if expired {
                    log(format!(
                        "move-size watchdog could not query GUI thread state; finalizing drag: {error}"
                    ));
                    unsafe {
                        on_event(
                            HWINEVENTHOOK(std::ptr::null_mut()),
                            EVENT_SYSTEM_MOVESIZEEND,
                            hwnd,
                            0,
                            0,
                            thread,
                            0,
                        );
                    }
                }
                return;
            }
        }
    }
    if wparam != timer {
        return;
    }
    unsafe {
        let _ = KillTimer(None, timer);
    }
    let (follow, reflow, enforce, live) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.timer = 0;
        s.dirty_since = None;
        let reflow = std::mem::take(&mut s.reflow);
        let enforce = std::mem::take(&mut s.enforce);
        let live = std::mem::take(&mut s.live);
        if reflow {
            s.clear_layouts(); // force a fresh layout (snap back / apply a swap)
        }
        (s.follow.take(), reflow, enforce, live)
    });
    match follow {
        Some((mi, w, h)) => switch_ws(mi, w, Some(h)),
        None if reflow => retile(),
        None if live => live_resize(),
        None if enforce => {
            let fullscreen = enforce_layout();
            // only walk all windows again if the set of windows really changed (or one of them
            // just went fullscreen and must be dropped from the layout)
            let changed =
                fullscreen || STATE.with(|s| std::mem::take(&mut s.borrow_mut().windows_changed));
            if changed {
                retile();
            }
        }
        None => retile(),
    }
    sync_border();
    sync_dividers();
}
