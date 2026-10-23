//! Layout reconciliation, capacity checks and interactive resizing.

use super::placement::MINIMIZE_GRACE;
use super::*;

/// Does the visible frame of `hwnd` differ from `target` by more than a few pixels?
pub(super) fn off_target(hwnd: HWND, target: &RECT) -> bool {
    // DWM can temporarily reject the query (for example during a transition). Use the same
    // fallback geometry as placement/animation rather than interpreting that as "on target".
    let Some(vis) = visible_rect(hwnd) else {
        return false;
    };
    let t = 4;
    (vis.left - target.left).abs() > t
        || (vis.top - target.top).abs() > t
        || (vis.right - target.right).abs() > t
        || (vis.bottom - target.bottom).abs() > t
}

/// Two resize hotkeys closer together than this are treated as one held key.
pub(super) const HOLD_GAP_MS: u128 = 200;

/// Hotkey: grow / shrink the focused window by `resize_step` pixels; the neighbours adapt.
/// Holding the key keeps resizing (smaller, instant steps).
pub(super) fn resize_focused(width: bool, grow: bool) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mi = s.mi(fg);
        if s.paused
            || s.dragging.is_some()
            || s.rects[mi].len() != s.last[mi].len()
            || s.regions[mi].len() != s.last[mi].len()
        {
            return;
        }
        let n = s.last[mi].len();
        let Some(k) = s.last[mi].iter().position(|h| *h == fg) else {
            return;
        };
        let Some((j, sign)) = split_for_layout(config::layout(), k, n, width) else {
            return;
        };
        let gap = config::inner_gap();
        let q = s.regions[mi][j];
        let span = if j % 2 == 0 {
            q.right - q.left - gap
        } else {
            q.bottom - q.top - gap
        };
        let held = s
            .last_resize
            .is_some_and(|t| t.elapsed().as_millis() < HOLD_GAP_MS);
        s.last_resize = Some(Instant::now());
        let step = effective_step(config::resize_step(), held) as f32 / span.max(1) as f32;
        let delta = step * sign * if grow { 1.0 } else { -1.0 };
        let cur = s.cur[mi];
        let ratios = {
            let rs = s.ratios[mi].entry(cur).or_default();
            rs.resize(n - 1, 0.5);
            let before = rs[j];
            rs[j] = (before + delta).clamp(MIN_RATIO.min(before), MAX_RATIO.max(before));
            if (rs[j] - before).abs() < 0.0005 {
                return; // already at the limit
            }
            rs.clone()
        };
        s.touch_ratios();
        let monocle = is_monocle(&s, mi, cur);
        let (tiles, regions) = fitted(monitor_work(mi), &s.last[mi], &ratios, &s.min_size, monocle);
        if !valid_layout(mi, &s.last[mi], &tiles, &s.min_size) {
            s.reflow = true;
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            return;
        }
        let targets: Vec<_> = s.last[mi]
            .iter()
            .copied()
            .zip(tiles.iter().copied())
            .collect();
        // A held key commits all neighbours together without queuing animations.
        if held {
            place_batch(&mut s, &targets);
        } else {
            for (h, target) in targets {
                move_window(&mut s, HWND(h as *mut _), target, true);
            }
        }
        s.rects[mi] = tiles;
        s.regions[mi] = regions;
    });
    refresh_border();
}

/// Workspace slide: how far the incoming windows start off their tiles (share of the monitor
/// width, capped), and up to how many windows it is shown.
const SLIDE_SHARE: f32 = 0.04;
const SLIDE_MAX_PX: f32 = 80.0;
const SLIDE_MAX_WINDOWS: usize = 6;

