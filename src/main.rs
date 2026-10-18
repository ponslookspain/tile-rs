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
#![windows_subsystem = "windows"]

mod config;
mod diagnostics;
mod divider;
mod event_queue;
mod geometry;
mod gpu_snapshot;
mod launcher;
mod monitor;
mod overlay;
mod prof;
mod snapshot;
mod tray;
mod update;
mod window;

use window::{classify, collect, is_fullscreen, visible_rect, Verdict, Win};

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::mem::size_of;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE,
    HWND, LPARAM, POINT, RECT, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, CancelWaitableTimer, CreateMutexW, CreateWaitableTimerExW,
    CreateWaitableTimerW, SetWaitableTimer, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
    TIMER_ALL_ACCESS,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetAncestor, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetShellWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, IsZoomed, KillTimer, MessageBoxW, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostMessageW, PostQuitMessage, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindowAsync,
    TranslateMessage, WindowFromPoint, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MENUPOPUPEND,
    EVENT_SYSTEM_MENUPOPUPSTART, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, GA_ROOT, GUITHREADINFO, GUI_INMOVESIZE,
    GWL_STYLE, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_TOPMOST, MB_YESNO, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_HOTKEY, WM_QUIT, WM_TIMER, WS_CHILD,
};

use config::{Action, Dir, FullscreenWindows, Layout, WS_COUNT};
use geometry::{
    anim_duration, apply_resize_for, ease, effective_step, fullscreen_action, lerp_rect, neighbour,
    place as layout_tiles, place_fitted, same_rect, split_for_layout, FsAction, Gaps, MAX_RATIO,
    MIN_RATIO, RESIZE_PX,
};

/// Where windows of inactive workspaces are parked (the same spot Windows uses for minimized ones).
const PARK_X: i32 = -32000;
const PARK_Y: i32 = -32000;

// ---------------------------------------------------------------------------
// logging (the release build has no console, so we log to a file)
// ---------------------------------------------------------------------------

use diagnostics::{anim_prof_finish, anim_prof_frame, anim_prof_reset, log_reset, Timer};
pub use diagnostics::{log, log_file};

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

/// How many tiles fit on one workspace for the current screen: the configured `max_tiles`, or a
/// value derived from the screen area and the comfortable minimum tile.
fn tile_budget(mi: usize) -> usize {
    let configured = config::max_tiles();
    if configured > 0 {
        return configured as usize;
    }
    let area = monitor_work(mi);
    let (mw, mh) = config::min_tile();
    let area_px = (area.right - area.left).max(1) as f64 * (area.bottom - area.top).max(1) as f64;
    let min_px = mw.max(1) as f64 * mh.max(1) as f64;
    let ratio = area_px / min_px;
    if ratio <= 1.0 {
        return 2;
    }
    (1.0 + ratio.log2()).floor().clamp(2.0, 8.0) as usize
}

/// The workspace a new window should open on: the current one if it still has room, otherwise the
/// next workspace (wrapping) with room.
fn workspace_for_new_window(s: &State, mi: usize, cur: u8) -> u8 {
    if !config::auto_overflow() {
        return cur;
    }
    let budget = tile_budget(mi);
    let count = |ws: u8| {
        s.ws.iter()
            .filter(|(h, w)| **w == ws && s.mi(**h) == mi)
            .count()
    };
    if count(cur) < budget {
        return cur;
    }
    for i in 1..WS_COUNT {
        let ws = (cur + i) % WS_COUNT;
        if count(ws) < budget {
            return ws;
        }
    }
    cur
}

/// The configured inner (between windows) and outer (screen edge) gaps.
fn gaps() -> Gaps {
    Gaps {
        inner: config::inner_gap(),
        outer: config::outer_gap(),
    }
}

/// The layout of `windows` on `area`: the configured layout with the adaptive fitting
/// (master shrink / observed per-window minimum sizes) applied (see `geometry::place_fitted`).
fn fitted(
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
) -> (Vec<RECT>, Vec<RECT>) {
    let (mw, mh) = config::min_tile();
    place_fitted(
        config::layout(),
        area,
        windows,
        ratios,
        mins,
        (mw, mh),
        gaps(),
    )
}

/// The work area (taskbar excluded) of the primary monitor, as Windows reports it.
fn raw_work_area() -> RECT {
    monitor::primary_work()
}

/// The work area (taskbar excluded) of monitor `mi`, minus the indicator bar.
fn monitor_work(mi: usize) -> RECT {
    let mut r = monitor::work(mi);
    // Each managed monitor has its own workspace indicator bar.
    if config::workspace_indicator() {
        r.top += overlay::indicator_height();
    }
    r
}

// ---------------------------------------------------------------------------
// layout
// ---------------------------------------------------------------------------

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
    let mut follow: Option<(usize, u8, isize)> = None;
    for w in wins {
        let h = w.hwnd.0 as isize;
        if s.fs_ws.contains_key(&h) {
            // A fullscreen window can be moved to another monitor while it remains fullscreen.
            // Refresh its owner while visible; when minimized, keep the last known monitor.
            if !unsafe { IsIconic(w.hwnd) }.as_bool() {
                s.fs_mon.insert(h, monitor::of_window(w.hwnd).min(MON - 1));
            }
            continue;
        }
        let mi = monitor::of_window(w.hwnd).min(MON - 1);
        // a fullscreen game / video player gets a workspace of its own, and we switch to it
        let ws = w
            .rule_ws
            .unwrap_or_else(|| next_free_workspace(s, mi, s.cur[mi]));
        s.fs_ws.insert(h, ws);
        s.fs_mon.insert(h, mi);
        if ws != s.cur[mi] && follow.is_none() {
            follow = Some((mi, ws, h));
        }
    }
    if follow.is_some() {
        s.follow = follow;
    }
    let fs_ws = &s.fs_ws;
    s.fs_mon.retain(|h, _| fs_ws.contains_key(h));
    s.fs_hidden.retain(|h| fs_ws.contains_key(h));
}

/// The first workspace (starting after `cur`, wrapping) that has no windows at all - used for a
/// fullscreen game, so it does not land on top of the current work.
fn next_free_workspace(s: &State, mi: usize, cur: u8) -> u8 {
    let used = |ws: u8| {
        s.ws.iter().any(|(h, w)| *w == ws && s.mi(*h) == mi)
            || s.fs_ws.iter().any(|(h, w)| {
                *w == ws
                    && s.fs_mon
                        .get(h)
                        .copied()
                        .unwrap_or_else(|| monitor::of_window(HWND(*h as *mut _)).min(MON - 1))
                        == mi
            })
    };
    for i in 1..=WS_COUNT {
        let ws = (cur + i) % WS_COUNT;
        if !used(ws) {
            return ws;
        }
    }
    (cur + 1) % WS_COUNT
}

/// Minimizes the fullscreen windows whose workspace is not shown, and restores the ones we hid
/// when their workspace becomes the current one.
fn apply_fullscreen(s: &mut State) {
    if s.paused || config::fullscreen_windows() == FullscreenWindows::Ignore {
        return;
    }
    let items: Vec<(isize, u8)> = s.fs_ws.iter().map(|(h, w)| (*h, *w)).collect();
    let mut changed = false;
    for (h, ws) in items {
        let hw = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hw)) }.as_bool() {
            continue;
        }
        let mi = s
            .fs_mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(hw).min(MON - 1));
        let cur = s.cur[mi];
        let hidden = s.fs_hidden.contains(&h);
        let minimized = unsafe { IsIconic(hw) }.as_bool();
        match fullscreen_action(ws, cur, hidden, minimized) {
            FsAction::Show => {
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_RESTORE);
                }
                s.fs_hidden.remove(&h);
                changed = true;
            }
            FsAction::Hide => {
                unsafe {
                    let _ = ShowWindowAsync(hw, SW_MINIMIZE);
                }
                s.fs_hidden.insert(h);
                changed = true;
            }
            FsAction::None => {}
        }
    }
    s.enforce |= changed;
}

/// Restores every fullscreen window we minimized (pause, quit), so nothing stays hidden.
fn restore_hidden_fullscreen(s: &mut State) {
    for h in std::mem::take(&mut s.fs_hidden) {
        let hw = HWND(h as *mut _);
        if unsafe { IsWindow(Some(hw)) }.as_bool() && unsafe { IsIconic(hw) }.as_bool() {
            unsafe {
                let _ = ShowWindowAsync(hw, SW_RESTORE);
            }
        }
    }
}

fn apply_window_pos(
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    flags: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
    operation: &'static str,
) -> bool {
    if prof::on() {
        prof::PLACEMENTS.fetch_add(1, Ordering::Relaxed);
    }
    // A slow client must not block this thread's input, capture cleanup and watchdogs.
    // Success means the request was accepted; geometry is checked by the verify timer.
    match unsafe { SetWindowPos(hwnd, None, x, y, width, height, flags | SWP_ASYNCWINDOWPOS) } {
        Ok(()) => true,
        Err(error) => {
            log(format!(
                "SetWindowPos failed: operation={operation} hwnd={:#x} error={error}",
                hwnd.0 as usize
            ));
            false
        }
    }
}

/// Remember failed geometry requests so a persistently rejecting client cannot trigger an
/// endless verify -> animate -> fail loop. A new target or a manual drag permits fresh attempts.
fn note_placement_result(s: &mut State, hwnd: HWND, target: RECT, succeeded: bool) {
    let h = hwnd.0 as isize;
    if succeeded {
        s.placement_failures.remove(&h);
        s.expected.insert(h, (target, Instant::now()));
        return;
    }
    let entry = s.placement_failures.entry(h).or_insert((target, 0));
    if !same_rect(&entry.0, &target, 0) {
        *entry = (target, 0);
    }
    entry.1 = entry.1.saturating_add(1);
}

fn placement_blocked(s: &State, hwnd: HWND, target: &RECT) -> bool {
    s.placement_failures
        .get(&(hwnd.0 as isize))
        .is_some_and(|(previous, attempts)| same_rect(previous, target, 0) && *attempts >= 2)
}

/// API success does not mean an application kept the requested position. Bound retries
/// for an unchanged target independently of Win32 error counts.
fn allow_geometry_retry(retries: &mut HashMap<isize, (RECT, u8)>, h: isize, target: RECT) -> bool {
    let entry = retries.entry(h).or_insert((target, 0));
    if !same_rect(&entry.0, &target, 0) {
        *entry = (target, 0);
    }
    if entry.1 >= 3 {
        return false;
    }
    entry.1 += 1;
    true
}

fn native_target(hwnd: HWND, target: RECT) -> Option<RECT> {
    unsafe {
        // Windows 10/11 windows have an invisible resize border; compensate so the
        // *visible* frame lands exactly on `target`.
        let mut win = RECT::default();
        let mut vis = RECT::default();
        GetWindowRect(hwnd, &mut win).ok()?;
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
        // DWM frame bounds can lag a park/restore or DPI transition. Never interpret a stale
        // screen position as a multi-thousand-pixel invisible border.
        let (dl, dt, dr, db) = if [dl, dt, dr, db].iter().all(|v| (0..=64).contains(v)) {
            (dl, dt, dr, db)
        } else {
            (0, 0, 0, 0)
        };
        Some(RECT {
            left: target.left - dl,
            top: target.top - dt,
            right: target.right + dr,
            bottom: target.bottom + db,
        })
    }
}

fn place(hwnd: HWND, target: RECT) -> bool {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
    native_target(hwnd, target).is_some_and(|r| {
        apply_window_pos(
            hwnd,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            "place",
        )
    })
}

/// Commit one layout as a batch. A failed batch falls back to bounded individual placements;
/// accepting the request never substitutes for checking the actual client geometry afterwards.
fn place_batch(s: &mut State, targets: &[(isize, RECT)]) {
    use windows::Win32::UI::WindowsAndMessaging::{
        BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos,
    };
    let _profile = Timer::new("place_batch");
    let requested: HashSet<_> = targets.iter().map(|(h, _)| *h).collect();
    s.anims.retain(|a| !requested.contains(&a.hwnd));
    let operations: Vec<_> = targets
        .iter()
        .filter_map(|(h, target)| {
            let hwnd = HWND(*h as *mut _);
            if placement_blocked(s, hwnd, target) {
                return None;
            }
            if visible_rect(hwnd).is_some_and(|actual| same_rect(&actual, target, 0)) {
                return None;
            }
            native_target(hwnd, *target).map(|r| (*h, *target, r))
        })
        .collect();
    if operations.is_empty() {
        return;
    }
    let result = unsafe {
        BeginDeferWindowPos(operations.len() as i32).and_then(|mut batch| {
            for (h, _, r) in &operations {
                batch = DeferWindowPos(
                    batch,
                    HWND(*h as *mut _),
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
                )?;
            }
            EndDeferWindowPos(batch)
        })
    };
    if let Err(error) = &result {
        log(format!("batch placement fallback: {error}"));
    }
    if result.is_ok() && prof::on() {
        prof::PLACEMENTS.fetch_add(operations.len() as u32, Ordering::Relaxed);
    }
    for (h, target, _) in operations {
        s.anims.retain(|a| a.hwnd != h);
        let hwnd = HWND(h as *mut _);
        let ok = result.is_ok() || place(hwnd, target);
        note_placement_result(s, hwnd, target, ok);
        if ok {
            s.min_size_reflow |= observe_min_size(s, hwnd, &target);
        }
    }
}

