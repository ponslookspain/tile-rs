//! Input surfaces for adjustable tile gaps. No foreign HWND is resized during a gesture.
use crate::config::Layout;
use crate::geometry::{place, Gaps, MAX_RATIO, MIN_RATIO};
use std::cell::{Cell, RefCell};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetCapture, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE,
    TRACKMOUSEEVENT, VK_ESCAPE, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handle {
    pub monitor: usize,
    pub split: usize,
    pub vertical: bool,
    pub rect: RECT,
}

pub fn handles(
    mi: usize,
    layout: Layout,
    tiles: &[RECT],
    regions: &[RECT],
    gap: i32,
) -> Vec<Handle> {
    if tiles.len() < 2 || regions.len() != tiles.len() || gap <= 0 {
        return Vec::new();
    }
    let count = match layout {
        Layout::Spiral => tiles.len() - 1,
        Layout::MasterStack => 1,
        _ => 0,
    };
    (0..count)
        .filter_map(|split| {
            let vertical = layout == Layout::MasterStack || split.is_multiple_of(2);
            let t = tiles[split];
            let q = regions[split];
            // Stay within the gap: never steal an application's own resize border.
            let thickness = gap.min(8);
            let middle = if vertical { t.right } else { t.bottom } + gap / 2;
            let rect = if vertical {
                RECT {
                    left: middle - thickness / 2,
                    right: middle - thickness / 2 + thickness,
                    top: q.top,
                    bottom: q.bottom,
                }
            } else {
                RECT {
                    top: middle - thickness / 2,
                    bottom: middle - thickness / 2 + thickness,
                    left: q.left,
                    right: q.right,
                }
            };
            (rect.right > rect.left && rect.bottom > rect.top).then_some(Handle {
                monitor: mi,
                split,
                vertical,
                rect,
            })
        })
        .collect()
}

pub struct Resize {
    pub handle: Handle,
    pub layout: Layout,
    pub area: RECT,
    pub gaps: Gaps,
    pub windows: Vec<isize>,
    pub workspace: u8,
    pub ratios: Vec<f32>,
    pub tiles: Vec<RECT>,
    pub preview: Vec<RECT>,
    pub regions: Vec<RECT>,
    pub minimums: Vec<(i32, i32)>,
    pub origin: POINT,
    pub last_pointer_axis: i32,
}

impl Resize {
    /// Ignore motion parallel to the separator and repeated timer samples.
    pub fn pointer_changed(&mut self, point: POINT) -> bool {
        let axis = if self.handle.vertical {
            point.x
        } else {
            point.y
        };
        if axis == self.last_pointer_axis {
            return false;
        }
        self.last_pointer_axis = axis;
        true
    }
    /// Always calculate from the gesture's original geometry, never from previous frames.
    pub fn target(&self, point: POINT) -> (Vec<f32>, Vec<RECT>, Vec<RECT>) {
        let j = self.handle.split;
        let q = self.regions[j];
        let span = if self.handle.vertical {
            q.right - q.left
        } else {
            q.bottom - q.top
        } - self.gaps.inner;
        let delta = if self.handle.vertical {
            point.x - self.origin.x
        } else {
            point.y - self.origin.y
        };
        if delta == 0 {
            return (
                self.ratios.clone(),
                self.tiles.clone(),
                self.regions.clone(),
            );
        }
        let start = self.ratios[j];
        let requested = (start + delta as f32 / span.max(1) as f32).clamp(MIN_RATIO, MAX_RATIO);
        let calculate = |ratio| {
            let mut ratios = self.ratios.clone();
            ratios[j] = ratio;
            let (tiles, regions) = place(
                self.layout,
                self.area,
                self.windows.len(),
                &ratios,
                self.gaps,
            );
            (ratios, tiles, regions)
        };
        let fits = |tiles: &[RECT]| {
            tiles
                .iter()
                .zip(&self.minimums)
                .all(|(r, (w, h))| r.right - r.left >= *w && r.bottom - r.top >= *h)
        };
        let candidate = calculate(requested);
        if fits(&candidate.1) {
            return candidate;
        }
        let (mut good, mut bad) = (start, requested);
        for _ in 0..20 {
            let mid = (good + bad) * 0.5;
            if fits(&calculate(mid).1) {
                good = mid;
            } else {
                bad = mid;
            }
        }
        calculate(good)
    }
}

