//! Native window placement, frame insets, batching and off-screen parking.

use super::*;

/// How many tiles fit on one workspace of monitor `mi`: the configured `max_tiles` (applies even
/// with `auto_overflow` off), otherwise - with `auto_overflow` on - a count derived from the work
/// area and the comfortable minimum tile. Past it, the next window opens on the next workspace.
/// The real geometry check (`valid_layout`) still applies on top of this count.
pub(super) fn tile_budget(mi: usize) -> usize {
    let configured = config::max_tiles();
    if configured > 0 {
        return configured as usize;
    }
    if !config::auto_overflow() {
        return usize::MAX;
    }
    let area = monitor_work(mi);
    let (mw, mh) = config::min_tile();
    auto_tile_budget(
        i64::from(area.right - area.left),
        i64::from(area.bottom - area.top),
        i64::from(mw),
        i64::from(mh),
    )
}

/// Tiles that fit a `width` x `height` work area: 1 + log2(area / minimum tile area), from 2 to 8.
pub(super) fn auto_tile_budget(width: i64, height: i64, min_w: i64, min_h: i64) -> usize {
    let area_px = width.max(1) as f64 * height.max(1) as f64;
    let min_px = min_w.max(1) as f64 * min_h.max(1) as f64;
    let ratio = area_px / min_px;
    if ratio <= 1.0 {
        return 2;
    }
    (1.0 + ratio.log2()).floor().clamp(2.0, 8.0) as usize
}

pub(super) fn valid_layout(
    mi: usize,
    windows: &[isize],
    rects: &[RECT],
    mins: &HashMap<isize, (i32, i32)>,
) -> bool {
    layout_is_valid(
        monitor_work(mi),
        windows,
        rects,
        mins,
        config::min_tile(),
        layout_gaps(windows.len()),
    )
}

/// Gaps of a layout of `count` windows: none for a lone window with `smart_gaps`. The layout and
/// its validation must agree, or a lone window filling the screen is rejected as out of bounds.
pub(super) fn layout_gaps(count: usize) -> Gaps {
    if config::smart_gaps() && count == 1 {
        Gaps { inner: 0, outer: 0 }
    } else {
        gaps()
    }
}

pub(super) fn workspace_windows(s: &State, mi: usize, ws: u8) -> Vec<isize> {
    let belongs =
        |h: &isize| s.ws.get(h) == Some(&ws) && s.mi(*h) == mi && !s.suspended.contains(h);
    let mut windows: Vec<_> = s.order[mi].iter().copied().filter(belongs).collect();
    let mut added: Vec<_> =
        s.ws.keys()
            .copied()
            .filter(|h| belongs(h) && !windows.contains(h))
            .collect();
    added.sort_unstable();
    windows.extend(added);
    windows
}

pub(super) fn workspace_accepts(s: &State, mi: usize, ws: u8, h: isize) -> bool {
    let mut windows = workspace_windows(s, mi, ws);
    if !windows.contains(&h) {
        let rank = s.order[mi]
            .iter()
            .position(|window| *window == h)
            .unwrap_or(usize::MAX);
        let at = windows
            .iter()
            .position(|window| {
                s.order[mi]
                    .iter()
                    .position(|candidate| candidate == window)
                    .unwrap_or(usize::MAX)
                    > rank
            })
            .unwrap_or(windows.len());
        windows.insert(at, h);
    }
    if windows.len() > tile_budget(mi) {
        return false;
    }
    let ratios = s.ratios[mi].get(&ws).map(Vec::as_slice).unwrap_or(&[]);
    let monocle = is_monocle(s, mi, ws);
    let (tiles, _) = fitted(monitor_work(mi), &windows, ratios, &s.min_size, monocle);
    valid_layout(mi, &windows, &tiles, &s.min_size)
}

/// Admission is based on a real prospective layout, not an area/logarithm estimate.
pub(super) fn workspace_for_new_window(s: &State, mi: usize, cur: u8, h: isize) -> Option<u8> {
    let count = if config::auto_overflow() { WS_COUNT } else { 1 };
    (0..count)
        .map(|offset| (cur + offset) % WS_COUNT)
        .find(|ws| workspace_accepts(s, mi, *ws, h))
}