fn visual_commit(
    s: &mut State,
    mi: usize,
    targets: &[(isize, RECT)],
    ms: u32,
    entering: &[isize],
) -> bool {
    if !config::snapshot_animations() || !config::animation().0 || s.anim_timer.is_invalid() {
        return false;
    }
    if !snapshot::supports(mi, targets) {
        let sources: Vec<_> = targets
            .iter()
            .filter_map(|(h, _)| visible_rect(HWND(*h as *mut _)).map(|r| (*h, r)))
            .collect();
        if sources.len() != targets.len() || !snapshot::prepare(mi, monitor_work(mi), &sources) {
            return false;
        }
    }
    snapshot::entering(mi, entering);
    if !snapshot::reveal(mi) {
        snapshot::cancel(mi);
        return false;
    }
    place_batch(s, targets);
    if snapshot::animate(mi, targets, ms) {
        arm_anim_timer(s);
    } else {
        snapshot::cancel(mi);
    }
    true
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
    let _ = apply_window_pos(
        hwnd,
        PARK_X,
        PARK_Y,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        "park",
    );
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

const DEBOUNCE_MS: u32 = 40;
/// Delay of the check after a window was placed instantly (apps that move themselves).
const VERIFY_MS: u32 = 90;
/// Poll interval of the focus-follows-mouse feature (only runs while it is enabled).
const FFM_MS: u32 = 100;
/// Move-size loop watchdog interval (only runs while a tiled window is being dragged).
const DRAG_WATCHDOG_MS: u32 = 250;

/// Per-monitor slots in `State`. Windows can only tile on the monitors they are on; this caps how
/// many monitors are tracked at once.
const MON: usize = 9;

#[derive(Default)]
struct State {
    divider: Option<divider::Resize>,
    /// Rects of the layout last applied, same indices as `last`, per monitor.
    rects: [Vec<RECT>; MON],
    /// Stable window order (so the spiral does not reshuffle when focus changes), per monitor.
    order: [Vec<isize>; MON],
    /// Workspace (0-based) of every known window.
    ws: HashMap<isize, u8>,
    /// Observed minimum size of a window (an app that refuses to shrink below it): its tile is
    /// grown to at least this size so it does not overlap its neighbours.
    min_size: HashMap<isize, (i32, i32)>,
    /// Workspace (0-based) of every known fullscreen window (they are never tiled).
    fs_ws: HashMap<isize, u8>,
    /// Monitor of every known fullscreen window. Recorded while the window is visible (a minimized
    /// window's `MonitorFromWindow` is not reliable), so show/hide decisions use the right screen.
    fs_mon: HashMap<isize, usize>,
    /// Fullscreen windows we minimized because their workspace is not shown (to restore later).
    fs_hidden: HashSet<isize>,
    /// Windows the user floated with `toggle_float` (never tiled this session).
    manual_float: HashSet<isize>,
    /// Timer polling the pointer for `focus_follows_mouse` (0 = disabled).
    ffm_timer: usize,
    /// Watchdog for a lost EVENT_SYSTEM_MOVESIZEEND notification.
    drag_timer: usize,
    /// Last window the pointer selected for focus-follows-mouse (avoids re-focusing it).
    ffm_last: Option<isize>,
    /// Current workspace (0-based) of each monitor.
    cur: [u8; MON],
    /// Windows of the current workspace in the layout that was applied last time, per monitor;
    /// if nothing changed we do not touch windows.
    last: [Vec<isize>; MON],
    /// Monitor of every known window (an index into `monitor::list`).
    mon: HashMap<isize, usize>,
    /// Failed placement count by HWND and target rectangle (bounded retries per target).
    placement_failures: HashMap<isize, (RECT, u8)>,
    geometry_retries: HashMap<isize, (RECT, u8)>,
    /// Most recently requested frame, used to recognize our events per window.
    expected: HashMap<isize, (RECT, Instant)>,
    /// Display-device names for the monitor slots in the per-monitor arrays.
    monitor_ids: [Option<String>; MON],
    /// The set of top-level windows may have changed (created / destroyed / shown / hidden /
    /// cloaked / minimized): a full `EnumWindows` walk (`retile`) is due.
    windows_changed: bool,
    paused: bool,
    timer: usize,
    dirty_since: Option<Instant>,
    /// A window of another workspace got focus (taskbar / Alt+Tab): follow it.
    follow: Option<(usize, u8, isize)>,
    last_switch: Option<Instant>,
    /// Tiled window the user is dragging / resizing right now.
    dragging: Option<isize>,
    /// Start time for the move-size watchdog's conservative API-failure fallback.
    drag_started: Option<Instant>,
    /// Size of its tile when the drag started (to tell a move from a resize).
    drag_size: (i32, i32),
    /// The current drag is a resize (edges moved), not a move.
    resizing: bool,
    /// While resizing: re-layout the other windows now (throttled by a short timer).
    live: bool,
    /// Live drag & drop preview: the tile index whose window is currently slid into the dragged
    /// window's slot, with that window's handle (see `update_drag_preview`).
    drag_preview: Option<(usize, isize)>,
    /// Split ratios per workspace (see `spiral`), per monitor.
    ratios: [HashMap<u8, Vec<f32>>; MON],
    /// Free region each window of the last layout was cut out of (same indices as `last`), per
    /// monitor.
    regions: [Vec<RECT>; MON],
    /// Drag finished: rebuild the layout (snap back, or after a swap).
    reflow: bool,
    /// A tiled window changed place/size on its own: pull it back.
    enforce: bool,
    /// When the last resize hotkey fired (to recognise a held key).
    last_resize: Option<Instant>,
    /// Window that currently has our coloured border.
    border_on: Option<isize>,
    /// Windows currently gliding to their new tiles.
    anims: Vec<Anim>,
    /// High-resolution waitable timer that drives the animation frames (null = none).
    anim_timer: HANDLE,
    frame_armed: bool,
    frame_fallback: usize,
    /// The next layout pass must place windows instantly (workspace switch, startup, ...).
    no_anim: bool,
    /// An animated client refused its target size; reflow once after all current animations settle.
    min_size_reflow: bool,
    /// Registered hotkeys: (id, action)
    hotkeys: Vec<(i32, Action)>,
}

type PendingEvent = (u32, HWND, i32, i32, u32, u32);

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static PENDING_EVENTS: RefCell<event_queue::DeferredQueue<PendingEvent, isize>> = RefCell::new(event_queue::DeferredQueue::default());
    static PENDING_WAKE_POSTED: Cell<bool> = const { Cell::new(false) };
    static DISPLAY_HZ: Cell<Option<u32>> = const { Cell::new(None) };
}

const WM_PENDING_EVENTS: u32 = 0x8000 + 42;

fn replay_events() {
    let budget = PENDING_EVENTS.with(|pending| pending.borrow().len().min(64));
    let started = Instant::now();
    for _ in 0..budget {
        if started.elapsed() >= Duration::from_millis(4) {
            break;
        }
        let Some((event, hwnd, object, child, thread, time)) =
            PENDING_EVENTS.with(|pending| pending.borrow_mut().pop_front())
        else {
            break;
        };
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                event,
                hwnd,
                object,
                child,
                thread,
                time,
            );
        }
    }
    wake_pending_events();
}

fn wake_pending_events() {
    if PENDING_EVENTS.with(|q| q.borrow().is_empty())
        || PENDING_WAKE_POSTED.with(|p| p.replace(true))
    {
        return;
    }
    let posted = unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
            windows::Win32::System::Threading::GetCurrentThreadId(),
            WM_PENDING_EVENTS,
            WPARAM(0),
            LPARAM(0),
        )
    };
    if posted.is_err() {
        PENDING_WAKE_POSTED.with(|p| p.set(false));
    }
}

impl State {
    fn clear_placement_history(&mut self) {
        self.placement_failures.clear();
        self.geometry_retries.clear();
    }
    fn animation_current(&self, h: isize) -> bool {
        !self.paused
            && self.dragging != Some(h)
            && self
                .ws
                .get(&h)
                .is_some_and(|ws| *ws == self.cur[self.mi(h)])
    }

    fn expected_frame(&self, h: isize, actual: &RECT, now: Instant) -> bool {
        self.expected.get(&h).is_some_and(|(target, requested)| {
            now.saturating_duration_since(*requested) < Duration::from_secs(1)
                && same_rect(actual, target, 4)
        })
    }

    fn placement_pending(&self, h: isize, target: &RECT, now: Instant) -> bool {
        self.expected.get(&h).is_some_and(|(requested, time)| {
            same_rect(requested, target, 0)
                && now.saturating_duration_since(*time) < Duration::from_millis(VERIFY_MS.into())
        })
    }
    /// The monitor slot a window belongs to.
    fn mi(&self, h: isize) -> usize {
        self.mon.get(&h).copied().unwrap_or(0).min(MON - 1)
    }

    /// Clears every monitor's remembered layout (forces a fresh pass).
    fn clear_layouts(&mut self) {
        for i in 0..MON {
            self.last[i].clear();
        }
    }
}

/// The monitor the user is working on: the foreground window's monitor if it is known, otherwise
/// the monitor under the pointer, otherwise the primary one.
fn active_mon(s: &State) -> usize {
    let h = unsafe { GetForegroundWindow() }.0 as isize;
    if s.mon.contains_key(&h) {
        return s.mi(h);
    }
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        return monitor::at(pt).min(MON - 1);
    }
    0
}

/// Seed the slot identities before the first window collection. The Win32 enumeration order is
/// convenient for indexing, but must not be treated as the identity of a physical display.
fn remember_monitor_slots() {
    let monitors = monitor::list();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        for (i, mon) in monitors.iter().take(MON).enumerate() {
            state.monitor_ids[i] = Some(mon.device.clone());
        }
    });
}

fn monitor_slot_map(
    old: &[monitor::Monitor],
    new: &[monitor::Monitor],
    identities: &[Option<String>; MON],
) -> ([usize; MON], [bool; MON]) {
    let primary = monitor::primary_index(new).min(MON - 1);
    let mut old_to_new = [primary; MON];
    let mut exact = [false; MON];
    for old_i in 0..MON {
        let key = identities[old_i]
            .as_deref()
            .or_else(|| old.get(old_i).map(|m| m.device.as_str()));
        if let Some(key) = key {
            if let Some(new_i) = new
                .iter()
                .take(MON)
                .position(|candidate| candidate.device == key)
            {
                old_to_new[old_i] = new_i;
                exact[old_i] = true;
                continue;
            }
        }
        if let Some(previous) = old.get(old_i) {
            let center = POINT {
                x: previous.full.left + (previous.full.right - previous.full.left) / 2,
                y: previous.full.top + (previous.full.bottom - previous.full.top) / 2,
            };
            old_to_new[old_i] = monitor::nearest(&new[..new.len().min(MON)], center);
        }
    }
    (old_to_new, exact)
}

/// Preserve each screen's workspace/layout when Windows changes monitor enumeration order. When
/// a display disappeared, its visible windows follow Windows' re-homing; parked windows go to
/// the nearest surviving display and its current workspace.
fn reconcile_monitor_slots(old: &[monitor::Monitor], new: &[monitor::Monitor]) {
    if new.is_empty() {
        return;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let (old_to_new, exact) = monitor_slot_map(old, new, &s.monitor_ids);

        let mut new_cur = [0u8; MON];
        let mut new_ratios: [HashMap<u8, Vec<f32>>; MON] = std::array::from_fn(|_| HashMap::new());
        let mut has_exact = [false; MON];
        for old_i in 0..MON {
            if exact[old_i] {
                let new_i = old_to_new[old_i];
                if !has_exact[new_i] {
                    new_cur[new_i] = s.cur[old_i];
                    new_ratios[new_i] = std::mem::take(&mut s.ratios[old_i]);
                    has_exact[new_i] = true;
                }
            }
        }
        let mut new_order: [Vec<isize>; MON] = std::array::from_fn(|_| Vec::new());
        let old_window_mon = s.mon.clone();
        for old_i in 0..MON {
            let fallback = old_to_new[old_i];
            for h in std::mem::take(&mut s.order[old_i]) {
                let hwnd = HWND(h as *mut _);
                let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool() && !is_parked(hwnd) {
                    monitor::of_window(hwnd).min(MON - 1)
                } else {
                    fallback
                };
                if dest != fallback || !exact[old_i] {
                    s.ws.insert(h, new_cur[dest]);
                }
                s.mon.insert(h, dest);
                if !new_order[dest].contains(&h) {
                    new_order[dest].push(h);
                }
            }
        }
        // Include any tracked window whose order entry was already lost, and refresh fullscreen
        // monitor ownership without trusting MonitorFromWindow for our off-screen parked windows.
        for (h, old_slot) in old_window_mon {
            let previous = old_slot.min(MON - 1);
            let fallback = old_to_new[previous];
            let hwnd = HWND(h as *mut _);
            let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool() && !is_parked(hwnd) {
                monitor::of_window(hwnd).min(MON - 1)
            } else {
                fallback
            };
            if dest != fallback || !exact[previous] {
                s.ws.insert(h, new_cur[dest]);
            }
            s.mon.insert(h, dest);
            if !new_order[dest].contains(&h) {
                new_order[dest].push(h);
            }
        }
        let fullscreen: Vec<(isize, usize)> = s.fs_mon.iter().map(|(h, mi)| (*h, *mi)).collect();
        for (h, old_i) in fullscreen {
            let old_i = old_i.min(MON - 1);
            let fallback = old_to_new[old_i];
            let hwnd = HWND(h as *mut _);
            let dest = if unsafe { IsWindow(Some(hwnd)) }.as_bool()
                && !unsafe { IsIconic(hwnd) }.as_bool()
            {
                monitor::of_window(hwnd).min(MON - 1)
            } else {
                fallback
            };
            if dest != fallback || !exact[old_i] {
                s.fs_ws.insert(h, new_cur[dest]);
            }
            s.fs_mon.insert(h, dest);
        }

        s.cur = new_cur;
        s.order = new_order;
        s.ratios = new_ratios;
        s.monitor_ids = std::array::from_fn(|i| new.get(i).map(|mon| mon.device.clone()));
        for i in 0..MON {
            s.last[i].clear();
            s.rects[i].clear();
            s.regions[i].clear();
        }
        s.reflow = true;
    });
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
fn set_frame(hwnd: isize, vis: &RECT, b: (i32, i32, i32, i32)) -> bool {
    apply_window_pos(
        HWND(hwnd as *mut _),
        vis.left - b.0,
        vis.top - b.1,
        (vis.right - vis.left) + b.0 + b.2,
        (vis.bottom - vis.top) + b.1 + b.3,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        "animation-frame",
    )
}

/// Moves a window to `target`: instantly, or - if `animate` and animations are enabled -
/// by gliding. Always cancels an animation that was already running for this window.
fn move_window(s: &mut State, hwnd: HWND, target: RECT, animate: bool) {
    let h = hwnd.0 as isize;
    if animate
        && config::animation().0
        && s.anims
            .iter()
            .any(|a| a.hwnd == h && same_rect(&a.to, &target, 0))
    {
        return;
    }
    s.anims.retain(|a| a.hwnd != h);
    if placement_blocked(s, hwnd, &target) {
        return;
    }
    let (on, ms) = config::animation();
    let busy = unsafe { IsZoomed(hwnd).as_bool() } || is_parked(hwnd);
    // if the frame timer could not be created, place windows instantly rather than freeze them
    if !animate || !on || ms == 0 || busy || s.anim_timer.is_invalid() {
        let succeeded = place(hwnd, target);
        note_placement_result(s, hwnd, target, succeeded);
        if succeeded && observe_min_size(s, hwnd, &target) {
            s.min_size_reflow = true;
        }
        if s.border_on == Some(h) {
            sync_border_to(h);
        }
        return;
    }
    let Some(from) = visible_rect(hwnd) else {
        let succeeded = place(hwnd, target);
        note_placement_result(s, hwnd, target, succeeded);
        if succeeded && observe_min_size(s, hwnd, &target) {
            s.min_size_reflow = true;
        }
        if s.border_on == Some(h) {
            sync_border_to(h);
        }
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
        if prof::on() {
            anim_prof_reset();
        }
        arm_anim_timer(s);
    }
}

