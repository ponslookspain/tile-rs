//! Frame scheduling, animation batches and evidence of client size constraints.

use super::*;

/// One window gliding from `from` to `to` (both are *visible* frames, not window rects).
#[derive(Clone)]
pub(super) struct Anim {
    pub(super) hwnd: isize,
    pub(super) from: RECT,
    pub(super) to: RECT,
    /// invisible resize border: (left, top, right, bottom)
    pub(super) border: (i32, i32, i32, i32),
    pub(super) t0: Instant,
    /// Duration of this animation (scaled by the distance, see `anim_duration`).
    pub(super) dur_ms: u32,
    /// Last visible frame we actually applied (to skip frames that do not move the window).
    pub(super) last: Option<RECT>,
}

/// Put the *visible* frame of a window at `vis`, given its invisible border.
pub(super) fn set_frame(hwnd: isize, vis: &RECT, b: (i32, i32, i32, i32)) -> bool {
    apply_window_pos(
        HWND(hwnd as *mut _),
        vis.left - b.0,
        vis.top - b.1,
        (vis.right - vis.left) + b.0 + b.2,
        (vis.bottom - vis.top) + b.1 + b.3,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        "animation-frame",
    )
}

pub(super) type FrameBorder = (i32, i32, i32, i32);

/// Submit the frames of several windows as one batch. Windows that shrink are placed before
/// windows that grow, so a tile vacates its space before its neighbour expands into it and
/// the transient overlap inside a frame is minimal. Returns, per input, whether the request was
/// accepted. A failed batch falls back to individual requests.
pub(super) fn apply_frames(ops: &[(isize, RECT, FrameBorder)]) -> Vec<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{
        BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos,
    };
    if ops.len() < 2 {
        return ops.iter().map(|(h, r, b)| set_frame(*h, r, *b)).collect();
    }
    struct Item {
        index: usize,
        native: RECT,
        shrink: i64,
        flags: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
    }
    let mut results = vec![true; ops.len()];
    let mut items: Vec<Item> = Vec::with_capacity(ops.len());
    for (index, (h, vis, b)) in ops.iter().enumerate() {
        let native = RECT {
            left: vis.left - b.0,
            top: vis.top - b.1,
            right: vis.right + b.2,
            bottom: vis.bottom + b.3,
        };
        let mut current = RECT::default();
        if unsafe { GetWindowRect(HWND(*h as *mut _), &mut current) }.is_err() {
            results[index] = false;
            continue;
        }
        if current == native {
            continue; // already there
        }
        let area = |r: &RECT| i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
        items.push(Item {
            index,
            native,
            shrink: area(&native) - area(&current),
            flags: unchanged_position_flags(&current, &native),
        });
    }
    if items.is_empty() {
        return results;
    }
    items.sort_by_key(|item| item.shrink); // most negative (shrinking) first
    if prof::on() {
        prof::PLACEMENTS.fetch_add(items.len() as u32, Ordering::Relaxed);
    }
    let batch = unsafe {
        BeginDeferWindowPos(items.len() as i32).and_then(|mut batch| {
            for item in &items {
                batch = DeferWindowPos(
                    batch,
                    HWND(ops[item.index].0 as *mut _),
                    None,
                    item.native.left,
                    item.native.top,
                    item.native.right - item.native.left,
                    item.native.bottom - item.native.top,
                    SWP_NOZORDER
                        | SWP_NOACTIVATE
                        | SWP_NOOWNERZORDER
                        | SWP_ASYNCWINDOWPOS
                        | item.flags,
                )?;
            }
            EndDeferWindowPos(batch)
        })
    };
    if let Err(error) = batch {
        log(format!("frame batch fallback: {error}"));
        for item in &items {
            let (h, vis, b) = &ops[item.index];
            results[item.index] = set_frame(*h, vis, *b);
        }
    }
    results
}