/// Live drag & drop preview: while the user drags a tiled window over the tile of another one,
/// slide that window into the dragged window's (now empty) slot so it is obvious that releasing
/// here swaps the two. Moving away - or hovering the dragged window's own slot - undoes it.
/// `State::drag_preview` remembers which window was slid where.
pub(super) fn update_drag_preview(s: &mut State, h: isize) {
    let mi = s.mi(h);
    if s.rects[mi].len() != s.last[mi].len() {
        return;
    }
    let Some(k) = s.last[mi].iter().position(|x| *x == h) else {
        return;
    };
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return;
    }
    let hit = s.rects[mi]
        .iter()
        .position(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom);
    let slot = s.rects[mi][k];
    // outline the tile the dragged window would land in (none over its own slot)
    match hit.filter(|i| *i != k) {
        Some(i) => preview::show(&[s.rects[mi][i]]),
        None => preview::hide(),
    }
    // cursor over the dragged window's own slot (or over nothing): undo any preview
    if hit.is_none() || hit == Some(k) {
        if let Some((pi, ph)) = s.drag_preview.take() {
            if pi < s.rects[mi].len() {
                let back = s.rects[mi][pi];
                move_window(s, HWND(ph as *mut _), back, true);
            }
        }
        return;
    }
    let i = hit.unwrap();
    if i >= s.last[mi].len() {
        return;
    }
    let partner = s.last[mi][i];
    if partner == h {
        return;
    }
    if let Some((pi, ph)) = s.drag_preview {
        if pi == i {
            // already previewing this slot: only re-slide if something (a re-tile) moved it away
            if let Some(vis) = visible_rect(HWND(ph as *mut _)) {
                if same_rect(&vis, &slot, 2) {
                    return;
                }
            }
        } else {
            // hovering a different tile: send the previous window back first
            if pi < s.rects[mi].len() {
                let back = s.rects[mi][pi];
                move_window(s, HWND(ph as *mut _), back, true);
            }
        }
    }
    move_window(s, HWND(partner as *mut _), slot, true);
    s.drag_preview = Some((i, partner));
}

/// While the user drags an edge of a tiled window: recompute the split ratios from the dragged
/// window's current frame and move all the *other* windows right away.
pub(super) fn live_resize() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || !s.resizing {
            return;
        }
        let Some(h) = s.dragging else { return };
        let mi = s.mi(h);
        let Some(k) = s.last[mi].iter().position(|x| *x == h) else {
            return;
        };
        let Some(vis) = visible_rect(HWND(h as *mut _)) else {
            return;
        };
        let cur = s.cur[mi];
        let (tiles, regions) = (s.rects[mi].clone(), s.regions[mi].clone());
        let mut rs = s.ratios[mi].remove(&cur).unwrap_or_default();
        let changed = apply_resize_for(
            config::layout(),
            &mut rs,
            &tiles,
            &regions,
            k,
            vis,
            config::inner_gap(),
        );
        if changed {
            // While the user is dragging, lay out the *plain* layout straight from the ratios:
            // the adaptive fitting (`fitted`) reshuffles every tile the moment a minimum size or
            // the narrowed master crosses a threshold, which makes the neighbours jump around
            // mid-drag. The full fitted layout is applied once on release (see `retile`).
            let (new_tiles, new_regions) = layout_tiles(
                config::layout(),
                monitor_work(mi),
                s.last[mi].len(),
                &rs,
                gaps(),
            );
            let last = s.last[mi].clone();
            let others: Vec<_> = last
                .iter()
                .copied()
                .zip(new_tiles.iter().copied())
                .filter(|(hw, _)| *hw != h)
                .collect();
            if use_resize_preview(&s, last.len()) {
                s.resize_preview = true;
                let outlines: Vec<RECT> = others.iter().map(|(_, r)| *r).collect();
                preview::show(&outlines);
            } else {
                s.live_resize.queue(&others);
            }
            s.rects[mi] = new_tiles;
            s.regions[mi] = new_regions;
        }
        s.ratios[mi].insert(cur, rs);
    });
}