/// The configured inner (between windows) and outer (screen edge) gaps.
pub(super) fn gaps() -> Gaps {
    Gaps {
        inner: config::inner_gap(),
        outer: config::outer_gap(),
    }
}

/// Whether workspace `ws` of monitor `mi` is shown in monocle.
pub(super) fn is_monocle(s: &State, mi: usize, ws: u8) -> bool {
    s.monocle[mi].contains(&ws)
}

/// The layout of `windows` on `area`: the configured layout with the adaptive fitting
/// (master shrink / observed per-window minimum sizes) applied (see `geometry::place_fitted`).
pub(super) fn fitted(
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
    monocle: bool,
) -> (Vec<RECT>, Vec<RECT>) {
    let (mw, mh) = config::min_tile();
    let gaps = layout_gaps(windows.len());
    if monocle {
        return geometry::monocle(area, windows.len(), gaps);
    }
    place_fitted(
        config::layout(),
        area,
        windows,
        ratios,
        mins,
        (mw, mh),
        gaps,
    )
}

/// The work area (taskbar excluded) of monitor `mi`, minus the indicator bar.
pub(super) fn monitor_work(mi: usize) -> RECT {
    let mut r = monitor::work(mi);
    // Each managed monitor has its own workspace indicator bar.
    if config::workspace_indicator() {
        r.top += overlay::indicator_bar_height(monitor::dpi(mi));
    }
    r
}

// ---------------------------------------------------------------------------
// layout
// ---------------------------------------------------------------------------

/// Brings `fs_ws` / `fs_hidden` in sync with the windows that are really on screen now.
/// A window that is minimized is kept (it may be one we hid ourselves, and its geometry can no
/// longer be detected reliably); a visible window that is no longer fullscreen is dropped.
pub(super) fn reconcile_fullscreen(s: &mut State, wins: &[Win]) {
    let rules = config::rules();
    let excluded: Vec<_> = s
        .fs_ws
        .keys()
        .copied()
        .filter(|h| {
            s.manual_float.contains(h)
                || s.capacity_float.contains(h)
                || window::policy_excludes(HWND(*h as *mut _), &rules)
                || config::fullscreen_windows() == FullscreenWindows::Ignore
        })
        .collect();
    for h in excluded {
        // Release only hiding performed by us; never restore an app the user minimized.
        HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
        if s.fs_hidden.remove(&h) {
            unsafe {
                let _ = ShowWindowAsync(HWND(h as *mut _), SW_RESTORE);
            }
        }
        s.fs_ws.remove(&h);
        s.fs_mon.remove(&h);
        if s.follow.is_some_and(|(_, _, window)| window == h) {
            s.follow = None;
        }
    }
    let detected: HashSet<isize> = wins.iter().map(|w| w.hwnd.0 as isize).collect();
    s.fs_ws.retain(|h, _| {
        let hw = HWND(*h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            return false;
        }
        detected.contains(h) || unsafe { IsIconic(hw) }.as_bool()
    });
    let mut follow: Option<(usize, u8, isize)> = None;
    for w in wins {
        let h = w.hwnd.0 as isize;
        if s.fs_ws.contains_key(&h) {
            // A fullscreen window can be moved to another monitor while it remains fullscreen.
            // Refresh its owner while visible; when minimized, keep the last known monitor.
            if !unsafe { IsIconic(w.hwnd) }.as_bool() {
                s.fs_mon.insert(h, monitor::of_window(w.hwnd).min(MON - 1));
            }
            continue;
        }
        let mi = monitor::of_window(w.hwnd).min(MON - 1);
        // a fullscreen game / video player gets a workspace of its own, and we switch to it
        let ws = w
            .rule_ws
            .or_else(|| s.ws.get(&h).copied())
            .unwrap_or_else(|| next_free_workspace(s, mi, s.cur[mi]));
        s.fs_ws.insert(h, ws);
        s.fs_mon.insert(h, mi);
        remember_identity(s, w.hwnd);
        if ws != s.cur[mi] && follow.is_none() {
            follow = Some((mi, ws, h));
        }
    }
    if follow.is_some() {
        s.follow = follow;
    }
    let fs_ws = &s.fs_ws;
    s.fs_mon.retain(|h, _| fs_ws.contains_key(h));
    s.fs_hidden.retain(|h| fs_ws.contains_key(h));
    HIDDEN_FULLSCREEN.with(|hidden| {
        let mut hidden = hidden.borrow_mut();
        hidden.clone_from(&s.fs_hidden);
        hidden.extend(s.max_hidden.keys().copied());
    });
}

