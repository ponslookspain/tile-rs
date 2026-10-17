//! tile-rs: a tiny spiral (dwindle) tiling window manager for Windows.
//!
//!   tile-rs.exe          launcher: first start asks about autostart, then runs in the tray
//!   tile-rs.exe --autostart   run silently (what the autostart entry uses)
//!   tile-rs.exe list     show windows that would be tiled, and why others are skipped
//!   tile-rs.exe tile     arrange the current windows once and exit
//!
//! Settings (hotkeys, gap, window rules): %APPDATA%\tile-rs\config.toml
//! Workspaces are emulated: windows of inactive workspaces are parked off-screen.
//! Run only with other window managers (GlazeWM, Astur, ...) stopped.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod geometry;
mod tray;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE,
    HWND, LPARAM, POINT, RECT, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CLOAKED,
    DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CancelWaitableTimer, CreateMutexW, CreateWaitableTimerExW, CreateWaitableTimerW, OpenProcess,
    QueryFullProcessImageNameW, SetWaitableTimer, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetShellWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, KillTimer,
    MessageBoxW, MsgWaitForMultipleObjectsEx, PeekMessageW, PostQuitMessage, SetForegroundWindow,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, GWL_EXSTYLE,
    GWL_STYLE, GW_OWNER, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_TOPMOST, MB_YESNO,
    MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
    SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_HOTKEY, WM_QUIT, WM_TIMER, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP, WS_THICKFRAME,
};

use config::{Action, Dir, FullscreenWindows, Rule, WS_COUNT};
use geometry::{
    anim_duration, apply_resize, ease, effective_step, fullscreen_action, lerp_rect, neighbour,
    same_rect, spiral, spiral_with_mins, split_for, FsAction, MAX_RATIO, MIN_RATIO, RESIZE_PX,
};

/// Where windows of inactive workspaces are parked (the same spot Windows uses for minimized ones).
const PARK_X: i32 = -32000;
const PARK_Y: i32 = -32000;

/// In `run` mode, windows the user maximized / made fullscreen are left alone.
static SKIP_ZOOMED: AtomicBool = AtomicBool::new(false);

/// Set once at startup from `TILE_RS_PROFILE=1`. When off, profiling costs one bool load.
static PROFILE: AtomicBool = AtomicBool::new(false);

fn profile_on() -> bool {
    PROFILE.load(Ordering::Relaxed)
}

/// Skip reason for a window that runs fullscreen / borderless fullscreen.
const FULLSCREEN_REASON: &str = "fullscreen (game / borderless)";

/// Window classes that are never tiled (shell UI, overlays).
const SKIP_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
];

// ---------------------------------------------------------------------------
// logging (the release build has no console, so we log to a file)
// ---------------------------------------------------------------------------

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn log_file() -> &'static PathBuf {
    LOG_PATH.get_or_init(|| {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tile-rs");
        let _ = std::fs::create_dir_all(&base);
        base.join("tile-rs.log")
    })
}

pub fn log(msg: impl AsRef<str>) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file())
    {
        let _ = writeln!(f, "{}", msg.as_ref());
    }
}

fn log_reset() {
    let _ = std::fs::write(log_file(), "");
}

// ---------------------------------------------------------------------------
// profiling (enabled with TILE_RS_PROFILE=1; see docs/PLAN-performance.md)
// ---------------------------------------------------------------------------

/// How many times `collect()` (a full `EnumWindows` walk) ran.
static COLLECT_CALLS: AtomicU32 = AtomicU32::new(0);

/// Logs how long a block of code took, when profiling is on (on every exit path).
struct Timer(&'static str, Option<Instant>);

impl Timer {
    fn new(name: &'static str) -> Self {
        Timer(name, profile_on().then(Instant::now))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(t) = self.1 {
            log(format!(
                "profile: {} {}us collect={}",
                self.0,
                t.elapsed().as_micros(),
                COLLECT_CALLS.load(Ordering::Relaxed)
            ));
        }
    }
}

/// Per-frame stats, aggregated until the animation queue drains.
#[derive(Default)]
struct AnimProf {
    start: Option<Instant>,
    last_frame: Option<Instant>,
    frames: u32,
    intervals_ms: Vec<f32>,
    setpos_us: Vec<f32>,
}

thread_local! {
    static ANIM_PROF: RefCell<AnimProf> = RefCell::new(AnimProf::default());
}

fn anim_prof_reset() {
    ANIM_PROF.with(|p| *p.borrow_mut() = AnimProf::default());
}

/// Records one animation frame (only called when profiling is on).
fn anim_prof_frame(now: Instant, setpos_us: f32) {
    ANIM_PROF.with(|p| {
        let mut p = p.borrow_mut();
        if p.start.is_none() {
            p.start = Some(now);
        }
        if let Some(prev) = p.last_frame {
            p.intervals_ms.push((now - prev).as_secs_f32() * 1000.0);
        }
        p.last_frame = Some(now);
        p.frames += 1;
        p.setpos_us.push(setpos_us);
    });
}

/// Logs the summary of a finished animation and resets the stats.
fn anim_prof_finish(now: Instant) {
    ANIM_PROF.with(|p| {
        let mut p = p.borrow_mut();
        let dur = p
            .start
            .map(|s| (now - s).as_secs_f32() * 1000.0)
            .unwrap_or(0.0);
        let setpos_avg = if p.setpos_us.is_empty() {
            0.0
        } else {
            p.setpos_us.iter().sum::<f32>() / p.setpos_us.len() as f32
        };
        let setpos_max = p.setpos_us.iter().cloned().fold(0.0_f32, f32::max);
        log(format!(
            "anim: frames={} dur={:.0}ms interval avg/p95/max={}ms setpos avg/max={:.2}/{:.2}ms",
            p.frames,
            dur,
            stat_line(&p.intervals_ms),
            setpos_avg / 1000.0,
            setpos_max / 1000.0
        ));
        *p = AnimProf::default();
    });
}

fn stat_line(v: &[f32]) -> String {
    if v.is_empty() {
        return "0/0/0".into();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg = s.iter().sum::<f32>() / s.len() as f32;
    let p95 = s[((s.len() as f32 * 0.95) as usize).min(s.len() - 1)];
    let max = *s.last().unwrap();
    format!("{avg:.1}/{p95:.1}/{max:.1}")
}

/// Message box. Returns true if the user pressed Yes (only meaningful when `question`).
pub fn msgbox(title: &str, text: &str, question: bool) -> bool {
    let (t, x) = (tray::wide(title), tray::wide(text));
    let flags = if question {
        MB_YESNO | MB_ICONQUESTION
    } else {
        MB_OK | MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(x.as_ptr()),
            PCWSTR(t.as_ptr()),
            flags | MB_TOPMOST,
        ) == IDYES
    }
}

/// Non-blocking message box (own thread), so the main message loop keeps running.
pub fn notify(title: &str, text: &str) {
    let (t, x) = (title.to_string(), text.to_string());
    std::thread::spawn(move || {
        msgbox(&t, &x, false);
    });
}

// ---------------------------------------------------------------------------
// autostart (HKCU\...\Run)
// ---------------------------------------------------------------------------

/// The autostart entry lives here (the same place the old `reg.exe` version used).
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "tile-rs";

/// Reads the value from `HKCU\<subkey>` without spawning a process.
fn autostart_enabled_at(subkey: &str, name: &str) -> bool {
    let sub = tray::wide(subkey);
    let val = tray::wide(name);
    let mut buf = [0u16; 2048];
    let mut size = (buf.len() * size_of::<u16>()) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    err == ERROR_SUCCESS
}

/// Writes (`Some`) or removes (`None`) the value in `HKCU\<subkey>`.
fn set_autostart_at(subkey: &str, name: &str, value: Option<&str>) -> bool {
    let sub = tray::wide(subkey);
    let val = tray::wide(name);
    match value {
        Some(v) => {
            let data: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    data.as_ptr() as *const u8,
                    data.len() * size_of::<u16>(),
                )
            };
            let err = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(val.as_ptr()),
                    REG_SZ.0,
                    Some(bytes.as_ptr() as *const _),
                    bytes.len() as u32,
                )
            };
            err == ERROR_SUCCESS
        }
        None => {
            let err = unsafe {
                RegDeleteKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(val.as_ptr()),
                )
            };
            err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND
        }
    }
}