/// After a placement has had time to settle, remember its size if the app refused to shrink
/// below it (an application minimum size). The layout then grows its tile instead of letting the
/// window overlap its neighbour. Only ever grows the remembered size: once a tile has been grown
/// to fit, the window also fits it, so forgetting the minimum would make the tile shrink again
/// and start an endless grow/shrink loop.
fn observe_min_size(s: &mut State, hwnd: HWND, target: &RECT) -> bool {
    let h = hwnd.0 as isize;
    // Posted placements may still be queued. The old (larger) window is not evidence
    // of a minimum size; learning it here can permanently pin an extreme layout.
    if s.placement_pending(h, target, Instant::now()) {
        return false;
    }
    let Some(vis) = visible_rect(hwnd) else {
        return false;
    };
    // DWM can still report the previous frame after a placement request.
    // A visible frame outside the current native window is not minimum-size evidence.
    let mut native = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut native) }.is_err()
        || vis.left < native.left - 2
        || vis.top < native.top - 2
        || vis.right > native.right + 2
        || vis.bottom > native.bottom + 2
    {
        return false;
    }
    let (vw, vh) = (vis.right - vis.left, vis.bottom - vis.top);
    let (tw, th) = (target.right - target.left, target.bottom - target.top);
    let previous = s.min_size.get(&h).copied().unwrap_or((0, 0));
    let observed = rejected_minimum(previous, (tw, th), (vw, vh));
    if observed != previous {
        s.min_size.insert(h, observed);
        return true;
    }
    false
}

/// A rejected height must never turn an accepted (possibly very wide) width into a minimum.
fn rejected_minimum(previous: (i32, i32), target: (i32, i32), actual: (i32, i32)) -> (i32, i32) {
    (
        if actual.0 > target.0 + 2 {
            previous.0.max(actual.0)
        } else {
            previous.0
        },
        if actual.1 > target.1 + 2 {
            previous.1.max(actual.1)
        } else {
            previous.1
        },
    )
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

/// Cache display queries until a display change; cap visual work independently of input.
fn anim_frame_ms() -> u32 {
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    let hz = DISPLAY_HZ.with(|cache| {
        if let Some(hz) = cache.get() {
            return hz;
        }
        let hz = monitor::list()
            .iter()
            .filter_map(|mon| {
                let device: Vec<u16> = mon.device.encode_utf16().chain(Some(0)).collect();
                let mut mode = DEVMODEW {
                    dmSize: size_of::<DEVMODEW>() as u16,
                    ..Default::default()
                };
                if unsafe {
                    EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
                }
                .as_bool()
                    && (30..=1000).contains(&mode.dmDisplayFrequency)
                {
                    Some(mode.dmDisplayFrequency)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(60);
        cache.set(Some(hz));
        hz
    });
    frame_interval_ms(hz, config::animation_fps_limit())
}

fn frame_interval_ms(display_hz: u32, fps_limit: u32) -> u32 {
    let fps = display_hz.max(1).min(fps_limit.clamp(15, 240));
    1000u32.div_ceil(fps)
}

/// (Re)arms the frame timer: the first frame fires in one frame interval, then every interval.
fn arm_anim_timer(s: &mut State) {
    if s.frame_armed {
        return;
    }
    let frame = anim_frame_ms().max(1);
    let due = -(frame as i64) * 10_000; // relative time, in 100 ns units
    unsafe {
        s.frame_armed = !s.anim_timer.is_invalid()
            && SetWaitableTimer(s.anim_timer, &due, frame as i32, None, None, false).is_ok();
        if !s.frame_armed {
            s.frame_fallback = SetTimer(None, s.frame_fallback, frame, None);
            s.frame_armed = s.frame_fallback != 0;
            if !s.frame_armed {
                snapshot::clear();
            }
        }
    }
}

/// Stops the frame timer once nothing is animating.
fn cancel_anim_timer(s: &mut State) {
    s.frame_armed = false;
    if s.frame_fallback != 0 {
        unsafe {
            let _ = KillTimer(None, s.frame_fallback);
        }
        s.frame_fallback = 0;
    }
    if s.anim_timer.is_invalid() {
        return;
    }
    unsafe {
        let _ = CancelWaitableTimer(s.anim_timer);
    }
}

/// One animation frame (called from the frame timer).
fn anim_tick() {
    divider::tick();
    if !divider::captured() && STATE.with(|s| s.borrow().divider.is_some()) {
        end_divider(false);
    }
    let live = STATE.with(|s| std::mem::take(&mut s.borrow_mut().live));
    if live {
        live_resize();
    }
    if snapshot::tick() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if !s.paused && s.dragging.is_none() {
                s.enforce = true;
                s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
            }
        });
    }
    let profiling = prof::on();
    let easing = config::easing();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.anims.is_empty() {
            // The queue was emptied outside `anim_tick` (e.g. a drag started): stop the timer so
            // it does not keep firing at 60 Hz doing nothing.
            if !snapshot::is_animating() && s.divider.is_none() {
                cancel_anim_timer(&mut s);
            }
            return;
        }
        let now = Instant::now();
        let mut keep = Vec::with_capacity(s.anims.len());
        let mut setpos = Duration::ZERO;
        let mut min_size_changed = false;
        for mut a in std::mem::take(&mut s.anims) {
            if !s.animation_current(a.hwnd) {
                continue;
            }
            if !unsafe { IsWindow(Some(HWND(a.hwnd as *mut _))) }.as_bool() {
                continue; // closed while gliding
            }
            let dur = (a.dur_ms.max(1) as f32) / 1000.0;
            let t = (now - a.t0).as_secs_f32() / dur;
            let (mut rect, mut done) = if t >= 1.0 {
                (a.to, true) // exact final position
            } else {
                (lerp_rect(&a.from, &a.to, ease(easing, t)), false)
            };
            let mut can_observe_size = true;
            // skip frames that would not move the window (the tail of an ease-out is almost flat)
            if a.last.as_ref().is_none_or(|l| !same_rect(l, &rect, 0)) {
                let ts = profiling.then(Instant::now);
                let applied = set_frame(a.hwnd, &rect, a.border);
                if applied {
                    s.placement_failures.remove(&a.hwnd);
                    s.expected.insert(a.hwnd, (rect, Instant::now()));
                }
                if let Some(ts) = ts {
                    setpos += ts.elapsed();
                }
                if !applied {
                    // Do not keep posting frames to a window that rejected placement. Make one
                    // direct final attempt and let the delayed geometry verification reconcile it.
                    can_observe_size = place(HWND(a.hwnd as *mut _), a.to);
                    note_placement_result(&mut s, HWND(a.hwnd as *mut _), a.to, can_observe_size);
                    rect = a.to;
                    done = true;
                }
                a.last = Some(rect);
            }
            let ahwnd = a.hwnd;
            if done {
                // the window is at its tile now: note it if the app kept a bigger minimum size
                if can_observe_size {
                    min_size_changed |= observe_min_size(&mut s, HWND(ahwnd as *mut _), &a.to);
                }
            } else {
                keep.push(a);
            }
        }
        s.anims = keep;
        s.min_size_reflow |= min_size_changed;
        if s.anims.is_empty() {
            if !snapshot::is_animating() && s.divider.is_none() {
                cancel_anim_timer(&mut s);
            }
            // A client can reject the target size while an animation is running. Reflow once
            // after the animation settles so the newly observed minimum is reflected by the
            // layout instead of leaving a temporary overlap until an unrelated later action.
            if std::mem::take(&mut s.min_size_reflow) && !s.paused {
                s.reflow = true;
                s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            } else if !s.paused {
                // A client may reposition itself or constrain a frame after accepting the
                // animation request. Verify the settled geometry once, not on every frame.
                s.enforce = true;
                s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
            }
        }
        if profiling {
            anim_prof_frame(now, setpos.as_secs_f32() * 1e6);
            if s.anims.is_empty() {
                anim_prof_finish(now);
            }
        }
    });
    // Posted requests may still be pending: follow the observed frame instead
    // of drawing the border at the next animation target.
    sync_border();
}

// ---------------------------------------------------------------------------
// focus border (drawn by tile-rs itself, see `overlay.rs`)
// ---------------------------------------------------------------------------

/// Draws the border around the *visible* frame `vis` of `target`. The border window is placed just
/// below `target` in the z-order and clipped to a hollow frame, so it can never cover the window
/// with a solid colour (for example while the window is being placed or maximized).
fn place_border(vis: &RECT, target: isize) {
    let w = config::border_width();
    let hw = HWND(target as *mut _);
    // a maximized / minimized window has no tiled frame: no border
    if w <= 0 || unsafe { IsZoomed(hw).as_bool() || IsIconic(hw).as_bool() } {
        overlay::hide_border();
        return;
    }
    // never draw a border that fills the whole monitor (a window in transition)
    let mon = raw_work_area();
    if (vis.right - vis.left) >= (mon.right - mon.left)
        || (vis.bottom - vis.top) >= (mon.bottom - mon.top)
    {
        overlay::hide_border();
        return;
    }
    place_border_frame(vis, target, w);
}

/// Positions the border overlay around the observed visible frame.
fn place_border_frame(vis: &RECT, target: isize, w: i32) {
    if w <= 0 {
        overlay::hide_border();
        return;
    }
    let rect = RECT {
        left: vis.left - w,
        top: vis.top - w,
        right: vis.right + w,
        bottom: vis.bottom + w,
    };
    // rounded corners: the window's own corner radius (~8 px) plus the border width
    overlay::show_border(rect, HWND(target as *mut _), 8 + w, w);
}

/// Picks the window that should carry the border (the focused tiled window of the current
/// workspace) and positions the overlay around it.
fn update_border(s: &mut State, focus: isize) {
    // a tiled window of the current workspace: in the applied layout, or (should the layout list
    // be momentarily stale) a known window that belongs to the current workspace
    let mi = s.mi(focus);
    let tiled = s.last[mi].contains(&focus) || s.ws.get(&focus) == Some(&s.cur[mi]);
    s.border_on =
        (config::border().is_some() && !s.paused && s.dragging.is_none() && tiled).then_some(focus);
}

/// Re-evaluate which window should have the border (after layout / pause / config changes).
fn refresh_border() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    STATE.with(|s| update_border(&mut s.borrow_mut(), fg));
    sync_border();
    sync_dividers();
}

fn sync_dividers() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let handles = STATE.with(|state| {
        let s = state.borrow();
        if s.paused
            || s.dragging.is_some()
            || !s.anims.is_empty()
            || snapshot::is_animating()
            || !config::snapshot_animations()
            || !s.last.iter().any(|windows| windows.contains(&fg))
        {
            return Vec::new();
        }
        (0..monitor::count().min(MON))
            .flat_map(|mi| {
                divider::handles(
                    mi,
                    config::layout(),
                    &s.rects[mi],
                    &s.regions[mi],
                    config::inner_gap(),
                )
            })
            .collect()
    });
    divider::sync(handles);
}

fn begin_divider(handle: divider::Handle, origin: POINT) -> bool {
    let _profile = Timer::new("divider_prepare");
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return false;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let mi = handle.monitor;
        if mi >= MON
            || s.paused
            || s.dragging.is_some()
            || s.divider.is_some()
            || s.windows_changed
            || !s.anims.is_empty()
            || snapshot::is_animating()
            || !config::snapshot_animations()
            || !divider::handles(
                mi,
                config::layout(),
                &s.rects[mi],
                &s.regions[mi],
                config::inner_gap(),
            )
            .contains(&handle)
        {
            return false;
        }
        let windows = s.last[mi].clone();
        let sources: Vec<_> = windows
            .iter()
            .filter_map(|h| visible_rect(HWND(*h as *mut _)).map(|r| (*h, r)))
            .collect();
        if sources.len() != windows.len()
            || sources
                .iter()
                .zip(&s.rects[mi])
                .any(|((_, actual), target)| !same_rect(actual, target, 2))
            || !snapshot::prepare(mi, monitor_work(mi), &sources)
            || !snapshot::reveal(mi)
        {
            snapshot::cancel(mi);
            return false;
        }
        let layout = config::layout();
        let gaps = gaps();
        // Recover ratios from settled geometry, including any adaptive minimum-size fitting.
        let mut ratios = vec![0.5; windows.len() - 1];
        let count = if layout == Layout::MasterStack {
            1
        } else {
            ratios.len()
        };
        for (j, ratio) in ratios.iter_mut().enumerate().take(count) {
            let q = s.regions[mi][j];
            let t = s.rects[mi][j];
            *ratio = if layout == Layout::MasterStack || j.is_multiple_of(2) {
                ((t.right - q.left) as f32 + 0.25) / (q.right - q.left - gaps.inner).max(1) as f32
            } else {
                ((t.bottom - q.top) as f32 + 0.25) / (q.bottom - q.top - gaps.inner).max(1) as f32
            };
        }
        let configured = config::min_tile();
        let minimums = windows
            .iter()
            .zip(&s.rects[mi])
            .map(|(h, r)| {
                let observed = s.min_size.get(h).copied().unwrap_or_default();
                (
                    (configured.0.max(observed.0)).min(r.right - r.left),
                    (configured.1.max(observed.1)).min(r.bottom - r.top),
                )
            })
            .collect();
        s.divider = Some(divider::Resize {
            handle,
            layout,
            area: monitor_work(mi),
            gaps,
            windows,
            workspace: s.cur[mi],
            ratios,
            tiles: s.rects[mi].clone(),
            preview: s.rects[mi].clone(),
            regions: s.regions[mi].clone(),
            minimums,
            origin,
            last_pointer_axis: if handle.vertical { origin.x } else { origin.y },
        });
        s.border_on = None;
        overlay::hide_border();
        if s.timer != 0 {
            unsafe {
                let _ = KillTimer(None, s.timer);
            }
            s.timer = 0;
        }
        arm_anim_timer(&mut s);
        true
    })
}

fn update_divider(point: POINT) {
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return;
    }
    let valid = STATE.with(|state| {
        let mut s = state.borrow_mut();
        let Some(gesture) = s.divider.as_ref() else {
            return false;
        };
        let mi = gesture.handle.monitor;
        if s.paused
            || s.cur[mi] != gesture.workspace
            || s.last[mi] != gesture.windows
            || config::layout() != gesture.layout
            || !snapshot::contains(mi)
        {
            return false;
        }
        if !s.divider.as_mut().unwrap().pointer_changed(point) {
            return true;
        }
        let gesture = s.divider.as_ref().unwrap();
        let (_, tiles, _) = gesture.target(point);
        if tiles == gesture.preview {
            return true;
        }
        let targets: Vec<_> = gesture
            .windows
            .iter()
            .copied()
            .zip(tiles.iter().copied())
            .collect();
        if !snapshot::preview(mi, &targets) {
            return false;
        }
        s.divider.as_mut().unwrap().preview = tiles;
        true
    });
    if !valid {
        end_divider(false);
    }
}

