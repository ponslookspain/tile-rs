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

use crate::config::IndicatorSize;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, FillRect,
    GetStockObject, InvalidateRect, RoundRect, SelectObject, HDC, HGDIOBJ, NULL_PEN, PAINTSTRUCT,
    PS_SOLID,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, IDC_HAND, LWA_COLORKEY,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
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
static INDICATOR_SCALE: AtomicU32 = AtomicU32::new(1);
/// Colour key that is made transparent (the dots are drawn in other colours).
const IND_KEY: u32 = 0x00FF_00FF;
/// Hover animation: progress runs 0..=HOVER_MAX, HOVER_STEP per timer tick, and a hovered dot
/// grows by up to HOVER_GROW pixels.
const HOVER_MAX: u32 = 100;
const HOVER_STEP: u32 = 25;
const HOVER_GROW: i32 = 2;
/// Timer id of the hover animation on the indicator window.
const TIMER_HOVER: usize = 1;

fn indicator_metrics() -> (i32, i32, i32) {
    match INDICATOR_SCALE.load(Ordering::Relaxed) {
        // Visual diameter, breathing room between dots, and outer click-area padding.
        // The 24/28/32 px pitch keeps the targets easy to hit while the dots stay discreet.
        0 => (6, 18, 10),
        2 => (10, 22, 14),
        _ => (8, 20, 12),
    }
}

/// One indicator per monitor; the window index is the monitor slot.
const MAX_MON: usize = 9;

/// All state of one monitor's indicator window (all fields are behind atomics so the window proc
/// can read them without a lock).
struct Ind {
    hwnd: AtomicIsize,
    cur: AtomicU32,
    pop: AtomicU32,
    /// Dot currently under the cursor (-1 = none).
    hover: AtomicI32,
    /// Per-dot hover animation progress, 0..=HOVER_MAX.
    anim: [AtomicU32; DOTS as usize],
    /// State the indicator was last shown with: an unchanged `show_indicator` (the common case on a
    /// re-tile) then does nothing instead of repositioning and repainting. Visibility is not cached -
    /// it is read from the window itself (the system may hide it behind our back).
    rect: [AtomicI32; 4],
    s_cur: AtomicU32,
    s_pop: AtomicU32,
}

impl Ind {
    const fn new() -> Self {
        Ind {
            hwnd: AtomicIsize::new(0),
            cur: AtomicU32::new(0),
            pop: AtomicU32::new(0),
            hover: AtomicI32::new(-1),
            anim: [const { AtomicU32::new(0) }; DOTS as usize],
            rect: [const { AtomicI32::new(0) }; 4],
            s_cur: AtomicU32::new(u32::MAX),
            s_pop: AtomicU32::new(u32::MAX),
        }
    }
}

static INDS: [Ind; MAX_MON] = [const { Ind::new() }; MAX_MON];
/// Callback run when the user clicks a workspace dot: (monitor index, 0-based workspace index).
static IND_CLICK: OnceLock<fn(usize, u8)> = OnceLock::new();

/// Registers the callback run when the user clicks a workspace dot.
pub fn set_indicator_click(f: fn(usize, u8)) {
    let _ = IND_CLICK.set(f);
}

/// The dot index whose hit area (the dot plus half the gap on each side) contains `x`, or -1.
fn dot_at_x(x: i32, width: i32) -> i32 {
    let (dot, gap, _) = indicator_metrics();
    let content = DOTS * dot + (DOTS - 1) * gap;
    let x0 = (width - content) / 2;
    let slot = dot + gap;
    for i in 0..DOTS {
        let cx = x0 + i * slot + dot / 2;
        if (x - cx).abs() <= slot / 2 {
            return i;
        }
    }
    -1
}

