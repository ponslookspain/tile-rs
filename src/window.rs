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

use crate::config::{self, FullscreenWindows, MediaWindows, Rule};
use crate::prof;

/// In `run` mode, windows the user maximized / made fullscreen are left alone.
pub static SKIP_ZOOMED: AtomicBool = AtomicBool::new(false);

/// Skip reason for a window that runs fullscreen / borderless fullscreen.
pub const FULLSCREEN_REASON: &str = "fullscreen (game / borderless)";

/// Skip reason for a photo / video window left floating by the `media_windows` setting.
pub const MEDIA_REASON: &str = "media (photo/video): floating";

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

/// Photo and video file extensions (lower-case, without the dot). Audio is deliberately left out:
/// an audio player is an ordinary window, while a photo/video viewer is usually best left floating.
const MEDIA_EXTS: &[&str] = &[
    // photos
    "jpg", "jpeg", "jfif", "png", "gif", "bmp", "webp", "tif", "tiff", "heic", "heif", "avif",
    "svg", "raw", "cr2", "nef", "arw", "dng", // videos
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "3gp", "ogv",
    "vob", "rmvb", "divx",
];

/// Executables (lower-case, without ".exe") that are photo / video viewers or players, so a window
/// of one of them counts as a media window even if its title does not name the file.
const MEDIA_PROCESSES: &[&str] = &[
    // photos
    "microsoft.photos",
    "photosapp",
    "imageglass",
    "irfanview",
    "irfanview64",
    "honeyview",
    "xnview",
    "xnviewmp",
    "nomacs",
    "jpegview",
    "fsviewer",
    "fsviewer64",
    // video
    "vlc",
    "mpv",
    "mpvnet",
    "mpc-hc",
    "mpc-hc64",
    "mpc-be",
    "mpc-be64",
    "wmplayer",
    "potplayer",
    "potplayermini",
    "potplayermini64",
    "kmplayer",
    "kmplayer64",
    "5kplayer",
];

/// Executables (lower-case, without ".exe") that are web browsers. A browser tab that opens an
/// image or video names it in the window title ("holiday.jpg - Google Chrome"), but the browser is
/// an ordinary window, so a title match from one of these is ignored.
const BROWSER_PROCESSES: &[&str] = &[
    "chrome",
    "msedge",
    "firefox",
    "brave",
    "opera",
    "opera_gx",
    "vivaldi",
    "chromium",
    "iexplore",
    "waterfox",
    "librewolf",
    "thorium",
    "arc",
];

/// True if `title` names a photo / video file: the extension must be followed by a boundary, so a
/// title like "index.ts" or a word merely containing the letters is not mistaken for a media file.
fn title_has_media_ext(title: &str) -> bool {
    let t = title.to_ascii_lowercase();
    let bytes = t.as_bytes();
    MEDIA_EXTS.iter().any(|e| {
        let needle = format!(".{e}");
        t.match_indices(&needle)
            .any(|(i, _)| match bytes.get(i + needle.len()) {
                None => true,
                Some(&c) => !c.is_ascii_alphanumeric() && c != b'_',
            })
    })
}

/// True if the window looks like a photo / video opened in a viewer or player: its title names a
/// media file ("holiday.jpg - Photos", "clip.mp4 - VLC media player") or its process is a known
/// viewer / player. A browser is never treated as a viewer just because its title names a file.
fn is_media(title: &str, process: &str) -> bool {
    MEDIA_PROCESSES.contains(&process)
        || (!BROWSER_PROCESSES.contains(&process) && title_has_media_ext(title))
}

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
    // window titles can be long; a small buffer would truncate them (and break `title` rules)
    let mut buf = [0u16; 1024];
    let n = f(&mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Executable name without ".exe", lower-case (empty if unknown).
fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        // a full executable path can be very long (up to ~32767 chars with a \\?\ prefix), so use
        // a buffer big enough that `process` rules match even for unusually deep installations
        let mut buf = vec![0u16; 32768];
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

/// What a matching config rule asked for.
enum RuleAction {
    /// Rule matched but only set a workspace (or nothing actionable).
    None,
    /// `action = "float"`: never tile this window.
    Float,
    /// `action = "tile"`: tile it even if it would otherwise float (e.g. a media window).
    Tile,
}

/// Applies the config rules to `w`. `process` is the cached executable name of `w`.
fn apply_rules(w: &mut Win, rules: &[Rule], process: &str) -> RuleAction {
    if rules.is_empty() {
        return RuleAction::None;
    }
    if let Some(r) = rules.iter().find(|r| rule_matches(r, w, process)) {
        w.rule_ws = r.workspace.map(|n| n - 1);
        return match r.action.as_deref() {
            Some("float") => RuleAction::Float,
            Some("tile") => RuleAction::Tile,
            _ => RuleAction::None,
        };
    }
    RuleAction::None
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
            let media_float = config::media_windows() == MediaWindows::Float;
            let pn = if !rules.is_empty() || media_float {
                process_name_cached(w.pid)
            } else {
                String::new()
            };
            match apply_rules(&mut w, rules, &pn) {
                RuleAction::Float => return Verdict::Skipped(w, "rule: float"),
                RuleAction::Tile => return Verdict::Tileable(w),
                RuleAction::None => {}
            }
            if media_float && is_media(&w.title, &pn) {
                return Verdict::Skipped(w, MEDIA_REASON);
            }
            Verdict::Tileable(w)
        }
        Some(r) if r == FULLSCREEN_REASON => {
            if config::fullscreen_windows() == FullscreenWindows::Ignore {
                Verdict::Skipped(w, r)
            } else {
                // a rule may assign it to a workspace
                let pn = process_name_cached(w.pid);
                apply_rules(&mut w, rules, &pn);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_titles_are_detected_with_a_boundary() {
        assert!(title_has_media_ext("holiday.jpg - Photos"));
        assert!(title_has_media_ext("clip.mp4"));
        assert!(title_has_media_ext("movie.MKV - VLC")); // case-insensitive
        assert!(title_has_media_ext("clip.mp4"));
        // the extension must end the token, so words that merely contain it do not match
        assert!(!title_has_media_ext("report.pngs.zip"));
        assert!(!title_has_media_ext("my_mp4_notes.txt"));
        assert!(!title_has_media_ext("index.ts")); // ts is deliberately not a video extension
        assert!(!title_has_media_ext("Just a normal document"));
    }

    #[test]
    fn is_media_uses_the_process_or_the_title_but_not_for_browsers() {
        // a known player/viewer is media even without a file in the title
        assert!(is_media("VLC media player", "vlc"));
        assert!(is_media("", "microsoft.photos"));
        // an unknown process is media only when its title names a file
        assert!(is_media("holiday.jpg", "someviewer"));
        assert!(!is_media("Notepad", "notepad"));
        // a browser is never a viewer, even when a tab names an image
        assert!(!is_media("holiday.jpg - Google Chrome", "chrome"));
        assert!(!is_media("clip.mp4 - Microsoft Edge", "msedge"));
    }
}