/// Moves a window to `target`: instantly, or - if `animate` and animations are enabled -
/// by gliding. Always cancels an animation that was already running for this window.
pub(super) fn move_window(s: &mut State, hwnd: HWND, target: RECT, animate: bool) {
    let target = monitor::clamp_to_work(target);
    let h = hwnd.0 as isize;
    if animate
        && config::animation().0
        && s.anims
            .iter()
            .any(|a| a.hwnd == h && same_rect(&a.to, &target, 0))
    {
        return;
    }
    s.anims.retain(|a| a.hwnd != h);
    if placement_blocked(s, hwnd, &target) {
        return;
    }
    let (on, ms) = config::animation();
    let busy = unsafe { IsZoomed(hwnd).as_bool() } || is_parked(hwnd) || animations_suspended(s);
    // if the frame timer could not be created, place windows instantly rather than freeze them
    if !animate || !on || ms == 0 || busy || s.anim_timer.is_invalid() {
        let before = visible_rect(hwnd);
        let succeeded = place(hwnd, target);
        note_placement_result(s, hwnd, target, before, succeeded);
        if succeeded && observe_min_size(s, hwnd, &target) {
            s.min_size_reflow = true;
        }
        if s.border_on == Some(h) {
            sync_border_to(h);
        }
        return;
    }
    let Some(from) = visible_rect(hwnd) else {
        let succeeded = place(hwnd, target);
        note_placement_result(s, hwnd, target, None, succeeded);
        if succeeded && observe_min_size(s, hwnd, &target) {
            s.min_size_reflow = true;
        }
        if s.border_on == Some(h) {
            sync_border_to(h);
        }
        return;
    };
    if same_rect(&from, &target, 1) {
        return; // already there: nothing to do (and nothing to repaint)
    }
    let mut win = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut win);
    }
    let border = frame_insets(&win, &from).unwrap_or_default();
    let dist = (from.left - target.left)
        .abs()
        .max((from.top - target.top).abs())
        .max((from.right - target.right).abs())
        .max((from.bottom - target.bottom).abs());
    let dur_ms = anim_duration(ms, dist);
    let was_empty = s.anims.is_empty();
    s.anims.push(Anim {
        hwnd: h,
        from,
        to: target,
        border,
        t0: Instant::now(),
        dur_ms,
        last: None,
    });
    if was_empty {
        if prof::on() {
            anim_prof_reset();
        }
        arm_anim_timer(s);
    }
}

/// After a placement has had time to settle, remember its size if the app refused to shrink
/// below it (an application minimum size). The layout then grows its tile instead of letting the
/// window overlap its neighbour. Only ever grows the remembered size: once a tile has been grown
/// to fit, the window also fits it, so forgetting the minimum would make the tile shrink again
/// and start an endless grow/shrink loop.
pub(super) fn observe_min_size(s: &mut State, hwnd: HWND, target: &RECT) -> bool {
    let h = hwnd.0 as isize;
    // Posted placements may still be queued. The old (larger) window is not evidence
    // of a minimum size; learning it here can permanently pin an extreme layout.
    if s.placement_pending(h, target, Instant::now()) {
        return false;
    }
    let Some(vis) = visible_rect(hwnd) else {
        return false;
    };
    // DWM can still report the previous frame after a placement request.
    // A visible frame outside the current native window is not minimum-size evidence.
    let mut native = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut native) }.is_err()
        || vis.left < native.left - 2
        || vis.top < native.top - 2
        || vis.right > native.right + 2
        || vis.bottom > native.bottom + 2
    {
        return false;
    }
    let (vw, vh) = (vis.right - vis.left, vis.bottom - vis.top);
    let (tw, th) = (target.right - target.left, target.bottom - target.top);
    if vw <= tw + 2 && vh <= th + 2 {
        return false;
    }
    let Some(request) = s
        .placements
        .get_mut(&h)
        .filter(|request| request.target == *target)
    else {
        return false;
    };
    if !request.ready_to_query(vis, Instant::now()) {
        if !request.queried {
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        }
        return false;
    }
    // Tracking minima are explicit client data, unlike the old large frame of a stalled
    // application. Successful queries happen once per generation; timed-out queries
    // can be retried during a bounded two-second responsiveness grace period.
    let Some(reported) = query_minimum(hwnd, &native, &vis) else {
        if request.submitted.elapsed() < Duration::from_secs(2) {
            request.sample = Some((vis, Instant::now()));
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        } else {
            request.queried = true; // bounded stall, never a learned minimum
        }
        return false;
    };
    request.queried = true;
    let previous = s.min_size.get(&h).copied().unwrap_or((0, 0));
    let observed = confirmed_minimum(previous, (tw, th), (vw, vh), reported);
    if observed != previous {
        s.min_size.insert(h, observed);
        log(format!("minimum confirmed: hwnd={h:#x} target={tw}x{th} actual={vw}x{vh} reported={reported:?} minimum={observed:?}"));
        return true;
    }
    false
}