/// Advances the hover animation by one step. Returns true while anything is still moving.
fn hover_tick(hwnd: HWND, ind: &Ind) -> bool {
    let hover = ind.hover.load(Ordering::Relaxed);
    let mut moving = false;
    for (i, a) in ind.anim.iter().enumerate() {
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

/// Paints one opaque circle. The indicator's colour-key background keeps everything around it
/// transparent, so concentric circles can create a simple glow and outline without layered alpha.
unsafe fn draw_dot(hdc: HDC, cx: i32, cy: i32, diameter: i32, color: u32) {
    let left = cx - diameter / 2;
    let top = cy - diameter / 2;
    let brush = CreateSolidBrush(COLORREF(color));
    let pen = CreatePen(PS_SOLID, 1, COLORREF(color));
    let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
    let _ = Ellipse(hdc, left, top, left + diameter, top + diameter);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

/// Width and height of the indicator bar (the tiling area leaves this much room at the top).
pub fn indicator_size() -> (i32, i32) {
    let (dot, gap, pad) = indicator_metrics();
    let w = pad * 2 + DOTS * dot + (DOTS - 1) * gap;
    let h = pad * 2 + dot;
    (w, h)
}

/// Height of the reserved bar at the top of the work area.
pub fn indicator_height() -> i32 {
    indicator_size().1
}

/// Changes the indicator scale and resizes the existing overlay windows immediately.
pub fn set_indicator_size(size: IndicatorSize) {
    let scale = match size {
        IndicatorSize::Small => 0,
        IndicatorSize::Medium => 1,
        IndicatorSize::Large => 2,
    };
    INDICATOR_SCALE.store(scale, Ordering::Relaxed);
    let (w, h) = indicator_size();
    for ind in &INDS {
        let raw = ind.hwnd.load(Ordering::Relaxed);
        if raw != 0 {
            unsafe {
                let _ = SetWindowPos(
                    HWND(raw as *mut _),
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOACTIVATE
                        | SWP_NOZORDER
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
                );
                let _ = InvalidateRect(Some(HWND(raw as *mut _)), None, false);
            }
        }
    }
}

unsafe extern "system" fn indicator_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let mi = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as usize;
    if mi >= MAX_MON || INDS[mi].hwnd.load(Ordering::Relaxed) != hwnd.0 as isize {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let ind = &INDS[mi];
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
                let cur = ind.cur.load(Ordering::Relaxed) as i32;
                let pop = ind.pop.load(Ordering::Relaxed);
                let (dot, gap, _) = indicator_metrics();
                let content = DOTS * dot + (DOTS - 1) * gap;
                let x0 = (rc.right - content) / 2;
                let cy = rc.bottom / 2;
                for (i, a) in ind.anim.iter().enumerate() {
                    let i = i as i32;
                    let grow = (a.load(Ordering::Relaxed) as i32 * HOVER_GROW) / HOVER_MAX as i32;
                    let cx = x0 + i * (dot + gap) + dot / 2;
                    let hovered = ind.hover.load(Ordering::Relaxed) == i;
                    let selected = i == cur;
                    let occupied = pop & (1 << i) != 0;
                    let size = dot + grow * 2;
                    if selected {
                        // A bright blue outline distinguishes the current desktop even when it
                        // has no windows; the blue center also keeps its state obvious at a glance.
                        draw_dot(hdc, cx, cy, size + 8, 0x00623E17);
                        draw_dot(hdc, cx, cy, size + 4, 0x00FFD69A);
                        draw_dot(hdc, cx, cy, size, 0x00FF8F25);
                    } else if occupied {
                        // Occupied desktops glow blue but remain distinct from the selected one.
                        draw_dot(hdc, cx, cy, size + 6, 0x00502F16);
                        draw_dot(hdc, cx, cy, size, 0x00E27E28);
                    } else if hovered {
                        draw_dot(hdc, cx, cy, size + 4, 0x005E5540);
                        draw_dot(hdc, cx, cy, size, 0x00484239);
                    } else {
                        draw_dot(hdc, cx, cy, size, 0x00554B42);
                    }
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let idx = dot_at_x(x, rc.right);
            if idx != ind.hover.load(Ordering::Relaxed) {
                ind.hover.store(idx, Ordering::Relaxed);
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
            if ind.hover.load(Ordering::Relaxed) != -1 {
                ind.hover.store(-1, Ordering::Relaxed);
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
                    f(mi, idx as u8);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HOVER => {
            if !hover_tick(hwnd, ind) {
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
pub fn create_indicator(count: usize) -> bool {
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
        let mut created = true;
        for (mi, ind) in INDS.iter().enumerate().take(count.min(MAX_MON)) {
            // The colour-key background stays click-through while the dots receive the mouse.
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
                created = false;
                continue;
            };
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, mi as isize);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(IND_KEY), 0, LWA_COLORKEY);
            ind.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
        }
        created
    }
}

/// Adds or removes indicator windows to match the current monitor count.
pub fn sync_indicator_count(count: usize) -> bool {
    let count = count.min(MAX_MON);
    let mut ok = true;
    for (mi, ind) in INDS.iter().enumerate() {
        if mi < count && ind.hwnd.load(Ordering::Relaxed) == 0 {
            let hmod = unsafe { GetModuleHandleW(None).unwrap_or_default() };
            let hinst = HINSTANCE(hmod.0);
            let (w, h) = indicator_size();
            let created = unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("tile-rs-indicator"),
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
                )
            };
            match created {
                Ok(hwnd) => {
                    unsafe {
                        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, mi as isize);
                        let _ =
                            SetLayeredWindowAttributes(hwnd, COLORREF(IND_KEY), 0, LWA_COLORKEY);
                    }
                    ind.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
                    ind.s_cur.store(u32::MAX, Ordering::Relaxed);
                }
                Err(_) => ok = false,
            }
        } else if mi >= count {
            let raw = ind.hwnd.swap(0, Ordering::Relaxed);
            if raw != 0 {
                unsafe {
                    let hwnd = HWND(raw as *mut _);
                    let _ = KillTimer(Some(hwnd), TIMER_HOVER);
                    let _ = DestroyWindow(hwnd);
                }
                reset_hover(ind);
            }
        }
    }
    ok
}

/// Shows the indicator at `rect`: which workspace is current and which ones have windows.
pub fn show_indicator(mi: usize, rect: RECT, cur: u8, populated: u32) {
    let Some(ind) = INDS.get(mi).filter(|_| mi < MAX_MON) else {
        return;
    };
    ind.cur.store(cur as u32, Ordering::Relaxed);
    ind.pop.store(populated, Ordering::Relaxed);
    let raw = ind.hwnd.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut _);
    let r = [rect.left, rect.top, rect.right, rect.bottom];
    let unchanged = unsafe { IsWindowVisible(hwnd).as_bool() }
        && ind.s_cur.load(Ordering::Relaxed) == cur as u32
        && ind.s_pop.load(Ordering::Relaxed) == populated
        && ind
            .rect
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
    ind.s_cur.store(cur as u32, Ordering::Relaxed);
    ind.s_pop.store(populated, Ordering::Relaxed);
    for (a, v) in ind.rect.iter().zip(r) {
        a.store(v, Ordering::Relaxed);
    }
}

pub fn hide_indicator() {
    for ind in &INDS {
        let raw = ind.hwnd.load(Ordering::Relaxed);
        if raw == 0 {
            continue;
        }
        unsafe {
            let hwnd = HWND(raw as *mut _);
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        reset_hover(ind);
    }
}

/// Clears the hover state and its animation (the indicator is hidden or gone).
fn reset_hover(ind: &Ind) {
    ind.hover.store(-1, Ordering::Relaxed);
    for a in ind.anim.iter() {
        a.store(0, Ordering::Relaxed);
    }
}

pub fn destroy_indicator() {
    for ind in &INDS {
        let raw = ind.hwnd.swap(0, Ordering::Relaxed);
        if raw == 0 {
            continue;
        }
        unsafe {
            let hwnd = HWND(raw as *mut _);
            let _ = KillTimer(Some(hwnd), TIMER_HOVER);
            let _ = DestroyWindow(hwnd);
        }
        reset_hover(ind);
    }
}
