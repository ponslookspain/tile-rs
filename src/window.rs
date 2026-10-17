//! Window discovery and classification: which top-level windows are tiled, which are skipped and
//! why, and which ones run fullscreen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, GWL_EXSTYLE, GWL_STYLE,
    GW_OWNER, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_THICKFRAME,
};

use crate::config::{self, FullscreenWindows, Rule};
use crate::prof;

/// In `run` mode, windows the user maximized / made fullscreen are left alone.
pub static SKIP_ZOOMED: AtomicBool = AtomicBool::new(false);

/// Skip reason for a window that runs fullscreen / borderless fullscreen.
pub const FULLSCREEN_REASON: &str = "fullscreen (game / borderless)";

/// Window classes that are never tiled (shell UI, overlays, our own overlays).
const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "tile-rs-border",
    "tile-rs-indicator",
];

#[derive(Debug)]
pub struct Win {
    pub hwnd: HWND,
    pub title: String,
    pub class: String,
    pub pid: u32,
    /// from a config rule: open on this workspace (0-based)
    pub rule_ws: Option<u8>,
}

fn text_of(f: impl Fn(&mut [u16]) -> i32) -> String {
    let mut buf = [0u16; 256];
    let n = f(&mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Executable name without ".exe", lower-case (empty if unknown).
fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        file.strip_suffix(".exe").unwrap_or(&file).to_string()
    }
}

fn rule_matches(r: &Rule, w: &Win, proc_name: &str) -> bool {
    if let Some(p) = &r.process {
        let p = p.to_ascii_lowercase();
        if p.strip_suffix(".exe").unwrap_or(&p) != proc_name {
            return false;
        }
    }
    if let Some(c) = &r.class {
        if !c.eq_ignore_ascii_case(&w.class) {
            return false;
        }
    }
    if let Some(t) = &r.title {
        if !w.title.to_lowercase().contains(&t.to_lowercase()) {
            return false;
        }
    }
    true
}

/// The window's visible frame, falling back to `GetWindowRect` (DWM can fail on exclusive
/// fullscreen windows that are not composited).
fn frame_rect(hwnd: HWND) -> Option<RECT> {
    if let Some(r) = visible_rect(hwnd) {
        return Some(r);
    }
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r).ok()? };
    Some(r)
}

pub fn visible_rect(hwnd: HWND) -> Option<RECT> {
    let mut vis = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    Some(vis)
}

/// A window that covers its whole monitor and has no title bar (a game in exclusive or borderless
/// fullscreen, a fullscreen video player, ...). Such windows are never tiled, so they stay
/// fullscreen; a normal titled window is never reported here.
pub fn is_fullscreen(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let borderless = style & WS_POPUP.0 != 0 || style & WS_CAPTION.0 == 0;
    if !borderless {
        return false;
    }
    let Some(vis) = frame_rect(hwnd) else {
        return false;
    };
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        return false;
    }
    let m = mi.rcMonitor;
    let t = 2; // a couple of pixels of tolerance
    (vis.left - m.left).abs() <= t
        && (vis.top - m.top).abs() <= t
        && (vis.right - m.right).abs() <= t
        && (vis.bottom - m.bottom).abs() <= t
}

/// Returns None if the window is tileable, otherwise the reason it is skipped.
fn skip_reason(hwnd: HWND, title: &str, class: &str) -> Option<&'static str> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return Some("invisible");
        }
        if IsIconic(hwnd).as_bool() {
            return Some("minimized");
        }
        if SKIP_ZOOMED.load(Ordering::Relaxed) && IsZoomed(hwnd).as_bool() {
            return Some("maximized/fullscreen by user");
        }
        if title.is_empty() {
            return Some("no title");
        }
        if SKIP_CLASSES.contains(&class) {
            return Some("shell/system class");
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|w| !w.is_invalid()) {
            return Some("owned window (dialog)");
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return Some("tool/no-activate window");
        }
        if is_fullscreen(hwnd) {
            return Some(FULLSCREEN_REASON);
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_THICKFRAME.0 == 0 {
            return Some("not resizable");
        }
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return Some("cloaked (other desktop / hidden UWP)");
        }
    }
    None
}

pub struct Collector {
    pub tileable: Vec<Win>,
    /// Windows that run fullscreen / borderless fullscreen (tracked per workspace).
    pub fullscreen: Vec<Win>,
    pub skipped: Vec<(Win, &'static str)>,
    rules: Vec<Rule>,
}

thread_local! {
    /// pid -> executable name (the lookup opens the process, so cache it)
    static PROC_CACHE: RefCell<HashMap<u32, String>> = RefCell::new(HashMap::new());
}

fn process_name_cached(pid: u32) -> String {
    PROC_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(n) = c.get(&pid) {
            return n.clone();
        }
        let n = process_name(pid);
        if c.len() > 256 {
            c.clear();
        }
        c.insert(pid, n.clone());
        n
    })
}

fn win_info(hwnd: HWND) -> Win {
    let title = text_of(|b| unsafe { GetWindowTextW(hwnd, b) });
    let class = text_of(|b| unsafe { GetClassNameW(hwnd, b) });
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    Win {
        hwnd,
        title,
        class,
        pid,
        rule_ws: None,
    }
}

pub enum Verdict {
    /// not a window we care about (invisible etc.) - not even listed
    Ignore,
    Tileable(Win),
    /// fullscreen / borderless fullscreen window (tracked per workspace, never tiled)
    Fullscreen(Win),
    Skipped(Win, &'static str),
}

/// Applies the config rules to `w`. Returns true if a rule asked to float it.
fn apply_rules(w: &mut Win, rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return false;
    }
    let pn = process_name_cached(w.pid);
    if let Some(r) = rules.iter().find(|r| rule_matches(r, w, &pn)) {
        w.rule_ws = r.workspace.map(|n| n - 1);
        return r.action.as_deref() == Some("float");
    }
    false
}

/// Decides what to do with one window. Cheap checks first: invisible windows (the vast majority
/// of top-level windows) are rejected before any string is fetched.
pub fn classify(hwnd: HWND, rules: &[Rule]) -> Verdict {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return Verdict::Ignore;
    }
    let mut w = win_info(hwnd);
    match skip_reason(hwnd, &w.title, &w.class) {
        None => {
            if apply_rules(&mut w, rules) {
                return Verdict::Skipped(w, "rule: float");
            }
            Verdict::Tileable(w)
        }
        Some(r) if r == FULLSCREEN_REASON => {
            if config::fullscreen_windows() == FullscreenWindows::Ignore {
                Verdict::Skipped(w, r)
            } else {
                apply_rules(&mut w, rules); // a rule may assign it to a workspace
                Verdict::Fullscreen(w)
            }
        }
        // keep only titled ones in the skip list to keep `list` output readable
        Some(r) if !w.title.is_empty() => Verdict::Skipped(w, r),
        Some(_) => Verdict::Ignore,
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let c = &mut *(lparam.0 as *mut Collector);
    match classify(hwnd, &c.rules) {
        Verdict::Ignore => {}
        Verdict::Tileable(w) => c.tileable.push(w),
        Verdict::Fullscreen(w) => c.fullscreen.push(w),
        Verdict::Skipped(w, r) => c.skipped.push((w, r)),
    }
    BOOL(1)
}

pub fn collect() -> Collector {
    if prof::on() {
        prof::COLLECT_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    let mut c = Collector {
        tileable: vec![],
        fullscreen: vec![],
        skipped: vec![],
        rules: config::rules(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut c as *mut _ as isize));
    }
    // EnumWindows walks top-to-bottom in Z order; keep that, it is a stable, sensible order.
    c
}
