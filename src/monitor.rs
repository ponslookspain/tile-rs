//! The monitors attached to the system and how points / windows map to them.

use std::cell::RefCell;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};

const MONITORINFOF_PRIMARY: u32 = 0x1;

#[derive(Clone, Debug)]
pub struct Monitor {
    pub handle: HMONITOR,
    /// Windows display-device name, used to keep monitor-scoped state attached across
    /// enumeration reorderings. This is a session identity, not a guaranteed hardware serial.
    pub device: String,
    pub work: RECT,
    pub full: RECT,
    pub primary: bool,
}

fn from_handle(handle: HMONITOR) -> Option<Monitor> {
    let mut mi = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(handle, &mut mi.monitorInfo) }.as_bool() {
        return None;
    }
    let device_len = mi
        .szDevice
        .iter()
        .position(|ch| *ch == 0)
        .unwrap_or(mi.szDevice.len());
    Some(Monitor {
        handle,
        device: String::from_utf16_lossy(&mi.szDevice[..device_len]),
        work: mi.monitorInfo.rcWork,
        full: mi.monitorInfo.rcMonitor,
        primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
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
    with_cached(|list| list.to_vec())
}

/// Do not call Win32 or client callbacks while borrowing this cache.
fn with_cached<T>(read: impl FnOnce(&[Monitor]) -> T) -> T {
    let empty = CACHE.with(|c| c.borrow().is_empty());
    if empty {
        refresh();
    }
    CACHE.with(|c| read(&c.borrow()))
}

pub fn count() -> usize {
    with_cached(|list| list.len())
}

/// A stale monitor index falls back to the last real monitor without cloning device strings.
pub fn work(mi: usize) -> RECT {
    with_cached(|list| {
        list.get(mi.min(list.len().saturating_sub(1)))
            .map(|m| m.work)
            .unwrap_or_default()
    })
}

/// Effective DPI of monitor `mi` (96 when it cannot be read).
pub fn dpi(mi: usize) -> u32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let handle = with_cached(|list| {
        list.get(mi.min(list.len().saturating_sub(1)))
            .map(|m| m.handle)
    });
    let (mut x, mut y) = (0u32, 0u32);
    match handle {
        Some(h) if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok() => {
            x.max(96)
        }
        _ => 96,
    }
}

/// Shifts `r` into `area`; a rectangle larger than `area` is shrunk to it.
pub fn clamp_into(r: RECT, area: RECT) -> RECT {
    let w = (r.right - r.left).min(area.right - area.left).max(0);
    let h = (r.bottom - r.top).min(area.bottom - area.top).max(0);
    let left = r.left.clamp(area.left, (area.right - w).max(area.left));
    let top = r.top.clamp(area.top, (area.bottom - h).max(area.top));
    RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

/// `r` kept inside the work area of the monitor that holds its centre, so a placement request
/// can never push a window past a screen edge. A rectangle centred on no monitor is an
/// intentional off-screen placement (parking) and is left alone.
pub fn clamp_to_work(r: RECT) -> RECT {
    let centre = POINT {
        x: r.left + (r.right - r.left) / 2,
        y: r.top + (r.bottom - r.top) / 2,
    };
    with_cached(|list| {
        list.iter()
            .find(|m| contains(m.full, centre))
            .map_or(r, |m| clamp_into(r, m.work))
    })
}

/// The index of the monitor under `pt` (nearest).
pub fn at(pt: POINT) -> usize {
    with_cached(|list| nearest(list, pt))
}

/// The index of the monitor a window is on (nearest).
pub fn of_window(hwnd: HWND) -> usize {
    let handle = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    with_cached(|list| {
        list.iter()
            .position(|m| m.handle == handle)
            .unwrap_or_default()
    })
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
            device: format!("monitor-{left}-{top}"),
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
    fn clamp_keeps_rects_on_screen() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 600,
        };
        let r = |l, t, rt, b| RECT {
            left: l,
            top: t,
            right: rt,
            bottom: b,
        };
        let eq = |a: RECT, b: RECT| {
            (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
        };
        assert!(eq(
            clamp_into(r(100, 100, 400, 300), area),
            r(100, 100, 400, 300)
        ));
        assert!(eq(
            clamp_into(r(900, 500, 1200, 700), area),
            r(700, 400, 1000, 600)
        ));
        assert!(eq(
            clamp_into(r(-50, -20, 250, 180), area),
            r(0, 0, 300, 200)
        ));
        assert!(eq(
            clamp_into(r(-100, 0, 1400, 900), area),
            r(0, 0, 1000, 600)
        ));
    }

    #[test]
    fn primary_index_finds_the_primary_flag() {
        let mons = [mon(0, 0, 1920, 1080, false), mon(1920, 0, 3840, 1080, true)];
        assert_eq!(primary_index(&mons), 1);
        let none = [mon(0, 0, 100, 100, false)];
        assert_eq!(primary_index(&none), 0);
    }
}