fn end_divider(commit: bool) {
    divider::release();
    // SetWindowPos can pump a capture-loss callback during a placement transaction.
    // The next frame cancels the gesture once the State borrow has been released.
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        return;
    }
    if STATE.with(|s| s.borrow().divider.is_none()) {
        return;
    }
    STATE.with(|state| {
        let mut s = state.borrow_mut();
        let Some(gesture) = s.divider.take() else {
            return;
        };
        let mi = gesture.handle.monitor;
        let valid = !s.paused
            && s.cur[mi] == gesture.workspace
            && s.last[mi] == gesture.windows
            && config::layout() == gesture.layout
            && snapshot::contains(mi);
        if commit && valid {
            let mut point = POINT::default();
            if unsafe { GetCursorPos(&mut point) }.is_ok() {
                let (ratios, tiles, regions) = gesture.target(point);
                let targets: Vec<_> = gesture
                    .windows
                    .iter()
                    .copied()
                    .zip(tiles.iter().copied())
                    .collect();
                place_batch(&mut s, &targets);
                s.ratios[mi].insert(gesture.workspace, ratios);
                s.rects[mi] = tiles;
                s.regions[mi] = regions;
                // Retain the final preview for one frame while applications begin repainting.
                if snapshot::animate(mi, &targets, 1) {
                    arm_anim_timer(&mut s);
                } else {
                    snapshot::cancel(mi);
                }
                s.enforce = true;
                if std::mem::take(&mut s.min_size_reflow) {
                    s.reflow = true;
                }
                s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
            } else {
                snapshot::cancel(mi);
            }
        } else {
            snapshot::cancel(mi);
        }
    });
    refresh_border();
}

/// Repositions the border overlay around the current border window (`State::border_on`).
fn sync_border() {
    let target = STATE.with(|s| s.borrow().border_on);
    match target {
        Some(h) => sync_border_to(h),
        None => overlay::hide_border(),
    }
}

fn sync_border_to(h: isize) {
    match visible_rect(HWND(h as *mut _)) {
        Some(vis) => place_border(&vis, h),
        None => overlay::hide_border(),
    }
}

/// Hide the border (pause, quit, colour change).
fn clear_border() {
    STATE.with(|s| s.borrow_mut().border_on = None);
    overlay::hide_border();
}

/// Updates each monitor's workspace indicator; the tiling area leaves room for its own bar.
fn sync_indicator() {
    if !config::workspace_indicator() {
        overlay::hide_indicator();
        return;
    }
    let mons = monitor::list();
    let (w, h) = overlay::indicator_size();
    STATE.with(|s| {
        let s = s.borrow();
        for (mi, mon) in mons.iter().enumerate().take(MON) {
            let x = mon.work.left + (mon.work.right - mon.work.left - w) / 2;
            let rect = RECT {
                left: x,
                top: mon.work.top,
                right: x + w,
                bottom: mon.work.top + h,
            };
            let mut mask = 0u32;
            for (window, ws) in &s.ws {
                if s.mi(*window) == mi {
                    mask |= 1u32 << (*ws as u32).min(31);
                }
            }
            for (window, ws) in &s.fs_ws {
                let fs_mi = s
                    .fs_mon
                    .get(window)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(*window as *mut _)))
                    .min(MON - 1);
                if fs_mi == mi {
                    mask |= 1u32 << (*ws as u32).min(31);
                }
            }
            overlay::show_indicator(mi, rect, s.cur[mi], mask);
        }
    });
}

/// (current workspace 0-based, paused) - for the tray.
pub fn ui_state() -> (u8, bool) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.cur[active_mon(&s)], s.paused)
    })
}