/// The first workspace (starting after `cur`, wrapping) that has no windows at all - used for a
/// fullscreen game, so it does not land on top of the current work.
pub(super) fn next_free_workspace(s: &State, mi: usize, cur: u8) -> u8 {
    let used = |ws: u8| {
        s.ws.iter().any(|(h, w)| *w == ws && s.mi(*h) == mi)
            || s.fs_ws.iter().any(|(h, w)| {
                *w == ws
                    && s.fs_mon
                        .get(h)
                        .copied()
                        .unwrap_or_else(|| monitor::of_window(HWND(*h as *mut _)).min(MON - 1))
                        == mi
            })
    };
    for i in 1..=WS_COUNT {
        let ws = (cur + i) % WS_COUNT;
        if !used(ws) {
            return ws;
        }
    }
    cur // all workspaces occupied: do not switch to an arbitrary occupied workspace
}

/// Minimizes the fullscreen windows whose workspace is not shown, and restores the ones we hid
/// when their workspace becomes the current one.
pub(super) fn apply_fullscreen(s: &mut State) {
    if s.session_ending() || s.paused || config::fullscreen_windows() == FullscreenWindows::Ignore {
        return;
    }
    let items: Vec<(isize, u8)> = s.fs_ws.iter().map(|(h, w)| (*h, *w)).collect();
    let mut changed = false;
    for (h, ws) in items {
        let hw = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            continue;
        }
        let mi = s
            .fs_mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(hw).min(MON - 1));
        let cur = s.cur[mi];
        let hidden = s.fs_hidden.contains(&h);
        let minimized = unsafe { IsIconic(hw) }.as_bool();
        match fullscreen_action(ws, cur, hidden, minimized) {
            FsAction::Show => {
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_RESTORE);
                }
                s.fs_hidden.remove(&h);
                HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
                changed = true;
            }
            FsAction::Hide => {
                if s.session_ending() {
                    return;
                }
                s.fs_hidden.insert(h);
                HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().insert(h));
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_MINIMIZE);
                }
                changed = true;
            }
            FsAction::None => {}
        }
    }
    s.enforce |= changed;
}

/// How long our own asynchronous minimize of a maximized window may take to show up.
pub(super) const MINIMIZE_GRACE: Duration = Duration::from_millis(1500);