/// Pulls every tiled window of the current workspace back to its tile. Returns true if a window
/// that is now fullscreen was found: the caller must re-tile so the layout drops it.
pub(super) fn enforce_layout() -> bool {
    let _t = Timer::new("enforce_layout");
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || s.dragging.is_some() || s.divider.is_some() {
            return false;
        }
        let mut moved = 0;
        let mut fullscreen = false;
        let animating: HashSet<isize> = s.anims.iter().map(|a| a.hwnd).collect();
        for mi in 0..MON {
            if s.rects[mi].len() != s.last[mi].len() {
                continue;
            }
            let (last, rects) = (s.last[mi].clone(), s.rects[mi].clone());
            for (h, r) in last.iter().zip(rects.iter()) {
                let hw = HWND(*h as *mut _);
                if temporarily_unavailable(hw) {
                    continue; // maximized / minimized by the user: leave it alone
                }
                if is_fullscreen(hw) {
                    fullscreen = true; // a game went fullscreen: let it be, re-tile drops it
                    continue;
                }
                if animating.contains(h) {
                    continue; // already on its way
                }
                if placement_blocked(&s, hw, r) {
                    continue; // a previous bounded retry also failed for this exact target
                }
                if s.placement_pending(*h, r, Instant::now()) {
                    s.enforce = true;
                    s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
                    continue;
                }
                if observe_min_size(&mut s, hw, r) {
                    s.reflow = true;
                    s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
                    continue;
                }
                if off_target(hw, r) {
                    if s.placements.get(h).is_some_and(|request| {
                        !request.queried && request.submitted.elapsed() < Duration::from_secs(2)
                    }) && visible_rect(hw).is_some_and(|actual| {
                        actual.right - actual.left > r.right - r.left + 2
                            || actual.bottom - actual.top > r.bottom - r.top + 2
                    }) {
                        s.enforce = true;
                        s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
                        continue; // collect settled evidence before posting another resize
                    }
                    if !allow_geometry_retry(&mut s.geometry_retries, *h, *r) {
                        float_for_capacity(
                            &mut s,
                            *h,
                            "client repeatedly rejected placement without a confirmed minimum",
                        );
                        s.reflow = true;
                        s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
                        continue;
                    }
                    move_window(&mut s, hw, *r, true);
                    moved += 1;
                }
            }
        }
        if moved > 0 {
            log(format!("snapped {} window(s) back to their tiles", moved));
        }
        fullscreen
    })
}

/// A window was just shown. If it is a new, tileable window of the current workspace, put it
/// into its tile *right now* (the others glide aside) instead of waiting for the debounce timer.
/// Returns false if the normal (slower, complete) path should handle it.
pub(super) fn fast_add(s: &mut State, hwnd: HWND) -> bool {
    let _t = Timer::new("fast_add");
    let h = hwnd.0 as isize;
    if s.ws.contains_key(&h) || s.manual_float.contains(&h) || s.capacity_float.contains(&h) {
        return false; // known window shown again: the normal path decides
    }
    let mi = monitor::of_window(hwnd).min(MON - 1);
    if s.last[mi].len() >= tile_budget(mi) {
        return false; // this workspace is full: the normal path moves it to the next one
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    if style & WS_CHILD.0 != 0 {
        return false; // controls and other child windows
    }
    if s.rects[mi].len() != s.last[mi].len() || s.regions[mi].len() != s.last[mi].len() {
        return false;
    }
    let rules = config::rules();
    let w = match window::classify_with_manual(hwnd, &rules, &s.manual_float) {
        Verdict::Tileable(w) => w,
        _ => return false, // not tileable (yet): e.g. title not set, dialog, tool window
    };
    let cur = s.cur[mi];
    if w.rule_ws.is_some_and(|n| n != cur) {
        return false; // belongs to another workspace: the normal path parks it
    }
    seed_min_size(s, hwnd);
    let mut layout = s.last[mi].clone();
    layout.push(h);
    let ratios = {
        let rs = s.ratios[mi].entry(cur).or_default();
        rs.resize(layout.len() - 1, 0.5);
        rs.clone()
    };
    let monocle = is_monocle(s, mi, cur);
    let (rects, regions) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size, monocle);
    if !valid_layout(mi, &layout, &rects, &s.min_size) {
        return false;
    }
    s.ws.insert(h, cur);
    s.mon.insert(h, mi);
    remember_identity(s, hwnd);
    if !s.order[mi].contains(&h) {
        s.order[mi].push(h);
    }
    for (i, hh) in layout.iter().enumerate() {
        // the new window appears in its tile instantly; the others glide to make room
        move_window(s, HWND(*hh as *mut _), rects[i], *hh != h);
    }
    s.rects[mi] = rects;
    s.regions[mi] = regions;
    s.last[mi] = layout;
    log(format!(
        "workspace {}: new window placed instantly ({} tiled)",
        cur + 1,
        s.last[mi].len()
    ));
    true
}