/// The screen work area changed (display mode, taskbar size / auto-hide): rebuild the layout.
pub fn request_relayout() {
    end_divider(false);
    DISPLAY_HZ.with(|cache| cache.set(None));
    snapshot::clear();
    let old_monitors = monitor::list();
    monitor::refresh();
    let new_monitors = monitor::list();
    reconcile_monitor_slots(&old_monitors, &new_monitors);
    if config::workspace_indicator() {
        let _ = overlay::sync_indicator_count(new_monitors.len());
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.clear_placement_history();
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
        Action::WorkspaceNext => "workspace_next".into(),
        Action::WorkspacePrev => "workspace_prev".into(),
        Action::MoveToNext => "move_to_next_workspace".into(),
        Action::MoveToPrev => "move_to_prev_workspace".into(),
        Action::Resize { width, grow } => format!(
            "{}_{}",
            if grow { "grow" } else { "shrink" },
            if width { "width" } else { "height" }
        ),
        Action::Minimize => "minimize".into(),
        Action::ToggleFloat => "toggle_float".into(),
        Action::Close => "close".into(),
        Action::Pause => "pause".into(),
        Action::Quit => "quit".into(),
        Action::Retile => "retile".into(),
        Action::CycleLayout => "cycle_layout".into(),
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

/// Drop every piece of cached state for a destroyed HWND immediately. HWND values are reusable;
/// waiting for the debounced EnumWindows pass can accidentally transfer stale workspace/order
/// state to a newly created window that receives the same numeric handle.
fn forget_window(s: &mut State, h: isize) -> bool {
    let tracked = s.ws.contains_key(&h)
        || s.mon.contains_key(&h)
        || s.fs_ws.contains_key(&h)
        || s.fs_mon.contains_key(&h)
        || s.fs_hidden.contains(&h)
        || s.manual_float.contains(&h)
        || s.min_size.contains_key(&h)
        || s.placement_failures.contains_key(&h)
        || s.anims.iter().any(|animation| animation.hwnd == h)
        || s.border_on == Some(h)
        || s.order.iter().any(|order| order.contains(&h))
        || s.last.iter().any(|last| last.contains(&h));
    if !tracked {
        return false;
    }
    snapshot::cancel(s.mi(h));
    s.ws.remove(&h);
    s.mon.remove(&h);
    s.fs_ws.remove(&h);
    s.fs_mon.remove(&h);
    s.fs_hidden.remove(&h);
    s.manual_float.remove(&h);
    s.min_size.remove(&h);
    s.placement_failures.remove(&h);
    s.geometry_retries.remove(&h);
    s.expected.remove(&h);
    s.anims.retain(|a| a.hwnd != h);
    for mi in 0..MON {
        s.order[mi].retain(|window| *window != h);
        if let Some(i) = s.last[mi].iter().position(|window| *window == h) {
            s.last[mi].remove(i);
            if i < s.rects[mi].len() {
                s.rects[mi].remove(i);
            }
            if i < s.regions[mi].len() {
                s.regions[mi].remove(i);
            }
        }
    }
    if s.dragging == Some(h) {
        s.dragging = None;
        s.drag_started = None;
        s.resizing = false;
        s.live = false;
        if s.drag_timer != 0 {
            unsafe {
                let _ = KillTimer(None, s.drag_timer);
            }
            s.drag_timer = 0;
        }
    }
    if s.drag_preview.is_some_and(|(_, preview)| preview == h) {
        s.drag_preview = None;
    }
    if s.border_on == Some(h) {
        s.border_on = None;
        overlay::hide_border();
    }
    s.clear_layouts();
    s.reflow = true;
    true
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole-window events (OBJID_WINDOW == 0, no child element). Popup menus report
    // OBJID_CLIENT and would be filtered out here, but they matter: our border would otherwise be
    // left sitting above the menu.
    let menu = event == EVENT_SYSTEM_MENUPOPUPSTART || event == EVENT_SYSTEM_MENUPOPUPEND;
    if !menu && (id_object != 0 || id_child != 0 || hwnd.is_invalid()) {
        return;
    }
    // Foreign window APIs can pump WinEvents while a layout borrows STATE. Preserve their order
    // and replay after that transaction instead of panicking across the Win32 callback boundary.
    if STATE.with(|s| s.try_borrow_mut().is_err()) {
        PENDING_EVENTS.with(|pending| {
            let mut pending = pending.borrow_mut();
            let previous = pending.len();
            let key = (event == EVENT_OBJECT_LOCATIONCHANGE).then_some(hwnd.0 as isize);
            pending.push((event, hwnd, id_object, id_child, _thread, _time), key);
            if prof::on() {
                prof::EVENTS_QUEUED.fetch_add(1, Ordering::Relaxed);
                prof::EVENT_QUEUE_PEAK.fetch_max(
                    pending.len().min(u32::MAX as usize) as u32,
                    Ordering::Relaxed,
                );
                if pending.len() == previous {
                    prof::EVENTS_COALESCED.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        wake_pending_events();
        return;
    }
    let h = hwnd.0 as isize;
    let cancel_divider = STATE.with(|state| {
        let s = state.borrow();
        let Some(gesture) = s.divider.as_ref() else {
            return false;
        };
        match event {
            EVENT_OBJECT_DESTROY
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_CLOAKED
            | EVENT_SYSTEM_MINIMIZESTART => gesture.windows.contains(&h),
            EVENT_OBJECT_SHOW
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZEEND
            | EVENT_SYSTEM_MOVESIZESTART
            | EVENT_SYSTEM_FOREGROUND
            | EVENT_SYSTEM_MENUPOPUPSTART => true,
            EVENT_OBJECT_LOCATIONCHANGE => gesture
                .windows
                .iter()
                .position(|window| *window == h)
                .is_some_and(|i| {
                    visible_rect(hwnd).is_none_or(|r| !same_rect(&r, &gesture.tiles[i], 2))
                }),
            _ => false,
        }
    });
    if cancel_divider {
        end_divider(false);
    }
    let mut resync = false;
    // Focus moved within the current workspace: the border target was just set to the event's
    // window, so it only has to be repositioned (no re-derivation from `GetForegroundWindow`,
    // which can still race with the click).
    let mut reposition = false;
    // Did this event ask for any actual work? Only then is the debounce timer armed: an event we
    // ignore (e.g. an unrelated window moving) must not cause a whole re-collect 40 ms later.
    let mut dirty = false;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        match event {
            EVENT_SYSTEM_MENUPOPUPSTART => {
                // a popup menu (e.g. a right-click menu) is drawn above the windows but below our
                // border overlay, so hide the border while the menu is up
                s.border_on = None;
                overlay::hide_border();
                return;
            }
            EVENT_SYSTEM_MENUPOPUPEND => {
                // the menu is gone: let the caller re-evaluate and re-show the border
                resync = true;
                return;
            }
            EVENT_SYSTEM_MOVESIZESTART => {
                // let the user drag freely; we decide what to do while / when they let go
                if !s.paused {
                    let mi = s.mi(h);
                    if let Some(k) = s.last[mi].iter().position(|x| *x == h) {
                        if k < s.rects[mi].len() {
                            s.dragging = Some(h);
                            s.placement_failures.remove(&h);
                            s.geometry_retries.remove(&h);
                            s.drag_started = Some(Instant::now());
                            if s.drag_timer != 0 {
                                let _ = KillTimer(None, s.drag_timer);
                            }
                            s.drag_timer = SetTimer(None, 0, DRAG_WATCHDOG_MS, None);
                            let affected: HashSet<isize> = s.last[mi].iter().copied().collect();
                            s.anims.retain(|a| !affected.contains(&a.hwnd));
                            snapshot::cancel(mi);
                            s.resizing = false;
                            s.live = false;
                            s.drag_preview = None;
                            // the overlay would otherwise chase the window (and its "whole monitor"
                            // guard makes it blink while the frame is in transition), so hide it
                            // for the duration of the drag and let the re-tile restore it
                            s.border_on = None;
                            overlay::hide_border();
                            if config::snapshot_animations() {
                                let sources: Vec<_> = s.last[mi]
                                    .iter()
                                    .filter_map(|h| {
                                        visible_rect(HWND(*h as *mut _)).map(|r| (*h, r))
                                    })
                                    .collect();
                                if sources.len() == s.last[mi].len() {
                                    snapshot::prepare(mi, monitor_work(mi), &sources);
                                }
                            }
                            // Remember the window's *actual* size, not the tile's: an app with a
                            // minimum size (or one that resized itself) can be larger than its
                            // tile, and comparing a plain move against the tile size would then be
                            // mistaken for a resize, which corrupts the split ratios.
                            s.drag_size = visible_rect(hwnd)
                                .map(|v| (v.right - v.left, v.bottom - v.top))
                                .unwrap_or_else(|| {
                                    let t = s.rects[mi][k];
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
                    s.placement_failures.remove(&h);
                    s.geometry_retries.remove(&h);
                    s.drag_started = None;
                    if s.drag_timer != 0 {
                        let _ = KillTimer(None, s.drag_timer);
                        s.drag_timer = 0;
                    }
                    let old_mi = s.mi(h);
                    let new_mi = monitor::of_window(hwnd).min(MON - 1);
                    if old_mi != new_mi {
                        snapshot::cancel(old_mi);
                        snapshot::cancel(new_mi);
                        // A manual cross-monitor drag is an explicit move to the destination
                        // monitor's currently visible workspace. Do not interpret its final size
                        // as a resize of a split on the source monitor.
                        s.order[old_mi].retain(|window| *window != h);
                        if !s.order[new_mi].contains(&h) {
                            s.order[new_mi].push(h);
                        }
                        s.mon.insert(h, new_mi);
                        let destination_workspace = s.cur[new_mi];
                        s.ws.insert(h, destination_workspace);
                        s.last[old_mi].clear();
                        s.last[new_mi].clear();
                        s.resizing = false;
                        s.live = false;
                        s.drag_preview = None;
                    } else if s.resizing {
                        let mi = old_mi;
                        // final size: take the last position of the dragged edges
                        if let (Some(k), Some(vis)) =
                            (s.last[mi].iter().position(|x| *x == h), visible_rect(hwnd))
                        {
                            let cur = s.cur[mi];
                            let (tiles, regions) = (s.rects[mi].clone(), s.regions[mi].clone());
                            let mut rs = s.ratios[mi].remove(&cur).unwrap_or_default();
                            apply_resize_for(
                                config::layout(),
                                &mut rs,
                                &tiles,
                                &regions,
                                k,
                                vis,
                                config::inner_gap(),
                            );
                            s.ratios[mi].insert(cur, rs);
                        }
                        s.resizing = false;
                        s.live = false;
                    } else {
                        let mi = old_mi;
                        snapshot::cancel(mi);
                        // dropped onto another tile? -> swap the two windows; otherwise snap back
                        let mut p = POINT::default();
                        let _ = GetCursorPos(&mut p);
                        let hit = s.rects[mi].iter().position(|r| {
                            p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
                        });
                        let swap_at = hit.filter(|i| *i < s.last[mi].len() && s.last[mi][*i] != h);
                        // the live preview already slid the hovered window into our slot: it stays
                        // there if it is the one we swap with, otherwise it goes back
                        if let Some((pi, ph)) = s.drag_preview.take() {
                            if swap_at != Some(pi) && pi < s.rects[mi].len() {
                                let back = s.rects[mi][pi];
                                move_window(&mut s, HWND(ph as *mut _), back, true);
                            }
                        }
                        if let Some(i) = swap_at {
                            let other = s.last[mi][i];
                            let (ia, ib) = (
                                s.order[mi].iter().position(|x| *x == h),
                                s.order[mi].iter().position(|x| *x == other),
                            );
                            if let (Some(ia), Some(ib)) = (ia, ib) {
                                s.order[mi].swap(ia, ib);
                            }
                        }
                    }
                    s.reflow = true;
                    dirty = true;
                } else {
                    return;
                }
            }
            EVENT_OBJECT_LOCATIONCHANGE => {
                if s.paused {
                    return;
                }
                let actual = visible_rect(hwnd);
                // Even our own accepted placement can settle after the last animation
                // tick. Update the border before suppressing layout reconciliation.
                if s.border_on == Some(h) {
                    match actual {
                        Some(vis) => place_border(&vis, h),
                        None => overlay::hide_border(),
                    }
                }
                if s.dragging.is_none()
                    && actual
                        .as_ref()
                        .is_some_and(|r| s.expected_frame(h, r, Instant::now()))
                {
                    return;
                }
                if let Some(d) = s.dragging {
                    // during a drag only the dragged window matters: is it being resized?
                    if d == h {
                        if let Some(vis) = actual {
                            let (w, hh) = (vis.right - vis.left, vis.bottom - vis.top);
                            if !s.resizing
                                && ((w - s.drag_size.0).abs() > RESIZE_PX
                                    || (hh - s.drag_size.1).abs() > RESIZE_PX)
                            {
                                s.resizing = true;
                            }
                        }
                        if s.resizing {
                            // a resize is not a drop: undo any live swap preview
                            let mi = s.mi(h);
                            if let Some((pi, ph)) = s.drag_preview.take() {
                                if pi < s.rects[mi].len() {
                                    let back = s.rects[mi][pi];
                                    move_window(&mut s, HWND(ph as *mut _), back, true);
                                }
                            }
                            s.live = true;
                            // Coalesce input to the next frame without resetting its deadline.
                            arm_anim_timer(&mut s);
                        } else {
                            update_drag_preview(&mut s, h);
                        }
                    }
                    return;
                }
                // a window moved/resized outside of a drag (and not because of us)
                let parked =
                    s.ws.get(&h).is_some_and(|ws| *ws != s.cur[s.mi(h)]) && is_parked(hwnd);
                if parked {
                    return;
                }
                if s.last[s.mi(h)].contains(&h) {
                    let mi = s.mi(h);
                    let exhausted = s
                        .geometry_retries
                        .get(&h)
                        .is_some_and(|(target, attempts)| {
                            *attempts >= 3
                                && s.last[mi]
                                    .iter()
                                    .position(|window| *window == h)
                                    .and_then(|i| s.rects[mi].get(i))
                                    .is_some_and(|r| same_rect(r, target, 0))
                        });
                    if exhausted {
                        return;
                    }
                    s.enforce = true; // a tiled window moved on its own: pull it back
                    dirty = true;
                } else if s.fs_ws.contains_key(&h) || h == GetForegroundWindow().0 as isize {
                    // a tracked fullscreen window changed size (a game left fullscreen), or the
                    // foreground window did: reconcile the window set
                    s.windows_changed = true;
                    dirty = true;
                }
            }
            EVENT_OBJECT_SHOW => {
                if let Some(dragged) = s.dragging {
                    snapshot::cancel(s.mi(dragged));
                }
                s.windows_changed = true;
                dirty = true;
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
                let mi = s
                    .mon
                    .get(&h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
                    .min(MON - 1);
                let ws = s.ws.get(&h).copied().or_else(|| s.fs_ws.get(&h).copied());
                if !recently || ws != Some(s.cur[mi]) {
                    snapshot::clear();
                }
                match ws {
                    Some(w) if w != s.cur[mi] && !s.paused && !recently => {
                        s.follow = Some((mi, w, h));
                        s.windows_changed = true;
                        dirty = true;
                    }
                    _ => {
                        // focus change inside the current workspace (a click on a tile): no re-tile
                        // is needed, but the border overlay must move to the newly focused window
                        reposition = true;
                        return;
                    }
                }
            }
            EVENT_OBJECT_CREATE if !IsWindowVisible(hwnd).as_bool() => {}
            EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED | EVENT_SYSTEM_MINIMIZESTART
                if !s.ws.contains_key(&h) && !s.fs_ws.contains_key(&h) => {}
            EVENT_OBJECT_CREATE
            | EVENT_OBJECT_HIDE
            | EVENT_OBJECT_CLOAKED
            | EVENT_OBJECT_UNCLOAKED
            | EVENT_SYSTEM_MINIMIZESTART
            | EVENT_SYSTEM_MINIMIZEEND => {
                s.windows_changed = true;
                dirty = true;
            }
            EVENT_OBJECT_DESTROY if forget_window(&mut s, h) => {
                s.windows_changed = true;
                dirty = true;
            }
            _ => {}
        }
        // (Re)arm the debounce timer only when this event actually changed something. Bursts still
        // collapse into a single re-tile (SetTimer resets the countdown), but events we ignore no
        // longer schedule a full re-collect of every window.
        if dirty {
            let now = Instant::now();
            let since = *s.dirty_since.get_or_insert(now);
            // Stop moving the deadline after a sustained burst. A noisy client
            // cannot keep resetting another window's reconciliation indefinitely.
            if s.timer == 0 || now.saturating_duration_since(since) < Duration::from_millis(120) {
                s.timer = SetTimer(None, s.timer, DEBOUNCE_MS, None);
            }
        }
    });
    if reposition {
        // the borrow above is released: move the border onto the window that just got focus
        sync_border();
    }
    if reposition || dirty {
        sync_dividers();
    }
    if resync {
        // the menu just closed: re-evaluate the border from the real foreground window
        refresh_border();
    }
}

/// Does the visible frame of `hwnd` differ from `target` by more than a few pixels?
fn off_target(hwnd: HWND, target: &RECT) -> bool {
    // DWM can temporarily reject the query (for example during a transition). Use the same
    // fallback geometry as placement/animation rather than interpreting that as "on target".
    let Some(vis) = visible_rect(hwnd) else {
        return false;
    };
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
        let mi = s.mi(fg);
        if s.paused
            || s.dragging.is_some()
            || s.rects[mi].len() != s.last[mi].len()
            || s.regions[mi].len() != s.last[mi].len()
        {
            return;
        }
        let n = s.last[mi].len();
        let Some(k) = s.last[mi].iter().position(|h| *h == fg) else {
            return;
        };
        let Some((j, sign)) = split_for_layout(config::layout(), k, n, width) else {
            return;
        };
        let gap = config::inner_gap();
        let q = s.regions[mi][j];
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
        let cur = s.cur[mi];
        let ratios = {
            let rs = s.ratios[mi].entry(cur).or_default();
            rs.resize(n - 1, 0.5);
            let before = rs[j];
            rs[j] = (before + delta).clamp(MIN_RATIO, MAX_RATIO);
            if (rs[j] - before).abs() < 0.0005 {
                return; // already at the limit
            }
            rs.clone()
        };
        let (tiles, regions) = fitted(monitor_work(mi), &s.last[mi], &ratios, &s.min_size);
        let last = s.last[mi].clone();
        let targets: Vec<_> = last.iter().copied().zip(tiles.iter().copied()).collect();
        // Hotkeys must not capture/upload a monitor or queue a frozen transition on
        // every repeat. In particular, a held key commits all neighbours together.
        snapshot::cancel(mi);
        if held {
            place_batch(&mut s, &targets);
        } else {
            for (i, h) in last.iter().enumerate() {
                move_window(&mut s, HWND(*h as *mut _), tiles[i], true);
            }
        }
        s.rects[mi] = tiles;
        s.regions[mi] = regions;
    });
    refresh_border();
}

/// Live drag & drop preview: while the user drags a tiled window over the tile of another one,
/// slide that window into the dragged window's (now empty) slot so it is obvious that releasing
/// here swaps the two. Moving away - or hovering the dragged window's own slot - undoes it.
/// `State::drag_preview` remembers which window was slid where.
fn update_drag_preview(s: &mut State, h: isize) {
    let mi = s.mi(h);
    if s.rects[mi].len() != s.last[mi].len() {
        return;
    }
    let Some(k) = s.last[mi].iter().position(|x| *x == h) else {
        return;
    };
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return;
    }
    let hit = s.rects[mi]
        .iter()
        .position(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom);
    let slot = s.rects[mi][k];
    // cursor over the dragged window's own slot (or over nothing): undo any preview
    if hit.is_none() || hit == Some(k) {
        if let Some((pi, ph)) = s.drag_preview.take() {
            if pi < s.rects[mi].len() {
                let back = s.rects[mi][pi];
                move_window(s, HWND(ph as *mut _), back, true);
            }
        }
        return;
    }
    let i = hit.unwrap();
    if i >= s.last[mi].len() {
        return;
    }
    let partner = s.last[mi][i];
    if partner == h {
        return;
    }
    if let Some((pi, ph)) = s.drag_preview {
        if pi == i {
            // already previewing this slot: only re-slide if something (a re-tile) moved it away
            if let Some(vis) = visible_rect(HWND(ph as *mut _)) {
                if same_rect(&vis, &slot, 2) {
                    return;
                }
            }
        } else {
            // hovering a different tile: send the previous window back first
            if pi < s.rects[mi].len() {
                let back = s.rects[mi][pi];
                move_window(s, HWND(ph as *mut _), back, true);
            }
        }
    }
    move_window(s, HWND(partner as *mut _), slot, true);
    s.drag_preview = Some((i, partner));
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
        let mi = s.mi(h);
        let Some(k) = s.last[mi].iter().position(|x| *x == h) else {
            return;
        };
        let Some(vis) = visible_rect(HWND(h as *mut _)) else {
            return;
        };
        let cur = s.cur[mi];
        let (tiles, regions) = (s.rects[mi].clone(), s.regions[mi].clone());
        let mut rs = s.ratios[mi].remove(&cur).unwrap_or_default();
        let changed = apply_resize_for(
            config::layout(),
            &mut rs,
            &tiles,
            &regions,
            k,
            vis,
            config::inner_gap(),
        );
        if changed {
            // While the user is dragging, lay out the *plain* layout straight from the ratios:
            // the adaptive fitting (`fitted`) reshuffles every tile the moment a minimum size or
            // the narrowed master crosses a threshold, which makes the neighbours jump around
            // mid-drag. The full fitted layout is applied once on release (see `retile`).
            let (new_tiles, new_regions) = layout_tiles(
                config::layout(),
                monitor_work(mi),
                s.last[mi].len(),
                &rs,
                gaps(),
            );
            let last = s.last[mi].clone();
            let targets: Vec<_> = last
                .iter()
                .copied()
                .zip(new_tiles.iter().copied())
                .collect();
            let preview = snapshot::contains(mi) && snapshot::preview(mi, &targets);
            if !preview {
                snapshot::cancel(mi);
                let others: Vec<_> = targets.into_iter().filter(|(hw, _)| *hw != h).collect();
                place_batch(&mut s, &others);
            }
            s.rects[mi] = new_tiles;
            s.regions[mi] = new_regions;
        }
        s.ratios[mi].insert(cur, rs);
    });
}

/// Pulls every tiled window of the current workspace back to its tile. Returns true if a window
/// that is now fullscreen was found: the caller must re-tile so the layout drops it.
fn enforce_layout() -> bool {
    let _t = Timer::new("enforce_layout");
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || s.dragging.is_some() || s.divider.is_some() {
            return false;
        }
        let mut moved = 0;
        let mut fullscreen = false;
        let animating: HashSet<isize> = s.anims.iter().map(|a| a.hwnd).collect();
        for mi in 0..MON {
            if s.rects[mi].len() != s.last[mi].len() {
                continue;
            }
            let (last, rects) = (s.last[mi].clone(), s.rects[mi].clone());
            for (h, r) in last.iter().zip(rects.iter()) {
                let hw = HWND(*h as *mut _);
                if unsafe { IsZoomed(hw).as_bool() || IsIconic(hw).as_bool() } {
                    continue; // maximized / minimized by the user: leave it alone
                }
                if is_fullscreen(hw) {
                    fullscreen = true; // a game went fullscreen: let it be, re-tile drops it
                    continue;
                }
                if animating.contains(h) {
                    continue; // already on its way
                }
                if placement_blocked(&s, hw, r) {
                    continue; // a previous bounded retry also failed for this exact target
                }
                if s.placement_pending(*h, r, Instant::now()) {
                    s.enforce = true;
                    s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
                    continue;
                }
                if observe_min_size(&mut s, hw, r) {
                    s.reflow = true;
                    s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
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
                    if !allow_geometry_retry(&mut s.geometry_retries, *h, *r) {
                        continue;
                    }
                    move_window(&mut s, hw, *r, true);
                    moved += 1;
                }
            }
        }
        if moved > 0 {
            log(format!("snapped {} window(s) back to their tiles", moved));
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
    let mi = monitor::of_window(hwnd).min(MON - 1);
    if config::auto_overflow() && s.last[mi].len() >= tile_budget(mi) {
        return false; // this workspace is full: the normal path moves it to the next one
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    if style & WS_CHILD.0 != 0 {
        return false; // controls and other child windows
    }
    if s.rects[mi].len() != s.last[mi].len() || s.regions[mi].len() != s.last[mi].len() {
        return false;
    }
    let rules = config::rules();
    let w = match classify(hwnd, &rules) {
        Verdict::Tileable(w) => w,
        _ => return false, // not tileable (yet): e.g. title not set, dialog, tool window
    };
    let cur = s.cur[mi];
    if w.rule_ws.is_some_and(|n| n != cur) {
        return false; // belongs to another workspace: the normal path parks it
    }
    s.ws.insert(h, cur);
    s.mon.insert(h, mi);
    if !s.order[mi].contains(&h) {
        s.order[mi].push(h);
    }
    let mut layout = s.last[mi].clone();
    layout.push(h);
    let ratios = {
        let rs = s.ratios[mi].entry(cur).or_default();
        rs.resize(layout.len() - 1, 0.5);
        rs.clone()
    };
    // Put the new window in its provisional tile first: an app with a minimum size refuses to
    // shrink below it, and only after it has been placed do we know that minimum.
    let (prov, _) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size);
    if !place(hwnd, prov[layout.len() - 1]) {
        note_placement_result(s, hwnd, prov[layout.len() - 1], false);
        s.ws.remove(&h);
        s.mon.remove(&h);
        s.order[mi].retain(|window| *window != h);
        return false;
    }
    note_placement_result(s, hwnd, prov[layout.len() - 1], true);
    observe_min_size(s, hwnd, &prov[layout.len() - 1]);
    // Now that the minimum is known, lay the whole workspace out again (the new window's tile may
    // have grown, and the neighbours move aside).
    let (rects, regions) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size);
    let targets: Vec<_> = layout.iter().copied().zip(rects.iter().copied()).collect();
    if !visual_commit(s, mi, &targets, config::animation().1, &[h]) {
        for (i, hh) in layout.iter().enumerate() {
            // the new window appears in its tile instantly; the others glide to make room
            move_window(s, HWND(*hh as *mut _), rects[i], *hh != h);
        }
    }
    s.rects[mi] = rects;
    s.regions[mi] = regions;
    s.last[mi] = layout;
    log(format!(
        "workspace {}: new window placed instantly ({} tiled)",
        cur + 1,
        s.last[mi].len()
    ));
    true
}

fn retile() {
    if STATE.with(|s| s.borrow().divider.is_some()) {
        end_divider(false);
    }
    let _t = Timer::new("retile");
    if STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused {
            return true;
        }
        if s.dragging.is_some() {
            s.windows_changed = true;
            true
        } else {
            false
        }
    }) {
        return;
    }
    let manual = STATE.with(|s| s.borrow().manual_float.clone());
    let c = collect(false, &manual);
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
        let present_set: HashSet<isize> = present.iter().copied().collect();
        // windows we have not seen before are placed instantly; known ones glide
        let fresh: HashSet<isize> = present
            .iter()
            .copied()
            .filter(|h| !s.ws.contains_key(h))
            .collect();
        let animate_all = !std::mem::take(&mut s.no_anim);
        // forget windows that are gone (their observed minimum size goes with them)
        s.min_size
            .retain(|h, _| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        s.manual_float
            .retain(|h| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        // forget windows that are gone; new windows join the current workspace, the workspace a
        // config rule asks for, or - if the current workspace is full - the next workspace
        s.ws.retain(|h, _| present_set.contains(h));
        s.mon.retain(|h, _| present_set.contains(h));
        let mut overflow: Option<(usize, u8, isize)> = None;
        for w in &c.tileable {
            let h = w.hwnd.0 as isize;
            let mi = monitor::of_window(w.hwnd).min(MON - 1);
            if s.ws.contains_key(&h) {
                // remember which monitor the window is on, unless it is parked off-screen (where
                // the nearest monitor would be wrong)
                if !is_parked(w.hwnd) {
                    s.mon.insert(h, mi);
                }
                continue;
            }
            let cur = s.cur[mi];
            let ws = w
                .rule_ws
                .unwrap_or_else(|| workspace_for_new_window(&s, mi, cur));
            s.ws.insert(h, ws);
            s.mon.insert(h, mi);
            if ws != cur && overflow.is_none() {
                overflow = Some((mi, ws, h));
            }
        }
        if overflow.is_some() {
            s.follow = overflow; // switch to the new workspace, like the taskbar "follow"
        }
        // keep known windows in their old order, drop the gone ones, append the new ones
        for mi in 0..MON {
            let mut order: Vec<isize> = s.order[mi]
                .iter()
                .copied()
                .filter(|h| present_set.contains(h) && s.mon.get(h).copied() == Some(mi))
                .collect();
            let mut seen: HashSet<isize> = order.iter().copied().collect();
            for h in &present {
                if s.mon.get(h).copied() == Some(mi) && seen.insert(*h) {
                    order.push(*h);
                }
            }
            s.order[mi] = order;
        }
        // make sure windows of other workspaces stay out of sight, and group the shown ones
        let mut layouts: Vec<Vec<isize>> = vec![Vec::new(); MON];
        for (mi, layout) in layouts.iter_mut().enumerate() {
            let order = s.order[mi].clone();
            for h in order {
                if s.ws[&h] != s.cur[mi] {
                    park(HWND(h as *mut _));
                } else {
                    layout.push(h);
                }
            }
        }
        let mut changed = false;
        for (mi, layout) in layouts.iter().enumerate() {
            let layout = layout.clone();
            let cur = s.cur[mi];
            let ratios = {
                let rs = s.ratios[mi].entry(cur).or_default();
                rs.resize(layout.len().saturating_sub(1), 0.5);
                rs.clone()
            };
            let (rects, regions) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size);
            if layout == s.last[mi] && rects == s.rects[mi] {
                // same windows and same tiles: leave them alone (respects manual tweaks)
                continue;
            }
            let targets: Vec<_> = layout.iter().copied().zip(rects.iter().copied()).collect();
            let (animation_on, ms) = config::animation();
            if !snapshot::supports(mi, &targets) {
                snapshot::cancel(mi);
            }
            // Reuse a scene prepared by a native resize, but never capture here:
            // this path also handles latency-sensitive swaps and drag previews.
            let entering: Vec<_> = layout
                .iter()
                .filter(|h| fresh.contains(h))
                .copied()
                .collect();
            snapshot::entering(mi, &entering);
            let visual = snapshot::contains(mi) && snapshot::reveal(mi);
            s.rects[mi] = rects.clone();
            s.regions[mi] = regions;
            if visual {
                place_batch(&mut s, &targets);
                if snapshot::animate(mi, &targets, if animation_on { ms } else { 1 }) {
                    arm_anim_timer(&mut s);
                } else {
                    snapshot::cancel(mi);
                }
            } else {
                for (h, r) in layout.iter().zip(rects) {
                    if s.dragging == Some(*h) {
                        continue; // the user is dragging it right now: leave it under the cursor
                    }
                    let animate = animate_all && !fresh.contains(h);
                    move_window(&mut s, HWND(*h as *mut _), r, animate);
                }
            }
            log(format!(
                "monitor {}: workspace {}: tiled {} window(s)",
                mi + 1,
                s.cur[mi] + 1,
                layout.len()
            ));
            s.last[mi] = layout;
            changed = true;
        }
        if std::mem::take(&mut s.min_size_reflow) && s.anims.is_empty() && !s.paused {
            s.reflow = true;
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
        } else if changed && s.anims.is_empty() && !s.paused {
            // Verify instant placements too; this catches clients that adjust their own
            // geometry after the asynchronous placement request was posted.
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        }
    });
    // the focused window may only now have joined the layout: make sure it has the border
    refresh_border();
    sync_indicator();
}

fn focus_hwnd(h: isize) {
    unsafe {
        let _ = SetForegroundWindow(HWND(h as *mut _));
    }
    // our own SetForegroundWindow produces a foreground event that the hook skips
    // (WINEVENT_SKIPOWNPROCESS), so update the border here as well
    STATE.with(|s| update_border(&mut s.borrow_mut(), h));
    sync_border();
}

fn focus_first_or_desktop() {
    let first = STATE.with(|s| {
        let s = s.borrow();
        let mi = active_mon(&s);
        s.last[mi].first().copied()
    });
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
        let mi = s.mi(fg);
        if s.paused || s.last[mi].is_empty() || s.rects[mi].len() != s.last[mi].len() {
            return;
        }
        let Some(cur) = s.last[mi].iter().position(|h| *h == fg) else {
            // foreground window is not tiled here: just jump to the first tiled one
            if !swap {
                to_focus = Some(s.last[mi][0]);
            }
            return;
        };
        let Some(to) = neighbour(&s.rects[mi], cur, dir) else {
            return;
        };
        if swap {
            snapshot::cancel(mi);
            let (a, b) = (s.last[mi][cur], s.last[mi][to]);
            let (ia, ib) = (
                s.order[mi].iter().position(|h| *h == a),
                s.order[mi].iter().position(|h| *h == b),
            );
            if let (Some(ia), Some(ib)) = (ia, ib) {
                s.order[mi].swap(ia, ib);
            }
            s.last[mi].clear(); // force a re-layout with the new order
            do_retile = true;
        } else {
            to_focus = Some(s.last[mi][to]);
        }
    });
    if let Some(h) = to_focus {
        focus_hwnd(h);
    }
    if do_retile {
        retile();
    }
}