/// Maximized windows are tracked like any other window (`State::ws`) but are not tiled while
/// maximized, and a maximized window left in place would cover the workspace the user switched
/// to. So the ones whose workspace is not shown are minimized, and restored (Windows brings them
/// back maximized) when their workspace is shown again. Only windows this function minimized are
/// ever restored by it: an app the user minimized stays minimized.
pub(super) fn apply_maximized(s: &mut State) {
    if s.session_ending() || s.paused {
        return;
    }
    // minimized by us, but no longer assigned to a workspace (closed, excluded, reassigned)
    let orphans: Vec<isize> = s
        .max_hidden
        .keys()
        .copied()
        .filter(|h| !s.ws.contains_key(h))
        .collect();
    for h in orphans {
        s.max_hidden.remove(&h);
        HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
        let hw = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hw)) }.as_bool() && unsafe { IsIconic(hw) }.as_bool() {
            unsafe {
                let _ = ShowWindowAsync(hw, SW_RESTORE);
            }
        }
    }
    let items: Vec<(isize, u8)> = s
        .suspended
        .iter()
        .filter_map(|h| s.ws.get(h).map(|ws| (*h, *ws)))
        .chain(
            s.max_hidden
                .keys()
                .filter(|h| !s.suspended.contains(h))
                .filter_map(|h| s.ws.get(h).map(|ws| (*h, *ws))),
        )
        .collect();
    let mut changed = false;
    for (h, ws) in items {
        let hw = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            continue;
        }
        let mi = s.mi(h);
        let cur = s.cur[mi];
        let hidden = s.max_hidden.contains_key(&h);
        let minimized = unsafe { IsIconic(hw) }.as_bool();
        // The user brought a window we minimized back out (taskbar, Alt+Tab) while its workspace
        // is not shown: it now belongs to the workspace they are on. The grace period covers our
        // own asynchronous minimize that has not been carried out yet.
        let settled = s
            .max_hidden
            .get(&h)
            .is_some_and(|since| since.elapsed() >= MINIMIZE_GRACE);
        if hidden && !minimized && ws != cur && settled {
            s.max_hidden.remove(&h);
            HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
            s.ws.insert(h, cur);
            s.mon.insert(h, mi);
            changed = true;
            continue;
        }
        match fullscreen_action(ws, cur, hidden, minimized) {
            FsAction::Show => {
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_RESTORE);
                }
                s.max_hidden.remove(&h);
                HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().remove(&h));
                changed = true;
            }
            // visible and not cloaked: a window on another Windows virtual desktop is left alone
            FsAction::Hide if unsafe { IsZoomed(hw) }.as_bool() && border_window_shown(hw) => {
                s.max_hidden.insert(h, Instant::now());
                HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().insert(h));
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_MINIMIZE);
                }
                changed = true;
            }
            _ => {}
        }
    }
    s.enforce |= changed;
}

/// Restores every window we minimized (pause, quit), so nothing stays hidden.
pub(super) fn restore_hidden_fullscreen(s: &mut State) {
    HIDDEN_FULLSCREEN.with(|hidden| hidden.borrow_mut().clear());
    let hidden: Vec<isize> = std::mem::take(&mut s.max_hidden).into_keys().collect();
    for h in hidden {
        let hw = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hw)) }.as_bool() && unsafe { IsIconic(hw) }.as_bool() {
            unsafe {
                let _ = ShowWindowAsync(hw, SW_RESTORE);
            }
        }
    }
    for h in std::mem::take(&mut s.fs_hidden) {
        let hw = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hw)) }.as_bool() && unsafe { IsIconic(hw) }.as_bool() {
            unsafe {
                let _ = ShowWindowAsync(hw, SW_RESTORE);
            }
        }
    }
}

pub(super) fn apply_window_pos(
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    mut flags: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
    operation: &'static str,
) -> bool {
    let mut current = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut current) }.is_ok() {
        let target = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        if current == target {
            return true;
        }
        flags |= unchanged_position_flags(&current, &target);
    }
    if prof::on() {
        prof::PLACEMENTS.fetch_add(1, Ordering::Relaxed);
    }
    if SESSION_ENDING.with(Cell::get) {
        return false;
    }
    // A slow client must not block this thread's input, capture cleanup and watchdogs.
    // Success means the request was accepted; geometry is checked by the verify timer.
    match unsafe { SetWindowPos(hwnd, None, x, y, width, height, flags | SWP_ASYNCWINDOWPOS) } {
        Ok(()) => true,
        Err(error) => {
            log(format!(
                "SetWindowPos failed: operation={operation} hwnd={:#x} error={error}",
                hwnd.0 as usize
            ));
            false
        }
    }
}

pub(super) fn unchanged_position_flags(
    current: &RECT,
    target: &RECT,
) -> windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS {
    let mut flags = windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0);
    if current.left == target.left && current.top == target.top {
        flags |= windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE;
    }
    if current.right - current.left == target.right - target.left
        && current.bottom - current.top == target.bottom - target.top
    {
        flags |= SWP_NOSIZE;
    }
    flags
}

pub(super) type FrameInsets = (i32, i32, i32, i32);

pub(super) fn frame_insets(native: &RECT, visible: &RECT) -> Option<FrameInsets> {
    let b = (
        visible.left - native.left,
        visible.top - native.top,
        native.right - visible.right,
        native.bottom - visible.bottom,
    );
    [b.0, b.1, b.2, b.3]
        .iter()
        .all(|v| (0..=64).contains(v))
        .then_some(b)
}