pub fn autostart_enabled() -> bool {
    autostart_enabled_at(RUN_SUBKEY, RUN_VALUE)
}

pub fn set_autostart(on: bool) -> bool {
    if on {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let value = format!("\"{}\" --autostart", exe.display());
        set_autostart_at(RUN_SUBKEY, RUN_VALUE, Some(&value))
    } else {
        !autostart_enabled() || set_autostart_at(RUN_SUBKEY, RUN_VALUE, None)
    }
}

// ---------------------------------------------------------------------------
// window discovery
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Win {
    hwnd: HWND,
    title: String,
    class: String,
    pid: u32,
    /// from a config rule: open on this workspace (0-based)
    rule_ws: Option<u8>,
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

struct Collector {
    tileable: Vec<Win>,
    /// Windows that run fullscreen / borderless fullscreen (tracked per workspace).
    fullscreen: Vec<Win>,
    skipped: Vec<(Win, &'static str)>,
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

enum Verdict {
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
fn classify(hwnd: HWND, rules: &[Rule]) -> Verdict {
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

fn collect() -> Collector {
    if profile_on() {
        COLLECT_CALLS.fetch_add(1, Ordering::Relaxed);
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

fn work_area() -> RECT {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}

// ---------------------------------------------------------------------------
// layout
// ---------------------------------------------------------------------------

fn visible_rect(hwnd: HWND) -> Option<RECT> {
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

/// A window that covers its whole monitor and has no title bar (a game in exclusive or borderless
/// fullscreen, a fullscreen video player, ...). Such windows are never tiled, so they stay
/// fullscreen; a normal titled window is never reported here.
fn is_fullscreen(hwnd: HWND) -> bool {
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

/// Brings `fs_ws` / `fs_hidden` in sync with the windows that are really on screen now.
/// A window that is minimized is kept (it may be one we hid ourselves, and its geometry can no
/// longer be detected reliably); a visible window that is no longer fullscreen is dropped.
fn reconcile_fullscreen(s: &mut State, wins: &[Win]) {
    let detected: HashSet<isize> = wins.iter().map(|w| w.hwnd.0 as isize).collect();
    s.fs_ws.retain(|h, _| {
        let hw = HWND(*h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            return false;
        }
        detected.contains(h) || unsafe { IsIconic(hw) }.as_bool()
    });
    for w in wins {
        s.fs_ws
            .entry(w.hwnd.0 as isize)
            .or_insert(w.rule_ws.unwrap_or(s.cur));
    }
    let fs_ws = &s.fs_ws;
    s.fs_hidden.retain(|h| fs_ws.contains_key(h));
}

/// Minimizes the fullscreen windows whose workspace is not shown, and restores the ones we hid
/// when their workspace becomes the current one.
fn apply_fullscreen(s: &mut State) {
    if s.paused || config::fullscreen_windows() == FullscreenWindows::Ignore {
        return;
    }
    let cur = s.cur;
    let items: Vec<(isize, u8)> = s.fs_ws.iter().map(|(h, w)| (*h, *w)).collect();
    let mut changed = false;
    for (h, ws) in items {
        let hw = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            continue;
        }
        let hidden = s.fs_hidden.contains(&h);
        let minimized = unsafe { IsIconic(hw) }.as_bool();
        match fullscreen_action(ws, cur, hidden, minimized) {
            FsAction::Show => {
                unsafe {
                    let _ = ShowWindow(hw, SW_RESTORE);
                }
                s.fs_hidden.remove(&h);
                changed = true;
            }
            FsAction::Hide => {
                unsafe {
                    let _ = ShowWindow(hw, SW_MINIMIZE);
                }
                s.fs_hidden.insert(h);
                changed = true;
            }
            FsAction::None => {}
        }
    }
    if changed {
        // our own minimize / restore must not look like the user moving windows
        s.ignore_until = Some(Instant::now() + Duration::from_millis(300));
    }
}

/// Restores every fullscreen window we minimized (pause, quit), so nothing stays hidden.
fn restore_hidden_fullscreen(s: &mut State) {
    for h in std::mem::take(&mut s.fs_hidden) {
        let hw = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hw)) }.as_bool() && unsafe { IsIconic(hw) }.as_bool() {
            unsafe {
                let _ = ShowWindow(hw, SW_RESTORE);
            }
        }
    }
}

fn place(hwnd: HWND, target: RECT) {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        // Windows 10/11 windows have an invisible resize border; compensate so the
        // *visible* frame lands exactly on `target`.
        let mut win = RECT::default();
        let mut vis = RECT::default();
        let _ = GetWindowRect(hwnd, &mut win);
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .is_ok();
        let (dl, dt, dr, db) = if ok {
            (
                vis.left - win.left,
                vis.top - win.top,
                win.right - vis.right,
                win.bottom - vis.bottom,
            )
        } else {
            (0, 0, 0, 0)
        };
        let _ = SetWindowPos(
            hwnd,
            None,
            target.left - dl,
            target.top - dt,
            (target.right - target.left) + dl + dr,
            (target.bottom - target.top) + dt + db,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

fn is_parked(hwnd: HWND) -> bool {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r.left <= PARK_X + 1000
}

/// Move a window off-screen (keeps its size).
fn park(hwnd: HWND) {
    if is_parked(hwnd) {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            PARK_X,
            PARK_Y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

const DEBOUNCE_MS: u32 = 40;
/// Frame interval of window animations (~60 fps).
const ANIM_FRAME_MS: u32 = 16;
/// Interval of live re-layout while the user drags a window edge.
const LIVE_MS: u32 = 30;
/// Delay of the check after a window was placed instantly (apps that move themselves).
const VERIFY_MS: u32 = 90;

#[derive(Default)]
struct State {
    /// Rects of the layout last applied, same indices as `last`.
    rects: Vec<RECT>,
    /// Stable window order (so the spiral does not reshuffle when focus changes).
    order: Vec<isize>,
    /// Workspace (0-based) of every known window.
    ws: HashMap<isize, u8>,
    /// Observed minimum size of a window (an app that refuses to shrink below it): its tile is
    /// grown to at least this size so it does not overlap its neighbours.
    min_size: HashMap<isize, (i32, i32)>,
    /// Workspace (0-based) of every known fullscreen window (they are never tiled).
    fs_ws: HashMap<isize, u8>,
    /// Fullscreen windows we minimized because their workspace is not shown (to restore later).
    fs_hidden: HashSet<isize>,
    /// Current workspace (0-based).
    cur: u8,
    /// Windows of the current workspace in the layout that was applied last time;
    /// if nothing changed we do not touch windows.
    last: Vec<isize>,
    /// The set of top-level windows may have changed (created / destroyed / shown / hidden /
    /// cloaked / minimized): a full `EnumWindows` walk (`retile`) is due.
    windows_changed: bool,
    paused: bool,
    timer: usize,
    /// A window of another workspace got focus (taskbar / Alt+Tab): follow it.
    follow: Option<(u8, isize)>,
    last_switch: Option<Instant>,
    /// Tiled window the user is dragging / resizing right now.
    dragging: Option<isize>,
    /// Size of its tile when the drag started (to tell a move from a resize).
    drag_size: (i32, i32),
    /// The current drag is a resize (edges moved), not a move.
    resizing: bool,
    /// While resizing: re-layout the other windows now (throttled by a short timer).
    live: bool,
    /// Split ratios per workspace (see `spiral`).
    ratios: HashMap<u8, Vec<f32>>,
    /// Free region each window of the last layout was cut out of (same indices as `last`).
    regions: Vec<RECT>,
    /// Drag finished: rebuild the layout (snap back, or after a swap).
    reflow: bool,
    /// A tiled window changed place/size on its own: pull it back.
    enforce: bool,
    /// Ignore location events until then (they are caused by our own placement).
    ignore_until: Option<Instant>,
    /// When the last resize hotkey fired (to recognise a held key).
    last_resize: Option<Instant>,
    /// Window that currently has our coloured border.
    border_on: Option<isize>,
    /// Windows currently gliding to their new tiles.
    anims: Vec<Anim>,
    /// High-resolution waitable timer that drives the animation frames (null = none).
    anim_timer: HANDLE,
    /// The next layout pass must place windows instantly (workspace switch, startup, ...).
    no_anim: bool,
    /// Registered hotkeys: (id, action)
    hotkeys: Vec<(i32, Action)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

// ---------------------------------------------------------------------------
// animations
// ---------------------------------------------------------------------------

/// One window gliding from `from` to `to` (both are *visible* frames, not window rects).
#[derive(Clone)]
struct Anim {
    hwnd: isize,
    from: RECT,
    to: RECT,
    /// invisible resize border: (left, top, right, bottom)
    border: (i32, i32, i32, i32),
    t0: Instant,
    /// Duration of this animation (scaled by the distance, see `anim_duration`).
    dur_ms: u32,
    /// Last visible frame we actually applied (to skip frames that do not move the window).
    last: Option<RECT>,
}

/// Put the *visible* frame of a window at `vis`, given its invisible border.
fn set_frame(hwnd: isize, vis: &RECT, b: (i32, i32, i32, i32)) {
    unsafe {
        let _ = SetWindowPos(
            HWND(hwnd as *mut _),
            None,
            vis.left - b.0,
            vis.top - b.1,
            (vis.right - vis.left) + b.0 + b.2,
            (vis.bottom - vis.top) + b.1 + b.3,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Moves a window to `target`: instantly, or - if `animate` and animations are enabled -
/// by gliding. Always cancels an animation that was already running for this window.
fn move_window(s: &mut State, hwnd: HWND, target: RECT, animate: bool) {
    let h = hwnd.0 as isize;
    s.anims.retain(|a| a.hwnd != h);
    let (on, ms) = config::animation();
    let busy = unsafe { IsZoomed(hwnd).as_bool() } || is_parked(hwnd);
    // if the frame timer could not be created, place windows instantly rather than freeze them
    if !animate || !on || ms == 0 || busy || s.anim_timer.is_invalid() {
        place(hwnd, target);
        observe_min_size(s, hwnd, &target);
        return;
    }
    let Some(from) = visible_rect(hwnd) else {
        place(hwnd, target);
        observe_min_size(s, hwnd, &target);
        return;
    };
    if same_rect(&from, &target, 1) {
        return; // already there: nothing to do (and nothing to repaint)
    }
    let mut win = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut win);
    }
    let border = (
        from.left - win.left,
        from.top - win.top,
        win.right - from.right,
        win.bottom - from.bottom,
    );
    let dist = (from.left - target.left)
        .abs()
        .max((from.top - target.top).abs())
        .max((from.right - target.right).abs())
        .max((from.bottom - target.bottom).abs());
    let dur_ms = anim_duration(ms, dist);
    let was_empty = s.anims.is_empty();
    s.anims.push(Anim {
        hwnd: h,
        from,
        to: target,
        border,
        t0: Instant::now(),
        dur_ms,
        last: None,
    });
    if was_empty {
        if profile_on() {
            anim_prof_reset();
        }
        arm_anim_timer(s);
    }
}

/// After a window was placed instantly, remember its actual size if the app refused to shrink
/// below it (an application minimum size). The layout then grows its tile instead of letting the
/// window overlap its neighbour. Only ever grows the remembered size: once a tile has been grown
/// to fit, the window also fits it, so forgetting the minimum would make the tile shrink again
/// and start an endless grow/shrink loop.
fn observe_min_size(s: &mut State, hwnd: HWND, target: &RECT) {
    let h = hwnd.0 as isize;
    let Some(vis) = visible_rect(hwnd) else {
        return;
    };
    let (vw, vh) = (vis.right - vis.left, vis.bottom - vis.top);
    let (tw, th) = (target.right - target.left, target.bottom - target.top);
    if vw > tw + 2 || vh > th + 2 {
        s.min_size.insert(h, (vw.max(tw), vh.max(th)));
    }
}

/// Creates the timer that drives animation frames. Prefers the high-resolution variant
/// (Windows 10 1803+); falls back to a normal waitable timer on older systems.
fn create_anim_timer() -> HANDLE {
    unsafe {
        CreateWaitableTimerExW(
            None,
            PCWSTR::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS.0,
        )
        .or_else(|_| CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0))
        .or_else(|_| CreateWaitableTimerW(None, false, PCWSTR::null()))
        .unwrap_or_default()
    }
}

/// (Re)arms the frame timer: the first frame fires in `ANIM_FRAME_MS`, then every `ANIM_FRAME_MS`.
fn arm_anim_timer(s: &State) {
    if s.anim_timer.is_invalid() {
        return;
    }
    let due = -(ANIM_FRAME_MS as i64) * 10_000; // relative time, in 100 ns units
    unsafe {
        let _ = SetWaitableTimer(s.anim_timer, &due, ANIM_FRAME_MS as i32, None, None, false);
    }
}

/// Stops the frame timer once nothing is animating.
fn cancel_anim_timer(s: &State) {
    if s.anim_timer.is_invalid() {
        return;
    }
    unsafe {
        let _ = CancelWaitableTimer(s.anim_timer);
    }
}

/// One animation frame (called from the frame timer).
fn anim_tick() {
    let profiling = profile_on();
    let easing = config::easing();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.anims.is_empty() {
            // The queue was emptied outside `anim_tick` (e.g. a drag started): stop the timer so
            // it does not keep firing at 60 Hz doing nothing.
            cancel_anim_timer(&s);
            return;
        }
        let now = Instant::now();
        let mut keep = Vec::with_capacity(s.anims.len());
        let mut setpos = Duration::ZERO;
        for mut a in std::mem::take(&mut s.anims) {
            if !unsafe { IsWindow(Some(HWND(a.hwnd as *mut _))) }.as_bool() {
                continue; // closed while gliding
            }
            let dur = (a.dur_ms.max(1) as f32) / 1000.0;
            let t = (now - a.t0).as_secs_f32() / dur;
            let (rect, done) = if t >= 1.0 {
                (a.to, true) // exact final position
            } else {
                (lerp_rect(&a.from, &a.to, ease(easing, t)), false)
            };
            // skip frames that would not move the window (the tail of an ease-out is almost flat)
            if a.last.as_ref().is_none_or(|l| !same_rect(l, &rect, 0)) {
                let ts = profiling.then(Instant::now);
                set_frame(a.hwnd, &rect, a.border);
                if let Some(ts) = ts {
                    setpos += ts.elapsed();
                }
                a.last = Some(rect);
            }
            if !done {
                keep.push(a);
            }
        }
        s.anims = keep;
        // our own moves must not look like the user moving windows
        s.ignore_until = Some(now + Duration::from_millis(300));
        if s.anims.is_empty() {
            cancel_anim_timer(&s);
        }
        if profiling {
            anim_prof_frame(now, setpos.as_secs_f32() * 1e6);
            if s.anims.is_empty() {
                anim_prof_finish(now);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// focus border
// ---------------------------------------------------------------------------

/// DWM value meaning "use the system's default border colour".
const DWM_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

fn set_border(hwnd: isize, color: Option<u32>) {
    let hw = HWND(hwnd as *mut _);
    if !unsafe { IsWindow(Some(hw)) }.as_bool() {
        return;
    }
    let c = color.unwrap_or(DWM_COLOR_DEFAULT);
    unsafe {
        // Windows 11 only; on older systems the call just fails and nothing happens
        let _ = DwmSetWindowAttribute(
            hw,
            DWMWA_BORDER_COLOR,
            &c as *const u32 as *const _,
            size_of::<u32>() as u32,
        );
    }
}

/// Colour the border of `focus` (if it is a tiled window of the current workspace) and take it
/// away from the window that had it before.
fn update_border(s: &mut State, focus: isize) {
    let color = config::border();
    // a tiled window of the current workspace: in the applied layout, or (should the layout list
    // be momentarily stale) a known window that belongs to the current workspace
    let tiled = s.last.contains(&focus) || s.ws.get(&focus) == Some(&s.cur);
    let target = (color.is_some() && !s.paused && tiled).then_some(focus);
    if s.border_on != target {
        if let Some(prev) = s.border_on.take() {
            set_border(prev, None);
        }
        s.border_on = target;
    }
    // always (re-)apply: a window that lost its colour while it was moved gets it back
    if let (Some(h), Some(c)) = (target, color) {
        set_border(h, Some(c));
    }
}

/// Re-evaluate which window should have the border (after layout / pause / config changes).
fn refresh_border() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    STATE.with(|s| update_border(&mut s.borrow_mut(), fg));
}

/// Remove our border colour from every window (pause, quit, colour change).
fn clear_border() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(prev) = s.border_on.take() {
            set_border(prev, None);
        }
    });
}

/// (current workspace 0-based, paused) - for the tray.
pub fn ui_state() -> (u8, bool) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.cur, s.paused)
    })
}

/// The screen work area changed (display mode, taskbar size / auto-hide): rebuild the layout.
pub fn request_relayout() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.reflow = true;
        s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
    });
}

pub fn action_name(a: Action) -> String {
    let dir = |d: Dir| match d {
        Dir::Left => "left",
        Dir::Down => "down",
        Dir::Up => "up",
        Dir::Right => "right",
    };
    match a {
        Action::Focus(d) => format!("focus_{}", dir(d)),
        Action::Swap(d) => format!("swap_{}", dir(d)),
        Action::Workspace(n) => format!("workspace_{}", n + 1),
        Action::MoveTo(n) => format!("move_to_workspace_{}", n + 1),
        Action::Resize { width, grow } => format!(
            "{}_{}",
            if grow { "grow" } else { "shrink" },
            if width { "width" } else { "height" }
        ),
        Action::Minimize => "minimize".into(),
        Action::Pause => "pause".into(),
        Action::Quit => "quit".into(),
        Action::Retile => "retile".into(),
        Action::ReloadConfig => "reload_config".into(),
        Action::EditConfig => "edit_config".into(),
    }
}

// ---------------------------------------------------------------------------
// hotkeys
// ---------------------------------------------------------------------------

/// (Re)registers every hotkey from the config. While paused only the "essential" ones stay
/// registered, so the other Alt+... keys go back to the applications.
/// Returns descriptions of bindings that could not be registered.
fn register_hotkeys() -> Vec<String> {
    let cfg = config::current();
    let (old, paused) = STATE.with(|s| {
        let s = s.borrow();
        (
            s.hotkeys.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            s.paused,
        )
    });
    for id in old {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
    let mut new = Vec::new();
    let mut failed = Vec::new();
    let mut next = 100;
    for (action, spec) in &cfg.bindings {
        let essential = matches!(
            action,
            Action::Pause | Action::Quit | Action::ReloadConfig | Action::EditConfig
        );
        if paused && !essential {
            continue;
        }
        match config::parse_hotkey(spec) {
            Ok((mods, vk)) => {
                let id = next;
                next += 1;
                // resize keys keep firing while held; everything else fires once per press
                let flags = if matches!(action, Action::Resize { .. }) {
                    mods
                } else {
                    mods | MOD_NOREPEAT
                };
                if unsafe { RegisterHotKey(None, id, flags, vk) }.is_ok() {
                    new.push((id, *action));
                } else {
                    failed.push(format!(
                        "{} = {} (already used by another program?)",
                        action_name(*action),
                        spec
                    ));
                }
            }
            Err(e) => failed.push(e),
        }
    }
    STATE.with(|s| s.borrow_mut().hotkeys = new);
    for f in &failed {
        log(format!("hotkey not registered: {f}"));
    }
    failed
}

// ---------------------------------------------------------------------------
// event handling
// ---------------------------------------------------------------------------

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole-window events (OBJID_WINDOW == 0, no child element).
    if id_object != 0 || id_child != 0 || hwnd.is_invalid() {
        return;
    }
    let h = hwnd.0 as isize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        match event {
            EVENT_SYSTEM_MOVESIZESTART => {
                // let the user drag freely; we decide what to do while / when they let go
                if !s.paused {
                    if let Some(k) = s.last.iter().position(|x| *x == h) {
                        if k < s.rects.len() {
                            s.dragging = Some(h);
                            s.anims.retain(|a| a.hwnd != h);
                            s.resizing = false;
                            s.live = false;
                            // Remember the window's *actual* size, not the tile's: an app with a
                            // minimum size (or one that resized itself) can be larger than its
                            // tile, and comparing a plain move against the tile size would then be
                            // mistaken for a resize, which corrupts the split ratios.
                            s.drag_size = visible_rect(hwnd)
                                .map(|v| (v.right - v.left, v.bottom - v.top))
                                .unwrap_or_else(|| {
                                    let t = s.rects[k];
                                    (t.right - t.left, t.bottom - t.top)
                                });
                        }
                    }
                }
                return;
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                if s.dragging == Some(h) {
                    s.dragging = None;
                    if s.resizing {
                        // final size: take the last position of the dragged edges
                        if let (Some(k), Some(vis)) =
                            (s.last.iter().position(|x| *x == h), visible_rect(hwnd))
                        {
                            let cur = s.cur;
                            let (tiles, regions) = (s.rects.clone(), s.regions.clone());
                            let mut rs = s.ratios.remove(&cur).unwrap_or_default();
                            apply_resize(&mut rs, &tiles, &regions, k, vis);
                            s.ratios.insert(cur, rs);
                        }
                        s.resizing = false;
                        s.live = false;
                    } else {
                        // dropped onto another tile? -> swap the two windows; otherwise snap back
                        let mut p = POINT::default();
                        let _ = GetCursorPos(&mut p);
                        let hit = s.rects.iter().position(|r| {
                            p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
                        });
                        if let Some(i) = hit {
                            if i < s.last.len() && s.last[i] != h {
                                let other = s.last[i];
                                let (ia, ib) = (
                                    s.order.iter().position(|x| *x == h),
                                    s.order.iter().position(|x| *x == other),
                                );
                                if let (Some(ia), Some(ib)) = (ia, ib) {
                                    s.order.swap(ia, ib);
                                }
                            }
                        }
                    }
                    s.reflow = true;
                } else {
                    return;
                }
            }
            EVENT_OBJECT_LOCATIONCHANGE => {
                if s.paused {
                    return;
                }
                if let Some(d) = s.dragging {
                    // during a drag only the dragged window matters: is it being resized?
                    if d == h {
                        if let Some(vis) = visible_rect(hwnd) {
                            let (w, hh) = (vis.right - vis.left, vis.bottom - vis.top);
                            if !s.resizing
                                && ((w - s.drag_size.0).abs() > RESIZE_PX
                                    || (hh - s.drag_size.1).abs() > RESIZE_PX)
                            {
                                s.resizing = true;
                            }
                        }
                        if s.resizing {
                            s.live = true;
                            // throttle (not debounce): one live update every ~30 ms
                            if s.timer == 0 {
                                s.timer = SetTimer(None, 0, LIVE_MS, None);
                            }
                        }
                    }
                    return;
                }
                // a window moved/resized outside of a drag (and not because of us)
                let quiet = s.ignore_until.is_none_or(|t| Instant::now() >= t);
                if !quiet {
                    return;
                }
                if s.last.contains(&h) {
                    s.enforce = true; // a tiled window moved on its own: pull it back
                } else if h == GetForegroundWindow().0 as isize {
                    // an untracked window changed size, e.g. a game just left fullscreen and must
                    // be tiled again: reconcile the window set
                    s.windows_changed = true;
                }
            }
            EVENT_OBJECT_SHOW => {
                s.windows_changed = true;
                // a window just appeared: put it into its tile right away, without waiting
                if !s.paused && s.dragging.is_none() && fast_add(&mut s, hwnd) {
                    // some apps move/resize themselves right after showing: check again shortly
                    s.enforce = true;
                    s.timer = SetTimer(None, s.timer, VERIFY_MS, None);
                    return;
                }
            }
            EVENT_SYSTEM_FOREGROUND => {
                update_border(&mut s, h);
                let recently = s
                    .last_switch
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
                // a window of another workspace got focus (taskbar / Alt+Tab), tiled or fullscreen
                let ws = s.ws.get(&h).copied().or_else(|| s.fs_ws.get(&h).copied());
                match ws {
                    Some(w) if w != s.cur && !s.paused && !recently => {
                        s.follow = Some((w, h));
                        s.windows_changed = true;
                    }
                    _ => return, // focus change inside the current workspace: nothing to do
                }
            }
            EVENT_OBJECT_CREATE
            | EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_CLOAKED
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND => {
                s.windows_changed = true;
            }
            _ => {}
        }
        // (Re)arm the debounce timer: bursts of events cause a single re-tile.
        s.timer = SetTimer(None, s.timer, DEBOUNCE_MS, None);
    });
}