/// Ask a new window for its minimum size before it is laid out, so a layout that cannot hold it
/// is rejected up front instead of overlapping neighbours until a placement fails. Only sizes the
/// client itself reports above the system default (and that fit its monitor) are kept.
pub(super) fn seed_min_size(s: &mut State, hwnd: HWND) {
    let h = hwnd.0 as isize;
    if s.min_size.contains_key(&h) {
        return;
    }
    let mut native = RECT::default();
    let Some(visible) = visible_rect(hwnd) else {
        return;
    };
    if unsafe { GetWindowRect(hwnd, &mut native) }.is_err() {
        return;
    }
    let (Some(reported), Some(b)) = (
        query_minimum(hwnd, &native, &visible),
        frame_insets(&native, &visible),
    ) else {
        return;
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let default = (
        (unsafe { GetSystemMetricsForDpi(SM_CXMINTRACK, dpi) } - b.0 - b.2).max(0),
        (unsafe { GetSystemMetricsForDpi(SM_CYMINTRACK, dpi) } - b.1 - b.3).max(0),
    );
    let area = monitor::work(monitor::of_window(hwnd));
    let axis = |reported: i32, default: i32, limit: i32| {
        if reported > default + 2 && reported <= limit {
            reported
        } else {
            0
        }
    };
    let min = (
        axis(reported.0, default.0, area.right - area.left),
        axis(reported.1, default.1, area.bottom - area.top),
    );
    if min != (0, 0) {
        s.min_size.insert(h, min);
        log(format!(
            "minimum reported up front: hwnd={h:#x} minimum={min:?}"
        ));
    }
}

pub(super) fn query_minimum(hwnd: HWND, native: &RECT, visible: &RECT) -> Option<(i32, i32)> {
    let mut info = MINMAXINFO::default();
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    info.ptMinTrackSize = POINT {
        x: unsafe { GetSystemMetricsForDpi(SM_CXMINTRACK, dpi) },
        y: unsafe { GetSystemMetricsForDpi(SM_CYMINTRACK, dpi) },
    };
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&mut info as *mut MINMAXINFO as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            10,
            None,
        )
    };
    if result.0 == 0 {
        return None;
    }
    let borders = frame_insets(native, visible)?;
    Some((
        (info.ptMinTrackSize.x - borders.0 - borders.2).max(0),
        (info.ptMinTrackSize.y - borders.1 - borders.3).max(0),
    ))
}

pub(super) fn confirmed_minimum(
    previous: (i32, i32),
    target: (i32, i32),
    actual: (i32, i32),
    reported: (i32, i32),
) -> (i32, i32) {
    let axis = |previous: i32, target: i32, actual: i32, reported: i32| {
        if actual > target + 2 && reported > target + 2 && reported <= actual + 2 {
            previous.max(reported)
        } else {
            previous
        }
    };
    (
        axis(previous.0, target.0, actual.0, reported.0),
        axis(previous.1, target.1, actual.1, reported.1),
    )
}

/// Creates the timer that drives animation frames. Prefers the high-resolution variant
/// (Windows 10 1803+); falls back to a normal waitable timer on older systems.
pub(super) fn create_anim_timer() -> HANDLE {
    unsafe {
        CreateWaitableTimerExW(
            None,
            PCWSTR::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS.0,
        )
        .or_else(|_| CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0))
        .or_else(|_| CreateWaitableTimerW(None, false, PCWSTR::null()))
        .unwrap_or_default()
    }
}

