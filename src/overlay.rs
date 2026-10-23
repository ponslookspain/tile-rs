//! Overlay windows drawn by tile-rs itself: the focus border and the workspace indicator.
//!
//! The border is a click-through, tool-window, layered window. Drawing it ourselves makes it
//! independent of the DWM border colour, which DWM drops whenever a window moves and which some
//! windows (e.g. console windows such as Windows PowerShell) never show at all. The indicator is
//! a per-pixel-alpha layered window that keeps the mouse, so its dots can be hovered and clicked.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::config::IndicatorSize;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetStockObject, InvalidateRect,
    RoundRect, SelectObject, HDC, HGDIOBJ, NULL_PEN, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    IsWindowVisible, KillTimer, LoadCursorW, RegisterClassW, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, IDC_HAND, LWA_COLORKEY,
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONUP,
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
static BORDER_PAINT_WIDTH: AtomicI32 = AtomicI32::new(0);
static BORDER_PAINT_HEIGHT: AtomicI32 = AtomicI32::new(0);
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
    let drawing_changed = BORDER_WIDTH.swap(width.max(1) as u32, Ordering::Relaxed)
        != width.max(1) as u32
        || BORDER_RADIUS.load(Ordering::Relaxed) != radius.max(1) as u32
        || BORDER_PAINT_WIDTH.load(Ordering::Relaxed) != w
        || BORDER_PAINT_HEIGHT.load(Ordering::Relaxed) != h;
    BORDER_RADIUS.store(radius.max(1) as u32, Ordering::Relaxed);
    BORDER_PAINT_WIDTH.store(w, Ordering::Relaxed);
    BORDER_PAINT_HEIGHT.store(h, Ordering::Relaxed);
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
        // Pure translations reuse the existing layered bitmap.
        if drawing_changed {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
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

#[cfg(test)]
pub(crate) fn border_rect() -> Option<RECT> {
    let hwnd = HWND(BORDER.load(Ordering::Relaxed) as *mut _);
    let mut rect = RECT::default();
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect).ok()?;
    }
    Some(rect)
}

pub fn destroy_border() {
    BORDER_PAINT_WIDTH.store(0, Ordering::Relaxed);
    BORDER_PAINT_HEIGHT.store(0, Ordering::Relaxed);
    BORDER_VISIBLE.store(false, Ordering::Relaxed);
    let raw = BORDER.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(raw as *mut _));
        }
    }
}

// ---------------------------------------------------------------------------
// workspace indicator: a slim row of dots at the top centre
// ---------------------------------------------------------------------------
//
// The dots are rendered in software into a 32-bit premultiplied-alpha bitmap and handed to
// `UpdateLayeredWindow`, so their edges are anti-aliased (a colour key can only cut hard,
// jagged edges). Every background pixel keeps an alpha of 1/255: invisible, but still hit by the
// mouse, so the whole row (and the outer gap under it) is a click target while only a slim bar is
// reserved from the tiling area.

use windows::Win32::Foundation::{POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, GetDC, ReleaseDC, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

const DOTS: i32 = 9;
static INDICATOR_SCALE: AtomicU32 = AtomicU32::new(1);
/// Accent colour of the indicator as a COLORREF (0x00BBGGRR).
static INDICATOR_ACCENT: AtomicU32 = AtomicU32::new(0x00FF_8C00);
/// Hover animation: progress runs 0..=HOVER_MAX, HOVER_STEP per timer tick.
const HOVER_MAX: u32 = 100;
const HOVER_STEP: u32 = 20;
/// Timer id of the hover animation on the indicator window.
const TIMER_HOVER: usize = 1;

/// Indicator geometry. All lengths below are in 96-DPI pixels and are multiplied by `Metrics::k`.
///
/// The indicator is a white pill (28 high, 8 padding, 8 gap) holding one 24x12 capsule for the
/// current workspace and 12x12 dots for the others, with soft shadows beneath. `TOP`, `SIDE` and
/// `BOTTOM` are transparent margins of the window: `TOP` is kept free of tiles together with the
/// pill; the rest only gives the shadow room.
const PILL_H: f32 = 28.0;
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
const DOT: f32 = 12.0;
const ACTIVE_W: f32 = 24.0;
const TOP: f32 = 8.0;
const SIDE: f32 = 12.0;
const BOTTOM: f32 = 8.0;
/// Width of the pill: every dot plus one wider (active) capsule.
const PILL_W: f32 = 2.0 * PAD + (DOTS as f32 - 1.0) * DOT + ACTIVE_W + (DOTS as f32 - 1.0) * GAP;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// Device pixels per design pixel: monitor DPI times the configured indicator size.
    k: f32,
}

