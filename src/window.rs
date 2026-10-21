//! Window discovery and classification: which top-level windows are tiled, which are skipped and
//! why, and which ones run fullscreen.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, GA_ROOTOWNER,
    GWL_EXSTYLE, GWL_STYLE, GW_OWNER, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP, WS_THICKFRAME,
};

use crate::config::{self, FullscreenWindows, MediaWindows, Rule};
use crate::prof;

/// In `run` mode, windows the user maximized / made fullscreen are left alone.
pub static SKIP_ZOOMED: AtomicBool = AtomicBool::new(false);

/// Skip reason for a window that runs fullscreen / borderless fullscreen.
pub const FULLSCREEN_REASON: &str = "fullscreen (game / borderless)";

/// Skip reason for a photo / video window left floating by the `media_windows` setting.
pub const MEDIA_REASON: &str = "media (photo/video): floating";

/// Skip reason for a window the user chose to float manually (`toggle_float`).
pub const MANUAL_REASON: &str = "floating (by toggle_float)";

/// Skip reason for a window of an elevated process while tile-rs is not elevated itself.
pub const ELEVATED_REASON: &str = "elevated (start tile-rs as administrator)";

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

/// True if `title` names a photo / video file: a `.extension` whose extension is a media one. Only
/// the run of letters/digits right after the dot counts, so "index.ts" or a word merely containing
/// the letters is not mistaken for a media file. No title-sized lower-case allocation is needed.
fn title_has_media_ext(title: &str) -> bool {
    for (i, _) in title.match_indices('.') {
        let rest = &title[i + 1..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        if end > 0
            && MEDIA_EXTS
                .iter()
                .any(|extension| rest[..end].eq_ignore_ascii_case(extension))
        {
            return true;
        }
    }
    false
}

/// True if the window looks like a photo / video opened in a viewer or player: its title names a
/// media file ("holiday.jpg - Photos", "clip.mp4 - VLC media player") or its process is a known
/// viewer / player. A browser is never treated as a viewer just because its title names a file.
fn is_media(title: &str, process: &str) -> bool {
    MEDIA_PROCESSES.contains(&process)
        || (!BROWSER_PROCESSES.contains(&process) && title_has_media_ext(title))
}

fn is_telegram_viewer(title: &str, process: &str, class: &str) -> bool {
    // Telegram's OverlayWidget uses lng_mediaview_title, not the photo's filename.
    // Match the viewer's exact title and Qt window class, never the whole process or chats.
    // Sources: Telegram/SourceFiles/media/view/media_view_overlay_widget.cpp and
    // Telegram/Resources/langs/lang.strings in telegramdesktop/tdesktop.
    process == "telegram"
        && class.starts_with("Qt")
        && class.contains("QWindow")
        && matches!(title.trim(), "Media viewer" | "Просмотр медиа")
}

fn is_picture_in_picture(title: &str, process: &str, class: &str) -> bool {
    BROWSER_PROCESSES.contains(&process)
        && matches!(class, "Chrome_WidgetWin_1" | "MozillaDialogClass")
        && matches!(
            title.trim(),
            "Picture in picture" | "Picture-in-Picture" | "Картинка в картинке"
        )
}

fn is_terminal(class: &str, process: &str) -> bool {
    // Legacy console hosts can omit WS_THICKFRAME while still accepting window placement.
    // Match the actual HWND host, not the shell executing inside the terminal.
    matches!(
        (class, process),
        ("ConsoleWindowClass", "conhost" | "openconsole")
            | ("CASCADIA_HOSTING_WINDOW_CLASS", "windowsterminal")
    )
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

thread_local! {
    /// Reused by `process_name` so the common case does not allocate a fresh (large) buffer for
    /// every process we look at. Grown only if a path turns out to be unusually long.
    static PATH_BUF: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// File name of a UTF-16 executable path, without ".exe", lower-case.
fn basename_no_ext(path: &[u16]) -> String {
    let start = path
        .iter()
        .rposition(|&c| c == b'\\' as u16 || c == b'/' as u16)
        .map_or(0, |i| i + 1);
    let mut file = String::from_utf16_lossy(&path[start..]).to_ascii_lowercase();
    if file.ends_with(".exe") {
        file.truncate(file.len() - 4);
    }
    file
}

/// Executable name without ".exe", lower-case (empty if unknown).
fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let name = PATH_BUF.with(|b| {
            let mut buf = b.borrow_mut();
            if buf.is_empty() {
                buf.resize(1024, 0);
            }
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            if !ok {
                // the path did not fit: grow to the size the call asked for and retry once (a very
                // deep installation or a \\?\ long path)
                let need = (len as usize).max(32768);
                buf.resize(need, 0);
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(
                    h,
                    PROCESS_NAME_WIN32,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                )
                .is_err()
                {
                    return String::new();
                }
                return basename_no_ext(&buf[..len as usize]);
            }
            basename_no_ext(&buf[..len as usize])
        });
        let _ = CloseHandle(h);
        name
    }
}

/// Whether the process `pid` runs with an elevated (administrator) token.
fn process_elevated(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut token = HANDLE::default();
        if OpenProcessToken(h, TOKEN_QUERY, &mut token).is_err() {
            let _ = CloseHandle(h);
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut std::ffi::c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        let _ = CloseHandle(h);
        ok && elev.TokenIsElevated != 0
    }
}

thread_local! {
    /// Whether tile-rs itself runs elevated (never changes)
    static SELF_ELEVATED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn process_elevated_cached(pid: u32) -> bool {
    with_process_info(pid, |info| info.elevated).unwrap_or(false)
}

/// Whether tile-rs itself runs with an elevated token (a non-elevated tile-rs cannot manage the
/// windows of elevated processes).
fn self_elevated() -> bool {
    SELF_ELEVATED.with(|c| match c.get() {
        Some(v) => v,
        None => {
            let v = process_elevated(std::process::id());
            c.set(Some(v));
            v
        }
    })
}

/// `s` without a trailing ".exe", case-insensitively (no allocation).
fn strip_exe_ci(s: &str) -> &str {
    match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    }
}

/// `proc_name` is the window's executable name, already lower-case and without ".exe"; `title_lc`
/// is the window title already lower-cased once for the whole rule list.
fn rule_matches(r: &Rule, w: &Win, proc_name: &str, title_lc: &str) -> bool {
    if let Some(p) = &r.process {
        if !strip_exe_ci(p).eq_ignore_ascii_case(proc_name) {
            return false;
        }
    }
    if let Some(c) = &r.class {
        if !c.eq_ignore_ascii_case(&w.class) {
            return false;
        }
    }
    if let Some(t) = &r.title {
        if !title_lc.contains(&t.to_lowercase()) {
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

/// Structural exclusions cannot be overridden by a user rule.
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
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_CHILD.0 != 0 {
            return Some("child window");
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|w| !w.is_invalid()) {
            return Some("owned window (dialog)");
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return Some("tool/no-activate window");
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

/// Visible application windows as (process, class, title), one per process, for the settings
/// launcher's "add a rule from a running window" list.
pub fn running_apps() -> Vec<(String, String, String)> {
    let collected = collect(true, &HashSet::new());
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    let skipped = collected.skipped.iter().map(|(w, _)| w);
    for w in collected
        .tileable
        .iter()
        .chain(collected.fullscreen.iter())
        .chain(skipped)
    {
        let process = process_name(w.pid);
        if process.is_empty() || w.title.trim().is_empty() || !seen.insert(process.clone()) {
            continue;
        }
        apps.push((process, w.class.clone(), w.title.clone()));
    }
    apps.sort_by(|a, b| a.0.cmp(&b.0));
    apps
}

pub struct Collector {
    pub tileable: Vec<Win>,
    /// Windows that run fullscreen / borderless fullscreen (tracked per workspace).
    pub fullscreen: Vec<Win>,
    pub skipped: Vec<(Win, &'static str)>,
    /// Whether `skipped` is filled. The hot paths (re-tile) only need `tileable` + `fullscreen`;
    /// keeping the skip reasons is only worth it for the `list` command.
    keep_skipped: bool,
    rules: Vec<Rule>,
    /// Windows the user floated with `toggle_float`: never tiled this session.
    manual_float: HashSet<isize>,
}

struct ProcessInfo {
    handle: HANDLE,
    name: String,
    elevated: bool,
}

impl Drop for ProcessInfo {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

impl ProcessInfo {
    fn alive(&self) -> bool {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe {
            GetProcessTimes(
                self.handle,
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .is_ok()
            && exited.dwLowDateTime == 0
            && exited.dwHighDateTime == 0
    }
}

thread_local! {
    // A retained process handle identifies the original process even after PID reuse.
    // Reject exited handles before trusting cached name/elevation; keep the cache bounded.
    static PROC_CACHE: RefCell<HashMap<u32, ProcessInfo>> = RefCell::new(HashMap::new());
}

fn with_process_info<T>(pid: u32, read: impl FnOnce(&ProcessInfo) -> T) -> Option<T> {
    PROC_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.get(&pid).is_some_and(|info| !info.alive()) {
            cache.remove(&pid);
        }
        if cache.len() >= 256 && !cache.contains_key(&pid) {
            cache.retain(|_, info| info.alive());
            if cache.len() >= 256 {
                cache.clear();
            }
        }
        let info = match cache.entry(pid) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let handle =
                    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
                entry.insert(ProcessInfo {
                    handle,
                    name: process_name(pid),
                    elevated: process_elevated(pid),
                })
            }
        };
        Some(read(info))
    })
}

fn process_name_cached(pid: u32) -> String {
    with_process_info(pid, |info| info.name.clone()).unwrap_or_default()
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    // lower-case the title at most once for the whole rule list (not once per title rule)
    let title_lc = if rules.iter().any(|r| r.title.is_some()) {
        w.title.to_lowercase()
    } else {
        String::new()
    };
    if let Some(r) = rules
        .iter()
        .find(|r| rule_matches(r, w, process, &title_lc))
    {
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
    let w = win_info(hwnd);
    // The Slint settings window runs in a child tile-rs process. Keep it out of the managed
    // window set so opening settings never resizes or moves the UI itself.
    if w.title == "tile-rs Settings" && process_name_cached(w.pid).eq_ignore_ascii_case("tile-rs") {
        return Verdict::Skipped(w, "tile-rs settings launcher");
    }
    if let Some(reason) = skip_reason(hwnd, &w.title, &w.class) {
        return if w.title.is_empty() {
            Verdict::Ignore
        } else {
            Verdict::Skipped(w, reason)
        };
    }
    if !self_elevated() && process_elevated_cached(w.pid) {
        return Verdict::Skipped(w, ELEVATED_REASON);
    }
    let pn = process_name_cached(w.pid);
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    classify_eligible(
        w,
        &pn,
        rules,
        is_fullscreen(hwnd),
        style & WS_THICKFRAME.0 != 0,
        config::media_windows(),
        config::fullscreen_windows(),
    )
}

fn classify_eligible(
    mut w: Win,
    process: &str,
    rules: &[Rule],
    fullscreen: bool,
    resizable: bool,
    media_policy: MediaWindows,
    fullscreen_policy: FullscreenWindows,
) -> Verdict {
    let action = apply_rules(&mut w, rules, process);
    let media = is_media(&w.title, process) || is_telegram_viewer(&w.title, process, &w.class);
    let pip = is_picture_in_picture(&w.title, process, &w.class);
    if let Some(reason) = floating_reason(action, media, pip, media_policy) {
        return Verdict::Skipped(w, reason);
    }
    // Rules/media are resolved before fullscreen admission: a floating viewer must not
    // acquire a fullscreen workspace. A tile rule never resizes a genuine fullscreen game.
    if fullscreen {
        return if fullscreen_policy == FullscreenWindows::Ignore {
            Verdict::Skipped(w, FULLSCREEN_REASON)
        } else {
            Verdict::Fullscreen(w)
        };
    }
    if !resizable && action != RuleAction::Tile && !is_terminal(&w.class, process) {
        return Verdict::Skipped(w, "not resizable");
    }
    Verdict::Tileable(w)
}

fn floating_reason(
    action: RuleAction,
    media: bool,
    pip: bool,
    media_policy: MediaWindows,
) -> Option<&'static str> {
    match action {
        RuleAction::Float => Some("rule: float"),
        RuleAction::Tile => None,
        RuleAction::None if pip => Some("picture-in-picture: floating"),
        RuleAction::None if media && media_policy == MediaWindows::Float => Some(MEDIA_REASON),
        RuleAction::None => None,
    }
}

/// Shared by event admission and full enumeration, including fullscreen windows.
pub fn classify_with_manual(hwnd: HWND, rules: &[Rule], manual_float: &HashSet<isize>) -> Verdict {
    match classify(hwnd, rules) {
        Verdict::Tileable(w) | Verdict::Fullscreen(w)
            if manual_float.contains(&(hwnd.0 as isize)) =>
        {
            Verdict::Skipped(w, MANUAL_REASON)
        }
        verdict => verdict,
    }
}

/// Policy exclusions also apply to temporarily hidden/minimized windows. Their native
/// availability must not keep an old workspace assignment alive after a float rule changes.
pub fn policy_excludes(hwnd: HWND, rules: &[Rule]) -> bool {
    let mut window = win_info(hwnd);
    let process = process_name_cached(window.pid);
    let action = apply_rules(&mut window, rules, &process);
    let media = is_media(&window.title, &process)
        || is_telegram_viewer(&window.title, &process, &window.class);
    floating_reason(
        action,
        media,
        is_picture_in_picture(&window.title, &process, &window.class),
        config::media_windows(),
    )
    .is_some()
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let c = &mut *(lparam.0 as *mut Collector);
    // Runtime reconciliation needs no diagnostic strings for windows that cannot be
    // managed. Preserve detailed classification for the user's `list` command.
    if !c.keep_skipped {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
            || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0
            || GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid())
        {
            return BOOL(1);
        }
    }
    match classify_with_manual(hwnd, &c.rules, &c.manual_float) {
        Verdict::Ignore => {}
        Verdict::Tileable(w) => c.tileable.push(w),
        Verdict::Fullscreen(w) => c.fullscreen.push(w),
        Verdict::Skipped(w, r) => {
            if c.keep_skipped {
                c.skipped.push((w, r));
            }
        }
    }
    BOOL(1)
}

/// Walks every top-level window and classifies it. `keep_skipped` only matters for the `list`
/// command; the tiling hot path passes `false` so the skip entries (and their strings) are dropped.
/// `manual_float` holds the windows the user floated with `toggle_float` (never tiled).
pub fn collect(keep_skipped: bool, manual_float: &HashSet<isize>) -> Collector {
    if prof::on() {
        prof::COLLECT_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    let mut c = Collector {
        tileable: vec![],
        fullscreen: vec![],
        skipped: vec![],
        keep_skipped,
        rules: config::rules(),
        manual_float: manual_float.clone(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut c as *mut _ as isize));
    }
    // EnumWindows walks top-to-bottom in Z order; keep that, it is a stable, sensible order.
    c
}

fn rect_json(rect: Option<RECT>) -> serde_json::Value {
    rect.map(|r| serde_json::json!([r.left, r.top, r.right, r.bottom]))
        .unwrap_or(serde_json::Value::Null)
}

/// Read-only native facts and the same classification used by event admission.
/// Titles are omitted unless explicitly requested by the diagnostic caller.
pub fn diagnostic_snapshot(
    hwnd: HWND,
    rules: &[Rule],
    manual_float: &HashSet<isize>,
    include_title: bool,
) -> serde_json::Value {
    let w = win_info(hwnd);
    let process = process_name_cached(w.pid);
    let title_lc = w.title.to_lowercase();
    let rule = rules
        .iter()
        .position(|rule| rule_matches(rule, &w, &process, &title_lc));
    let (verdict, reason, workspace) = match classify_with_manual(hwnd, rules, manual_float) {
        Verdict::Tileable(w) => ("tileable", None, w.rule_ws),
        Verdict::Fullscreen(w) => ("fullscreen", Some(FULLSCREEN_REASON), w.rule_ws),
        Verdict::Skipped(w, reason) => ("skipped", Some(reason), w.rule_ws),
        Verdict::Ignore => ("ignored", skip_reason(hwnd, &w.title, &w.class), None),
    };
    let mut native = RECT::default();
    let native = unsafe { GetWindowRect(hwnd, &mut native) }
        .is_ok()
        .then_some(native);
    let mut cloaked = 0u32;
    let cloaked = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
    }
    .is_ok()
    .then_some(cloaked);
    serde_json::json!({
        "hwnd": hwnd.0 as isize,
        "pid": w.pid,
        "thread_id": unsafe { GetWindowThreadProcessId(hwnd, None) },
        "process": process,
        "class": w.class,
        "title": include_title.then_some(w.title),
        "owner": unsafe { GetWindow(hwnd, GW_OWNER) }.ok().map(|owner| owner.0 as isize),
        "root_owner": unsafe { GetAncestor(hwnd, GA_ROOTOWNER) }.0 as isize,
        "style": unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32,
        "ex_style": unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32,
        "visible": unsafe { IsWindowVisible(hwnd) }.as_bool(),
        "minimized": unsafe { IsIconic(hwnd) }.as_bool(),
        "maximized": unsafe { IsZoomed(hwnd) }.as_bool(),
        "cloaked": cloaked,
        "elevated": process_elevated_cached(w.pid),
        "manager_elevated": self_elevated(),
        "native_rect": rect_json(native),
        "visible_rect": rect_json(visible_rect(hwnd)),
        "verdict": verdict,
        "reason": reason,
        "matched_rule": rule.map(|index| index + 1),
        "rule_workspace": workspace.map(|workspace| workspace + 1),
        "manual_float": manual_float.contains(&(hwnd.0 as isize)),
    })
}

/// Snapshot visible top-level windows without moving, resizing, focusing or starting them.
pub fn diagnostic_snapshots(
    rules: &[Rule],
    manual_float: &HashSet<isize>,
    include_titles: bool,
) -> Vec<serde_json::Value> {
    struct Context<'a> {
        rules: &'a [Rule],
        manual_float: &'a HashSet<isize>,
        include_titles: bool,
        windows: Vec<serde_json::Value>,
    }
    unsafe extern "system" fn snapshot_proc(hwnd: HWND, data: LPARAM) -> BOOL {
        let context = &mut *(data.0 as *mut Context<'_>);
        if IsWindowVisible(hwnd).as_bool() {
            context.windows.push(diagnostic_snapshot(
                hwnd,
                context.rules,
                context.manual_float,
                context.include_titles,
            ));
        }
        BOOL(1)
    }
    let mut context = Context {
        rules,
        manual_float,
        include_titles,
        windows: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(snapshot_proc), LPARAM(&mut context as *mut _ as isize));
    }
    context.windows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window(title: &str, class: &str) -> Win {
        Win {
            hwnd: HWND::default(),
            title: title.into(),
            class: class.into(),
            pid: 1,
            rule_ws: None,
        }
    }

    fn rule(action: &str) -> Rule {
        Rule {
            process: None,
            class: None,
            title: Some("fixture".into()),
            action: Some(action.into()),
            workspace: Some(3),
        }
    }

    #[test]
    fn floating_rules_and_media_are_resolved_before_fullscreen_admission() {
        let float = [rule("float")];
        assert!(matches!(
            classify_eligible(
                sample_window("fixture", "Game"),
                "game",
                &float,
                true,
                true,
                MediaWindows::Tile,
                FullscreenWindows::Minimize
            ),
            Verdict::Skipped(_, "rule: float")
        ));
        assert!(matches!(
            classify_eligible(
                sample_window("Media viewer", "Qt663QWindowIcon"),
                "telegram",
                &[],
                true,
                true,
                MediaWindows::Float,
                FullscreenWindows::Minimize
            ),
            Verdict::Skipped(_, MEDIA_REASON)
        ));
        assert!(matches!(
            classify_eligible(
                sample_window("movie", "Player"),
                "vlc",
                &[],
                true,
                true,
                MediaWindows::Float,
                FullscreenWindows::Minimize
            ),
            Verdict::Skipped(_, MEDIA_REASON)
        ));
        let tile = [rule("tile")];
        assert!(matches!(
            classify_eligible(
                sample_window("fixture", "Game"),
                "game",
                &tile,
                true,
                true,
                MediaWindows::Float,
                FullscreenWindows::Minimize
            ),
            Verdict::Fullscreen(_)
        ));
        assert!(matches!(
            classify_eligible(
                sample_window("fixture", "Game"),
                "game",
                &tile,
                true,
                true,
                MediaWindows::Float,
                FullscreenWindows::Ignore
            ),
            Verdict::Skipped(_, FULLSCREEN_REASON)
        ));
    }

    #[test]
    fn telegram_viewer_profile_keeps_main_window_and_chats_tileable() {
        for title in ["Media viewer", "Просмотр медиа"] {
            assert!(is_telegram_viewer(title, "telegram", "Qt663QWindowIcon"));
        }
        for title in [
            "Telegram",
            "Alice — Telegram",
            "Media viewer — Telegram",
            "Просмотр медиа — Telegram",
        ] {
            assert!(!is_telegram_viewer(title, "telegram", "Qt663QWindowIcon"));
            assert!(matches!(
                classify_eligible(
                    sample_window(title, "Qt663QWindowIcon"),
                    "telegram",
                    &[],
                    false,
                    true,
                    MediaWindows::Float,
                    FullscreenWindows::Minimize
                ),
                Verdict::Tileable(_)
            ));
        }
        assert!(!is_telegram_viewer(
            "Media viewer",
            "otherapp",
            "Qt663QWindowIcon"
        ));
        assert!(!is_telegram_viewer(
            "Media viewer",
            "telegram",
            "Chrome_WidgetWin_1"
        ));
    }

    #[test]
    fn browser_pip_requires_a_known_process_class_and_exact_title() {
        assert!(is_picture_in_picture(
            "Picture in picture",
            "chrome",
            "Chrome_WidgetWin_1"
        ));
        assert!(is_picture_in_picture(
            "Picture-in-Picture",
            "firefox",
            "MozillaDialogClass"
        ));
        assert!(!is_picture_in_picture(
            "Picture in picture - Google Chrome",
            "chrome",
            "Chrome_WidgetWin_1"
        ));
        assert!(!is_picture_in_picture(
            "Picture in picture",
            "otherapp",
            "Chrome_WidgetWin_1"
        ));
        assert!(!is_picture_in_picture(
            "Picture in picture",
            "chrome",
            "OtherClass"
        ));
    }

    #[test]
    fn console_hosts_and_explicit_tile_can_override_only_the_resizable_heuristic() {
        for (class, process) in [
            ("ConsoleWindowClass", "conhost"),
            ("ConsoleWindowClass", "openconsole"),
            ("CASCADIA_HOSTING_WINDOW_CLASS", "windowsterminal"),
        ] {
            assert!(matches!(
                classify_eligible(
                    sample_window("fixture", class),
                    process,
                    &[],
                    false,
                    false,
                    MediaWindows::Float,
                    FullscreenWindows::Minimize
                ),
                Verdict::Tileable(_)
            ));
        }
        assert!(matches!(
            classify_eligible(
                sample_window("fixture", "ConsoleWindowClass"),
                "otherapp",
                &[],
                false,
                false,
                MediaWindows::Float,
                FullscreenWindows::Minimize
            ),
            Verdict::Skipped(_, "not resizable")
        ));
        assert!(matches!(
            classify_eligible(
                sample_window("fixture", "FixedWindow"),
                "otherapp",
                &[rule("tile")],
                false,
                false,
                MediaWindows::Float,
                FullscreenWindows::Minimize
            ),
            Verdict::Tileable(Win {
                rule_ws: Some(2),
                ..
            })
        ));
    }

    struct Fixture(HWND);

    impl Fixture {
        fn new(owner: Option<HWND>) -> Self {
            use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, WS_VISIBLE};
            Self(unsafe {
                CreateWindowExW(
                    Default::default(),
                    windows::core::w!("STATIC"),
                    windows::core::w!("tile-rs classification fixture"),
                    WS_POPUP | WS_VISIBLE,
                    -25000,
                    -25000,
                    400,
                    300,
                    owner,
                    None,
                    None,
                    None,
                )
                .unwrap()
            })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn native_owned_dialog_stays_excluded_even_with_tile_rule() {
        let owner = Fixture::new(None);
        let dialog = Fixture::new(Some(owner.0));
        assert!(matches!(
            classify(dialog.0, &[rule("tile")]),
            Verdict::Skipped(_, "owned window (dialog)")
        ));
    }

    #[test]
    fn event_and_enumeration_admission_both_honor_manual_float() {
        let window = Fixture::new(None);
        let rules = vec![rule("tile")];
        let manual = HashSet::from([window.0 .0 as isize]);
        assert!(matches!(classify(window.0, &rules), Verdict::Tileable(_)));
        assert!(matches!(
            classify_with_manual(window.0, &rules, &manual),
            Verdict::Skipped(_, MANUAL_REASON)
        ));
        let mut collector = Collector {
            tileable: Vec::new(),
            fullscreen: Vec::new(),
            skipped: Vec::new(),
            keep_skipped: true,
            rules,
            manual_float: manual,
        };
        unsafe {
            let _ = enum_proc(window.0, LPARAM(&mut collector as *mut _ as isize));
        }
        assert!(collector.tileable.is_empty() && collector.fullscreen.is_empty());
        assert_eq!(collector.skipped[0].1, MANUAL_REASON);
    }

    #[test]
    fn diagnostic_snapshot_reports_facts_without_exposing_titles_by_default() {
        let window = Fixture::new(None);
        let snapshot = diagnostic_snapshot(window.0, &[rule("tile")], &HashSet::new(), false);
        assert!(snapshot["title"].is_null());
        assert_eq!(snapshot["verdict"], "tileable");
        assert_eq!(snapshot["matched_rule"], 1);
        assert_eq!(snapshot["rule_workspace"], 3);
        assert_eq!(snapshot["pid"], std::process::id());
        assert!(snapshot["native_rect"].is_array());
        let with_title = diagnostic_snapshot(window.0, &[], &HashSet::new(), true);
        assert_eq!(with_title["title"], "tile-rs classification fixture");
    }

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
