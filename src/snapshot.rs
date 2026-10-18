//! Short-lived, in-memory desktop snapshots. A click-through scene covers real windows while
//! their frozen images resize. Foreign applications are never suspended or sent WM_SETREDRAW.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Instant;
use windows::core::w;
use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const MAX_PIXELS: i64 = 16_000_000; // At most ~64 MB per captured monitor.

struct Timeline {
    start: Instant,
    ms: u32,
    from: Vec<(isize, RECT)>,
    to: Vec<(isize, RECT)>,
    fade_out: bool,
}

struct Scene {
    area: RECT,
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    sources: Vec<(isize, RECT)>,
    tiles: RefCell<Vec<(isize, RECT)>>,
    timeline: RefCell<Option<Timeline>>,
    hwnd: Cell<HWND>,
    opacity: Cell<u8>,
    frames: Cell<u32>,
    paint_us: Cell<u64>,
    gpu: RefCell<Option<crate::gpu_snapshot::Renderer>>,
    gpu_attempted: Cell<bool>,
    entering: RefCell<HashSet<isize>>,
    progress: Cell<f32>,
}

impl Drop for Scene {
    fn drop(&mut self) {
        self.gpu.get_mut().take();
        if crate::prof::on() && self.frames.get() > 0 {
            crate::log(format!(
                "snapshot: frames={} paint_avg_us={} capture_count=1 pixels={}",
                self.frames.get(),
                self.paint_us.get() / u64::from(self.frames.get()),
                i64::from(self.area.right - self.area.left)
                    * i64::from(self.area.bottom - self.area.top)
            ));
        }
        unsafe {
            if !self.hwnd.get().is_invalid() {
                let _ = DestroyWindow(self.hwnd.get());
            }
            SelectObject(self.dc, self.old_bitmap);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static SCENES: RefCell<HashMap<usize, Box<Scene>>> = RefCell::new(HashMap::new());
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Scene;
    if msg == WM_PAINT && !ptr.is_null() {
        let scene = &*ptr;
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let accelerated = scene.gpu.borrow().as_ref().map(|gpu| {
            gpu.draw(
                &scene.area,
                &scene.sources,
                &scene.tiles.borrow(),
                &scene.entering.borrow(),
                scene.progress.get(),
            )
        });
        if accelerated == Some(true) {
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        if accelerated == Some(false) {
            scene.gpu.borrow_mut().take();
            crate::log("snapshot GPU device lost; using GDI for this scene");
        }
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let brush = CreateSolidBrush(COLORREF(0x001B1713));
        FillRect(dc, &client, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        SetStretchBltMode(dc, COLORONCOLOR);
        for (h, dest) in scene.tiles.borrow().iter() {
            if let Some((_, src)) = scene.sources.iter().find(|(source, _)| source == h) {
                if scene.entering.borrow().contains(h) {
                    let _ = AlphaBlend(
                        dc,
                        dest.left - scene.area.left,
                        dest.top - scene.area.top,
                        dest.right - dest.left,
                        dest.bottom - dest.top,
                        scene.dc,
                        src.left - scene.area.left,
                        src.top - scene.area.top,
                        src.right - src.left,
                        src.bottom - src.top,
                        BLENDFUNCTION {
                            BlendOp: AC_SRC_OVER as u8,
                            BlendFlags: 0,
                            SourceConstantAlpha: (scene.progress.get() * 255.0).round() as u8,
                            AlphaFormat: 0,
                        },
                    );
                    continue;
                }
                let _ = StretchBlt(
                    dc,
                    dest.left - scene.area.left,
                    dest.top - scene.area.top,
                    dest.right - dest.left,
                    dest.bottom - dest.top,
                    Some(scene.dc),
                    src.left - scene.area.left,
                    src.top - scene.area.top,
                    src.right - src.left,
                    src.bottom - src.top,
                    SRCCOPY,
                );
            }
        }
        let _ = EndPaint(hwnd, &ps);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

pub fn cancel(mi: usize) {
    // Drop outside the map borrow: DestroyWindow dispatches messages synchronously.
    let old = SCENES.with(|scenes| scenes.borrow_mut().remove(&mi));
    drop(old);
}

pub fn clear() {
    let old = SCENES.with(|scenes| std::mem::take(&mut *scenes.borrow_mut()));
    drop(old);
}

pub struct HeldScene(Box<Scene>);

pub fn hold(mi: usize) -> Option<HeldScene> {
    // Cover the outgoing workspace BEFORE its real windows are parked/restored.
    // Otherwise depart() first shows this image after the new workspace is already
    // visible, producing a new -> old -> new flash.
    if !reveal(mi) {
        cancel(mi);
        return None;
    }
    SCENES.with(|scenes| scenes.borrow_mut().remove(&mi).map(HeldScene))
}

pub fn depart(mi: usize, held: HeldScene, offset: i32, ms: u32) -> bool {
    cancel(mi);
    let to = held
        .0
        .sources
        .iter()
        .map(|(h, r)| {
            (
                *h,
                RECT {
                    left: r.left + offset,
                    right: r.right + offset,
                    ..*r
                },
            )
        })
        .collect();
    *held.0.timeline.borrow_mut() = Some(Timeline {
        start: Instant::now(),
        ms: ms.max(1),
        from: held.0.sources.clone(),
        to,
        fade_out: true,
    });
    SCENES.with(|scenes| {
        scenes.borrow_mut().insert(mi, held.0);
    });
    reveal(mi)
}

pub fn contains(mi: usize) -> bool {
    SCENES.with(|scenes| scenes.borrow().contains_key(&mi))
}

pub fn entering(mi: usize, handles: &[isize]) {
    SCENES.with(|scenes| {
        if let Some(scene) = scenes.borrow().get(&mi) {
            *scene.entering.borrow_mut() = handles.iter().copied().collect();
            scene
                .progress
                .set(if handles.is_empty() { 1.0 } else { 0.0 });
        }
    });
}

pub fn supports(mi: usize, targets: &[(isize, RECT)]) -> bool {
    SCENES.with(|scenes| {
        scenes.borrow().get(&mi).is_some_and(|scene| {
            targets
                .iter()
                .all(|(h, _)| scene.sources.iter().any(|(source, _)| source == h))
        })
    })
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

struct Visibility {
    pending: Vec<(isize, RECT)>,
    occluded: bool,
}

unsafe extern "system" fn visible_source(hwnd: HWND, data: LPARAM) -> BOOL {
    let state = &mut *(data.0 as *mut Visibility);
    if let Some(index) = state
        .pending
        .iter()
        .position(|(h, _)| *h == hwnd.0 as isize)
    {
        state.pending.remove(index);
        return BOOL::from(!state.pending.is_empty());
    }
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1);
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == std::process::id() {
        return BOOL(1);
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        (&mut cloaked as *mut u32).cast(),
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
    {
        return BOOL(1);
    }
    if let Some(frame) = crate::window::visible_rect(hwnd) {
        if state
            .pending
            .iter()
            .any(|(_, source)| overlaps(source, &frame))
        {
            state.occluded = true;
            return BOOL(0);
        }
    }
    BOOL(1)
}

pub fn reveal(mi: usize) -> bool {
    SCENES.with(|scenes| scenes.borrow().get(&mi).is_some_and(|scene| render(scene)))
}

/// Capture only currently visible pixels. No application IPC, disk I/O, or background capture.
/// Unsupported/oversized surfaces fall back to the ordinary placement path.
pub fn prepare(mi: usize, area: RECT, sources: &[(isize, RECT)]) -> bool {
    cancel(mi);
    let width = area.right - area.left;
    let height = area.bottom - area.top;
    if sources.is_empty()
        || width <= 0
        || height <= 0
        || i64::from(width) * i64::from(height) > MAX_PIXELS
    {
        return false;
    }
    let other_pixels: i64 = SCENES.with(|scenes| {
        scenes
            .borrow()
            .values()
            .map(|scene| {
                i64::from(scene.area.right - scene.area.left)
                    * i64::from(scene.area.bottom - scene.area.top)
            })
            .sum()
    });
    if other_pixels + i64::from(width) * i64::from(height) > 24_000_000 {
        return false;
    }
    // Cropping a partly off-screen source would stretch the wrong pixels.
    if sources.iter().any(|(_, r)| {
        r.left < area.left
            || r.top < area.top
            || r.right > area.right
            || r.bottom > area.bottom
            || r.right <= r.left
            || r.bottom <= r.top
    }) {
        return false;
    }
    // Screen pixels are not private window surfaces: reject overlaps instead of stretching
    // another application's popup as if it belonged to the source window.
    if sources
        .iter()
        .enumerate()
        .any(|(i, (_, a))| sources[i + 1..].iter().any(|(_, b)| overlaps(a, b)))
    {
        return false;
    }
    let mut visibility = Visibility {
        pending: sources.to_vec(),
        occluded: false,
    };
    unsafe {
        let _ = EnumWindows(
            Some(visible_source),
            LPARAM((&mut visibility as *mut Visibility) as isize),
        );
    }
    if visibility.occluded || !visibility.pending.is_empty() {
        return false;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return false;
        }
        let dc = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if dc.is_invalid() || bitmap.is_invalid() {
            if !dc.is_invalid() {
                let _ = DeleteDC(dc);
            }
            if !bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            let _ = ReleaseDC(None, screen);
            return false;
        }
        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let captured = BitBlt(
            dc,
            0,
            0,
            width,
            height,
            Some(screen),
            area.left,
            area.top,
            SRCCOPY,
        )
        .is_ok();
        let _ = ReleaseDC(None, screen);
        let scene = Box::new(Scene {
            area,
            dc,
            bitmap,
            old_bitmap,
            sources: sources.to_vec(),
            tiles: RefCell::new(sources.to_vec()),
            timeline: RefCell::new(None),
            hwnd: Cell::new(HWND::default()),
            opacity: Cell::new(255),
            frames: Cell::new(0),
            paint_us: Cell::new(0),
            gpu: RefCell::new(None),
            gpu_attempted: Cell::new(false),
            entering: RefCell::new(HashSet::new()),
            progress: Cell::new(1.0),
        });
        if !captured {
            return false;
        }
        SCENES.with(|scenes| scenes.borrow_mut().insert(mi, scene));
    }
    true
}

fn render(scene: &Scene) -> bool {
    let started = crate::prof::on().then(Instant::now);
    unsafe {
        static CLASS: OnceLock<bool> = OnceLock::new();
        if !*CLASS.get_or_init(|| {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: HINSTANCE(instance.0),
                lpszClassName: w!("tile-rs-snapshot"),
                ..Default::default()
            }) != 0
        }) {
            return false;
        }
        let mut hwnd = scene.hwnd.get();
        if hwnd.is_invalid() {
            let Ok(window) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                w!("tile-rs-snapshot"),
                w!(""),
                WS_POPUP,
                scene.area.left,
                scene.area.top,
                scene.area.right - scene.area.left,
                scene.area.bottom - scene.area.top,
                None,
                None,
                Some(HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0)),
                None,
            ) else {
                return false;
            };
            hwnd = window;
            scene.hwnd.set(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, scene as *const Scene as isize);
            if SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).is_err() {
                return false;
            }
        }
        if !scene.gpu_attempted.replace(true) {
            *scene.gpu.borrow_mut() = crate::gpu_snapshot::Renderer::new(
                hwnd,
                scene.dc,
                scene.bitmap,
                scene.old_bitmap,
                (scene.area.right - scene.area.left) as u32,
                (scene.area.bottom - scene.area.top) as u32,
            );
            if crate::prof::on() {
                crate::log(format!(
                    "snapshot renderer: {}",
                    if scene.gpu.borrow().is_some() {
                        "Direct2D hardware"
                    } else {
                        "GDI fallback"
                    }
                ));
            }
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(hwnd), None, false);
        if SetLayeredWindowAttributes(hwnd, COLORREF(0), scene.opacity.get(), LWA_ALPHA).is_err() {
            return false;
        }
        // These are our own windows; painting cannot block on a foreign application's UI thread.
        let _ = UpdateWindow(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    scene.frames.set(scene.frames.get().saturating_add(1));
    if let Some(started) = started {
        scene.paint_us.set(
            scene
                .paint_us
                .get()
                .saturating_add(started.elapsed().as_micros() as u64),
        );
    }
    true
}

pub fn preview(mi: usize, tiles: &[(isize, RECT)]) -> bool {
    SCENES.with(|scenes| {
        let scenes = scenes.borrow();
        let Some(scene) = scenes.get(&mi) else {
            return false;
        };
        *scene.tiles.borrow_mut() = tiles.to_vec();
        render(scene)
    })
}

pub fn animate(mi: usize, to: &[(isize, RECT)], ms: u32) -> bool {
    SCENES.with(|scenes| {
        let scenes = scenes.borrow();
        let Some(scene) = scenes.get(&mi) else {
            return false;
        };
        *scene.timeline.borrow_mut() = Some(Timeline {
            start: Instant::now(),
            ms: ms.max(1),
            from: scene.tiles.borrow().clone(),
            to: to.to_vec(),
            fade_out: false,
        });
        render(scene)
    })
}

pub fn is_animating() -> bool {
    SCENES.with(|scenes| {
        scenes
            .borrow()
            .values()
            .any(|scene| scene.timeline.borrow().is_some())
    })
}

pub fn tick() -> bool {
    let finished = SCENES.with(|scenes| {
        let scenes = scenes.borrow();
        let mut finished = Vec::new();
        for (mi, scene) in scenes.iter() {
            let timeline = scene.timeline.borrow();
            let Some(timeline) = timeline.as_ref() else {
                continue;
            };
            let t = timeline.start.elapsed().as_secs_f32() * 1000.0 / timeline.ms as f32;
            if t >= 1.0 {
                if timeline.fade_out
                    || timeline.start.elapsed().as_millis() >= u128::from(timeline.ms) + 17
                {
                    finished.push(*mi);
                } else {
                    *scene.tiles.borrow_mut() = timeline.to.clone();
                    scene.progress.set(1.0);
                    if !render(scene) {
                        finished.push(*mi);
                    }
                }
                continue;
            }
            let eased = crate::geometry::ease(crate::config::easing(), t);
            scene.progress.set(eased);
            if timeline.fade_out {
                scene.opacity.set(((1.0 - eased) * 255.0).round() as u8);
            }
            *scene.tiles.borrow_mut() = timeline
                .to
                .iter()
                .filter_map(|(h, to)| {
                    timeline
                        .from
                        .iter()
                        .find(|(source, _)| source == h)
                        .map(|(_, from)| (*h, crate::geometry::lerp_rect(from, to, eased)))
                })
                .collect();
            if !render(scene) {
                finished.push(*mi);
            }
        }
        finished
    });
    let changed = !finished.is_empty();
    for mi in finished {
        cancel(mi);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_sources_fall_back_instead_of_capturing_the_wrong_window() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        let first = RECT { right: 60, ..area };
        let second = RECT { left: 40, ..area };
        assert!(!prepare(0, area, &[(1, first), (2, second)]));
        assert!(!contains(0));
    }

    #[test]
    fn capture_rejects_excessive_allocation_before_touching_the_desktop() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 20_000,
            bottom: 20_000,
        };
        assert!(!prepare(0, area, &[(1, area)]));
        assert!(!contains(0));
    }

    #[test]
    fn capture_rejects_empty_or_partly_offscreen_sources() {
        let area = RECT {
            left: -100,
            top: 0,
            right: 100,
            bottom: 100,
        };
        assert!(!prepare(0, area, &[]));
        let outside = RECT { left: -101, ..area };
        assert!(!prepare(0, area, &[(1, outside)]));
        assert!(!contains(0));
    }
}
