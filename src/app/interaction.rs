//! Divider gestures, workspace indicators and display-change handling.

use super::*;

pub(super) fn sync_dividers() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let handles = STATE.with(|state| {
        let s = state.borrow();
        if s.paused
            || s.dragging.is_some()
            || !s.anims.is_empty()
            || !s.last.iter().any(|windows| windows.contains(&fg))
        {
            return Vec::new();
        }
        (0..monitor::count().min(MON))
            .flat_map(|mi| {
                let frames: Vec<_> = s.last[mi]
                    .iter()
                    .filter_map(|h| visible_rect(HWND(*h as *mut _)))
                    .collect();
                divider::unobstructed(
                    divider::handles(
                        mi,
                        config::layout(),
                        &s.rects[mi],
                        &s.regions[mi],
                        config::inner_gap(),
                    ),
                    &frames,
                )
            })
            .collect()
    });
    divider::sync(handles);
}

pub(crate) fn begin_divider(handle: divider::Handle, origin: POINT) -> bool {
    let _profile = Timer::new("divider_prepare");
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return false;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let mi = handle.monitor;
        if mi >= MON
            || s.paused
            || s.dragging.is_some()
            || s.divider.is_some()
            || s.windows_changed
            || !s.anims.is_empty()
            || !divider::handles(
                mi,
                config::layout(),
                &s.rects[mi],
                &s.regions[mi],
                config::inner_gap(),
            )
            .iter()
            .any(|candidate| {
                candidate.split == handle.split && candidate.vertical == handle.vertical
            })
        {
            return false;
        }
        let windows = s.last[mi].clone();
        for h in &windows {
            s.placement_failures.remove(h);
            s.geometry_retries.remove(h);
        }
        let sources: Vec<_> = windows
            .iter()
            .filter_map(|h| visible_rect(HWND(*h as *mut _)).map(|r| (*h, r)))
            .collect();
        if sources.len() != windows.len()
            || sources
                .iter()
                .zip(&s.rects[mi])
                .any(|((_, actual), target)| !same_rect(actual, target, 2))
        {
            return false;
        }
        s.live_borders.clear();
        capture_live_borders(&mut s, &windows);
        let layout = config::layout();
        let gaps = gaps();
        // Recover ratios from settled geometry, including any adaptive minimum-size fitting.
        let mut ratios = vec![0.5; windows.len() - 1];
        let count = if layout == Layout::MasterStack {
            1
        } else {
            ratios.len()
        };
        for (j, ratio) in ratios.iter_mut().enumerate().take(count) {
            let q = s.regions[mi][j];
            let t = s.rects[mi][j];
            *ratio = if layout == Layout::MasterStack || j.is_multiple_of(2) {
                ((t.right - q.left) as f32 + 0.25) / (q.right - q.left - gaps.inner).max(1) as f32
            } else {
                ((t.bottom - q.top) as f32 + 0.25) / (q.bottom - q.top - gaps.inner).max(1) as f32
            };
        }
        let configured = config::min_tile();
        let minimums = windows
            .iter()
            .zip(&s.rects[mi])
            .map(|(h, r)| {
                let observed = s.min_size.get(h).copied().unwrap_or_default();
                (
                    (configured.0.max(observed.0)).min(r.right - r.left),
                    (configured.1.max(observed.1)).min(r.bottom - r.top),
                )
            })
            .collect();
        s.divider = Some(divider::Resize {
            handle,
            layout,
            area: monitor_work(mi),
            gaps,
            windows,
            workspace: s.cur[mi],
            ratios,
            tiles: s.rects[mi].clone(),
            preview: s.rects[mi].clone(),
            regions: s.regions[mi].clone(),
            minimums,
            origin,
            last_pointer_axis: if handle.vertical { origin.x } else { origin.y },
        });
        s.border_on = None;
        overlay::hide_border();
        if s.timer != 0 {
            unsafe {
                let _ = KillTimer(None, s.timer);
            }
            s.timer = 0;
        }
        arm_anim_timer(&mut s);
        if !s.frame_armed {
            s.divider = None;
            s.live_borders.clear();
            return false;
        }
        true
    })
}

pub(crate) fn update_divider(point: POINT) {
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return;
    }
    let valid = STATE.with(|state| {
        let mut s = state.borrow_mut();
        let Some(gesture) = s.divider.as_ref() else {
            return false;
        };
        let mi = gesture.handle.monitor;
        if s.paused
            || s.cur[mi] != gesture.workspace
            || s.last[mi] != gesture.windows
            || config::layout() != gesture.layout
        {
            return false;
        }
        let constraints = s
            .live_resize
            .constrained_targets(Instant::now(), |h| visible_rect(HWND(h as *mut _)));
        let mut minimum_changed = false;
        for (h, target) in constraints {
            if observe_min_size(&mut s, HWND(h as *mut _), &target) {
                let minimum = s.min_size[&h];
                let gesture = s.divider.as_mut().unwrap();
                if let Some(i) = gesture.windows.iter().position(|window| *window == h) {
                    gesture.minimums[i].0 = gesture.minimums[i].0.max(minimum.0);
                    gesture.minimums[i].1 = gesture.minimums[i].1.max(minimum.1);
                    minimum_changed = true;
                }
            }
        }
        if !s.divider.as_mut().unwrap().pointer_changed(point) && !minimum_changed {
            return true;
        }
        let gesture = s.divider.as_ref().unwrap();
        let (_, tiles, _) = gesture.target(point);
        if tiles == gesture.preview {
            return true;
        }
        let targets: Vec<_> = gesture
            .windows
            .iter()
            .copied()
            .zip(tiles.iter().copied())
            .collect();
        s.live_resize.queue(&targets);
        s.divider.as_mut().unwrap().preview = tiles;
        true
    });
    if !valid {
        end_divider(false);
    }
}