pub(super) fn inset_target(target: RECT, b: FrameInsets) -> RECT {
    RECT {
        left: target.left - b.0,
        top: target.top - b.1,
        right: target.right + b.2,
        bottom: target.bottom + b.3,
    }
}

pub(super) fn capture_live_borders(s: &mut State, windows: &[isize]) {
    for h in windows {
        let hwnd = HWND(*h as *mut _);
        let mut native = RECT::default();
        if let Some(visible) = visible_rect(hwnd) {
            if unsafe { GetWindowRect(hwnd, &mut native) }.is_ok() {
                if let Some(border) = frame_insets(&native, &visible) {
                    s.live_borders.insert(*h, border);
                }
            }
        }
    }
}

/// Remember failed geometry requests so a persistently rejecting client cannot trigger an
/// endless verify -> animate -> fail loop. A new target or a manual drag permits fresh attempts.
pub(super) fn note_placement_result(
    s: &mut State,
    hwnd: HWND,
    target: RECT,
    before: Option<RECT>,
    succeeded: bool,
) {
    let h = hwnd.0 as isize;
    if succeeded {
        s.placement_failures.remove(&h);
        s.expected.insert(h, (target, Instant::now()));
        if s.placements
            .get(&h)
            .is_none_or(|request| request.target != target)
        {
            s.placement_generation = s.placement_generation.wrapping_add(1);
            let request =
                PlacementRequest::new(s.placement_generation, before, target, Instant::now());
            if prof::on() {
                log(format!(
                    "placement: hwnd={h:#x} generation={} before={before:?} target={target:?}",
                    request.generation
                ));
            }
            s.placements.insert(h, request);
        }
        return;
    }
    let entry = s.placement_failures.entry(h).or_insert((target, 0));
    if !same_rect(&entry.0, &target, 0) {
        *entry = (target, 0);
    }
    entry.1 = entry.1.saturating_add(1);
}

pub(super) fn placement_blocked(s: &State, hwnd: HWND, target: &RECT) -> bool {
    s.placement_failures
        .get(&(hwnd.0 as isize))
        .is_some_and(|(previous, attempts)| same_rect(previous, target, 0) && *attempts >= 2)
}

/// API success does not mean an application kept the requested position. Bound retries
/// for an unchanged target independently of Win32 error counts.
pub(super) fn allow_geometry_retry(
    retries: &mut HashMap<isize, (RECT, u8)>,
    h: isize,
    target: RECT,
) -> bool {
    let entry = retries.entry(h).or_insert((target, 0));
    if !same_rect(&entry.0, &target, 0) {
        *entry = (target, 0);
    }
    if entry.1 >= 3 {
        return false;
    }
    entry.1 += 1;
    true
}

pub(super) fn native_target(hwnd: HWND, target: RECT) -> Option<RECT> {
    unsafe {
        // Windows 10/11 windows have an invisible resize border; compensate so the
        // *visible* frame lands exactly on `target`.
        let mut win = RECT::default();
        let mut vis = RECT::default();
        GetWindowRect(hwnd, &mut win).ok()?;
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .is_ok();
        let (dl, dt, dr, db) = if ok {
            (
                vis.left - win.left,
                vis.top - win.top,
                win.right - vis.right,
                win.bottom - vis.bottom,
            )
        } else {
            (0, 0, 0, 0)
        };
        // DWM frame bounds can lag a park/restore or DPI transition. Never interpret a stale
        // screen position as a multi-thousand-pixel invisible border.
        let (dl, dt, dr, db) = if [dl, dt, dr, db].iter().all(|v| (0..=64).contains(v)) {
            (dl, dt, dr, db)
        } else {
            (0, 0, 0, 0)
        };
        Some(RECT {
            left: target.left - dl,
            top: target.top - dt,
            right: target.right + dr,
            bottom: target.bottom + db,
        })
    }
}