thread_local! {
    static SURFACES: RefCell<Vec<(Handle, HWND)>> = const { RefCell::new(Vec::new()) };
    static CAPTURE: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

pub fn release() {
    let hwnd = CAPTURE.with(|capture| capture.replace(HWND::default()));
    if !hwnd.is_invalid() {
        unsafe {
            let _ = KillTimer(Some(hwnd), 1);
            let _ = ReleaseCapture();
        }
    }
}

pub fn captured() -> bool {
    CAPTURE.with(|c| !c.get().is_invalid())
}

/// Called by the high-resolution frame scheduler; the window timer only provides a watchdog.
pub fn tick() {
    if !captured() {
        return;
    }
    unsafe {
        if GetAsyncKeyState(i32::from(VK_ESCAPE.0)) < 0 {
            crate::end_divider(false);
        } else if GetAsyncKeyState(i32::from(VK_LBUTTON.0)) >= 0 {
            crate::end_divider(true);
        } else {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok() {
                crate::update_divider(point);
            } else {
                crate::end_divider(false);
            }
        }
    }
}

pub fn sync(handles: Vec<Handle>) {
    if CAPTURE.with(|c| !c.get().is_invalid()) {
        return;
    }
    let unchanged = SURFACES.with(|surfaces| {
        surfaces
            .borrow()
            .iter()
            .map(|(h, _)| *h)
            .eq(handles.iter().copied())
    });
    if unchanged {
        return;
    }
    // Remove handles from TLS before destroying their windows (Win32 can call us back).
    let mut old = SURFACES.with(|s| std::mem::take(&mut *s.borrow_mut()));
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0);
        if !handles.is_empty() && !REGISTERED.with(|r| r.get()) {
            let class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: w!("tile-rs-divider"),
                lpfnWndProc: Some(wndproc),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                SURFACES.with(|s| *s.borrow_mut() = old);
                return;
            }
            REGISTERED.with(|r| r.set(true));
        }
        for handle in handles {
            let r = handle.rect;
            if let Some(index) = old
                .iter()
                .position(|(h, _)| h.monitor == handle.monitor && h.split == handle.split)
            {
                let (previous, hwnd) = old.swap_remove(index);
                // Publish the updated handle before SetWindowPos can dispatch messages.
                SURFACES.with(|s| s.borrow_mut().push((handle, hwnd)));
                if previous.rect != r {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                }
                continue;
            }
            if let Ok(hwnd) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                w!("tile-rs-divider"),
                w!("tile-rs-divider"),
                WS_POPUP,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                Some(instance),
                None,
            ) {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
                SURFACES.with(|s| s.borrow_mut().push((handle, hwnd)));
                let _ = ShowWindow(hwnd, SW_SHOWNA);
            }
        }
        for (_, hwnd) in old {
            let _ = DestroyWindow(hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let vertical = SURFACES.with(|s| {
                s.borrow()
                    .iter()
                    .find(|(_, h)| *h == hwnd)
                    .is_none_or(|(h, _)| h.vertical)
            });
            SetCursor(LoadCursorW(None, if vertical { IDC_SIZEWE } else { IDC_SIZENS }).ok());
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => {
            let handle =
                SURFACES.with(|s| s.borrow().iter().find(|(_, h)| *h == hwnd).map(|(h, _)| *h));
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok()
                && handle.is_some_and(|h| crate::begin_divider(h, point))
            {
                CAPTURE.with(|c| c.set(hwnd));
                SetCapture(hwnd);
                if GetCapture() != hwnd || SetTimer(Some(hwnd), 1, 100, None) == 0 {
                    release();
                    crate::end_divider(false);
                }
            }
        }
        WM_MOUSEMOVE => {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 90, LWA_ALPHA);
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                ..Default::default()
            };
            let _ = TrackMouseEvent(&mut tracking);
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        }
        WM_TIMER if wp.0 == 1 => tick(),
        WM_LBUTTONUP => {
            release();
            crate::end_divider(true);
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            if CAPTURE.with(|c| c.get() == hwnd) {
                release();
                crate::end_divider(false);
            }
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let brush = CreateSolidBrush(COLORREF(0xffbd8d));
            FillRect(dc, &paint.rcPaint, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = EndPaint(hwnd, &paint);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gesture(layout: Layout, n: usize, split: usize) -> Resize {
        let area = RECT {
            left: -1200,
            top: -200,
            right: 0,
            bottom: 600,
        };
        let gaps = Gaps {
            inner: 10,
            outer: 10,
        };
        let ratios = vec![0.5; n - 1];
        let (tiles, regions) = place(layout, area, n, &ratios, gaps);
        let handle = handles(0, layout, &tiles, &regions, gaps.inner)[split];
        let origin = POINT {
            x: (handle.rect.left + handle.rect.right) / 2,
            y: (handle.rect.top + handle.rect.bottom) / 2,
        };
        Resize {
            handle,
            layout,
            area,
            gaps,
            windows: (1..=n as isize).collect(),
            workspace: 0,
            ratios,
            preview: tiles.clone(),
            tiles,
            regions,
            minimums: vec![(100, 80); n],
            origin,
            last_pointer_axis: if handle.vertical { origin.x } else { origin.y },
        }
    }

    #[test]
    fn repeated_and_parallel_pointer_motion_does_not_recompute_resize() {
        let mut resize = gesture(Layout::Spiral, 3, 0);
        let origin = resize.origin;
        assert!(!resize.pointer_changed(origin));
        assert!(!resize.pointer_changed(POINT {
            y: origin.y + 50,
            ..origin
        }));
        let moved = POINT {
            x: origin.x + 1,
            ..origin
        };
        assert!(resize.pointer_changed(moved));
        assert!(!resize.pointer_changed(moved));
        assert!(resize.pointer_changed(origin));
    }

    #[test]
    fn handles_stay_inside_the_gap_and_use_each_split_axis() {
        let s = gesture(Layout::Spiral, 5, 0);
        for h in handles(3, s.layout, &s.tiles, &s.regions, s.gaps.inner) {
            let t = s.tiles[h.split];
            assert_eq!(h.monitor, 3);
            if h.vertical {
                assert!(h.rect.left >= t.right && h.rect.right <= t.right + 10);
            } else {
                assert!(h.rect.top >= t.bottom && h.rect.bottom <= t.bottom + 10);
            }
        }
        assert!(handles(0, Layout::Grid, &s.tiles, &s.regions, 10).is_empty());
        assert!(handles(0, Layout::Columns, &s.tiles, &s.regions, 10).is_empty());
        assert!(handles(0, s.layout, &s.tiles, &s.regions, 0).is_empty());
    }

    #[test]
    fn nested_resize_respects_all_minimums_and_reverses_without_drift() {
        for split in 0..3 {
            let mut s = gesture(Layout::Spiral, 4, split);
            for delta in [-4000, -37, 25, 4000] {
                let p = POINT {
                    x: s.origin.x + if s.handle.vertical { delta } else { 0 },
                    y: s.origin.y + if s.handle.vertical { 0 } else { delta },
                };
                let (ratios, tiles, _) = s.target(p);
                for (r, (w, h)) in tiles.iter().zip(&s.minimums) {
                    assert!(r.right - r.left >= *w && r.bottom - r.top >= *h);
                }
                assert!(ratios.iter().all(|r| (MIN_RATIO..=MAX_RATIO).contains(r)));
                s.preview = tiles;
                assert_eq!(s.target(s.origin).1, s.tiles);
            }
        }
    }

    #[test]
    fn master_resize_keeps_stack_bands_equal_and_obeys_minimum_width() {
        let s = gesture(Layout::MasterStack, 4, 0);
        let (_, tiles, _) = s.target(POINT {
            x: s.origin.x + 4000,
            y: s.origin.y,
        });
        assert!(tiles[0].right > s.tiles[0].right);
        assert!(tiles[1..].iter().all(|r| r.right - r.left >= 100));
        let heights: Vec<_> = tiles[1..].iter().map(|r| r.bottom - r.top).collect();
        assert!(heights.iter().max().unwrap() - heights.iter().min().unwrap() <= 1);
    }
}