/// Does the visible frame of `hwnd` differ from `target` by more than a few pixels?
fn off_target(hwnd: HWND, target: &RECT) -> bool {
    let mut vis = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
        .is_ok()
    };
    if !ok {
        return false;
    }
    let t = 4;
    (vis.left - target.left).abs() > t
        || (vis.top - target.top).abs() > t
        || (vis.right - target.right).abs() > t
        || (vis.bottom - target.bottom).abs() > t
}

/// Two resize hotkeys closer together than this are treated as one held key.
const HOLD_GAP_MS: u128 = 200;

/// Hotkey: grow / shrink the focused window by `resize_step` pixels; the neighbours adapt.
/// Holding the key keeps resizing (smaller, instant steps).
fn resize_focused(width: bool, grow: bool) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused
            || s.dragging.is_some()
            || s.rects.len() != s.last.len()
            || s.regions.len() != s.last.len()
        {
            return;
        }
        let n = s.last.len();
        let Some(k) = s.last.iter().position(|h| *h == fg) else {
            return;
        };
        let Some((j, sign)) = split_for(k, n, width) else {
            return;
        };
        let gap = config::gap();
        let q = s.regions[j];
        let span = if j % 2 == 0 {
            q.right - q.left - gap
        } else {
            q.bottom - q.top - gap
        };
        let held = s
            .last_resize
            .is_some_and(|t| t.elapsed().as_millis() < HOLD_GAP_MS);
        s.last_resize = Some(Instant::now());
        let step = effective_step(config::resize_step(), held) as f32 / span.max(1) as f32;
        let delta = step * sign * if grow { 1.0 } else { -1.0 };
        let cur = s.cur;
        let ratios = {
            let rs = s.ratios.entry(cur).or_default();
            rs.resize(n - 1, 0.5);
            let before = rs[j];
            rs[j] = (before + delta).clamp(MIN_RATIO, MAX_RATIO);
            if (rs[j] - before).abs() < 0.0005 {
                return; // already at the limit
            }
            rs.clone()
        };
        let (tiles, regions) = spiral_with_mins(work_area(), &s.last, &ratios, &s.min_size);
        let last = s.last.clone();
        for (i, h) in last.iter().enumerate() {
            // single taps glide; a held key resizes instantly so it stays responsive
            move_window(&mut s, HWND(*h as *mut _), tiles[i], !held);
        }
        s.rects = tiles;
        s.regions = regions;
        s.ignore_until = Some(Instant::now() + Duration::from_millis(500));
    });
    refresh_border();
}