/// Partition in stable order; unavailable windows are neither parked nor placed.
pub(super) fn workspace_visibility(
    s: &State,
    mi: usize,
    present: &HashSet<isize>,
) -> (Vec<isize>, Vec<isize>) {
    s.order[mi]
        .iter()
        .copied()
        .filter(|h| present.contains(h) && !s.suspended.contains(h))
        .partition(|h| s.ws[h] != s.cur[mi])
}

/// Windows that fill a monitor without being tiled still cover the workspace they are on:
/// - a maximized window (also one that opened maximized) belongs to the workspace it appeared on,
///   like a tiled one, so it is hidden when that workspace is left and tiled when un-maximized;
/// - a floating window that fills its monitor (a media viewer, a window floated with
///   `toggle_float`) is pinned to its workspace while it does, and is free again afterwards.
///   Small floating windows stay visible everywhere.
pub(super) fn track_covering_windows(s: &mut State, c: &window::Collector) {
    for w in &c.maximized {
        let h = w.hwnd.0 as isize;
        let mi = monitor::of_window(w.hwnd).min(MON - 1);
        if s.ws.contains_key(&h) {
            // moved to another monitor while maximized (Win+Shift+Arrow): it belongs to the
            // workspace that monitor shows now, as a window dragged there does
            let from = s.mi(h);
            if from != mi {
                s.order[from].retain(|window| *window != h);
                if !s.order[mi].contains(&h) {
                    s.order[mi].push(h);
                }
                s.mon.insert(h, mi);
                s.ws.insert(h, s.cur[mi]);
            }
            continue;
        }
        if s.fs_ws.contains_key(&h) {
            continue;
        }
        s.ws.insert(h, w.rule_ws.unwrap_or(s.cur[mi]));
        s.mon.insert(h, mi);
        if !s.order[mi].contains(&h) {
            s.order[mi].push(h);
        }
        remember_identity(s, w.hwnd);
    }
    let covering: HashSet<isize> = c.covering.iter().map(|w| w.hwnd.0 as isize).collect();
    for w in &c.covering {
        let h = w.hwnd.0 as isize;
        if !s.pinned.contains_key(&h) {
            let mi = monitor::of_window(w.hwnd).min(MON - 1);
            s.pinned.insert(h, (mi, s.cur[mi]));
        }
    }
    // one we minimized (or are restoring) stays pinned until it is back; one that is no longer
    // covering is free
    let keep: HashSet<isize> = s
        .max_hidden
        .keys()
        .chain(
            s.restored
                .iter()
                .filter(|(_, at)| at.elapsed() < MINIMIZE_GRACE)
                .map(|(h, _)| h),
        )
        .copied()
        .collect();
    s.pinned.retain(|h, _| {
        unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool()
            && (covering.contains(h) || keep.contains(h))
    });
}

