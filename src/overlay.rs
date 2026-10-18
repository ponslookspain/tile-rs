//! Overlay windows drawn by tile-rs itself: the focus border and the workspace indicator.
//!
//! The border is a click-through, tool-window, layered window. Drawing it ourselves makes it
//! independent of the DWM border colour, which DWM drops whenever a window moves and which some
//! windows (e.g. console windows such as Windows PowerShell) never show at all. The indicator is
//! the same kind of window, but it keeps the mouse over its dots so they can be hovered and
//! clicked (the colour-keyed background between the dots stays click-through).

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, FillRect,
    GetStockObject, InvalidateRect, RoundRect, SelectObject, HDC, HGDIOBJ, NULL_BRUSH, NULL_PEN,
    PAINTSTRUCT, PS_SOLID,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IsWindowVisible, KillTimer,
    LoadCursorW, RegisterClassW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    IDC_HAND, LWA_COLORKEY, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

/// `WM_MOUSELEAVE` (0x02A3), not re-exported by the windows crate.
const WM_MOUSELEAVE: u32 = 0x02A3;

static BORDER: AtomicIsize = AtomicIsize::new(0);
/// Whether the border window is currently shown (so `show_border` can skip a redundant
/// `ShowWindow` on every animation frame).
static BORDER_VISIBLE: AtomicBool = AtomicBool::new(false);
static BORDER_COLOR: AtomicU32 = AtomicU32::new(0x00FF_FFFF);
static BORDER_WIDTH: AtomicU32 = AtomicU32::new(2);
static BORDER_RADIUS: AtomicU32 = AtomicU32::new(10);
/// Colour that is made transparent (the centre of the border window).
const BORDER_KEY: u32 = 0x00FF_00FE;

/// Draws the frame as a *rounded* ring: an outer rounded rectangle in the border colour with the
/// centre punched back out with the colour key, so both the outer edge and the hole are rounded
/// (not a square ring with a rounded hole).
unsafe fn draw_frame(hdc: HDC, w: i32, h: i32) {
    let bw = BORDER_WIDTH.load(Ordering::Relaxed) as i32;
    let radius = BORDER_RADIUS.load(Ordering::Relaxed) as i32;
    let rc = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    // start from a fully transparent surface so the pixels outside the rounded outer edge (the
    // corners of the window rect) are not left as opaque leftovers
    let key = CreateSolidBrush(COLORREF(BORDER_KEY));
    FillRect(hdc, &rc, key);
    let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    // outer rounded rectangle in the border colour
    let fill = CreateSolidBrush(COLORREF(BORDER_COLOR.load(Ordering::Relaxed)));
    let old_brush = SelectObject(hdc, HGDIOBJ(fill.0));
    let _ = RoundRect(hdc, 0, 0, w, h, radius * 2, radius * 2);
    // punch the rounded centre out, leaving a rounded ring
    SelectObject(hdc, HGDIOBJ(key.0));
    let ir = (radius - bw).max(1);
    let _ = RoundRect(hdc, bw, bw, w - bw, h - bw, ir * 2, ir * 2);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(fill.0));
    let _ = DeleteObject(HGDIOBJ(key.0));
}

unsafe extern "system" fn border_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let hdc = HDC(wparam.0 as *mut _);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            draw_frame(hdc, rc.right, rc.bottom);
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            draw_frame(hdc, rc.right, rc.bottom);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
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
        // the colour key is transparent; WS_EX_TRANSPARENT makes the window click-through
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(BORDER_KEY), 0, LWA_COLORKEY);
        BORDER.store(hwnd.0 as isize, Ordering::Relaxed);
        true
    }
}

pub fn set_border_color(color: u32) {
    BORDER_COLOR.store(color, Ordering::Relaxed);
    let raw = BORDER.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            // the frame paints every pixel (centre is the colour key), so no background erase
            let _ = InvalidateRect(Some(HWND(raw as *mut _)), None, false);
        }
    }
}