/// While the user drags an edge of a tiled window: recompute the split ratios from the dragged
/// window's current frame and move all the *other* windows right away.
fn live_resize() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || !s.resizing {
            return;
        }
        let Some(h) = s.dragging else { return };
        let Some(k) = s.last.iter().position(|x| *x == h) else {
            return;
        };
        let Some(vis) = visible_rect(HWND(h as *mut _)) else {
            return;
        };
        let cur = s.cur;
        let (tiles, regions) = (s.rects.clone(), s.regions.clone());
        let mut rs = s.ratios.remove(&cur).unwrap_or_default();
        let changed = apply_resize(&mut rs, &tiles, &regions, k, vis);
        if changed {
            let (new_tiles, new_regions) = spiral_with_mins(work_area(), &s.last, &rs, &s.min_size);
            let last = s.last.clone();
            for (i, hw) in last.iter().enumerate() {
                if *hw != h {
                    move_window(&mut s, HWND(*hw as *mut _), new_tiles[i], false);
                }
            }
            s.rects = new_tiles;
            s.regions = new_regions;
            s.ignore_until = Some(Instant::now() + Duration::from_millis(400));
        }
        s.ratios.insert(cur, rs);
    });
}

/// Pulls every tiled window of the current workspace back to its tile. Returns true if a window
/// that is now fullscreen was found: the caller must re-tile so the layout drops it.
fn enforce_layout() -> bool {
    let _t = Timer::new("enforce_layout");
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || s.dragging.is_some() || s.rects.len() != s.last.len() {
            return false;
        }
        let mut moved = 0;
        let mut fullscreen = false;
        let (last, rects) = (s.last.clone(), s.rects.clone());
        for (h, r) in last.iter().zip(rects.iter()) {
            let hw = HWND(*h as *mut _);
            if unsafe { IsZoomed(hw).as_bool() || IsIconic(hw).as_bool() } {
                continue; // maximized / minimized by the user: leave it alone
            }
            if is_fullscreen(hw) {
                fullscreen = true; // a game went fullscreen: let it be, re-tile drops it
                continue;
            }
            if s.anims.iter().any(|a| a.hwnd == *h) {
                continue; // already on its way
            }
            if off_target(hw, r) {
                // an app with a minimum size cannot shrink to a small tile: retrying every time
                // only makes it flicker, so leave it as it is (it overflows its tile instead)
                if let Some(vis) = visible_rect(hw) {
                    let bigger = (vis.right - vis.left) > (r.right - r.left) + 4
                        || (vis.bottom - vis.top) > (r.bottom - r.top) + 4;
                    if bigger {
                        continue;
                    }
                }
                move_window(&mut s, hw, *r, true);
                moved += 1;
            }
        }
        if moved > 0 {
            log(format!("snapped {} window(s) back to their tiles", moved));
            s.ignore_until = Some(Instant::now() + Duration::from_millis(500));
        }
        fullscreen
    })
}

