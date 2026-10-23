//! Map physical display identities to stable workspace slots.

use super::*;

/// The monitor the user is working on: the foreground window's monitor if it is known, otherwise
/// the monitor under the pointer, otherwise the primary one.
pub(super) fn active_mon(s: &State) -> usize {
    let h = unsafe { GetForegroundWindow() }.0 as isize;
    if s.mon.contains_key(&h) {
        return s.mi(h);
    }
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        return monitor::at(pt).min(MON - 1);
    }
    0
}

/// Seed the slot identities before the first window collection. The Win32 enumeration order is
/// convenient for indexing, but must not be treated as the identity of a physical display.
pub(super) fn remember_monitor_slots() {
    let monitors = monitor::list();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        for (i, mon) in monitors.iter().take(MON).enumerate() {
            state.monitor_ids[i] = Some(mon.device.clone());
        }
    });
}

pub(super) fn monitor_slot_map(
    old: &[monitor::Monitor],
    new: &[monitor::Monitor],
    identities: &[Option<String>; MON],
) -> ([usize; MON], [bool; MON]) {
    let primary = monitor::primary_index(new).min(MON - 1);
    let mut old_to_new = [primary; MON];
    let mut exact = [false; MON];
    for old_i in 0..MON {
        let key = identities[old_i]
            .as_deref()
            .or_else(|| old.get(old_i).map(|m| m.device.as_str()));
        if let Some(key) = key {
            if let Some(new_i) = new
                .iter()
                .take(MON)
                .position(|candidate| candidate.device == key)
            {
                old_to_new[old_i] = new_i;
                exact[old_i] = true;
                continue;
            }
        }
        if let Some(previous) = old.get(old_i) {
            let center = POINT {
                x: previous.full.left + (previous.full.right - previous.full.left) / 2,
                y: previous.full.top + (previous.full.bottom - previous.full.top) / 2,
            };
            old_to_new[old_i] = monitor::nearest(&new[..new.len().min(MON)], center);
        }
    }
    (old_to_new, exact)
}

/// Preserve each screen's workspace/layout when Windows changes monitor enumeration order. When
/// a display disappeared, its visible windows follow Windows' re-homing; parked windows go to
/// the nearest surviving display and its current workspace.
pub(super) fn reconcile_monitor_slots(old: &[monitor::Monitor], new: &[monitor::Monitor]) {
    if new.is_empty() {
        return;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let (old_to_new, exact) = monitor_slot_map(old, new, &s.monitor_ids);
        // Display changes supersede requests made against the previous monitor slots.
        s.pending_mon.clear();

        let mut new_cur = [0u8; MON];
        let mut new_ratios: [HashMap<u8, Vec<f32>>; MON] = std::array::from_fn(|_| HashMap::new());
        let mut has_exact = [false; MON];
        for old_i in 0..MON {
            if exact[old_i] {
                let new_i = old_to_new[old_i];
                if !has_exact[new_i] {
                    new_cur[new_i] = s.cur[old_i];
                    new_ratios[new_i] = std::mem::take(&mut s.ratios[old_i]);
                    has_exact[new_i] = true;
                }
            }
        }
        let mut new_order: [Vec<isize>; MON] = std::array::from_fn(|_| Vec::new());
        let old_window_mon = s.mon.clone();
        for old_i in 0..MON {
            let fallback = old_to_new[old_i];
            for h in std::mem::take(&mut s.order[old_i]) {
                let hwnd = HWND(h as *mut _);
                let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool() && !is_parked(hwnd) {
                    monitor::of_window(hwnd).min(MON - 1)
                } else {
                    fallback
                };
                if dest != fallback || !exact[old_i] {
                    s.ws.insert(h, new_cur[dest]);
                }
                s.mon.insert(h, dest);
                if !new_order[dest].contains(&h) {
                    new_order[dest].push(h);
                }
            }
        }
        // Include any tracked window whose order entry was already lost, and refresh fullscreen
        // monitor ownership without trusting MonitorFromWindow for our off-screen parked windows.
        for (h, old_slot) in old_window_mon {
            let previous = old_slot.min(MON - 1);
            let fallback = old_to_new[previous];
            let hwnd = HWND(h as *mut _);
            let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool() && !is_parked(hwnd) {
                monitor::of_window(hwnd).min(MON - 1)
            } else {
                fallback
            };
            if dest != fallback || !exact[previous] {
                s.ws.insert(h, new_cur[dest]);
            }
            s.mon.insert(h, dest);
            if !new_order[dest].contains(&h) {
                new_order[dest].push(h);
            }
        }
        let fullscreen: Vec<(isize, usize)> = s.fs_mon.iter().map(|(h, mi)| (*h, *mi)).collect();
        for (h, old_i) in fullscreen {
            let old_i = old_i.min(MON - 1);
            let fallback = old_to_new[old_i];
            let hwnd = HWND(h as *mut _);
            let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool()
                && !unsafe { IsIconic(hwnd) }.as_bool()
            {
                monitor::of_window(hwnd).min(MON - 1)
            } else {
                fallback
            };
            if dest != fallback || !exact[old_i] {
                s.fs_ws.insert(h, new_cur[dest]);
            }
            s.fs_mon.insert(h, dest);
        }

        s.cur = new_cur;
        s.order = new_order;
        s.ratios = new_ratios;
        s.monitor_ids = std::array::from_fn(|i| new.get(i).map(|mon| mon.device.clone()));
        for i in 0..MON {
            s.last[i].clear();
            s.rects[i].clear();
            s.regions[i].clear();
        }
        s.reflow = true;
    });
}