/// Cache display queries until a display change; cap visual work independently of input.
pub(super) fn anim_frame_ms() -> u32 {
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    let hz = DISPLAY_HZ.with(|cache| {
        if let Some(hz) = cache.get() {
            return hz;
        }
        let hz = monitor::list()
            .iter()
            .filter_map(|mon| {
                let device: Vec<u16> = mon.device.encode_utf16().chain(Some(0)).collect();
                let mut mode = DEVMODEW {
                    dmSize: size_of::<DEVMODEW>() as u16,
                    ..Default::default()
                };
                if unsafe {
                    EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
                }
                .as_bool()
                    && (30..=1000).contains(&mode.dmDisplayFrequency)
                {
                    Some(mode.dmDisplayFrequency)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(60);
        cache.set(Some(hz));
        hz
    });
    frame_interval_ms(hz, config::animation_fps_limit())
}

pub(super) fn frame_interval_ms(display_hz: u32, fps_limit: u32) -> u32 {
    let fps = display_hz.max(1).min(fps_limit.clamp(15, 240));
    1000u32.div_ceil(fps)
}

/// Relative due time (negative, 100 ns units) of the frame that should be submitted just before
/// the next display refresh. `vblank` is any past or future refresh time and `period` the refresh
/// period, both in QPC ticks of frequency `freq`; `now` is the current QPC value. The frame is
/// submitted `lead` ticks before the refresh so the compositor picks it up for that refresh.
pub(super) fn vsync_due_100ns(
    now: i64,
    vblank: i64,
    period: i64,
    freq: i64,
    lead: i64,
) -> Option<i64> {
    if period <= 0 || freq <= 0 {
        return None;
    }
    let earliest = now + freq / 1000; // never fire in less than 1 ms
    let elapsed = earliest + lead - vblank;
    let k = elapsed.div_euclid(period) + i64::from(elapsed.rem_euclid(period) != 0);
    let fire = vblank + k * period - lead;
    let ticks = (fire - now).max(0);
    Some(-(ticks as i128 * 10_000_000 / freq as i128) as i64)
}

/// The next vsync-aligned frame time, if the compositor reports its refresh timing.
pub(super) fn vsync_frame_due() -> Option<i64> {
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let mut info = DWM_TIMING_INFO {
        cbSize: size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    let (mut now, mut freq) = (0i64, 0i64);
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
        QueryPerformanceCounter(&mut now).ok()?;
        QueryPerformanceFrequency(&mut freq).ok()?;
    }
    vsync_due_100ns(
        now,
        info.qpcVBlank as i64,
        info.qpcRefreshPeriod as i64,
        freq,
        freq * 2 / 1000, // 2 ms ahead of the refresh
    )
}

/// Frames follow the display refresh unless the user capped the frame rate below it.
pub(super) fn frames_follow_vsync() -> bool {
    let _ = anim_frame_ms(); // fills the cached refresh rate
    let hz = DISPLAY_HZ.with(|cache| cache.get()).unwrap_or(60);
    config::animation_fps_limit() >= hz
}

/// After a vsync-aligned frame: schedule the next one for the next refresh. (A periodic timer
/// drifts against the refresh, which makes a frame get skipped every so often.)
pub(super) fn rearm_frame_timer(s: &mut State) {
    if !s.frame_armed || !s.frame_oneshot || s.anim_timer.is_invalid() {
        return;
    }
    match vsync_frame_due() {
        Some(due) => unsafe {
            let _ = SetWaitableTimer(s.anim_timer, &due, 0, None, None, false);
        },
        None => {
            // timing info became unavailable: fall back to a plain periodic timer
            let frame = anim_frame_ms().max(1);
            let due = -(frame as i64) * 10_000;
            s.frame_oneshot = false;
            unsafe {
                let _ = SetWaitableTimer(s.anim_timer, &due, frame as i32, None, None, false);
            }
        }
    }
}

/// (Re)arms the frame timer. With vsync timing it fires once, just before the next display
/// refresh, and is re-armed after every frame; otherwise one frame interval, then periodically.
pub(super) fn arm_anim_timer(s: &mut State) {
    if s.frame_armed {
        return;
    }
    let frame = anim_frame_ms().max(1);
    let synced = if frames_follow_vsync() {
        vsync_frame_due()
    } else {
        None
    };
    s.frame_oneshot = synced.is_some();
    let due = synced.unwrap_or(-(frame as i64) * 10_000); // relative time, in 100 ns units
    let period = if synced.is_some() { 0 } else { frame as i32 };
    unsafe {
        s.frame_armed = !s.anim_timer.is_invalid()
            && SetWaitableTimer(s.anim_timer, &due, period, None, None, false).is_ok();
        if !s.frame_armed {
            s.frame_fallback = SetTimer(None, s.frame_fallback, frame, None);
            s.frame_armed = s.frame_fallback != 0;
        }
    }
}

/// Stops the frame timer once nothing is animating.
pub(super) fn cancel_anim_timer(s: &mut State) {
    s.frame_armed = false;
    if s.frame_fallback != 0 {
        unsafe {
            let _ = KillTimer(None, s.frame_fallback);
        }
        s.frame_fallback = 0;
    }
    if s.anim_timer.is_invalid() {
        return;
    }
    unsafe {
        let _ = CancelWaitableTimer(s.anim_timer);
    }
}

/// One animation frame (called from the frame timer).
pub(super) fn anim_tick() {
    if SESSION_ENDING.with(Cell::get) {
        return;
    }
    let tick_started = Instant::now();
    let had_animations = STATE.with(|s| !s.borrow().anims.is_empty());
    divider::tick();
    if !divider::captured() && STATE.with(|s| s.borrow().divider.is_some()) {
        settle_divider();
    }
    let live = STATE.with(|s| std::mem::take(&mut s.borrow_mut().live));
    if live {
        live_resize();
    }
    STATE.with(|s| flush_live_resize(&mut s.borrow_mut()));
    STATE.with(|s| {
        let s = s.borrow();
        if let Some(gesture) = s.divider.as_ref() {
            let observed: Vec<_> = gesture
                .windows
                .iter()
                .enumerate()
                .map(|(i, h)| visible_rect(HWND(*h as *mut _)).unwrap_or(gesture.preview[i]))
                .collect();
            let handles: Vec<_> = divider::handles(
                gesture.handle.monitor,
                gesture.layout,
                &observed,
                &gesture.regions,
                gesture.gaps.inner,
            )
            .into_iter()
            .filter(|handle| handle.split == gesture.handle.split)
            .collect();
            divider::follow(divider::unobstructed(handles, &observed).into_iter().next());
        }
    });
    let profiling = prof::on();
    let easing = config::easing();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.anims.is_empty() {
            // The queue was emptied outside `anim_tick` (e.g. a drag started): stop the timer so
            // it does not keep firing at 60 Hz doing nothing.
            if s.divider.is_none() && !s.resizing {
                cancel_anim_timer(&mut s);
            }
            return;
        }
        let now = Instant::now();
        let mut keep = Vec::with_capacity(s.anims.len());
        let mut setpos = Duration::ZERO;
        let mut min_size_changed = false;
        // Pass 1: where is every window supposed to be in this frame?
        let mut frame: Vec<(Anim, RECT, bool, bool)> = Vec::with_capacity(s.anims.len());
        for a in std::mem::take(&mut s.anims) {
            if !s.animation_current(a.hwnd) {
                continue;
            }
            if !unsafe { IsWindow(Some(HWND(a.hwnd as *mut _))) }.as_bool() {
                continue; // closed while gliding
            }
            let dur = (a.dur_ms.max(1) as f32) / 1000.0;
            let t = (now - a.t0).as_secs_f32() / dur;
            let (rect, done) = if t >= 1.0 {
                (a.to, true) // exact final position
            } else {
                (lerp_rect(&a.from, &a.to, ease(easing, t)), false)
            };
            // skip frames that would not move the window (the tail of an ease-out is almost flat)
            let apply = a.last.as_ref().is_none_or(|l| !same_rect(l, &rect, 0));
            frame.push((a, rect, done, apply));
        }
        // Pass 2: submit all of them as ONE batch, so the whole layout changes in the same
        // composition frame instead of tile by tile.
        let ts = profiling.then(Instant::now);
        let befores: Vec<Option<RECT>> = frame
            .iter()
            .map(|(a, _, _, apply)| {
                apply
                    .then(|| visible_rect(HWND(a.hwnd as *mut _)))
                    .flatten()
            })
            .collect();
        let ops: Vec<(isize, RECT, FrameBorder)> = frame
            .iter()
            .filter(|(_, _, _, apply)| *apply)
            .map(|(a, rect, _, _)| (a.hwnd, *rect, a.border))
            .collect();
        let mut results = apply_frames(&ops).into_iter();
        if let Some(ts) = ts {
            setpos += ts.elapsed();
        }
        // Pass 3: book-keeping per window.
        for ((mut a, mut rect, mut done, apply), before) in frame.into_iter().zip(befores) {
            let mut can_observe_size = true;
            if apply {
                let applied = results.next().unwrap_or(false);
                note_placement_result(&mut s, HWND(a.hwnd as *mut _), rect, before, applied);
                if !applied {
                    // Do not keep posting frames to a window that rejected placement. Make one
                    // direct final attempt and let the delayed geometry verification reconcile it.
                    let before = visible_rect(HWND(a.hwnd as *mut _));
                    can_observe_size = place(HWND(a.hwnd as *mut _), a.to);
                    note_placement_result(
                        &mut s,
                        HWND(a.hwnd as *mut _),
                        a.to,
                        before,
                        can_observe_size,
                    );
                    rect = a.to;
                    done = true;
                }
                a.last = Some(rect);
            }
            let ahwnd = a.hwnd;
            if done {
                // the window is at its tile now: note it if the app kept a bigger minimum size
                if can_observe_size {
                    min_size_changed |= observe_min_size(&mut s, HWND(ahwnd as *mut _), &a.to);
                }
            } else {
                keep.push(a);
            }
        }
        s.anims = keep;
        s.min_size_reflow |= min_size_changed;
        if s.anims.is_empty() {
            if s.divider.is_none() && !s.resizing {
                cancel_anim_timer(&mut s);
            }
            // A client can reject the target size while an animation is running. Reflow once
            // after the animation settles so the newly observed minimum is reflected by the
            // layout instead of leaving a temporary overlap until an unrelated later action.
            if std::mem::take(&mut s.min_size_reflow) && !s.paused {
                s.reflow = true;
                s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            } else if !s.paused {
                // A client may reposition itself or constrain a frame after accepting the
                // animation request. Verify the settled geometry once, not on every frame.
                s.enforce = true;
                s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
            }
        }
        if profiling {
            anim_prof_frame(now, setpos.as_secs_f32() * 1e6);
            if s.anims.is_empty() {
                anim_prof_finish(now);
            }
        }
    });
    // Posted requests may still be pending: follow the observed frame instead
    // of drawing the border at the next animation target.
    sync_border();
    if had_animations && STATE.with(|s| s.borrow().anims.is_empty()) {
        sync_dividers();
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if had_animations {
            note_frame_cost(
                &mut s,
                tick_started.elapsed(),
                Duration::from_millis(anim_frame_ms().into()),
            );
        }
        rearm_frame_timer(&mut s);
        if s.anims.is_empty() {
            schedule_trim(&mut s);
        }
    });
}