/// Switch the workspace of the monitor whose indicator was clicked.
fn switch_ws_on_monitor(mi: usize, n: u8) {
    switch_ws(mi, n, None);
}

/// Switch the workspace on the active monitor (keyboard action).
fn switch_ws_active(n: u8) {
    let mi = STATE.with(|s| active_mon(&s.borrow()));
    switch_ws(mi, n, None);
}

/// The current workspace of `mi` offset by `step`, wrapped into `0..WS_COUNT`.
fn relative_ws(mi: usize, step: i8) -> u8 {
    let cur = STATE.with(|s| s.borrow().cur[mi]) as i8;
    (cur + step).rem_euclid(WS_COUNT as i8) as u8
}

/// Switch the workspace of the monitor the user is on, `step` away (wrapping).
fn switch_ws_relative(step: i8) {
    let mi = STATE.with(|s| active_mon(&s.borrow()));
    let n = relative_ws(mi, step);
    switch_ws(mi, n, None);
}

/// Move the focused window to the workspace `step` away (wrapping) and follow it there.
fn move_ws_relative(step: i8) {
    let mi = STATE.with(|s| {
        let s = s.borrow();
        let h = unsafe { GetForegroundWindow() }.0 as isize;
        s.mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
            .min(MON - 1)
    });
    let n = relative_ws(mi, step);
    move_to_ws(n);
}

fn switch_ws(mi: usize, n: u8, focus: Option<isize>) {
    end_divider(false);
    let mut departure = None;
    let mut offset = -48;
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if mi >= MON || s.paused || s.dragging.is_some() || n == s.cur[mi] || n >= WS_COUNT {
            return false;
        }
        snapshot::cancel(mi);
        if config::snapshot_animations() && config::animation().0 {
            let sources: Vec<_> = s.last[mi]
                .iter()
                .filter_map(|h| visible_rect(HWND(*h as *mut _)).map(|r| (*h, r)))
                .collect();
            if sources.len() == s.last[mi].len()
                && snapshot::prepare(mi, monitor_work(mi), &sources)
            {
                departure = snapshot::hold(mi);
                if n < s.cur[mi] {
                    offset = 48;
                }
            }
        }
        s.cur[mi] = n;
        s.no_anim = true;
        let affected: HashSet<isize> = s.order[mi].iter().copied().collect();
        s.anims.retain(|a| !affected.contains(&a.hwnd));
        s.last[mi].clear();
        s.last_switch = Some(Instant::now());
        true
    });
    if !changed {
        return;
    }
    STATE.with(|s| s.borrow_mut().clear_placement_history());
    retile();
    match focus {
        Some(h) => focus_hwnd(h),
        None => focus_workspace(mi, n),
    }
    if let Some(scene) = departure {
        if snapshot::depart(mi, scene, offset, config::animation().1) {
            STATE.with(|s| arm_anim_timer(&mut s.borrow_mut()));
        } else {
            snapshot::cancel(mi);
        }
    }
    tray::update();
    log(format!("switched to workspace {}", n + 1));
}

/// Focuses the fullscreen window of workspace `n` on monitor `mi` if it has one, otherwise the
/// first tiled one.
fn focus_workspace(mi: usize, n: u8) {
    let fs = STATE.with(|s| {
        let s = s.borrow();
        s.fs_ws
            .iter()
            .find(|(h, w)| {
                let hw = HWND(**h as *mut _);
                **w == n
                    && s.fs_mon
                        .get(*h)
                        .copied()
                        .unwrap_or_else(|| monitor::of_window(hw).min(MON - 1))
                        == mi
                    && !unsafe { IsIconic(hw) }.as_bool()
            })
            .map(|(h, _)| *h)
    });
    match fs {
        Some(h) => focus_hwnd(h),
        None => focus_first_or_desktop(),
    }
}

fn move_to_ws(n: u8) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let (mi, cur) = STATE.with(|s| {
        let s = s.borrow();
        let mi = s
            .mon
            .get(&fg)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(fg as *mut _)))
            .min(MON - 1);
        (mi, s.cur[mi])
    });
    // Reassign the window to workspace `n`: a tiled window moves its `ws` entry, a fullscreen one
    // its `fs_ws` entry. Either way `retile` below makes the new workspace show the right set.
    let moved = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.paused || s.dragging.is_some() || n >= WS_COUNT {
            return false;
        }
        if let Some(w) = s.fs_ws.get(&fg).copied() {
            if w == n {
                return false;
            }
            s.fs_ws.insert(fg, n);
            return true;
        }
        if !s.ws.contains_key(&fg) || s.ws[&fg] == n {
            return false;
        }
        s.ws.insert(fg, n);
        true
    });
    if !moved {
        return;
    }
    if n != cur {
        // follow the window: open the target workspace with the moved window focused
        switch_ws(mi, n, Some(fg));
    } else {
        retile();
        focus_hwnd(fg);
    }
    log(format!("moved a window to workspace {}", n + 1));
}

/// Windows left parked off-screen (e.g. after a crash) are brought back into view.
fn recover_parked() {
    let manual = STATE.with(|s| s.borrow().manual_float.clone());
    let c = collect(false, &manual);
    for w in &c.tileable {
        if is_parked(w.hwnd) {
            let _ = apply_window_pos(
                w.hwnd,
                100,
                100,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                "recover-parked",
            );
            log("recovered a window that was parked off-screen");
        }
    }
}

/// On exit: put every window of every workspace into the current workspace and tile them.
fn bring_everything_back() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        // bring every window to its own monitor's current workspace
        let ws: Vec<(isize, u8)> = s.ws.keys().map(|h| (*h, s.cur[s.mi(*h)])).collect();
        for (h, w) in ws {
            s.ws.insert(h, w);
        }
        let fs: Vec<(isize, u8)> = s
            .fs_ws
            .keys()
            .map(|h| {
                let mi = s
                    .fs_mon
                    .get(h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(HWND(*h as *mut _)).min(MON - 1));
                (*h, s.cur[mi])
            })
            .collect();
        for (h, w) in fs {
            s.fs_ws.insert(h, w); // so the fullscreen windows are not hidden again below
        }
        s.clear_layouts();
        s.paused = false;
        s.no_anim = true; // the process is about to exit: no gliding
        restore_hidden_fullscreen(&mut s);
    });
    retile();
}

fn toggle_pause() {
    end_divider(false);
    let paused = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.paused = !s.paused;
        if s.paused {
            s.anims.clear();
            cancel_anim_timer(&mut s);
            snapshot::clear();
        }
        s.paused
    });
    log(if paused { "paused" } else { "resumed" });
    register_hotkeys();
    tray::update();
    if paused {
        clear_border();
        sync_dividers();
        STATE.with(|s| restore_hidden_fullscreen(&mut s.borrow_mut()));
    }
    if !paused {
        STATE.with(|s| s.borrow_mut().clear_layouts());
        retile();
        refresh_border();
    }
}