fn metrics(dpi: u32) -> Metrics {
    let size = match INDICATOR_SCALE.load(Ordering::Relaxed) {
        0 => 0.8,
        2 => 1.25,
        _ => 1.0,
    };
    Metrics {
        k: dpi.max(96) as f32 / 96.0 * size,
    }
}

impl Metrics {
    fn px(&self, v: f32) -> f32 {
        v * self.k
    }
}

/// Height reserved for the indicator at the top of a monitor's work area (pill plus its top margin).
pub fn indicator_bar_height(dpi: u32) -> i32 {
    metrics(dpi).px(TOP + PILL_H).round() as i32
}

/// Size of the indicator window: the pill plus transparent margins for its shadow.
pub fn indicator_window_size(dpi: u32) -> (i32, i32) {
    let m = metrics(dpi);
    (
        m.px(PILL_W + 2.0 * SIDE).round() as i32,
        m.px(TOP + PILL_H + BOTTOM).round() as i32,
    )
}

/// Left edge and width (design pixels, window coordinates) of dot `i` when workspace `cur` is the
/// current one (drawn wider).
fn dot_span(i: i32, cur: i32) -> (f32, f32) {
    let wide = if cur >= 0 && cur < i {
        ACTIVE_W - DOT
    } else {
        0.0
    };
    let width = if i == cur { ACTIVE_W } else { DOT };
    (SIDE + PAD + i as f32 * (DOT + GAP) + wide, width)
}

/// One indicator per monitor; the window index is the monitor slot.
const MAX_MON: usize = 9;

/// All state of one monitor's indicator window (all fields are behind atomics so the window proc
/// can read them without a lock).
struct Ind {
    hwnd: AtomicIsize,
    cur: AtomicU32,
    pop: AtomicU32,
    dpi: AtomicU32,
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
            dpi: AtomicU32::new(96),
            hover: AtomicI32::new(-1),
            anim: [const { AtomicU32::new(0) }; DOTS as usize],
            rect: [const { AtomicI32::new(0) }; 4],
            s_cur: AtomicU32::new(u32::MAX),
            s_pop: AtomicU32::new(u32::MAX),
        }
    }

    fn size(&self) -> (i32, i32) {
        let r: Vec<i32> = self
            .rect
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        (r[2] - r[0], r[3] - r[1])
    }
}

static INDS: [Ind; MAX_MON] = [const { Ind::new() }; MAX_MON];
/// Callback run when the user clicks a workspace dot: (monitor index, 0-based workspace index).
static IND_CLICK: OnceLock<fn(usize, u8)> = OnceLock::new();

/// Registers the callback run when the user clicks a workspace dot.
pub fn set_indicator_click(f: fn(usize, u8)) {
    let _ = IND_CLICK.set(f);
}

/// Callback run when the user right-clicks a workspace dot: (monitor index, 0-based workspace).
static IND_MENU: OnceLock<fn(usize, u8)> = OnceLock::new();

/// Registers the callback run when the user right-clicks a workspace dot.
pub fn set_indicator_menu(f: fn(usize, u8)) {
    let _ = IND_MENU.set(f);
}