/// Frames that repeatedly cost more than two frame intervals mean the machine cannot animate
/// smoothly: finish the running animations at once and move windows instantly for a while,
/// instead of stuttering through every layout change.
pub(super) const SLOW_FRAMES_LIMIT: u32 = 3;
pub(super) const ANIM_SUSPEND: Duration = Duration::from_secs(15);

pub(super) fn note_frame_cost(s: &mut State, cost: Duration, interval: Duration) {
    if cost <= interval * 2 {
        s.slow_frames = 0;
        return;
    }
    s.slow_frames += 1;
    if s.slow_frames < SLOW_FRAMES_LIMIT {
        return;
    }
    s.slow_frames = 0;
    s.anim_suspended_until = Some(Instant::now() + ANIM_SUSPEND);
    for a in &mut s.anims {
        a.dur_ms = 1; // the next frame lands every window on its final tile
    }
    log(format!(
        "animations suspended for {}s: a frame took {cost:?} (interval {interval:?})",
        ANIM_SUSPEND.as_secs()
    ));
}

pub(super) fn animations_suspended(s: &State) -> bool {
    s.anim_suspended_until
        .is_some_and(|until| Instant::now() < until)
}

/// Idle time after which the working set is trimmed.
pub(super) const TRIM_AFTER_MS: u32 = 20_000;

/// Arms (or postpones) the one-shot working-set trim.
pub(super) fn schedule_trim(s: &mut State) {
    s.trim_timer = unsafe { SetTimer(None, s.trim_timer, TRIM_AFTER_MS, None) };
}

/// Hands pages the manager is not using back to the OS (it sits idle almost all the time).
pub(super) fn trim_working_set() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}