/// A window was just shown. If it is a new, tileable window of the current workspace, put it
/// into its tile *right now* (the others glide aside) instead of waiting for the debounce timer.
/// Returns false if the normal (slower, complete) path should handle it.
fn fast_add(s: &mut State, hwnd: HWND) -> bool {
    let _t = Timer::new("fast_add");
    let h = hwnd.0 as isize;
    if s.ws.contains_key(&h) {
        return false; // known window shown again: the normal path decides
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    if style & WS_CHILD.0 != 0 {
        return false; // controls and other child windows
    }
    if s.rects.len() != s.last.len() || s.regions.len() != s.last.len() {
        return false;
    }
    let rules = config::rules();
    let w = match classify(hwnd, &rules) {
        Verdict::Tileable(w) => w,
        _ => return false, // not tileable (yet): e.g. title not set, dialog, tool window
    };
    let cur = s.cur;
    if w.rule_ws.is_some_and(|n| n != cur) {
        return false; // belongs to another workspace: the normal path parks it
    }
    s.ws.insert(h, cur);
    if !s.order.contains(&h) {
        s.order.push(h);
    }
    let mut layout = s.last.clone();
    layout.push(h);
    let ratios = {
        let rs = s.ratios.entry(cur).or_default();
        rs.resize(layout.len() - 1, 0.5);
        rs.clone()
    };
    let (rects, regions) = spiral_with_mins(work_area(), &layout, &ratios, &s.min_size);
    for (i, hh) in layout.iter().enumerate() {
        // the new window appears in its tile instantly; the others glide to make room
        move_window(s, HWND(*hh as *mut _), rects[i], *hh != h);
    }
    s.rects = rects;
    s.regions = regions;
    s.last = layout;
    s.ignore_until = Some(Instant::now() + Duration::from_millis(60));
    log(format!(
        "workspace {}: new window placed instantly ({} tiled)",
        cur + 1,
        s.last.len()
    ));
    true
}

fn retile() {
    let _t = Timer::new("retile");
    let c = collect();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused {
            return;
        }
        // a drag whose end event we missed (e.g. the window was destroyed) must not stay stuck:
        // while `dragging` is set, snapping windows back and live resizing are disabled
        if let Some(d) = s.dragging {
            if !unsafe { IsWindow(Some(HWND(d as *mut _))) }.as_bool() {
                s.dragging = None;
            }
        }
        s.windows_changed = false; // this pass reconciles the whole window set
        reconcile_fullscreen(&mut s, &c.fullscreen);
        apply_fullscreen(&mut s);
        let present: Vec<isize> = c.tileable.iter().map(|w| w.hwnd.0 as isize).collect();
        let cur = s.cur;
        // windows we have not seen before are placed instantly; known ones glide
        let fresh: Vec<isize> = present
            .iter()
            .copied()
            .filter(|h| !s.ws.contains_key(h))
            .collect();
        let animate_all = !std::mem::take(&mut s.no_anim);
        // forget windows that are gone (their observed minimum size goes with them)
        s.min_size
            .retain(|h, _| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        // forget windows that are gone; new windows join the current workspace
        // (or the workspace a config rule asks for)
        s.ws.retain(|h, _| present.contains(h));
        for w in &c.tileable {
            s.ws.entry(w.hwnd.0 as isize)
                .or_insert(w.rule_ws.unwrap_or(cur));
        }
        // keep known windows in their old order, drop the gone ones, append the new ones
        let mut order: Vec<isize> = s
            .order
            .iter()
            .copied()
            .filter(|h| present.contains(h))
            .collect();
        for h in &present {
            if !order.contains(h) {
                order.push(*h);
            }
        }
        s.order = order.clone();
        // make sure windows of other workspaces stay out of sight
        for h in &order {
            if s.ws[h] != cur {
                park(HWND(*h as *mut _));
            }
        }
        let layout: Vec<isize> = order.into_iter().filter(|h| s.ws[h] == cur).collect();
        if layout == s.last {
            return; // same windows, same order: leave them alone (respects manual tweaks)
        }
        let ratios = {
            let rs = s.ratios.entry(cur).or_default();
            rs.resize(layout.len().saturating_sub(1), 0.5);
            rs.clone()
        };
        let (rects, regions) = spiral_with_mins(work_area(), &layout, &ratios, &s.min_size);
        s.rects = rects.clone();
        s.regions = regions;
        for (h, r) in layout.iter().zip(rects) {
            if s.dragging == Some(*h) {
                continue; // the user is dragging it right now: leave it under the cursor
            }
            let animate = animate_all && !fresh.contains(h);
            move_window(&mut s, HWND(*h as *mut _), r, animate);
        }
        log(format!(
            "workspace {}: tiled {} window(s)",
            cur + 1,
            layout.len()
        ));
        s.last = layout;
        s.ignore_until = Some(Instant::now() + Duration::from_millis(500));
    });
    // the focused window may only now have joined the layout: make sure it has the border
    refresh_border();
}