/// Opens the settings launcher (also used by the tray menu).
pub fn show_launcher() {
    launcher::show();
}

/// Quits tile-rs (used by the launcher's "Quit" button).
pub fn dispatch_quit() {
    unsafe {
        PostQuitMessage(0);
    }
}

pub fn reload_config() {
    end_divider(false);
    snapshot::clear();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.anims.clear();
        cancel_anim_timer(&mut s);
    });
    STATE.with(|s| s.borrow_mut().clear_placement_history());
    match config::load() {
        Ok(c) => {
            overlay::set_indicator_size(c.workspace_indicator_size);
            config::set(c);
            overlay::set_border_color(config::border_color_value());
            let failed = register_hotkeys();
            STATE.with(|s| s.borrow_mut().clear_layouts());
            retile();
            tray::update();
            clear_border(); // the colour (or the on/off switch) may have changed
            refresh_border();
            sync_indicator();
            apply_focus_follows_timer();
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
    });
    unsafe {
        let _ = ShowWindowAsync(fg, SW_MINIMIZE);
    }
    log("minimized the focused window");
}

/// Hotkey: close the focused window (like Alt+F4).
fn close_focused() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(fg), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    log("closed the focused window");
}

/// Hotkey: float the focused window (leave it where it is) or, if it is already floating, tile it
/// again. The choice lasts for this session; a config reload does not clear it.
fn toggle_float() {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let h = fg.0 as isize;
    let floating = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.manual_float.remove(&h) {
            false
        } else {
            s.manual_float.insert(h);
            true
        }
    });
    retile();
    refresh_border();
    if floating {
        log("floating the focused window");
    } else {
        log("tiling the focused window again");
    }
}

/// Hotkey: switch to the next layout (spiral -> master-stack -> grid -> columns -> spiral) and
/// remember it in `config.toml`, so the choice survives a restart like the launcher's picker.
fn cycle_layout() {
    let next = match config::layout() {
        Layout::Spiral => Layout::MasterStack,
        Layout::MasterStack => Layout::Grid,
        Layout::Grid => Layout::Columns,
        Layout::Columns => Layout::Spiral,
    };
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = config::edit_top(&text, &[("layout", format!("\"{}\"", next.name()))]);
    if let Err(e) = config::save_atomic(&path, &new) {
        log(format!("could not save the layout to config.toml: {e}"));
        return;
    }
    reload_config();
    notify("tile-rs", &format!("Layout: {}", next.name()));
}

pub fn dispatch(a: Action) {
    end_divider(false);
    match a {
        Action::Focus(d) => focus_or_swap(d, false),
        Action::Swap(d) => focus_or_swap(d, true),
        Action::Workspace(n) => switch_ws_active(n),
        Action::MoveTo(n) => move_to_ws(n),
        Action::WorkspaceNext => switch_ws_relative(1),
        Action::WorkspacePrev => switch_ws_relative(-1),
        Action::MoveToNext => move_ws_relative(1),
        Action::MoveToPrev => move_ws_relative(-1),
        Action::Resize { width, grow } => resize_focused(width, grow),
        Action::Minimize => minimize_focused(),
        Action::ToggleFloat => toggle_float(),
        Action::Close => close_focused(),
        Action::Pause => toggle_pause(),
        Action::Quit => unsafe { PostQuitMessage(0) },
        Action::Retile => {
            // also resets the sizes of every workspace to equal halves
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.clear_placement_history();
                for mi in 0..MON {
                    s.ratios[mi].clear();
                }
                s.clear_layouts();
            });
            retile();
        }
        Action::CycleLayout => cycle_layout(),
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

/// Starts or stops the focus-follows-mouse poll to match the current setting (on start-up and after
/// every config reload).
fn apply_focus_follows_timer() {
    let want = config::focus_follows_mouse();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let on = s.ffm_timer != 0;
        if want && !on {
            s.ffm_timer = unsafe { SetTimer(None, 0, FFM_MS, None) };
        } else if !want && on {
            unsafe {
                let _ = KillTimer(None, s.ffm_timer);
            }
            s.ffm_timer = 0;
        }
        if !want {
            s.ffm_last = None;
        }
    });
}

/// Focus-follows-mouse poll: focus the tiled window under the pointer, if it is not focused yet.
/// Runs on its own timer, only while the `focus_follows_mouse` setting is enabled.
fn focus_follows_tick() {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return;
    }
    let under = unsafe { WindowFromPoint(pt) };
    if under.is_invalid() {
        return;
    }
    let root = unsafe { GetAncestor(under, GA_ROOT) };
    if root.is_invalid() {
        return;
    }
    let h = root.0 as isize;
    let (is_tiled, dragging, last) = STATE.with(|s| {
        let s = s.borrow();
        let mi = s.mi(h);
        (
            s.ws.get(&h).is_some_and(|w| *w == s.cur[mi]) && !s.manual_float.contains(&h),
            s.dragging.is_some() || s.divider.is_some(),
            s.ffm_last,
        )
    });
    if dragging {
        return;
    }
    if !is_tiled {
        // the pointer is off the tiled area (desktop, taskbar, a floating window): let the next
        // tile the pointer enters take focus again
        if last.is_some() {
            STATE.with(|s| s.borrow_mut().ffm_last = None);
        }
        return;
    }
    if last == Some(h) {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg != root {
        // SetForegroundWindow is refused unless our process may set the foreground; attaching to
        // the current foreground thread lifts that restriction (the usual FFM trick).
        unsafe {
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let target_thread = GetWindowThreadProcessId(root, None);
            let attach = fg_thread != 0 && target_thread != 0 && fg_thread != target_thread;
            if attach {
                let _ = AttachThreadInput(fg_thread, target_thread, true);
            }
            let _ = SetForegroundWindow(root);
            if attach {
                let _ = AttachThreadInput(fg_thread, target_thread, false);
            }
        }
    }
    STATE.with(|s| s.borrow_mut().ffm_last = Some(h));
}

