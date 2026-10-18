//! The monitors attached to the system and how points / windows map to them.

use std::cell::RefCell;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};

const MONITORINFOF_PRIMARY: u32 = 0x1;

#[derive(Clone, Debug)]
pub struct Monitor {
    pub handle: HMONITOR,
    pub work: RECT,
    pub full: RECT,
    pub primary: bool,
}

fn from_handle(handle: HMONITOR) -> Option<Monitor> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(handle, &mut mi) }.as_bool() {
        return None;
    }
    Some(Monitor {
        handle,
        work: mi.rcWork,
        full: mi.rcMonitor,
        primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

unsafe extern "system" fn enum_proc(
    handle: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<Monitor>);
    if let Some(m) = from_handle(handle) {
        out.push(m);
    }
    BOOL(1)
}

/// All monitors, in the order Windows reports them (stable between calls without a hot-plug).
pub fn all() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&mut out as *mut _ as isize),
        );
    }
    if out.is_empty() {
        // EnumDisplayMonitors should never fail completely, but keep a usable fallback
        let handle = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        if let Some(m) = from_handle(handle) {
            out.push(m);
        }
    }
    out
}

fn contains(r: RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

fn dist2(r: RECT, pt: POINT) -> i64 {
    let dx = if pt.x < r.left {
        r.left - pt.x
    } else if pt.x >= r.right {
        pt.x - (r.right - 1)
    } else {
        0
    };
    let dy = if pt.y < r.top {
        r.top - pt.y
    } else if pt.y >= r.bottom {
        pt.y - (r.bottom - 1)
    } else {
        0
    };
    i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)
}

/// Index of the monitor whose rect contains `pt`, or of the nearest one.
pub fn nearest(monitors: &[Monitor], pt: POINT) -> usize {
    let mut best = 0;
    let mut best_d = i64::MAX;
    for (i, m) in monitors.iter().enumerate() {
        if contains(m.full, pt) {
            return i;
        }
        let d = dist2(m.full, pt);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Index of the primary monitor (0 if none is marked primary).
pub fn primary_index(monitors: &[Monitor]) -> usize {
    monitors.iter().position(|m| m.primary).unwrap_or(0)
}

thread_local! {
    static CACHE: RefCell<Vec<Monitor>> = const { RefCell::new(Vec::new()) };
}

/// Re-reads the monitor list (call on start-up and on a display change).
pub fn refresh() {
    let list = all();
    CACHE.with(|c| *c.borrow_mut() = list);
}

/// The cached monitor list, read once if it is still empty.
pub fn list() -> Vec<Monitor> {
    let empty = CACHE.with(|c| c.borrow().is_empty());
    if empty {
        refresh();
    }
    CACHE.with(|c| c.borrow().clone())
}

/// The work area (taskbar excluded) of the primary monitor.
pub fn primary_work() -> RECT {
    let list = list();
    let i = primary_index(&list);
    list.get(i).map(|m| m.work).unwrap_or_default()
}

/// The index of the monitor under `pt` (nearest).
#[allow(dead_code)]
pub fn at(pt: POINT) -> usize {
    nearest(&list(), pt)
}

/// The index of the monitor a window is on (nearest).
#[allow(dead_code)]
pub fn of_window(hwnd: HWND) -> usize {
    let handle = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let list = list();
    list.iter()
        .position(|m| m.handle == handle)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(left: i32, top: i32, right: i32, bottom: i32, primary: bool) -> Monitor {
        let full = RECT {
            left,
            top,
            right,
            bottom,
        };
        Monitor {
            handle: HMONITOR(std::ptr::null_mut()),
            work: full,
            full,
            primary,
        }
    }

    #[test]
    fn nearest_finds_the_containing_monitor() {
        let mons = [mon(0, 0, 1920, 1080, true), mon(1920, 0, 3840, 1080, false)];
        assert_eq!(nearest(&mons, POINT { x: 10, y: 10 }), 0);
        assert_eq!(nearest(&mons, POINT { x: 2000, y: 500 }), 1);
    }

    #[test]
    fn nearest_picks_the_closest_when_outside() {
        let mons = [mon(0, 0, 1920, 1080, true), mon(1920, 0, 3840, 1080, false)];
        // left of the primary belongs to the primary
        assert_eq!(nearest(&mons, POINT { x: -500, y: 500 }), 0);
        // far right belongs to the second monitor
        assert_eq!(nearest(&mons, POINT { x: 5000, y: 500 }), 1);
    }

    #[test]
    fn primary_index_finds_the_primary_flag() {
        let mons = [mon(0, 0, 1920, 1080, false), mon(1920, 0, 3840, 1080, true)];
        assert_eq!(primary_index(&mons), 1);
        let none = [mon(0, 0, 100, 100, false)];
        assert_eq!(primary_index(&none), 0);
    }
}