fn focus_hwnd(h: isize) {
    unsafe {
        let _ = SetForegroundWindow(HWND(h as *mut _));
    }
    // our own SetForegroundWindow produces a foreground event that the hook skips
    // (WINEVENT_SKIPOWNPROCESS), so update the border here as well
    STATE.with(|s| update_border(&mut s.borrow_mut(), h));
}

fn focus_first_or_desktop() {
    let first = STATE.with(|s| s.borrow().last.first().copied());
    match first {
        Some(h) => focus_hwnd(h),
        // empty workspace: take focus away from the (now hidden) old window
        None => focus_hwnd(unsafe { GetShellWindow() }.0 as isize),
    }
}

fn focus_or_swap(dir: Dir, swap: bool) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let mut do_retile = false;
    let mut to_focus: Option<isize> = None;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || s.last.is_empty() || s.rects.len() != s.last.len() {
            return;
        }
        let Some(cur) = s.last.iter().position(|h| *h == fg) else {
            // foreground window is not tiled here: just jump to the first tiled one
            if !swap {
                to_focus = Some(s.last[0]);
            }
            return;
        };
        let Some(to) = neighbour(&s.rects, cur, dir) else {
            return;
        };
        if swap {
            let (a, b) = (s.last[cur], s.last[to]);
            let (ia, ib) = (
                s.order.iter().position(|h| *h == a),
                s.order.iter().position(|h| *h == b),
            );
            if let (Some(ia), Some(ib)) = (ia, ib) {
                s.order.swap(ia, ib);
            }
            s.last.clear(); // force a re-layout with the new order
            do_retile = true;
        } else {
            to_focus = Some(s.last[to]);
        }
    });
    if let Some(h) = to_focus {
        focus_hwnd(h);
    }
    if do_retile {
        retile();
    }
}

