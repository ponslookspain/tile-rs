//! Outlines of tiles that are not there yet: the tiles a separator / edge drag would produce
//! (`live_resize = "preview"`) and the tile a dragged window would land in. One click-through,
//! translucent window per outline, drawn by tile-rs itself, so the real windows do not have to
//! re-lay out their content on every frame of a drag.

use std::cell::RefCell;
use std::mem::size_of;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect,
    HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Opacity of an outline: the window behind stays readable.
const ALPHA: u8 = 90;
/// Width of the brighter edge, in pixels.
const EDGE: i32 = 2;

thread_local! {
    /// Outline windows, reused from one frame to the next (hidden ones stay for the next drag).
    static OUTLINES: RefCell<Vec<(HWND, RECT)>> = const { RefCell::new(Vec::new()) };
    static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mix the accent with white for the edge (COLORREF channels).
fn lighter(color: u32) -> u32 {
    let mix = |shift: u32| {
        let c = (color >> shift) & 0xFF;
        (c + (255 - c) / 2) << shift
    };
    mix(0) | mix(8) | mix(16)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let accent = crate::config::accent_color_value();
            let fill = CreateSolidBrush(COLORREF(accent));
            FillRect(dc, &rc, fill);
            let edge = CreateSolidBrush(COLORREF(lighter(accent)));
            for inset in 0..EDGE {
                let ring = RECT {
                    left: rc.left + inset,
                    top: rc.top + inset,
                    right: rc.right - inset,
                    bottom: rc.bottom - inset,
                };
                FrameRect(dc, &ring, edge);
            }
            let _ = DeleteObject(HGDIOBJ(fill.0));
            let _ = DeleteObject(HGDIOBJ(edge.0));
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn create_outline() -> Option<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).ok()?.0);
        let class = w!("tile-rs-preview");
        if !REGISTERED.with(|r| r.get()) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return None;
            }
            REGISTERED.with(|r| r.set(true));
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class,
            w!("tile-rs-preview"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
        .ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), ALPHA, LWA_ALPHA);
        // rounded like the windows they stand for (Windows 11; ignored elsewhere)
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            size_of::<i32>() as u32,
        );
        Some(hwnd)
    }
}

/// Shows one outline per rectangle (screen coordinates); extra outlines are hidden.
pub fn show(rects: &[RECT]) {
    OUTLINES.with(|outlines| {
        let mut outlines = outlines.borrow_mut();
        while outlines.len() < rects.len() {
            let Some(hwnd) = create_outline() else {
                break;
            };
            outlines.push((hwnd, RECT::default()));
        }
        for (i, (hwnd, shown)) in outlines.iter_mut().enumerate() {
            let Some(r) = rects.get(i) else {
                if *shown != RECT::default() {
                    unsafe {
                        let _ = ShowWindow(*hwnd, SW_HIDE);
                    }
                    *shown = RECT::default();
                }
                continue;
            };
            if r.right <= r.left || r.bottom <= r.top || shown == r {
                continue;
            }
            let was_hidden = *shown == RECT::default();
            let resized = (shown.right - shown.left, shown.bottom - shown.top)
                != (r.right - r.left, r.bottom - r.top);
            *shown = *r;
            unsafe {
                let _ = SetWindowPos(
                    *hwnd,
                    Some(HWND_TOPMOST),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE,
                );
                if resized {
                    let _ = InvalidateRect(Some(*hwnd), None, false);
                }
                if was_hidden {
                    let _ = ShowWindow(*hwnd, SW_SHOWNA);
                }
            }
        }
    });
}

pub fn hide() {
    show(&[]);
}

pub fn destroy() {
    let outlines = OUTLINES.with(|o| std::mem::take(&mut *o.borrow_mut()));
    for (hwnd, _) in outlines {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_edge_is_halfway_between_the_accent_and_white() {
        assert_eq!(lighter(0x0000_0000), 0x007F_7F7F);
        assert_eq!(lighter(0x00FF_8C00), 0x00FF_C57F);
    }
}