/// Shows the border at `rect` (which already includes the border width), inserted just below
/// `window` in the z-order. Only a rounded frame is drawn (the centre is the transparent colour
/// key), so the window can never be covered by a solid colour while it is being placed or resized.
pub fn show_border(rect: RECT, window: HWND, radius: i32, width: i32) {
    let raw = BORDER.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        return;
    }
    BORDER_WIDTH.store(width.max(1) as u32, Ordering::Relaxed);
    BORDER_RADIUS.store(radius.max(1) as u32, Ordering::Relaxed);
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
        // the frame depends on the size, so repaint on every move; it paints every pixel, so no
        // background erase is needed
        let _ = InvalidateRect(Some(hwnd), None, false);
        // only show it if it is not already visible (this runs on every animation frame)
        if !BORDER_VISIBLE.swap(true, Ordering::Relaxed) {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
        }
    }
}

pub fn hide_border() {
    BORDER_VISIBLE.store(false, Ordering::Relaxed);
    let raw = BORDER.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = ShowWindow(HWND(raw as *mut _), SW_HIDE);
        }
    }
}

pub fn destroy_border() {
    BORDER_VISIBLE.store(false, Ordering::Relaxed);
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
const DOT_GAP: i32 = 5;
/// Thickness of the ring that marks the current workspace.
const RING: i32 = 2;
/// Transparent gap between the ring and the filled dot of the current workspace.
const ACTIVE_GAP: i32 = 1;
/// Padding above and below the dots.
const PAD: i32 = 6;
/// Colour key that is made transparent (the dots are drawn in other colours).
const IND_KEY: u32 = 0x00FF_00FF;
/// Hover animation: progress runs 0..=HOVER_MAX, HOVER_STEP per timer tick, and a hovered dot
/// grows by up to HOVER_GROW pixels.
const HOVER_MAX: u32 = 100;
const HOVER_STEP: u32 = 25;
const HOVER_GROW: i32 = 2;
/// Timer id of the hover animation on the indicator window.
const TIMER_HOVER: usize = 1;

static INDICATOR: AtomicIsize = AtomicIsize::new(0);
static IND_CUR: AtomicU32 = AtomicU32::new(0);
static IND_POP: AtomicU32 = AtomicU32::new(0);
static IND_ACTIVE: AtomicU32 = AtomicU32::new(0x00FF_BC8D);
/// Dot currently under the cursor (-1 = none).
static IND_HOVER: AtomicI32 = AtomicI32::new(-1);
/// Per-dot hover animation progress, 0..=HOVER_MAX.
static IND_ANIM: [AtomicU32; DOTS as usize] = [const { AtomicU32::new(0) }; DOTS as usize];
/// Callback run when the user clicks a workspace dot (0-based workspace index).
static IND_CLICK: OnceLock<fn(u8)> = OnceLock::new();
/// State the indicator was last shown with: an unchanged `show_indicator` (the common case on a
/// re-tile) then does nothing instead of repositioning and repainting. Visibility is not cached -
/// it is read from the window itself (the system may hide it behind our back).
static IND_RECT: [AtomicI32; 4] = [const { AtomicI32::new(0) }; 4];
static IND_S_CUR: AtomicU32 = AtomicU32::new(u32::MAX);
static IND_S_POP: AtomicU32 = AtomicU32::new(u32::MAX);
static IND_S_ACTIVE: AtomicU32 = AtomicU32::new(u32::MAX);

/// Registers the callback run when the user clicks a workspace dot.
pub fn set_indicator_click(f: fn(u8)) {
    let _ = IND_CLICK.set(f);
}

/// The dot index whose hit area (the dot plus half the gap on each side) contains `x`, or -1.
fn dot_at_x(x: i32, width: i32) -> i32 {
    let content = DOTS * DOT + (DOTS - 1) * DOT_GAP;
    let x0 = (width - content) / 2;
    let slot = DOT + DOT_GAP;
    for i in 0..DOTS {
        let cx = x0 + i * slot + DOT / 2;
        if (x - cx).abs() <= slot / 2 {
            return i;
        }
    }
    -1
}

/// Linear blend of two COLORREFs, `t` in 0..=HOVER_MAX.
fn blend(a: u32, b: u32, t: u32) -> u32 {
    let ch = |sh: u32| -> u32 {
        let ca = ((a >> sh) & 0xFF) as i32;
        let cb = ((b >> sh) & 0xFF) as i32;
        ((ca + (cb - ca) * t as i32 / HOVER_MAX as i32) as u32) & 0xFF
    };
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

/// Advances the hover animation by one step. Returns true while anything is still moving.
fn hover_tick(hwnd: HWND) -> bool {
    let hover = IND_HOVER.load(Ordering::Relaxed);
    let mut moving = false;
    for (i, a) in IND_ANIM.iter().enumerate() {
        let target = if hover == i as i32 { HOVER_MAX } else { 0 };
        let cur = a.load(Ordering::Relaxed);
        let next = if cur < target {
            (cur + HOVER_STEP).min(target)
        } else if cur > target {
            cur.saturating_sub(HOVER_STEP)
        } else {
            cur
        };
        if next != cur {
            a.store(next, Ordering::Relaxed);
            moving = true;
        }
    }
    if moving {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
    moving
}

/// Width and height of the indicator bar (the tiling area leaves this much room at the top).
pub fn indicator_size() -> (i32, i32) {
    let w = PAD * 2 + DOTS * DOT + (DOTS - 1) * DOT_GAP;
    let h = PAD * 2 + DOT;
    (w, h)
}

/// Height of the reserved bar at the top of the work area.
pub const INDICATOR_H: i32 = PAD * 2 + DOT;

unsafe extern "system" fn indicator_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // the paint handler fills the whole client rect with the colour key, so there is nothing
        // to erase here (this also avoids a redundant fill on every show)
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let bg = CreateSolidBrush(COLORREF(IND_KEY));
                FillRect(hdc, &rc, bg);
                let _ = DeleteObject(HGDIOBJ(bg.0));
                let cur = IND_CUR.load(Ordering::Relaxed) as i32;
                let pop = IND_POP.load(Ordering::Relaxed);
                let active = IND_ACTIVE.load(Ordering::Relaxed);
                // centre the row of dots in the (possibly wider) window
                let content = DOTS * DOT + (DOTS - 1) * DOT_GAP;
                let x0 = (rc.right - content) / 2;
                let cy = rc.bottom / 2;
                let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
                for (i, a) in IND_ANIM.iter().enumerate() {
                    let i = i as i32;
                    let grow = (a.load(Ordering::Relaxed) as i32 * HOVER_GROW) / HOVER_MAX as i32;
                    let r = DOT / 2 + grow;
                    let cx = x0 + i * (DOT + DOT_GAP) + DOT / 2;
                    if i == cur {
                        // the current workspace: a filled dot ...
                        let brush = CreateSolidBrush(COLORREF(active));
                        let old_b = SelectObject(hdc, HGDIOBJ(brush.0));
                        let _ = Ellipse(hdc, cx - r, cy - r, cx + r, cy + r);
                        SelectObject(hdc, old_b);
                        let _ = DeleteObject(HGDIOBJ(brush.0));
                        // ... with a ring around it, so it reads as "selected"
                        let rr = DOT / 2 + ACTIVE_GAP + RING + grow;
                        let pen = CreatePen(PS_SOLID, RING, COLORREF(active));
                        let old_p = SelectObject(hdc, HGDIOBJ(pen.0));
                        let old_br = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                        let _ = Ellipse(hdc, cx - rr, cy - rr, cx + rr, cy + rr);
                        SelectObject(hdc, old_br);
                        SelectObject(hdc, old_p);
                        let _ = DeleteObject(HGDIOBJ(pen.0));
                    } else {
                        // a workspace with windows is filled, an empty one is dimmer; hovering
                        // brightens the dot and grows it a little
                        let (base, hover) = if pop & (1 << i) != 0 {
                            (0x00A0_A0A0, 0x00E8_E8E8)
                        } else {
                            (0x0055_5555, 0x00A0_A0A0)
                        };
                        let color = blend(base, hover, a.load(Ordering::Relaxed));
                        let brush = CreateSolidBrush(COLORREF(color));
                        let old_b = SelectObject(hdc, HGDIOBJ(brush.0));
                        let _ = Ellipse(hdc, cx - r, cy - r, cx + r, cy + r);
                        SelectObject(hdc, old_b);
                        let _ = DeleteObject(HGDIOBJ(brush.0));
                    }
                }
                SelectObject(hdc, old_pen);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let idx = dot_at_x(x, rc.right);
            if idx != IND_HOVER.load(Ordering::Relaxed) {
                IND_HOVER.store(idx, Ordering::Relaxed);
                unsafe {
                    SetTimer(Some(hwnd), TIMER_HOVER, 15, None);
                }
            }
            // ask to be told when the cursor leaves the dots, so the hover can fade out
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = unsafe { TrackMouseEvent(&mut tme) };
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if IND_HOVER.load(Ordering::Relaxed) != -1 {
                IND_HOVER.store(-1, Ordering::Relaxed);
                unsafe {
                    SetTimer(Some(hwnd), TIMER_HOVER, 15, None);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let idx = dot_at_x(x, rc.right);
            if idx >= 0 {
                if let Some(f) = IND_CLICK.get() {
                    f(idx as u8);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HOVER => {
            if !hover_tick(hwnd) {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_HOVER);
                }
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
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let (w, h) = indicator_size();
        // no WS_EX_TRANSPARENT: the dots must receive the mouse; with LWA_COLORKEY the keyed
        // background between them is still click-through, so only the dots catch the cursor
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("tile-rs-indicator"),
            WS_POPUP,
            0,
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

/// Shows the indicator at `rect`: which workspace is current and which ones have windows.
pub fn show_indicator(rect: RECT, cur: u8, populated: u32, active: u32) {
    IND_CUR.store(cur as u32, Ordering::Relaxed);
    IND_POP.store(populated, Ordering::Relaxed);
    IND_ACTIVE.store(active, Ordering::Relaxed);
    let raw = INDICATOR.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut _);
    let r = [rect.left, rect.top, rect.right, rect.bottom];
    let unchanged = unsafe { IsWindowVisible(hwnd).as_bool() }
        && IND_S_CUR.load(Ordering::Relaxed) == cur as u32
        && IND_S_POP.load(Ordering::Relaxed) == populated
        && IND_S_ACTIVE.load(Ordering::Relaxed) == active
        && IND_RECT
            .iter()
            .zip(r)
            .all(|(a, v)| a.load(Ordering::Relaxed) == v);
    if unchanged {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    IND_S_CUR.store(cur as u32, Ordering::Relaxed);
    IND_S_POP.store(populated, Ordering::Relaxed);
    IND_S_ACTIVE.store(active, Ordering::Relaxed);
    for (a, v) in IND_RECT.iter().zip(r) {
        a.store(v, Ordering::Relaxed);
    }
}

pub fn hide_indicator() {
    let raw = INDICATOR.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let hwnd = HWND(raw as *mut _);
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        reset_hover();
    }
}

/// Clears the hover state and its animation (the indicator is hidden or gone).
fn reset_hover() {
    IND_HOVER.store(-1, Ordering::Relaxed);
    for a in IND_ANIM.iter() {
        a.store(0, Ordering::Relaxed);
    }
}

pub fn destroy_indicator() {
    let raw = INDICATOR.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let hwnd = HWND(raw as *mut _);
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            let _ = DestroyWindow(hwnd);
        }
    }
    reset_hover();
}