/// A window moved to another monitor may rescale itself for that monitor's DPI and miss the size
/// that was requested. Move it onto the destination monitor first (position only); the size is
/// then applied by the placement queued right after, once the DPI change has been processed.
pub(super) fn hop_monitor(hwnd: HWND, native: &RECT, target: &RECT) {
    use windows::Win32::Graphics::Gdi::{
        MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
    };
    let (from, to) = unsafe {
        (
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            MonitorFromRect(target, MONITOR_DEFAULTTONEAREST),
        )
    };
    if from != to {
        let _ = apply_window_pos(
            hwnd,
            native.left,
            native.top,
            native.right - native.left,
            native.bottom - native.top,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            "monitor-hop",
        );
    }
}

pub(super) fn place(hwnd: HWND, target: RECT) -> bool {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
    native_target(hwnd, target).is_some_and(|r| {
        hop_monitor(hwnd, &r, &target);
        apply_window_pos(
            hwnd,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            "place",
        )
    })
}

/// Commit one layout as a batch. A failed batch falls back to bounded individual placements;
/// accepting the request never substitutes for checking the actual client geometry afterwards.
pub(super) fn place_batch(s: &mut State, targets: &[(isize, RECT)]) {
    let _profile = Timer::new("place_batch");
    let requested: HashSet<_> = targets.iter().map(|(h, _)| *h).collect();
    s.anims.retain(|a| !requested.contains(&a.hwnd));
    let operations: Vec<_> = targets
        .iter()
        .filter_map(|(h, target)| {
            let hwnd = HWND(*h as *mut _);
            let clamped = monitor::clamp_to_work(*target);
            let target = &clamped;
            if placement_blocked(s, hwnd, target) {
                return None;
            }
            if visible_rect(hwnd).is_some_and(|actual| same_rect(&actual, target, 0)) {
                return None;
            }
            let r = match s.live_borders.get(h) {
                Some(border) => inset_target(*target, *border),
                None => native_target(hwnd, *target)?,
            };
            let mut current = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut current) }.ok()?;
            if current == r {
                return None;
            }
            hop_monitor(hwnd, &r, target);
            Some((
                *h,
                *target,
                r,
                unchanged_position_flags(&current, &r),
                visible_rect(hwnd),
            ))
        })
        .collect();
    if operations.is_empty() {
        return;
    }
    // One asynchronous request per window, shrinking windows first so a tile vacates its space
    // before its neighbour grows into it. DeferWindowPos cannot batch these: it rejects
    // SWP_ASYNCWINDOWPOS, and without it a hung application would block this thread.
    let mut operations = operations;
    let area = |r: &RECT| i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
    operations.sort_by_key(|(h, _, r, _, _)| {
        let mut current = RECT::default();
        let _ = unsafe { GetWindowRect(HWND(*h as *mut _), &mut current) };
        area(r) - area(&current)
    });
    for (h, target, r, _, before) in operations {
        s.anims.retain(|a| a.hwnd != h);
        let hwnd = HWND(h as *mut _);
        let ok = apply_window_pos(
            hwnd,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            "batch",
        );
        note_placement_result(s, hwnd, target, before, ok);
        if ok {
            s.min_size_reflow |= observe_min_size(s, hwnd, &target);
        }
    }
}

pub(super) fn flush_live_resize(s: &mut State) {
    let targets = s
        .live_resize
        .take_ready(Instant::now(), |h| visible_rect(HWND(h as *mut _)));
    if !targets.is_empty() {
        place_batch(s, &targets);
    }
}

pub(super) fn is_parked(hwnd: HWND) -> bool {
    parked::at_park_spot(hwnd)
}

/// Move a window off-screen (keeps its size) and record where it came from, so a crash can be
/// undone from the persisted list. The caller saves `s.parked` once after a batch of parks.
pub(super) fn park(s: &mut State, hwnd: HWND) {
    if s.session_ending() || is_parked(hwnd) {
        return;
    }
    let mut original = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut original) }.is_ok() {
        s.parked.insert(hwnd.0 as isize, original);
        // A shutdown query can re-enter while STATE is borrowed: its disk-only fallback must
        // already know about this window before the asynchronous move is submitted.
        parked::save(&s.parked);
        s.parked_dirty = false;
    }
    let _ = apply_window_pos(
        hwnd,
        PARK_X,
        PARK_Y,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        "park",
    );
}
