//! Overlay windows drawn by tile-rs itself: the focus border (and, later, the workspace
//! indicator).
//!
//! They are click-through, tool-window, layered windows. Drawing the border ourselves makes it
//! independent of the DWM border colour, which DWM drops whenever a window moves and which some
//! windows (e.g. console windows such as Windows PowerShell) never show at all.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, FillRect,
    GetStockObject, InvalidateRect, SelectObject, SetWindowRgn, HDC, HGDIOBJ, NULL_PEN,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetSystemMetrics,
    RegisterClassW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, LWA_ALPHA, LWA_COLORKEY,
    SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_PAINT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

static BORDER: AtomicIsize = AtomicIsize::new(0);
static BORDER_COLOR: AtomicU32 = AtomicU32::new(0x00FF_FFFF);
/// Size (width << 16 | height) the rounded region was last built for.
static BORDER_SIZE: AtomicU32 = AtomicU32::new(u32::MAX);

unsafe extern "system" fn border_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        let hdc = HDC(wparam.0 as *mut _);
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let brush = unsafe { CreateSolidBrush(COLORREF(BORDER_COLOR.load(Ordering::Relaxed))) };
        unsafe {
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
        return LRESULT(1);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Creates the (hidden) border window. Returns false if it could not be created.
pub fn create_border() -> bool {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let hinst = HINSTANCE(hmod.0);
        let class = w!("tile-rs-border");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(border_wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("tile-rs-border"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return false;
        };
        // fully opaque, but click-through (WS_EX_TRANSPARENT)
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        BORDER.store(hwnd.0 as isize, Ordering::Relaxed);
        true
    }
}

pub fn set_border_color(color: u32) {
    BORDER_COLOR.store(color, Ordering::Relaxed);
    let raw = BORDER.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = InvalidateRect(Some(HWND(raw as *mut _)), None, true);
        }
    }
}

/// Shows the border at `rect` (which already includes the border width), inserted just below
/// `window` in the z-order so the window itself covers the centre of the border. `radius` is the
/// corner radius in pixels (rounded corners, like the window itself).
pub fn show_border(rect: RECT, window: HWND, radius: i32) {
    let raw = BORDER.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    unsafe {
        let hwnd = HWND(raw as *mut _);
        let _ = SetWindowPos(
            hwnd,
            Some(window),
            rect.left,
            rect.top,
            w,
            h,
            SWP_NOACTIVATE,
        );
        // rounded corners: (re)create the window region only when the size changed
        let key = ((w as u32) << 16) | (h as u32 & 0xFFFF);
        if BORDER_SIZE.swap(key, Ordering::Relaxed) != key {
            let d = (radius * 2).max(0);
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, d, d);
            let _ = SetWindowRgn(hwnd, Some(rgn), true);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
}

pub fn hide_border() {
    let raw = BORDER.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = ShowWindow(HWND(raw as *mut _), SW_HIDE);
        }
    }
}

pub fn destroy_border() {
    let raw = BORDER.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(raw as *mut _));
        }
    }
}

// ---------------------------------------------------------------------------
// workspace indicator: a small row of dots at the top centre
// ---------------------------------------------------------------------------

const DOTS: i32 = 9;
const DOT: i32 = 6;
const DOT_GAP: i32 = 4;
/// Padding above and below the dots.
const PAD: i32 = 6;
/// Colour key that is made transparent (the dots are drawn in other colours).
const IND_KEY: u32 = 0x00FF_00FF;

static INDICATOR: AtomicIsize = AtomicIsize::new(0);
static IND_CUR: AtomicU32 = AtomicU32::new(0);
static IND_POP: AtomicU32 = AtomicU32::new(0);
static IND_ACTIVE: AtomicU32 = AtomicU32::new(0x00FF_BC8D);

fn indicator_size() -> (i32, i32) {
    let w = PAD * 2 + DOTS * DOT + (DOTS - 1) * DOT_GAP;
    let h = PAD * 2 + DOT;
    (w, h)
}

fn indicator_x() -> i32 {
    let (w, _) = indicator_size();
    (unsafe { GetSystemMetrics(SM_CXSCREEN) } - w) / 2
}

unsafe extern "system" fn indicator_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut _);
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let brush = unsafe { CreateSolidBrush(COLORREF(IND_KEY)) };
            unsafe {
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
            LRESULT(1)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let bg = CreateSolidBrush(COLORREF(IND_KEY));
                FillRect(hdc, &rc, bg);
                let _ = DeleteObject(HGDIOBJ(bg.0));
                let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
                let cur = IND_CUR.load(Ordering::Relaxed) as i32;
                let pop = IND_POP.load(Ordering::Relaxed);
                let active = IND_ACTIVE.load(Ordering::Relaxed);
                let cy = rc.bottom / 2;
                for i in 0..DOTS {
                    let color = if i == cur {
                        active
                    } else if pop & (1 << i) != 0 {
                        0x00A0_A0A0
                    } else {
                        0x0055_5555
                    };
                    let x = PAD + i * (DOT + DOT_GAP);
                    let brush = CreateSolidBrush(COLORREF(color));
                    let old_b = SelectObject(hdc, HGDIOBJ(brush.0));
                    let _ = Ellipse(hdc, x, cy - DOT / 2, x + DOT, cy + DOT / 2);
                    SelectObject(hdc, old_b);
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                }
                SelectObject(hdc, old_pen);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the (hidden) workspace indicator. Returns false if it could not be created.
pub fn create_indicator() -> bool {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let hinst = HINSTANCE(hmod.0);
        let class = w!("tile-rs-indicator");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(indicator_wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let (w, h) = indicator_size();
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("tile-rs-indicator"),
            WS_POPUP,
            indicator_x(),
            0,
            w,
            h,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return false;
        };
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(IND_KEY), 0, LWA_COLORKEY);
        INDICATOR.store(hwnd.0 as isize, Ordering::Relaxed);
        true
    }
}

/// Updates the indicator: which workspace is current and which ones have windows.
pub fn update_indicator(cur: u8, populated: u32, active: u32) {
    IND_CUR.store(cur as u32, Ordering::Relaxed);
    IND_POP.store(populated, Ordering::Relaxed);
    IND_ACTIVE.store(active, Ordering::Relaxed);
    let raw = INDICATOR.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let (w, h) = indicator_size();
    unsafe {
        let hwnd = HWND(raw as *mut _);
        let _ = SetWindowPos(
            hwnd,
            None,
            indicator_x(),
            0,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
}

pub fn hide_indicator() {
    let raw = INDICATOR.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = ShowWindow(HWND(raw as *mut _), SW_HIDE);
        }
    }
}

pub fn destroy_indicator() {
    let raw = INDICATOR.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(raw as *mut _));
        }
    }
}