/// Handles the thread timers: the debounce / live / verify timer and the focus-follows-mouse poll.
fn handle_debounce_timer(wparam: usize) {
    let frame = STATE.with(|s| s.borrow().frame_fallback);
    if frame != 0 && frame == wparam {
        anim_tick();
        return;
    }
    let (timer, ffm, drag_timer, dragging) = STATE.with(|s| {
        let s = s.borrow();
        (s.timer, s.ffm_timer, s.drag_timer, s.dragging)
    });
    if ffm != 0 && wparam == ffm {
        focus_follows_tick();
        return;
    }
    if drag_timer != 0 && wparam == drag_timer {
        let Some(h) = dragging else {
            unsafe {
                let _ = KillTimer(None, drag_timer);
            }
            STATE.with(|s| s.borrow_mut().drag_timer = 0);
            return;
        };
        let hwnd = HWND(h as *mut _);
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                forget_window(&mut s, h);
                s.windows_changed = true;
                s.drag_timer = 0;
                s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            });
            unsafe {
                let _ = KillTimer(None, drag_timer);
            }
            return;
        }
        let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        match unsafe { GetGUIThreadInfo(thread, &mut info) } {
            Ok(()) if info.flags.contains(GUI_INMOVESIZE) => return,
            Ok(()) => {
                // A missed end notification is safe to synthesize only after Windows confirms
                // the owning GUI thread has left its modal move/size loop.
                unsafe {
                    on_event(
                        HWINEVENTHOOK(std::ptr::null_mut()),
                        EVENT_SYSTEM_MOVESIZEEND,
                        hwnd,
                        0,
                        0,
                        thread,
                        0,
                    );
                }
                return;
            }
            Err(error) => {
                let expired = STATE.with(|s| {
                    s.borrow()
                        .drag_started
                        .is_some_and(|started| started.elapsed() >= Duration::from_secs(15))
                });
                if expired {
                    log(format!(
                        "move-size watchdog could not query GUI thread state; finalizing drag: {error}"
                    ));
                    unsafe {
                        on_event(
                            HWINEVENTHOOK(std::ptr::null_mut()),
                            EVENT_SYSTEM_MOVESIZEEND,
                            hwnd,
                            0,
                            0,
                            thread,
                            0,
                        );
                    }
                }
                return;
            }
        }
    }
    if wparam != timer {
        return;
    }
    unsafe {
        let _ = KillTimer(None, timer);
    }
    let (follow, reflow, enforce, live) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.timer = 0;
        s.dirty_since = None;
        let reflow = std::mem::take(&mut s.reflow);
        let enforce = std::mem::take(&mut s.enforce);
        let live = std::mem::take(&mut s.live);
        if reflow {
            s.clear_layouts(); // force a fresh layout (snap back / apply a swap)
        }
        (s.follow.take(), reflow, enforce, live)
    });
    match follow {
        Some((mi, w, h)) => switch_ws(mi, w, Some(h)),
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
    sync_border();
    sync_dividers();
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
    let _mutex = match unsafe { CreateMutexW(None, true, w!("Local\\tile-rs-single-instance")) } {
        Ok(handle) => handle,
        Err(error) => {
            log(format!("could not acquire manager singleton: {error}"));
            notify(
                "tile-rs",
                "Could not acquire the single-instance lock. See tile-rs.log.",
            );
            return;
        }
    };
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
        Ok(c) => {
            overlay::set_indicator_size(c.workspace_indicator_size);
            config::set(c);
        }
        Err(e) => {
            log(format!("config error: {e}"));
            notify("tile-rs: config error (built-in defaults are used)", &e);
        }
    }

    if interactive && first_run {
        let yes = msgbox(
            "tile-rs - first start",
            "Start tile-rs automatically when you sign in to Windows?\n\n\
             (You can change this later in the launcher or the tray menu.)",
            true,
        );
        if yes {
            set_autostart(true);
        }
    }

    window::SKIP_ZOOMED.store(true, Ordering::Relaxed);
    remember_monitor_slots();
    recover_parked();
    let mut event_hooks: Vec<HWINEVENTHOOK> = Vec::new();
    let mut hook_failures = Vec::new();
    unsafe {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let mut hook = |lo: u32, hi: u32| {
            let handle = SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, flags);
            if handle.0.is_null() {
                hook_failures.push(format!(
                    "{lo:#06x}-{hi:#06x}: Win32 error {}",
                    GetLastError().0
                ));
            } else {
                event_hooks.push(handle);
            }
        };
        hook(EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE); // created / destroyed / shown / hidden
        hook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND);
        hook(EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED); // other virtual desktop
        hook(EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND); // drag / resize
        hook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE);
        hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND);
        hook(EVENT_SYSTEM_MENUPOPUPSTART, EVENT_SYSTEM_MENUPOPUPEND); // right-click menus
    }
    if !hook_failures.is_empty() {
        log(format!(
            "WinEvent hook registration failed: {}",
            hook_failures.join("; ")
        ));
        notify(
            "tile-rs: some window events are unavailable",
            "Window tracking may be incomplete. Check tile-rs.log for the failed hooks.",
        );
    }
    let failed = register_hotkeys();
    tray::create();
    launcher::create();
    update::start_background_checks();
    if !overlay::create_border() {
        log("could not create the border overlay window");
    }
    overlay::set_border_color(config::border_color_value());
    overlay::set_indicator_click(switch_ws_on_monitor);
    overlay::create_indicator(monitor::count());
    let anim_timer = create_anim_timer();
    STATE.with(|s| s.borrow_mut().anim_timer = anim_timer);
    apply_focus_follows_timer();
    if config::focus_follows_mouse() {
        log("focus follows the mouse pointer");
    }
    log("tile-rs running");
    if !failed.is_empty() {
        notify(
            "tile-rs: some hotkeys were not registered",
            &failed.join("\n"),
        );
    }
    retile();
    refresh_border();
    if interactive {
        launcher::show();
    }

    // One thread, one message loop. The animation frames come from a high-resolution waitable
    // timer (not WM_TIMER, which is the lowest priority and stutters while the mouse moves);
    // the debounce / live / verify timers stay ordinary SetTimer messages.
    unsafe {
        let mut msg = MSG::default();
        let timers = [anim_timer];
        let handles = if anim_timer.is_invalid() {
            &timers[..0]
        } else {
            &timers[..]
        };
        loop {
            replay_events();
            let r = MsgWaitForMultipleObjectsEx(
                Some(handles),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if r == WAIT_FAILED {
                log("MsgWaitForMultipleObjectsEx failed; stopping the message loop");
                break;
            }
            if !handles.is_empty() && r == WAIT_OBJECT_0 {
                anim_tick();
            }
            let mut quit = false;
            let drain_started = Instant::now();
            let mut drained = 0;
            while drained < 64
                && drain_started.elapsed() < Duration::from_millis(4)
                && PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool()
            {
                drained += 1;
                if msg.message == WM_QUIT {
                    quit = true;
                    break;
                }
                if msg.message == WM_PENDING_EVENTS {
                    PENDING_WAKE_POSTED.with(|p| p.set(false));
                    replay_events();
                    continue;
                }
                // Only *thread* timers (created with a NULL hwnd, i.e. the debounce / live / verify
                // timer) are ours to handle here. A WM_TIMER that belongs to a window (the
                // indicator's hover animation) must go through DispatchMessage to its own window
                // procedure - swallowing it would leave that timer running forever.
                if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
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
                if drained >= 64 || drain_started.elapsed() >= Duration::from_millis(4) {
                    break;
                }
            }
            if quit {
                break;
            }
        }
    }
    end_divider(false);
    divider::sync(Vec::new());
    clear_border();
    snapshot::clear();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.anims.clear();
        cancel_anim_timer(&mut s);
    });
    bring_everything_back();
    tray::remove();
    launcher::destroy();
    overlay::destroy_indicator();
    overlay::destroy_border();
    let (ffm_timer, drag_timer) = STATE.with(|s| {
        let s = s.borrow();
        (s.ffm_timer, s.drag_timer)
    });
    if ffm_timer != 0 {
        unsafe {
            let _ = KillTimer(None, ffm_timer);
        }
    }
    if drag_timer != 0 {
        unsafe {
            let _ = KillTimer(None, drag_timer);
        }
    }
    unsafe {
        let _ = CloseHandle(anim_timer);
        for hook in event_hooks {
            let _ = UnhookWinEvent(hook);
        }
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
        prof::PROFILE.store(true, Ordering::Relaxed);
    }
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        None => start(true),
        Some("run") | Some("--autostart") => start(false),
        Some("--launcher-ui") => launcher::run(),
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
            let c = collect(true, &HashSet::new());
            if cmd.as_deref() == Some("list") {
                let _ = writeln!(out, "TILEABLE ({}):", c.tileable.len());
                let mut grouped = vec![Vec::new(); MON];
                for w in &c.tileable {
                    grouped[monitor::of_window(w.hwnd).min(MON - 1)].push(w);
                }
                for (mi, wins) in grouped.iter().enumerate() {
                    if wins.is_empty() {
                        continue;
                    }
                    let area = monitor_work(mi);
                    let _ = writeln!(
                        out,
                        "\nMONITOR {} ({}x{} at {}, {}) - {} tileable:",
                        mi + 1,
                        area.right - area.left,
                        area.bottom - area.top,
                        area.left,
                        area.top,
                        wins.len()
                    );
                    for w in wins {
                        let _ = writeln!(out, "  [{:>6}] {:<28} {}", w.pid, w.class, w.title);
                    }
                }
                if !c.fullscreen.is_empty() {
                    let _ = writeln!(out, "\nFULLSCREEN ({}):", c.fullscreen.len());
                    for w in &c.fullscreen {
                        let _ = writeln!(
                            out,
                            "  [{:>6}] {:<28} {}  <- {}",
                            w.pid,
                            w.class,
                            w.title,
                            window::FULLSCREEN_REASON
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
                for mi in 0..monitor::count().min(MON) {
                    let area = monitor_work(mi);
                    let _ = writeln!(
                        out,
                        "WORK AREA monitor {}: {}x{} at ({}, {})",
                        mi + 1,
                        area.right - area.left,
                        area.bottom - area.top,
                        area.left,
                        area.top
                    );
                }
            } else {
                let mut grouped = vec![Vec::new(); MON];
                for w in &c.tileable {
                    grouped[monitor::of_window(w.hwnd).min(MON - 1)].push(w);
                }
                for (mi, wins) in grouped.iter().enumerate() {
                    if wins.is_empty() {
                        continue;
                    }
                    let (rects, _) =
                        layout_tiles(config::layout(), monitor_work(mi), wins.len(), &[], gaps());
                    for (w, r) in wins.iter().zip(rects) {
                        let _ = writeln!(
                            out,
                            "{}: {} -> {}x{} at ({}, {})",
                            mi + 1,
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

    #[test]
    fn repeated_client_repositioning_has_a_budget_per_target_and_window() {
        let mut retries = HashMap::new();
        let target = RECT {
            left: 10,
            top: 10,
            right: 500,
            bottom: 500,
        };
        for _ in 0..3 {
            assert!(allow_geometry_retry(&mut retries, 1, target));
        }
        assert!(!allow_geometry_retry(&mut retries, 1, target));
        assert!(allow_geometry_retry(&mut retries, 2, target));
        assert!(allow_geometry_retry(
            &mut retries,
            1,
            RECT {
                right: 600,
                ..target
            }
        ));
    }

    #[test]
    fn frame_rate_limits_bound_visual_work_without_busy_polling() {
        assert_eq!(frame_interval_ms(240, 60), 17);
        assert_eq!(frame_interval_ms(144, 30), 34);
        assert_eq!(frame_interval_ms(60, 240), 17);
        assert_eq!(frame_interval_ms(240, 240), 5);
        assert_eq!(frame_interval_ms(0, 60), 1000);
    }

    #[test]
    fn unknown_destroy_does_not_schedule_a_desktop_scan() {
        STATE.with(|state| *state.borrow_mut() = State::default());
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                EVENT_OBJECT_DESTROY,
                HWND(987654usize as *mut _),
                0,
                0,
                0,
                0,
            );
        }
        STATE.with(|state| {
            let s = state.borrow();
            assert!(!s.windows_changed);
            assert_eq!(s.timer, 0);
        });
    }

    #[test]
    fn rejected_height_does_not_lock_width_and_vice_versa() {
        assert_eq!(rejected_minimum((0, 0), (2200, 600), (2200, 650)), (0, 650));
        assert_eq!(rejected_minimum((0, 0), (400, 1100), (480, 1100)), (480, 0));
        assert_eq!(
            rejected_minimum((480, 650), (2200, 800), (2201, 802)),
            (480, 650)
        );
        assert_eq!(
            rejected_minimum((480, 650), (400, 600), (500, 700)),
            (500, 700)
        );
    }

    #[test]
    fn pending_placement_cannot_be_learned_as_a_minimum_size() {
        let mut state = State::default();
        let target = RECT {
            left: 10,
            top: 10,
            right: 310,
            bottom: 210,
        };
        let now = Instant::now();
        state.expected.insert(11, (target, now));
        assert!(state.placement_pending(11, &target, now));
        assert!(!state.placement_pending(12, &target, now));
        assert!(!state.placement_pending(
            11,
            &RECT {
                right: 410,
                ..target
            },
            now
        ));
        assert!(!state.placement_pending(
            11,
            &target,
            now + Duration::from_millis(VERIFY_MS.into())
        ));
    }

    #[test]
    fn stalled_window_does_not_block_individual_or_batch_placement() {
        use std::sync::mpsc;
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};

        // This hidden window deliberately stops pumping messages. It models a client
        // busy with an expensive resize, without touching any of the user's windows.
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner = std::thread::spawn(move || unsafe {
            let hwnd = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs placement regression"),
                WS_POPUP,
                0,
                0,
                1000,
                800,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            ready_tx.send(hwnd.0 as isize).unwrap();
            release_rx.recv().unwrap();
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            let mut actual = RECT::default();
            GetWindowRect(hwnd, &mut actual).unwrap();
            DestroyWindow(hwnd).unwrap();
            actual
        });
        let h = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Always release the client, even if a regression makes placement synchronous.
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            release_tx.send(()).unwrap();
        });
        let hwnd = HWND(h as *mut _);
        let started = Instant::now();
        let accepted = apply_window_pos(
            hwnd,
            10,
            10,
            200,
            200,
            SWP_NOZORDER | SWP_NOACTIVATE,
            "test",
        );
        let individual_elapsed = started.elapsed();
        let target = RECT {
            left: 30,
            top: 40,
            right: 330,
            bottom: 240,
        };
        let mut state = State::default();
        let started = Instant::now();
        place_batch(&mut state, &[(h, target)]);
        let batch_elapsed = started.elapsed();
        let actual = owner.join().unwrap();
        release.join().unwrap();
        assert!(accepted);
        assert!(
            individual_elapsed < Duration::from_millis(450),
            "individual placement blocked: {individual_elapsed:?}"
        );
        assert!(
            batch_elapsed < Duration::from_millis(450),
            "batch placement blocked: {batch_elapsed:?}"
        );
        assert_eq!(actual, target);
        assert!(!state.min_size.contains_key(&h));
    }

    #[test]
    fn browser_can_grow_back_after_neighbour_rejects_height() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 3440,
            bottom: 1440,
        };
        let gaps = Gaps {
            inner: 10,
            outer: 10,
        };
        let ratios = vec![0.35, 0.55];
        let (tiles, regions) = layout_tiles(Layout::Spiral, area, 3, &ratios, gaps);
        let right_width = tiles[1].right - tiles[1].left;
        let observed = rejected_minimum((0, 0), (right_width, 600), (right_width, 650));
        let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gaps.inner)[0];
        let origin = POINT {
            x: handle.rect.left,
            y: handle.rect.top,
        };
        let gesture = divider::Resize {
            handle,
            layout: Layout::Spiral,
            area,
            gaps,
            windows: vec![1, 2, 3],
            workspace: 0,
            ratios,
            preview: tiles.clone(),
            tiles: tiles.clone(),
            regions,
            minimums: vec![
                (480, 320),
                (480.max(observed.0), 320.max(observed.1)),
                (480, 320),
            ],
            origin,
            last_pointer_axis: origin.x,
        };
        let (_, enlarged, _) = gesture.target(POINT {
            x: origin.x + 200,
            y: origin.y,
        });
        assert_eq!(enlarged[0].right - tiles[0].right, 200);
        let (_, shrunk, _) = gesture.target(POINT {
            x: origin.x - 200,
            y: origin.y,
        });
        assert_eq!(tiles[0].right - shrunk[0].right, 200);
    }

    #[test]
    fn reentrant_destroy_event_is_replayed_after_the_layout_borrow() {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            *state = State::default();
            state.ws.insert(11, 0);
            unsafe {
                on_event(
                    HWINEVENTHOOK(std::ptr::null_mut()),
                    EVENT_OBJECT_DESTROY,
                    HWND(11usize as *mut _),
                    0,
                    0,
                    0,
                    0,
                );
            }
            assert!(state.ws.contains_key(&11));
            assert_eq!(PENDING_EVENTS.with(|events| events.borrow().len()), 1);
        });
        replay_events();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            assert!(!state.ws.contains_key(&11));
            if state.timer != 0 {
                unsafe {
                    let _ = KillTimer(None, state.timer);
                }
            }
            *state = State::default();
        });
        assert!(PENDING_EVENTS.with(|events| events.borrow().is_empty()));
    }

    #[test]
    fn frame_timer_has_a_cancellable_fallback_without_a_waitable_handle() {
        let mut state = State::default();
        arm_anim_timer(&mut state);
        assert!(state.frame_armed);
        assert_ne!(state.frame_fallback, 0);
        cancel_anim_timer(&mut state);
        assert!(!state.frame_armed);
        assert_eq!(state.frame_fallback, 0);
    }

    #[test]
    fn animation_cannot_run_after_workspace_switch_pause_or_manual_drag() {
        let mut state = State::default();
        state.ws.insert(11, 0);
        assert!(state.animation_current(11));
        state.cur[0] = 1;
        assert!(!state.animation_current(11));
        state.cur[0] = 0;
        state.paused = true;
        assert!(!state.animation_current(11));
        state.paused = false;
        state.dragging = Some(11);
        assert!(!state.animation_current(11));
        assert!(!state.animation_current(12));
    }

    #[test]
    fn expected_geometry_does_not_suppress_another_window_or_user_resize() {
        let mut state = State::default();
        let now = Instant::now();
        let target = RECT {
            left: 0,
            top: 0,
            right: 500,
            bottom: 400,
        };
        state.expected.insert(11, (target, now));
        assert!(state.expected_frame(11, &target, now));
        assert!(!state.expected_frame(12, &target, now));
        let resized = RECT {
            right: 700,
            ..target
        };
        assert!(!state.expected_frame(11, &resized, now));
        assert!(!state.expected_frame(11, &target, now + Duration::from_secs(2)));
    }

    #[test]
    fn expected_location_event_still_corrects_a_stale_border() {
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};
        config::set(config::parse("border_width = 2").unwrap());
        unsafe {
            // Keep both test windows outside the desktop.
            let hwnd = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs border regression"),
                WS_POPUP,
                -20000,
                -20000,
                300,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let actual = visible_rect(hwnd).unwrap();
            let h = hwnd.0 as isize;
            assert!(overlay::create_border());
            let stale = RECT {
                left: actual.left - 30,
                ..actual
            };
            place_border(&stale, h);
            let before = overlay::border_rect().unwrap();
            STATE.with(|state| {
                let mut s = state.borrow_mut();
                *s = State::default();
                s.border_on = Some(h);
                s.expected.insert(h, (actual, Instant::now()));
            });
            on_event(
                HWINEVENTHOOK::default(),
                EVENT_OBJECT_LOCATIONCHANGE,
                hwnd,
                0,
                0,
                0,
                0,
            );
            let after = overlay::border_rect().unwrap();
            let reconciled = STATE.with(|state| state.borrow().enforce);
            overlay::destroy_border();
            DestroyWindow(hwnd).unwrap();
            STATE.with(|state| *state.borrow_mut() = State::default());
            assert_ne!(before.left, after.left);
            assert_eq!(after.left, actual.left - 2);
            assert_eq!(after.top, actual.top - 2);
            assert_eq!(after.right, actual.right + 2);
            assert_eq!(after.bottom, actual.bottom + 2);
            assert!(
                !reconciled,
                "own placements must still skip layout reconciliation"
            );
        }
    }

    #[test]
    fn repeating_an_animation_target_preserves_its_start_time() {
        config::set(config::parse("animations = true").unwrap());
        let mut state = State::default();
        let target = RECT {
            left: 0,
            top: 0,
            right: 500,
            bottom: 400,
        };
        let started = Instant::now() - Duration::from_millis(40);
        state.anims.push(Anim {
            hwnd: 11,
            from: RECT::default(),
            to: target,
            border: (0, 0, 0, 0),
            t0: started,
            dur_ms: 140,
            last: None,
        });
        move_window(&mut state, HWND(11usize as *mut _), target, true);
        assert_eq!(state.anims.len(), 1);
        assert_eq!(state.anims[0].t0, started);
    }

    #[test]
    fn destroying_a_window_clears_its_expected_geometry_and_pending_motion() {
        let mut state = State::default();
        state.ws.insert(11, 0);
        state.expected.insert(11, (RECT::default(), Instant::now()));
        state.dragging = Some(11);
        state.resizing = true;
        assert!(forget_window(&mut state, 11));
        assert!(!state.ws.contains_key(&11));
        assert!(!state.expected.contains_key(&11));
        assert!(state.dragging.is_none());
        assert!(!state.resizing);
        assert!(state.reflow);
    }

    fn test_monitor(device: &str, left: i32, right: i32, primary: bool) -> monitor::Monitor {
        let rect = RECT {
            left,
            top: 0,
            right,
            bottom: 100,
        };
        monitor::Monitor {
            handle: windows::Win32::Graphics::Gdi::HMONITOR(std::ptr::null_mut()),
            device: device.into(),
            work: rect,
            full: rect,
            primary,
        }
    }

    #[test]
    fn monitor_slot_map_follows_device_names_when_enumeration_reorders() {
        let old = [
            test_monitor("DISPLAY-A", 0, 100, true),
            test_monitor("DISPLAY-B", 100, 200, false),
        ];
        let new = [
            test_monitor("DISPLAY-B", 100, 200, false),
            test_monitor("DISPLAY-A", 0, 100, true),
        ];
        let identities = std::array::from_fn(|i| old.get(i).map(|m| m.device.clone()));
        let (mapped, exact) = monitor_slot_map(&old, &new, &identities);
        assert_eq!(&mapped[..2], &[1, 0]);
        assert!(exact[0] && exact[1]);
    }

    #[test]
    fn monitor_slot_map_uses_nearest_survivor_when_a_display_disappears() {
        let old = [
            test_monitor("DISPLAY-A", 0, 100, true),
            test_monitor("DISPLAY-B", 100, 200, false),
        ];
        let new = [test_monitor("DISPLAY-A", 0, 100, true)];
        let identities = std::array::from_fn(|i| old.get(i).map(|m| m.device.clone()));
        let (mapped, exact) = monitor_slot_map(&old, &new, &identities);
        assert_eq!(mapped[0], 0);
        assert_eq!(mapped[1], 0);
        assert!(exact[0]);
        assert!(!exact[1]);
    }

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