fn switch_ws(n: u8, focus: Option<isize>) {
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || n == s.cur || n >= WS_COUNT {
            return false;
        }
        s.cur = n;
        s.last.clear();
        s.last_switch = Some(Instant::now());
        true
    });
    if !changed {
        return;
    }
    retile();
    match focus {
        Some(h) => focus_hwnd(h),
        None => focus_workspace(n),
    }
    tray::update();
    log(format!("switched to workspace {}", n + 1));
}

/// Focuses the fullscreen window of workspace `n` if it has one, otherwise the first tiled one.
fn focus_workspace(n: u8) {
    let fs = STATE.with(|s| {
        s.borrow()
            .fs_ws
            .iter()
            .find(|(h, w)| **w == n && !unsafe { IsIconic(HWND(**h as *mut _)) }.as_bool())
            .map(|(h, _)| *h)
    });
    match fs {
        Some(h) => focus_hwnd(h),
        None => focus_first_or_desktop(),
    }
}

fn move_to_ws(n: u8) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    // which kind of window are we moving?
    let fullscreen = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || n >= WS_COUNT {
            return None;
        }
        if let Some(&w) = s.fs_ws.get(&fg) {
            if w == n {
                return None;
            }
            s.fs_ws.insert(fg, n);
            return Some(true);
        }
        if !s.ws.contains_key(&fg) || s.ws[&fg] == n {
            return None;
        }
        s.ws.insert(fg, n);
        s.last.clear();
        s.last_switch = Some(Instant::now());
        Some(false)
    });
    let Some(fullscreen) = fullscreen else {
        return;
    };
    let cur = STATE.with(|s| s.borrow().cur);
    if fullscreen {
        // retile syncs visibility: hidden if `n` is not the current workspace, shown if it is
        retile();
        if n == cur {
            focus_hwnd(fg);
        } else {
            focus_first_or_desktop();
        }
    } else {
        if n != cur {
            park(HWND(fg as *mut _));
        }
        retile();
        focus_first_or_desktop();
    }
    log(format!("moved a window to workspace {}", n + 1));
}

/// Windows left parked off-screen (e.g. after a crash) are brought back into view.
fn recover_parked() {
    let c = collect();
    for w in &c.tileable {
        if is_parked(w.hwnd) {
            unsafe {
                let _ = SetWindowPos(
                    w.hwnd,
                    None,
                    100,
                    100,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
            log("recovered a window that was parked off-screen");
        }
    }
}

/// On exit: put every window of every workspace into the current workspace and tile them.
fn bring_everything_back() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let cur = s.cur;
        for v in s.ws.values_mut() {
            *v = cur;
        }
        for v in s.fs_ws.values_mut() {
            *v = cur; // so the fullscreen windows are not hidden again below
        }
        s.last.clear();
        s.paused = false;
        s.no_anim = true; // the process is about to exit: no gliding
        restore_hidden_fullscreen(&mut s);
    });
    retile();
}

fn toggle_pause() {
    let paused = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.paused = !s.paused;
        s.paused
    });
    log(if paused { "paused" } else { "resumed" });
    register_hotkeys();
    tray::update();
    if paused {
        clear_border();
        STATE.with(|s| restore_hidden_fullscreen(&mut s.borrow_mut()));
    }
    if !paused {
        STATE.with(|s| s.borrow_mut().last.clear());
        retile();
        refresh_border();
    }
}

fn reload_config() {
    match config::load() {
        Ok(c) => {
            config::set(c);
            let failed = register_hotkeys();
            STATE.with(|s| s.borrow_mut().last.clear());
            retile();
            tray::update();
            clear_border(); // the colour (or the on/off switch) may have changed
            refresh_border();
            log("config reloaded");
            if !failed.is_empty() {
                notify(
                    "tile-rs: some hotkeys were not registered",
                    &failed.join("\n"),
                );
            }
        }
        Err(e) => {
            log(format!("config error: {e}"));
            notify("tile-rs: config error (the old settings stay active)", &e);
        }
    }
}

/// Hotkey: minimize the focused window (a fullscreen game, or a normal tiled window).
fn minimize_focused() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let h = fg.0 as isize;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        // the user chose to minimize it: do not restore it on the next workspace switch
        s.fs_hidden.remove(&h);
        s.ignore_until = Some(Instant::now() + Duration::from_millis(300));
    });
    unsafe {
        let _ = ShowWindow(fg, SW_MINIMIZE);
    }
    log("minimized the focused window");
}

pub fn dispatch(a: Action) {
    match a {
        Action::Focus(d) => focus_or_swap(d, false),
        Action::Swap(d) => focus_or_swap(d, true),
        Action::Workspace(n) => switch_ws(n, None),
        Action::MoveTo(n) => move_to_ws(n),
        Action::Resize { width, grow } => resize_focused(width, grow),
        Action::Minimize => minimize_focused(),
        Action::Pause => toggle_pause(),
        Action::Quit => unsafe { PostQuitMessage(0) },
        Action::Retile => {
            // also resets the sizes of the current workspace to equal halves
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let cur = s.cur;
                s.ratios.remove(&cur);
                s.last.clear();
            });
            retile();
        }
        Action::ReloadConfig => reload_config(),
        Action::EditConfig => {
            config::ensure_default();
            tray::open(
                "notepad.exe",
                &format!("\"{}\"", config::config_path().display()),
            );
        }
    }
}

/// Handles the debounce / live / verify timer (a classic `SetTimer` message).
fn handle_debounce_timer(wparam: usize) {
    let timer = STATE.with(|s| s.borrow().timer);
    if wparam != timer {
        return;
    }
    unsafe {
        let _ = KillTimer(None, timer);
    }
    let (follow, reflow, enforce, live) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.timer = 0;
        let reflow = std::mem::take(&mut s.reflow);
        let enforce = std::mem::take(&mut s.enforce);
        let live = std::mem::take(&mut s.live);
        if reflow {
            s.last.clear(); // force a fresh layout (snap back / apply a swap)
        }
        (s.follow.take(), reflow, enforce, live)
    });
    match follow {
        Some((w, h)) => switch_ws(w, Some(h)),
        None if reflow => retile(),
        None if live => live_resize(),
        None if enforce => {
            let fullscreen = enforce_layout();
            // only walk all windows again if the set of windows really changed (or one of them
            // just went fullscreen and must be dropped from the layout)
            let changed =
                fullscreen || STATE.with(|s| std::mem::take(&mut s.borrow_mut().windows_changed));
            if changed {
                retile();
            }
        }
        None => retile(),
    }
}

// ---------------------------------------------------------------------------
// start-up
// ---------------------------------------------------------------------------