pub(crate) fn end_divider(commit: bool) {
    finish_divider(if commit {
        DividerFinish::Pointer
    } else {
        DividerFinish::Cancel
    });
}

#[derive(Clone, Copy)]
pub(super) enum DividerFinish {
    Cancel,
    Pointer,
    Preview,
}

pub(crate) fn settle_divider() {
    // Escape may cause native capture cancellation before the frame timer polls it.
    finish_divider(if unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE.0)) } < 0 {
        DividerFinish::Cancel
    } else {
        DividerFinish::Preview
    });
}

pub(super) fn finish_divider(finish: DividerFinish) {
    divider::release();
    // SetWindowPos can pump a capture-loss callback during a placement transaction.
    // The next frame cancels the gesture once the State borrow has been released.
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return;
    }
    if STATE.with(|s| s.borrow().divider.is_none()) {
        return;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let Some(gesture) = s.divider.take() else {
            return;
        };
        let mi = gesture.handle.monitor;
        let valid = !s.paused
            && s.cur[mi] == gesture.workspace
            && s.last[mi] == gesture.windows
            && config::layout() == gesture.layout;
        s.live_resize.forget(&gesture.windows);
        let mut point = POINT::default();
        let result = valid
            .then(|| match finish {
                DividerFinish::Cancel => None,
                DividerFinish::Pointer if unsafe { GetCursorPos(&mut point) }.is_ok() => {
                    Some(gesture.target(point))
                }
                _ => Some(gesture.last_target()),
            })
            .flatten();
        let tiles = result
            .as_ref()
            .map_or(&gesture.tiles, |(_, tiles, _)| tiles);
        // Final placement bypasses the frame throttle. Cancellation restores the
        // original geometry, but must never resurrect or unpark a departed window.
        if s.cur[mi] == gesture.workspace {
            let targets: Vec<_> = gesture
                .windows
                .iter()
                .copied()
                .zip(tiles.iter().copied())
                .filter(|(h, _)| {
                    let hwnd = HWND(*h as *mut _);
                    s.ws.get(h) == Some(&gesture.workspace)
                        && s.mi(*h) == mi
                        && unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
                })
                .collect();
            place_batch(&mut s, &targets);
        }
        if let Some((ratios, tiles, regions)) = result {
            s.ratios[mi].insert(gesture.workspace, ratios);
            s.rects[mi] = tiles;
            s.regions[mi] = regions;
        }
        for h in &gesture.windows {
            s.live_borders.remove(h);
        }
        if !s.paused {
            s.enforce = true;
            if std::mem::take(&mut s.min_size_reflow) {
                s.reflow = true;
            }
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        }
    });
    refresh_border();
}

/// Updates each monitor's workspace indicator; the tiling area leaves room for its own bar.
pub(super) fn sync_indicator() {
    if !config::workspace_indicator() {
        overlay::hide_indicator();
        return;
    }
    let mons = monitor::list();
    STATE.with(|s| {
        let s = s.borrow();
        for (mi, mon) in mons.iter().enumerate().take(MON) {
            let dpi = monitor::dpi(mi);
            let (w, h) = overlay::indicator_window_size(dpi);
            let x = mon.work.left + (mon.work.right - mon.work.left - w) / 2;
            let rect = RECT {
                left: x,
                top: mon.work.top,
                right: x + w,
                bottom: mon.work.top + h,
            };
            let mut mask = 0u32;
            for (window, ws) in &s.ws {
                if s.mi(*window) == mi {
                    mask |= 1u32 << (*ws as u32).min(31);
                }
            }
            for (window, ws) in &s.fs_ws {
                let fs_mi = s
                    .fs_mon
                    .get(window)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(*window as *mut _)))
                    .min(MON - 1);
                if fs_mi == mi {
                    mask |= 1u32 << (*ws as u32).min(31);
                }
            }
            overlay::show_indicator(mi, rect, dpi, s.cur[mi], mask);
        }
    });
}

/// (current workspace 0-based, paused) - for the tray.
pub(crate) fn ui_state() -> (u8, bool) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.cur[active_mon(&s)], s.paused)
    })
}

/// The screen work area changed (display mode, taskbar size / auto-hide): rebuild the layout.
pub(crate) fn request_relayout() {
    end_divider(false);
    DISPLAY_HZ.with(|cache| *cache.borrow_mut() = None);
    let old_monitors = monitor::list();
    monitor::refresh();
    let new_monitors = monitor::list();
    reconcile_monitor_slots(&old_monitors, &new_monitors);
    if config::workspace_indicator() {
        let _ = overlay::sync_indicator_count(new_monitors.len());
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.invalidate_layout_constraints();
        s.reflow = true;
        s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
    });
}