pub(super) fn retile() {
    if SESSION_ENDING.with(Cell::get) {
        return;
    }
    if STATE.with(|s| s.borrow().divider.is_some()) {
        end_divider(false);
    }
    let _t = Timer::new("retile");
    if STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused {
            return true;
        }
        if s.dragging.is_some() {
            s.windows_changed = true;
            true
        } else {
            false
        }
    }) {
        return;
    }
    let manual = STATE.with(|s| {
        let mut s = s.borrow_mut();
        prune_identities(&mut s);
        s.manual_float.union(&s.capacity_float).copied().collect()
    });
    let c = collect(false, &manual);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused {
            return;
        }
        // a drag whose end event we missed (e.g. the window was destroyed) must not stay stuck:
        // while `dragging` is set, snapping windows back and live resizing are disabled
        if let Some(d) = s.dragging {
            if !unsafe { IsWindow(Some(HWND(d as *mut _))) }.as_bool() {
                s.dragging = None;
            }
        }
        s.windows_changed = false; // this pass reconciles the whole window set
                                   // Leaving fullscreen restores the previous assignment instead of admitting a new window.
        for window in &c.tileable {
            let h = window.hwnd.0 as isize;
            if let Some(ws) = s.fs_ws.get(&h).copied() {
                let mi = s
                    .fs_mon
                    .get(&h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(window.hwnd).min(MON - 1));
                s.ws.insert(h, ws);
                s.mon.insert(h, mi);
            }
        }
        reconcile_fullscreen(&mut s, &c.fullscreen);
        apply_fullscreen(&mut s);
        track_covering_windows(&mut s, &c);
        let present: Vec<isize> = c.tileable.iter().map(|w| w.hwnd.0 as isize).collect();
        let present_set: HashSet<isize> = present.iter().copied().collect();
        // windows we have not seen before are placed instantly; known ones glide
        let fresh: HashSet<isize> = present
            .iter()
            .copied()
            .filter(|h| !s.ws.contains_key(h))
            .collect();
        let animate_all = !std::mem::take(&mut s.no_anim);
        let slide = s.slide.take();
        // forget windows that are gone (their observed minimum size goes with them)
        s.min_size
            .retain(|h, _| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        s.manual_float
            .retain(|h| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        s.capacity_float
            .retain(|h| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        // forget windows that are gone; new windows join the current workspace, the workspace a
        // config rule asks for, or - if the current workspace is full - the next workspace
        let fullscreen: HashSet<_> = c
            .fullscreen
            .iter()
            .map(|window| window.hwnd.0 as isize)
            .collect();
        retain_assignments(&mut s, &present_set, &fullscreen, &manual);
        // `suspended` is current now: minimize / restore the maximized windows of other workspaces
        apply_maximized(&mut s);
        // Restore/migrate every known window's order before admitting any new one.
        // Otherwise a fullscreen exit later in the enumeration could change a
        // layout that an earlier new window already used for its capacity check.
        for w in &c.tileable {
            let h = w.hwnd.0 as isize;
            if !s.ws.contains_key(&h) {
                continue;
            }
            if !is_parked(w.hwnd) {
                s.observe_monitor(h, monitor::of_window(w.hwnd));
            }
            let mi = s.mi(h);
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
        }
        let mut overflow: Option<(usize, u8, isize)> = None;
        for w in &c.tileable {
            let h = w.hwnd.0 as isize;
            remember_identity(&mut s, w.hwnd);
            if s.ws.contains_key(&h) {
                continue;
            }
            seed_min_size(&mut s, w.hwnd);
            let mi = monitor::of_window(w.hwnd).min(MON - 1);
            let cur = s.cur[mi];
            let ws = match w.rule_ws {
                Some(ws) if workspace_accepts(&s, mi, ws, h) => Some(ws),
                Some(_) => None,
                None => workspace_for_new_window(&s, mi, cur, h),
            };
            let Some(ws) = ws else {
                float_for_capacity(
                    &mut s,
                    h,
                    "new window cannot fit the requested workspace(s)",
                );
                continue;
            };
            s.ws.insert(h, ws);
            s.mon.insert(h, mi);
            // Commit admission order now: later windows in the same scan must be
            // validated against the exact Spiral order that will be placed.
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
            if ws != cur && overflow.is_none() {
                overflow = Some((mi, ws, h));
            }
        }
        if overflow.is_some() {
            s.follow = overflow; // switch to the new workspace, like the taskbar "follow"
        }
        // keep known windows in their old order, drop the gone ones, append the new ones
        for mi in 0..MON {
            let mut order: Vec<isize> = s.order[mi]
                .iter()
                .copied()
                .filter(|h| s.ws.contains_key(h) && s.mon.get(h).copied() == Some(mi))
                .collect();
            let mut seen: HashSet<isize> = order.iter().copied().collect();
            for h in &present {
                if s.ws.contains_key(h) && s.mon.get(h).copied() == Some(mi) && seen.insert(*h) {
                    order.push(*h);
                }
            }
            s.order[mi] = order;
        }
        reconcile_capacity(&mut s);
        // make sure windows of other workspaces stay out of sight, and group the shown ones
        let mut layouts: Vec<Vec<isize>> = vec![Vec::new(); MON];
        for (mi, layout) in layouts.iter_mut().enumerate() {
            let (hidden, shown) = workspace_visibility(&s, mi, &present_set);
            for h in hidden {
                park(&mut s, HWND(h as *mut _));
            }
            *layout = shown;
        }
        sync_parked(&mut s);
        let mut changed = false;
        for (mi, layout) in layouts.into_iter().enumerate() {
            let cur = s.cur[mi];
            {
                let rs = s.ratios[mi].entry(cur).or_default();
                rs.resize(layout.len().saturating_sub(1), 0.5);
            }
            let monocle = is_monocle(&s, mi, cur);
            let (rects, regions) = fitted(
                monitor_work(mi),
                &layout,
                &s.ratios[mi][&cur],
                &s.min_size,
                monocle,
            );
            if layout == s.last[mi] && rects == s.rects[mi] {
                // same windows and same tiles: leave them alone (respects manual tweaks)
                continue;
            }
            s.rects[mi] = rects;
            s.regions[mi] = regions;
            // the windows of the workspace just switched to come in with a short slide from the
            // side of the dot they belong to; a slide only moves them, so it costs the apps little
            let slide_offset = slide
                .filter(|(smi, _, at)| {
                    *smi == mi
                        && layout.len() <= SLIDE_MAX_WINDOWS
                        && at.elapsed() < Duration::from_millis(1000)
                })
                .map(|(_, dir, _)| {
                    let work = monitor_work(mi);
                    dir * ((work.right - work.left) as f32 * SLIDE_SHARE).min(SLIDE_MAX_PX) as i32
                });
            for (i, h) in layout.iter().enumerate() {
                if s.dragging == Some(*h) {
                    continue; // the user is dragging it right now: leave it under the cursor
                }
                let animate = animate_all && !fresh.contains(h);
                let target = s.rects[mi][i];
                let hwnd = HWND(*h as *mut _);
                match slide_offset {
                    Some(dx) if !fresh.contains(h) && is_parked(hwnd) => {
                        let from = RECT {
                            left: target.left + dx,
                            right: target.right + dx,
                            ..target
                        };
                        slide_in(&mut s, hwnd, from, target);
                    }
                    _ => move_window(&mut s, hwnd, target, animate),
                }
            }
            log(format!(
                "monitor {}: workspace {}: tiled {} window(s)",
                mi + 1,
                s.cur[mi] + 1,
                layout.len()
            ));
            s.last[mi] = layout;
            changed = true;
        }
        if std::mem::take(&mut s.min_size_reflow) && s.anims.is_empty() && !s.paused {
            s.reflow = true;
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
        } else if changed && s.anims.is_empty() && !s.paused {
            // Verify instant placements too; this catches clients that adjust their own
            // geometry after the asynchronous placement request was posted.
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        }
        if s.follow.is_some() {
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
        }
    });
    // the focused window may only now have joined the layout: make sure it has the border
    refresh_border();
    sync_indicator();
}