/// `interactive` = started by double-click (launcher): may show dialogs.
fn start(interactive: bool) {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // single instance
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\tile-rs-single-instance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        log("(a second instance tried to start and exited)");
        if interactive {
            msgbox(
                "tile-rs",
                "tile-rs is already running.\n\nLook for its icon in the system tray (click ^ near the clock).",
                false,
            );
        }
        return;
    }
    log_reset();

    // config (created on first run; settings added by newer versions are appended to old files)
    let first_run = config::ensure_default();
    if !first_run && config::migrate_file() {
        log("config: added settings introduced by this version (backup: config.toml.bak)");
    }
    match config::load() {
        Ok(c) => config::set(c),
        Err(e) => {
            log(format!("config error: {e}"));
            notify("tile-rs: config error (built-in defaults are used)", &e);
        }
    }

    if interactive && first_run {
        let yes = msgbox(
            "tile-rs - first start",
            "Start tile-rs automatically when you sign in to Windows?\n\n\
             (You can change this later in the tray menu: \"Start with Windows\".)",
            true,
        );
        if yes {
            set_autostart(true);
        }
        msgbox(
            "tile-rs",
            "tile-rs is running.\n\n\
             - Its icon is in the system tray (click ^ near the clock). Right-click it for the menu.\n\
             - Hotkeys and window rules: tray menu -> \"Edit config\".\n\
             - Pause / resume: Ctrl+Alt+P.  Quit: Ctrl+Alt+Q.",
            false,
        );
    }

    SKIP_ZOOMED.store(true, Ordering::Relaxed);
    recover_parked();
    unsafe {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let hook = |lo: u32, hi: u32| {
            let _ = SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, flags);
        };
        hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE); // created / destroyed / shown / hidden
        hook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND);
        hook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED); // other virtual desktop
        hook(EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND); // drag / resize
        hook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE);
        hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND);
    }
    let failed = register_hotkeys();
    tray::create();
    let anim_timer = create_anim_timer();
    STATE.with(|s| s.borrow_mut().anim_timer = anim_timer);
    log("tile-rs running");
    if !failed.is_empty() {
        notify(
            "tile-rs: some hotkeys were not registered",
            &failed.join("\n"),
        );
    }
    retile();
    refresh_border();

    // One thread, one message loop. The animation frames come from a high-resolution waitable
    // timer (not WM_TIMER, which is the lowest priority and stutters while the mouse moves);
    // the debounce / live / verify timers stay ordinary SetTimer messages.
    unsafe {
        let mut msg = MSG::default();
        let handles = [anim_timer];
        loop {
            let r = MsgWaitForMultipleObjectsEx(
                Some(&handles),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if r == WAIT_FAILED {
                log("MsgWaitForMultipleObjectsEx failed; stopping the message loop");
                break;
            }
            if r == WAIT_OBJECT_0 {
                anim_tick();
            }
            let mut quit = false;
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = true;
                    break;
                }
                if msg.message == WM_TIMER {
                    handle_debounce_timer(msg.wParam.0);
                    continue;
                }
                if msg.message == WM_HOTKEY {
                    let id = msg.wParam.0 as i32;
                    let action = STATE.with(|s| {
                        s.borrow()
                            .hotkeys
                            .iter()
                            .find(|(i, _)| *i == id)
                            .map(|(_, a)| *a)
                    });
                    if let Some(a) = action {
                        dispatch(a);
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if quit {
                break;
            }
        }
    }
    clear_border();
    bring_everything_back();
    tray::remove();
    unsafe {
        let _ = CloseHandle(anim_timer);
    }
    log("bye");
}

fn console_out() -> Box<dyn Write> {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    match std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        Ok(f) => Box::new(f),
        Err(_) => Box::new(std::io::stdout()),
    }
}

fn main() {
    if std::env::var_os("TILE_RS_PROFILE").is_some() {
        PROFILE.store(true, Ordering::Relaxed);
    }
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        None => start(true),
        Some("run") | Some("--autostart") => start(false),
        Some("--version") | Some("-V") => {
            let mut out = console_out();
            let _ = writeln!(out, "tile-rs {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") => {
            let mut out = console_out();
            let _ = writeln!(
                out,
                "tile-rs {} - spiral tiling window manager for Windows",
                env!("CARGO_PKG_VERSION")
            );
            let _ = writeln!(out, "\nUSAGE:\n  tile-rs.exe              start (asks about autostart on the first run)");
            let _ = writeln!(
                out,
                "  tile-rs.exe --autostart  start silently (used by the autostart entry)"
            );
            let _ = writeln!(out, "  tile-rs.exe list         show which windows would be tiled, and why others are not");
            let _ = writeln!(
                out,
                "  tile-rs.exe tile         arrange the current windows once and exit"
            );
            let _ = writeln!(out, "  tile-rs.exe --version    print the version");
            let _ = writeln!(out, "\nConfig: {}", config::config_path().display());
        }
        Some("list") | Some("tile") => {
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            if let Ok(c) = config::load() {
                config::set(c);
            }
            let mut out = console_out();
            let c = collect();
            if cmd.as_deref() == Some("list") {
                let _ = writeln!(out, "TILEABLE ({}):", c.tileable.len());
                for w in &c.tileable {
                    let _ = writeln!(out, "  [{:>6}] {:<28} {}", w.pid, w.class, w.title);
                }
                if !c.fullscreen.is_empty() {
                    let _ = writeln!(out, "\nFULLSCREEN ({}):", c.fullscreen.len());
                    for w in &c.fullscreen {
                        let _ = writeln!(
                            out,
                            "  [{:>6}] {:<28} {}  <- {}",
                            w.pid, w.class, w.title, FULLSCREEN_REASON
                        );
                    }
                }
                let _ = writeln!(out, "\nSKIPPED ({}):", c.skipped.len());
                for (w, r) in &c.skipped {
                    let _ = writeln!(
                        out,
                        "  [{:>6}] {:<28} {}  <- {}",
                        w.pid, w.class, w.title, r
                    );
                }
                let a = work_area();
                let _ = writeln!(
                    out,
                    "\nWork area: {}x{} at ({}, {})",
                    a.right - a.left,
                    a.bottom - a.top,
                    a.left,
                    a.top
                );
            } else {
                let (rects, _) = spiral(work_area(), c.tileable.len(), &[]);
                for (w, r) in c.tileable.iter().zip(rects) {
                    let _ = writeln!(
                        out,
                        "{} -> {}x{} at ({}, {})",
                        w.title,
                        r.right - r.left,
                        r.bottom - r.top,
                        r.left,
                        r.top
                    );
                    place(w.hwnd, r);
                }
            }
        }
        Some(_) => {
            let mut out = console_out();
            let _ = writeln!(
                out,
                "usage: tile-rs [--autostart | list | tile | --version | --help]"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SUBKEY: &str = r"Software\tile-rs-test";
    const TEST_VALUE: &str = "tile-rs-test";

    #[test]
    fn autostart_registry_roundtrip() {
        let _ = set_autostart_at(TEST_SUBKEY, TEST_VALUE, None); // start clean
        assert!(!autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
        assert!(set_autostart_at(
            TEST_SUBKEY,
            TEST_VALUE,
            Some("\"C:\\tile-rs.exe\" --autostart")
        ));
        assert!(autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
        assert!(set_autostart_at(TEST_SUBKEY, TEST_VALUE, None));
        assert!(!autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
    }
}
