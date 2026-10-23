//! The focus border: picks the window that carries it and keeps the overlay (see `overlay.rs`)
//! on that window's visible frame, hiding it whenever there is nothing to outline.

use std::mem::size_of;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
};

use crate::window::visible_rect;
use crate::{config, monitor, overlay, sync_dividers, State, STATE};

/// Draws the border around the *visible* frame `vis` of `target`. The border window is placed just
/// below `target` in the z-order and clipped to a hollow frame, so it can never cover the window
/// with a solid colour (for example while the window is being placed or maximized).
pub(crate) fn place_border(vis: &RECT, target: isize) {
    let w = config::border_width();
    let hw = HWND(target as *mut _);
    // a maximized window has no tiled frame, and a hidden / minimized / cloaked / destroyed one
    // has nothing to outline (its frame rectangle can still be reported): no border
    if w <= 0 || unsafe { IsZoomed(hw).as_bool() } || !border_window_shown(hw) {
        overlay::hide_border();
        return;
    }
    // never draw a border that fills the whole monitor the window is on (a window in transition)
    let mon = monitor::work(monitor::of_window(hw));
    if (vis.right - vis.left) >= (mon.right - mon.left)
        || (vis.bottom - vis.top) >= (mon.bottom - mon.top)
    {
        overlay::hide_border();
        return;
    }
    place_border_frame(vis, target, w);
}

/// Whether `hwnd` is a live, visible, un-minimized, un-cloaked window (one a border can sit on).
pub(crate) fn border_window_shown(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool()
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
        {
            return false;
        }
        let mut cloaked: u32 = 0;
        let queried = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
        .is_ok();
        !(queried && cloaked != 0)
    }
}

/// Positions the border overlay around the observed visible frame.
fn place_border_frame(vis: &RECT, target: isize, w: i32) {
    if w <= 0 {
        overlay::hide_border();
        return;
    }
    let rect = RECT {
        left: vis.left - w,
        top: vis.top - w,
        right: vis.right + w,
        bottom: vis.bottom + w,
    };
    // rounded corners: the window's own corner radius (~8 px) plus the border width
    overlay::show_border(rect, HWND(target as *mut _), 8 + w, w);
}

/// Picks the window that should carry the border (the focused tiled window of the current
/// workspace) and positions the overlay around it.
pub(crate) fn update_border(s: &mut State, focus: isize) {
    if s.focus_cur != Some(focus) && (s.ws.contains_key(&focus) || s.fs_ws.contains_key(&focus)) {
        s.focus_prev = s.focus_cur;
        s.focus_cur = Some(focus);
    }
    // a tiled window of the current workspace: in the applied layout, or (should the layout list
    // be momentarily stale) a known window that belongs to the current workspace
    // a window we have no record of yet is placed by where it really is, not on monitor 0
    let mi = s
        .mon
        .get(&focus)
        .copied()
        .unwrap_or_else(|| monitor::of_window(HWND(focus as *mut _)))
        .min(crate::MON - 1);
    let tiled = s.last[mi].contains(&focus) || s.ws.get(&focus) == Some(&s.cur[mi]);
    s.border_on = (config::border().is_some()
        && border_window_shown(HWND(focus as *mut _))
        && !s.paused
        && s.dragging.is_none()
        && s.divider.is_none()
        && tiled)
        .then_some(focus);
}

/// Re-evaluate which window should have the border (after layout / pause / config changes).
pub(crate) fn refresh_border() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    STATE.with(|s| update_border(&mut s.borrow_mut(), fg));
    sync_border();
    sync_dividers();
}

/// Repositions the border overlay around the current border window (`State::border_on`).
pub(crate) fn sync_border() {
    let target = STATE.with(|s| s.borrow().border_on);
    match target {
        Some(h) => sync_border_to(h),
        None => overlay::hide_border(),
    }
}

pub(crate) fn sync_border_to(h: isize) {
    match visible_rect(HWND(h as *mut _)) {
        Some(vis) => place_border(&vis, h),
        None => overlay::hide_border(),
    }
}

/// Hide the border (pause, quit, colour change).
pub(crate) fn clear_border() {
    STATE.with(|s| s.borrow_mut().border_on = None);
    overlay::hide_border();
}