/// While a window is dragged the dots are kept above every window, so the dragged window cannot
/// cover the place it is about to be dropped on. They go back to normal afterwards: always on top
/// they would float over fullscreen videos and games.
pub fn raise_indicators(raise: bool) {
    let after = if raise { HWND_TOPMOST } else { HWND_NOTOPMOST };
    for ind in &INDS {
        let raw = ind.hwnd.load(Ordering::Relaxed);
        if raw == 0 {
            continue;
        }
        unsafe {
            let _ = SetWindowPos(
                HWND(raw as *mut _),
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// The workspace dot under screen point (`x`, `y`), as (monitor index, 0-based workspace).
/// Used while a window is dragged: the indicator gets no mouse messages then. The drop zone
/// reaches one indicator height below the dots, so a dragged window is easy to aim.
pub fn dot_at_screen(x: i32, y: i32) -> Option<(usize, u8)> {
    INDS.iter().enumerate().find_map(|(mi, ind)| {
        let raw = ind.hwnd.load(Ordering::Relaxed);
        if raw == 0 || !unsafe { IsWindowVisible(HWND(raw as *mut _)) }.as_bool() {
            return None;
        }
        let r: Vec<i32> = ind.rect.iter().map(|a| a.load(Ordering::Relaxed)).collect();
        let below = r[3] - r[1];
        if x < r[0] || x >= r[2] || y < r[1] || y >= r[3] + below {
            return None;
        }
        let idx = dot_at_x(
            x - r[0],
            ind.dpi.load(Ordering::Relaxed),
            ind.cur.load(Ordering::Relaxed) as i32,
        );
        (idx >= 0).then_some((mi, idx as u8))
    })
}

/// Highlights the dot a dragged window would be dropped onto (`None`: no dot), the same way a
/// hovered dot is highlighted.
pub fn set_drop_target(target: Option<(usize, u8)>) {
    for (mi, ind) in INDS.iter().enumerate() {
        let raw = ind.hwnd.load(Ordering::Relaxed);
        if raw == 0 {
            continue;
        }
        let want = match target {
            Some((m, n)) if m == mi => n as i32,
            _ => -1,
        };
        if ind.hover.load(Ordering::Relaxed) != want {
            ind.hover.store(want, Ordering::Relaxed);
            unsafe {
                SetTimer(Some(HWND(raw as *mut _)), TIMER_HOVER, 15, None);
            }
        }
    }
}

/// The dot whose click target (the dot plus half a gap on each side, and the pill's padding at the
/// ends) contains window x `x`, or -1.
fn dot_at_x(x: i32, dpi: u32, cur: i32) -> i32 {
    let u = x as f32 / metrics(dpi).k;
    for i in 0..DOTS {
        let (left, width) = dot_span(i, cur);
        let from = if i == 0 { SIDE } else { left - GAP / 2.0 };
        let to = if i == DOTS - 1 {
            SIDE + PILL_W
        } else {
            left + width + GAP / 2.0
        };
        if u >= from && u < to {
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
        } else {
            cur.saturating_sub(HOVER_STEP).max(target)
        };
        if next != cur {
            a.store(next, Ordering::Relaxed);
            moving = true;
        }
    }
    if moving {
        render_indicator(hwnd, ind);
    }
    moving
}

/// A premultiplied RGBA pixel in 0..=1.
#[derive(Clone, Copy, Default)]
struct Px([f32; 4]);

impl Px {
    /// Source-over compositing of `rgb` at `alpha` onto this pixel.
    fn over(&mut self, rgb: (u8, u8, u8), alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let a = alpha.min(1.0);
        let keep = 1.0 - a;
        let c = [rgb.0, rgb.1, rgb.2].map(|v| v as f32 / 255.0 * a);
        for (i, v) in c.iter().enumerate() {
            self.0[i] = v + self.0[i] * keep;
        }
        self.0[3] = a + self.0[3] * keep;
    }
}

/// Signed distance from (x, y) to a horizontal capsule centred at (cx, cy): `half` is the half
/// length of its straight part (0 = a circle) and `r` its radius. Negative inside.
fn capsule_sdf(x: f32, y: f32, cx: f32, cy: f32, half: f32, r: f32) -> f32 {
    let qx = ((x - cx).abs() - half).max(0.0);
    let qy = y - cy;
    (qx * qx + qy * qy).sqrt() - r
}

/// Anti-aliased coverage of the area `d <= edge`.
fn fill_cov(d: f32, edge: f32) -> f32 {
    (edge - d + 0.5).clamp(0.0, 1.0)
}

/// Anti-aliased coverage of the band `inner < d <= outer`.
fn band_cov(d: f32, inner: f32, outer: f32) -> f32 {
    (fill_cov(d, outer) - fill_cov(d, inner)).max(0.0)
}

const SHADOW: (u8, u8, u8) = (0x10, 0x1A, 0x2B);

/// `smoothstep`-shaped shadow coverage: 1 deep inside the shape, 0 beyond `blur` outside it.
fn shadow_cov(d: f32, blur: f32) -> f32 {
    let t = ((d + blur / 2.0) / blur).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// Renders the whole indicator as premultiplied BGRA rows (top-down). Background pixels are fully
/// transparent, so only the pill and its shadow take mouse input.
fn paint_dots(
    w: i32,
    h: i32,
    m: Metrics,
    cur: i32,
    pop: u32,
    hover: &[f32],
    accent: (u8, u8, u8),
) -> Vec<u32> {
    let mut px = vec![Px([0.0; 4]); (w.max(0) * h.max(0)) as usize];
    let cy = m.px(TOP + PILL_H / 2.0);
    let pill_cx = w as f32 / 2.0;
    let half = m.px((PILL_W - PILL_H) / 2.0);
    let r = m.px(PILL_H / 2.0);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let p = &mut px[(y * w + x) as usize];
            // soft drop shadows (wide and faint, then tight and darker), then the white pill
            let wide = capsule_sdf(fx, fy - m.px(4.0), pill_cx, cy, half, r);
            p.over(SHADOW, 0.16 * shadow_cov(wide, m.px(12.0)));
            let tight = capsule_sdf(fx, fy - m.px(1.0), pill_cx, cy, half, r);
            p.over(SHADOW, 0.10 * shadow_cov(tight, m.px(4.0)));
            let d = capsule_sdf(fx, fy, pill_cx, cy, half, r);
            p.over((255, 255, 255), fill_cov(d, 0.0));
        }
    }
    for i in 0..DOTS {
        let (left, width) = dot_span(i, cur);
        let cx = m.px(left + width / 2.0);
        let t = hover.get(i as usize).copied().unwrap_or(0.0);
        let selected = i == cur;
        let occupied = pop & (1 << i) != 0;
        let grow = if selected { 0.0 } else { m.px(1.0) * t };
        let (hw, radius) = (m.px((width - DOT) / 2.0), m.px(DOT / 2.0) + grow);
        let reach = hw + radius + m.px(5.0);
        let (x0, x1) = (
            ((cx - reach).floor() as i32).max(0),
            ((cx + reach).ceil() as i32).min(w - 1),
        );
        let (y0, y1) = (
            ((cy - reach).floor() as i32).max(0),
            ((cy + reach).ceil() as i32).min(h - 1),
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = capsule_sdf(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, radius);
                let p = &mut px[(y * w + x) as usize];
                if selected {
                    // a 4 px outer ring at 25% of the accent, then the solid capsule
                    p.over(accent, 0.25 * band_cov(d, 0.0, m.px(4.0)));
                    p.over(accent, fill_cov(d, 0.0));
                } else if occupied {
                    p.over(accent, fill_cov(d, 0.0));
                } else {
                    p.over(accent, (0.15 + 0.25 * t) * fill_cov(d, 0.0));
                }
            }
        }
    }
    px.iter()
        .map(|p| {
            let [r, g, b, a] = p.0.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u32);
            (a << 24) | (r << 16) | (g << 8) | b
        })
        .collect()
}

/// Draws the current state of `ind` into its layered window.
fn render_indicator(hwnd: HWND, ind: &Ind) {
    let (w, h) = ind.size();
    if w <= 0 || h <= 0 {
        return;
    }
    let dpi = ind.dpi.load(Ordering::Relaxed);
    let hover: Vec<f32> = ind
        .anim
        .iter()
        .map(|a| a.load(Ordering::Relaxed) as f32 / HOVER_MAX as f32)
        .collect();
    let accent = INDICATOR_ACCENT.load(Ordering::Relaxed);
    let pixels = paint_dots(
        w,
        h,
        metrics(dpi),
        ind.cur.load(Ordering::Relaxed) as i32,
        ind.pop.load(Ordering::Relaxed),
        &hover,
        (
            (accent & 0xff) as u8,
            ((accent >> 8) & 0xff) as u8,
            ((accent >> 16) & 0xff) as u8,
        ),
    );
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(bitmap) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
                let old = SelectObject(mem, HGDIOBJ(bitmap.0));
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let size = SIZE { cx: w, cy: h };
                let origin = POINT::default();
                let _ = UpdateLayeredWindow(
                    hwnd,
                    Some(screen),
                    None,
                    Some(&size),
                    Some(mem),
                    Some(&origin),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                SelectObject(mem, old);
            }
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

/// Sets the accent colour (COLORREF); every indicator is repainted on its next `show_indicator`.
pub fn set_indicator_accent(color: u32) {
    INDICATOR_ACCENT.store(color, Ordering::Relaxed);
    for ind in &INDS {
        ind.s_cur.store(u32::MAX, Ordering::Relaxed);
    }
}

/// Changes the indicator scale. The next `show_indicator` resizes and repaints every indicator.
pub fn set_indicator_size(size: IndicatorSize) {
    let scale = match size {
        IndicatorSize::Small => 0,
        IndicatorSize::Medium => 1,
        IndicatorSize::Large => 2,
    };
    INDICATOR_SCALE.store(scale, Ordering::Relaxed);
    for ind in &INDS {
        ind.s_cur.store(u32::MAX, Ordering::Relaxed);
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
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let idx = dot_at_x(
                x,
                ind.dpi.load(Ordering::Relaxed),
                ind.cur.load(Ordering::Relaxed) as i32,
            );
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
            let idx = dot_at_x(
                x,
                ind.dpi.load(Ordering::Relaxed),
                ind.cur.load(Ordering::Relaxed) as i32,
            );
            if idx >= 0 {
                if let Some(f) = IND_CLICK.get() {
                    f(mi, idx as u8);
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let idx = dot_at_x(
                x,
                ind.dpi.load(Ordering::Relaxed),
                ind.cur.load(Ordering::Relaxed) as i32,
            );
            if idx >= 0 {
                if let Some(f) = IND_MENU.get() {
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

fn create_indicator_window(mi: usize, hinst: HINSTANCE) -> Option<HWND> {
    let (w, h) = indicator_window_size(96);
    unsafe {
        let hwnd = CreateWindowExW(
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
        .ok()?;
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, mi as isize);
        Some(hwnd)
    }
}

/// Creates the (hidden) workspace indicator. Returns false if it could not be created.
pub fn create_indicator(count: usize) -> bool {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let hinst = HINSTANCE(hmod.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(indicator_wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            lpszClassName: w!("tile-rs-indicator"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let mut created = true;
        for (mi, ind) in INDS.iter().enumerate().take(count.min(MAX_MON)) {
            match create_indicator_window(mi, hinst) {
                Some(hwnd) => ind.hwnd.store(hwnd.0 as isize, Ordering::Relaxed),
                None => created = false,
            }
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
            match create_indicator_window(mi, HINSTANCE(hmod.0)) {
                Some(hwnd) => {
                    ind.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
                    ind.s_cur.store(u32::MAX, Ordering::Relaxed);
                }
                None => ok = false,
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

/// Shows the indicator at `rect` (sized with `indicator_window_size` for `dpi`): which workspace
/// is current and which ones have windows.
pub fn show_indicator(mi: usize, rect: RECT, dpi: u32, cur: u8, populated: u32) {
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
        && ind.dpi.load(Ordering::Relaxed) == dpi
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
    ind.dpi.store(dpi, Ordering::Relaxed);
    for (a, v) in ind.rect.iter().zip(r) {
        a.store(v, Ordering::Relaxed);
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
        render_indicator(hwnd, ind);
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    ind.s_cur.store(cur as u32, Ordering::Relaxed);
    ind.s_pop.store(populated, Ordering::Relaxed);
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

#[cfg(test)]
mod indicator_tests {
    use super::*;

    #[test]
    fn indicator_follows_the_design_proportions() {
        // 100%: 28 px pill + 8 px top margin reserved, 8 px shadow room below, 200 px pill
        assert_eq!(indicator_bar_height(96), 36);
        assert_eq!(indicator_window_size(96), (224, 44));
        assert_eq!(PILL_W, 200.0);
        // scales with the monitor DPI
        assert_eq!(indicator_bar_height(192), 72);
    }

    #[test]
    fn every_column_maps_to_its_dot_and_the_active_one_is_wider() {
        // workspace 3 (index 2) is current: it is 24 wide, so later dots shift right by 12
        let cur = 2;
        for i in 0..DOTS {
            let (left, width) = dot_span(i, cur);
            let mid = (left + width / 2.0) as i32;
            assert_eq!(dot_at_x(mid, 96, cur), i, "dot {i}");
        }
        assert_eq!(dot_span(2, cur).1, ACTIVE_W);
        assert_eq!(dot_span(3, cur).0 - dot_span(2, cur).0, ACTIVE_W + GAP);
        assert_eq!(dot_span(1, cur).0 - dot_span(0, cur).0, DOT + GAP);
        // outside the pill nothing is hit
        assert_eq!(dot_at_x(0, 96, cur), -1);
        assert_eq!(dot_at_x(indicator_window_size(96).0, 96, cur), -1);
    }

    #[test]
    fn pill_is_white_dots_use_the_accent_and_the_background_is_transparent() {
        let m = metrics(96);
        let (w, h) = indicator_window_size(96);
        let accent = (0x00, 0x8C, 0xFF);
        let px = paint_dots(w, h, m, 0, 0b10, &[0.0; DOTS as usize], accent);
        let at = |x: i32, y: i32| px[(y * w + x) as usize];
        let cy = (TOP + PILL_H / 2.0) as i32;
        // far corner: fully transparent (click-through)
        assert_eq!(at(0, 0) >> 24, 0);
        // just inside the pill's left edge, outside the active ring: opaque white
        let pad_x = (SIDE + 1.0) as i32;
        assert_eq!(at(pad_x, cy), 0xFFFF_FFFF);
        // the current workspace (first) is the accent colour
        let (left, width) = dot_span(0, 0);
        assert_eq!(at((left + width / 2.0) as i32, cy), 0xFF00_8CFF);
        // an occupied dot too; an empty one is a faint tint over white
        let (l1, w1) = dot_span(1, 0);
        assert_eq!(at((l1 + w1 / 2.0) as i32, cy), 0xFF00_8CFF);
        let (l2, w2) = dot_span(2, 0);
        let empty = at((l2 + w2 / 2.0) as i32, cy);
        assert_eq!(empty >> 24, 0xFF);
        assert!(
            empty & 0xFF > 0xE0 && (empty >> 16) & 0xFF > 0xC0,
            "{empty:#x}"
        );
        // the shadow below the pill is translucent, not opaque
        let below = (m.px(TOP + PILL_H) + 2.0) as i32;
        let a = at(w / 2, below) >> 24;
        assert!(a > 0 && a < 0xA0, "shadow alpha {a:#x}");
    }

    #[test]
    fn accent_can_be_any_colour() {
        let m = metrics(96);
        let (w, h) = indicator_window_size(96);
        let px = paint_dots(w, h, m, 0, 0, &[0.0; DOTS as usize], (0xFF, 0xCC, 0x00));
        let (left, width) = dot_span(0, 0);
        let c = px[(((TOP + PILL_H / 2.0) as i32) * w + (left + width / 2.0) as i32) as usize];
        assert_eq!(c, 0xFFFF_CC00);
    }
}
