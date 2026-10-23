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
mod launcher;
mod launcher_model;
mod live_resize;
mod monitor;
mod overlay;
mod parked;
mod prof;
mod tray;
mod update;
mod window;
#[cfg(test)]
mod window_lifecycle_tests;

use window::{collect, is_fullscreen, visible_rect, Verdict, Win};

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
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
};
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
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetAncestor, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetShellWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, IsZoomed, KillTimer, MessageBoxW, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostMessageW, PostQuitMessage, SendMessageTimeoutW, SetCursorPos, SetForegroundWindow,
    SetTimer, SetWindowPos, ShowWindowAsync, TranslateMessage, WindowFromPoint,
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MENUPOPUPEND,
    EVENT_SYSTEM_MENUPOPUPSTART, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, GA_ROOT, GUITHREADINFO, GUI_INMOVESIZE,
    GWL_STYLE, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_TOPMOST, MB_YESNO, MINMAXINFO,
    MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SMTO_ERRORONEXIT, SM_CXMINTRACK, SM_CYMINTRACK, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE, SW_RESTORE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_GETMINMAXINFO, WM_HOTKEY, WM_QUIT, WM_TIMER, WS_CHILD,
};

use config::{Action, Dir, FullscreenWindows, Layout, WS_COUNT};
use geometry::{
    anim_duration, apply_resize_for, ease, effective_step, fullscreen_action, layout_is_valid,
    lerp_rect, neighbour, place as layout_tiles, place_fitted, same_rect, split_for_layout,
    FsAction, Gaps, MAX_RATIO, MIN_RATIO, RESIZE_PX,
};

use parked::{PARK_X, PARK_Y};

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

/// How many tiles fit on one workspace of monitor `mi`: the configured `max_tiles` (applies even
/// with `auto_overflow` off), otherwise - with `auto_overflow` on - a count derived from the work
/// area and the comfortable minimum tile. Past it, the next window opens on the next workspace.
/// The real geometry check (`valid_layout`) still applies on top of this count.
fn tile_budget(mi: usize) -> usize {
    let configured = config::max_tiles();
    if configured > 0 {
        return configured as usize;
    }
    if !config::auto_overflow() {
        return usize::MAX;
    }
    let area = monitor_work(mi);
    let (mw, mh) = config::min_tile();
    auto_tile_budget(
        i64::from(area.right - area.left),
        i64::from(area.bottom - area.top),
        i64::from(mw),
        i64::from(mh),
    )
}

/// Tiles that fit a `width` x `height` work area: 1 + log2(area / minimum tile area), from 2 to 8.
fn auto_tile_budget(width: i64, height: i64, min_w: i64, min_h: i64) -> usize {
    let area_px = width.max(1) as f64 * height.max(1) as f64;
    let min_px = min_w.max(1) as f64 * min_h.max(1) as f64;
    let ratio = area_px / min_px;
    if ratio <= 1.0 {
        return 2;
    }
    (1.0 + ratio.log2()).floor().clamp(2.0, 8.0) as usize
}

fn valid_layout(
    mi: usize,
    windows: &[isize],
    rects: &[RECT],
    mins: &HashMap<isize, (i32, i32)>,
) -> bool {
    layout_is_valid(
        monitor_work(mi),
        windows,
        rects,
        mins,
        config::min_tile(),
        gaps(),
    )
}

fn workspace_windows(s: &State, mi: usize, ws: u8) -> Vec<isize> {
    let belongs =
        |h: &isize| s.ws.get(h) == Some(&ws) && s.mi(*h) == mi && !s.suspended.contains(h);
    let mut windows: Vec<_> = s.order[mi].iter().copied().filter(belongs).collect();
    let mut added: Vec<_> =
        s.ws.keys()
            .copied()
            .filter(|h| belongs(h) && !windows.contains(h))
            .collect();
    added.sort_unstable();
    windows.extend(added);
    windows
}

fn workspace_accepts(s: &State, mi: usize, ws: u8, h: isize) -> bool {
    let mut windows = workspace_windows(s, mi, ws);
    if !windows.contains(&h) {
        let rank = s.order[mi]
            .iter()
            .position(|window| *window == h)
            .unwrap_or(usize::MAX);
        let at = windows
            .iter()
            .position(|window| {
                s.order[mi]
                    .iter()
                    .position(|candidate| candidate == window)
                    .unwrap_or(usize::MAX)
                    > rank
            })
            .unwrap_or(windows.len());
        windows.insert(at, h);
    }
    if windows.len() > tile_budget(mi) {
        return false;
    }
    let ratios = s.ratios[mi].get(&ws).map(Vec::as_slice).unwrap_or(&[]);
    let monocle = is_monocle(s, mi, ws);
    let (tiles, _) = fitted(monitor_work(mi), &windows, ratios, &s.min_size, monocle);
    valid_layout(mi, &windows, &tiles, &s.min_size)
}

/// Admission is based on a real prospective layout, not an area/logarithm estimate.
fn workspace_for_new_window(s: &State, mi: usize, cur: u8, h: isize) -> Option<u8> {
    let count = if config::auto_overflow() { WS_COUNT } else { 1 };
    (0..count)
        .map(|offset| (cur + offset) % WS_COUNT)
        .find(|ws| workspace_accepts(s, mi, *ws, h))
}

/// The configured inner (between windows) and outer (screen edge) gaps.
fn gaps() -> Gaps {
    Gaps {
        inner: config::inner_gap(),
        outer: config::outer_gap(),
    }
}

/// Whether workspace `ws` of monitor `mi` is shown in monocle.
fn is_monocle(s: &State, mi: usize, ws: u8) -> bool {
    s.monocle[mi].contains(&ws)
}

/// The layout of `windows` on `area`: the configured layout with the adaptive fitting
/// (master shrink / observed per-window minimum sizes) applied (see `geometry::place_fitted`).
fn fitted(
    area: RECT,
    windows: &[isize],
    ratios: &[f32],
    mins: &HashMap<isize, (i32, i32)>,
    monocle: bool,
) -> (Vec<RECT>, Vec<RECT>) {
    let (mw, mh) = config::min_tile();
    let gaps = if config::smart_gaps() && windows.len() == 1 {
        Gaps { inner: 0, outer: 0 }
    } else {
        gaps()
    };
    if monocle {
        return geometry::monocle(area, windows.len(), gaps);
    }
    place_fitted(
        config::layout(),
        area,
        windows,
        ratios,
        mins,
        (mw, mh),
        gaps,
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
        r.top += overlay::indicator_bar_height(monitor::dpi(mi));
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
    let rules = config::rules();
    let excluded: Vec<_> = s
        .fs_ws
        .keys()
        .copied()
        .filter(|h| {
            s.manual_float.contains(h)
                || s.capacity_float.contains(h)
                || window::policy_excludes(HWND(*h as *mut _), &rules)
                || config::fullscreen_windows() == FullscreenWindows::Ignore
        })
        .collect();
    for h in excluded {
        // Release only hiding performed by us; never restore an app the user minimized.
        if s.fs_hidden.remove(&h) {
            unsafe {
                let _ = ShowWindowAsync(HWND(h as *mut _), SW_RESTORE);
            }
        }
        s.fs_ws.remove(&h);
        s.fs_mon.remove(&h);
        if s.follow.is_some_and(|(_, _, window)| window == h) {
            s.follow = None;
        }
    }
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
            .or_else(|| s.ws.get(&h).copied())
            .unwrap_or_else(|| next_free_workspace(s, mi, s.cur[mi]));
        s.fs_ws.insert(h, ws);
        s.fs_mon.insert(h, mi);
        remember_identity(s, w.hwnd);
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
    cur // all workspaces occupied: do not switch to an arbitrary occupied workspace
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
    mut flags: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
    operation: &'static str,
) -> bool {
    let mut current = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut current) }.is_ok() {
        let target = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        if current == target {
            return true;
        }
        flags |= unchanged_position_flags(&current, &target);
    }
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

fn unchanged_position_flags(
    current: &RECT,
    target: &RECT,
) -> windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS {
    let mut flags = windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0);
    if current.left == target.left && current.top == target.top {
        flags |= windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE;
    }
    if current.right - current.left == target.right - target.left
        && current.bottom - current.top == target.bottom - target.top
    {
        flags |= SWP_NOSIZE;
    }
    flags
}

type FrameInsets = (i32, i32, i32, i32);

fn frame_insets(native: &RECT, visible: &RECT) -> Option<FrameInsets> {
    let b = (
        visible.left - native.left,
        visible.top - native.top,
        native.right - visible.right,
        native.bottom - visible.bottom,
    );
    [b.0, b.1, b.2, b.3]
        .iter()
        .all(|v| (0..=64).contains(v))
        .then_some(b)
}

fn inset_target(target: RECT, b: FrameInsets) -> RECT {
    RECT {
        left: target.left - b.0,
        top: target.top - b.1,
        right: target.right + b.2,
        bottom: target.bottom + b.3,
    }
}

fn capture_live_borders(s: &mut State, windows: &[isize]) {
    for h in windows {
        let hwnd = HWND(*h as *mut _);
        let mut native = RECT::default();
        if let Some(visible) = visible_rect(hwnd) {
            if unsafe { GetWindowRect(hwnd, &mut native) }.is_ok() {
                if let Some(border) = frame_insets(&native, &visible) {
                    s.live_borders.insert(*h, border);
                }
            }
        }
    }
}

/// Remember failed geometry requests so a persistently rejecting client cannot trigger an
/// endless verify -> animate -> fail loop. A new target or a manual drag permits fresh attempts.
fn note_placement_result(
    s: &mut State,
    hwnd: HWND,
    target: RECT,
    before: Option<RECT>,
    succeeded: bool,
) {
    let h = hwnd.0 as isize;
    if succeeded {
        s.placement_failures.remove(&h);
        s.expected.insert(h, (target, Instant::now()));
        if s.placements
            .get(&h)
            .is_none_or(|request| request.target != target)
        {
            s.placement_generation = s.placement_generation.wrapping_add(1);
            let request =
                PlacementRequest::new(s.placement_generation, before, target, Instant::now());
            if prof::on() {
                log(format!(
                    "placement: hwnd={h:#x} generation={} before={before:?} target={target:?}",
                    request.generation
                ));
            }
            s.placements.insert(h, request);
        }
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

/// A window moved to another monitor may rescale itself for that monitor's DPI and miss the size
/// that was requested. Move it onto the destination monitor first (position only); the size is
/// then applied by the placement queued right after, once the DPI change has been processed.
fn hop_monitor(hwnd: HWND, native: &RECT, target: &RECT) {
    use windows::Win32::Graphics::Gdi::{
        MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
    };
    let (from, to) = unsafe {
        (
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            MonitorFromRect(target, MONITOR_DEFAULTTONEAREST),
        )
    };
    if from != to {
        let _ = apply_window_pos(
            hwnd,
            native.left,
            native.top,
            native.right - native.left,
            native.bottom - native.top,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            "monitor-hop",
        );
    }
}

fn place(hwnd: HWND, target: RECT) -> bool {
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
    native_target(hwnd, target).is_some_and(|r| {
        hop_monitor(hwnd, &r, &target);
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
            let clamped = monitor::clamp_to_work(*target);
            let target = &clamped;
            if placement_blocked(s, hwnd, target) {
                return None;
            }
            if visible_rect(hwnd).is_some_and(|actual| same_rect(&actual, target, 0)) {
                return None;
            }
            let r = match s.live_borders.get(h) {
                Some(border) => inset_target(*target, *border),
                None => native_target(hwnd, *target)?,
            };
            let mut current = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut current) }.ok()?;
            if current == r {
                return None;
            }
            hop_monitor(hwnd, &r, target);
            Some((
                *h,
                *target,
                r,
                unchanged_position_flags(&current, &r),
                visible_rect(hwnd),
            ))
        })
        .collect();
    if operations.is_empty() {
        return;
    }
    let result = unsafe {
        BeginDeferWindowPos(operations.len() as i32).and_then(|mut batch| {
            for (h, _, r, flags, _) in &operations {
                batch = DeferWindowPos(
                    batch,
                    HWND(*h as *mut _),
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS | *flags,
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
    for (h, target, _, _, before) in operations {
        s.anims.retain(|a| a.hwnd != h);
        let hwnd = HWND(h as *mut _);
        let ok = result.is_ok() || place(hwnd, target);
        note_placement_result(s, hwnd, target, before, ok);
        if ok {
            s.min_size_reflow |= observe_min_size(s, hwnd, &target);
        }
    }
}

fn flush_live_resize(s: &mut State) {
    let targets = s
        .live_resize
        .take_ready(Instant::now(), |h| visible_rect(HWND(h as *mut _)));
    if !targets.is_empty() {
        place_batch(s, &targets);
    }
}

fn is_parked(hwnd: HWND) -> bool {
    parked::at_park_spot(hwnd)
}

/// Move a window off-screen (keeps its size) and record where it came from, so a crash can be
/// undone from the persisted list. The caller saves `s.parked` once after a batch of parks.
fn park(s: &mut State, hwnd: HWND) {
    if is_parked(hwnd) {
        return;
    }
    let mut original = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut original) }.is_ok() {
        s.parked.insert(hwnd.0 as isize, original);
        s.parked_dirty = true;
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

/// A timeout alone is never evidence of a client minimum. Samples must settle,
/// and an explicit WM_GETMINMAXINFO response must confirm the rejected axis.
#[derive(Debug)]
struct PlacementRequest {
    generation: u64,
    before: Option<RECT>,
    target: RECT,
    submitted: Instant,
    sample: Option<(RECT, Instant)>,
    queried: bool,
}

impl PlacementRequest {
    fn new(generation: u64, before: Option<RECT>, target: RECT, submitted: Instant) -> Self {
        Self {
            generation,
            before,
            target,
            submitted,
            sample: None,
            queried: false,
        }
    }

    fn ready_to_query(&mut self, actual: RECT, now: Instant) -> bool {
        if self.queried
            || now.saturating_duration_since(self.submitted)
                < Duration::from_millis(VERIFY_MS.into())
        {
            return false;
        }
        if now.saturating_duration_since(self.submitted) >= Duration::from_secs(2) {
            return true;
        }
        match self.sample {
            Some((previous, since)) if same_rect(&previous, &actual, 2) => {
                now.saturating_duration_since(since) >= Duration::from_millis(VERIFY_MS.into())
            }
            _ => {
                self.sample = Some((actual, now));
                false
            }
        }
    }
}

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
    /// Windows currently parked off-screen with their original rectangles (mirrored to disk).
    parked: parked::Saved,
    parked_dirty: bool,
    /// Automatic safety fallback when no workspace can accommodate an ordinary window.
    capacity_float: HashSet<isize>,
    float_reasons: HashMap<isize, String>,
    /// Live but temporarily unavailable windows keep their workspace without taking a tile.
    suspended: HashSet<isize>,
    /// PID/thread identity guards stale assignments when a destroy notification is lost.
    identities: HashMap<isize, (u32, u32)>,
    /// Timer polling the pointer for `focus_follows_mouse` (0 = disabled).
    ffm_timer: usize,
    /// Watchdog for a lost EVENT_SYSTEM_MOVESIZEEND notification.
    drag_timer: usize,
    /// Last window the pointer selected for focus-follows-mouse (avoids re-focusing it).
    ffm_last: Option<isize>,
    /// Current workspace (0-based) of each monitor.
    cur: [u8; MON],
    /// Workspace each monitor showed before the current one (for `workspace_back_and_forth`).
    prev_ws: [Option<u8>; MON],
    /// Workspaces (per monitor) shown in monocle: every window of it fills the whole area and
    /// only the focused one is visible on top.
    monocle: [HashSet<u8>; MON],
    /// Managed window focused now, and the one focused before it (for `focus_last`).
    focus_cur: Option<isize>,
    focus_prev: Option<isize>,
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
    placements: HashMap<isize, PlacementRequest>,
    placement_generation: u64,
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
    live_resize: live_resize::Scheduler,
    /// Frozen at gesture start: DWM bounds can lag native size changes by a frame.
    live_borders: HashMap<isize, FrameInsets>,
    /// Live drag & drop preview: the tile index whose window is currently slid into the dragged
    /// window's slot, with that window's handle (see `update_drag_preview`).
    drag_preview: Option<(usize, isize)>,
    /// The workspace dot the dragged window was last over (the drop target), as (monitor,
    /// workspace). Kept from the move events so a late, synthesized drag end cannot pick a dot the
    /// cursor only reached afterwards.
    drop_dot: Option<(usize, u8)>,
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
    /// The waitable timer is a one-shot re-armed at every display refresh (see `rearm_frame_timer`).
    frame_oneshot: bool,
    /// Consecutive animation frames that took far longer than a frame interval.
    slow_frames: u32,
    /// While set and in the future, windows move instantly: the machine could not keep up.
    anim_suspended_until: Option<Instant>,
    /// One-shot thread timer that trims the working set after the manager has been idle.
    trim_timer: usize,
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
    /// A new configuration must be allowed to probe smaller sizes again.
    /// Old learned constraints and pending placements belong to the previous layout.
    fn invalidate_layout_constraints(&mut self) {
        self.min_size.clear();
        self.min_size_reflow = false;
        self.capacity_float.clear();
        self.float_reasons.clear();
        self.expected.clear();
        self.clear_placement_history();
        self.clear_layouts();
    }

    fn clear_placement_history(&mut self) {
        self.placement_failures.clear();
        self.geometry_retries.clear();
        self.live_resize.clear();
        self.live_borders.clear();
        self.placements.clear();
        self.expected.clear();
    }

    fn clear_placement_history_for(&mut self, windows: &HashSet<isize>) {
        self.placement_failures.retain(|h, _| !windows.contains(h));
        self.geometry_retries.retain(|h, _| !windows.contains(h));
        self.placements.retain(|h, _| !windows.contains(h));
        self.expected.retain(|h, _| !windows.contains(h));
        self.live_borders.retain(|h, _| !windows.contains(h));
        self.live_resize
            .forget(&windows.iter().copied().collect::<Vec<_>>());
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

type FrameBorder = (i32, i32, i32, i32);

/// Submit the frames of several windows as one batch. Windows that shrink are placed before
/// windows that grow, so a tile vacates its space before its neighbour expands into it and
/// the transient overlap inside a frame is minimal. Returns, per input, whether the request was
/// accepted. A failed batch falls back to individual requests.
fn apply_frames(ops: &[(isize, RECT, FrameBorder)]) -> Vec<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{
        BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos,
    };
    if ops.len() < 2 {
        return ops.iter().map(|(h, r, b)| set_frame(*h, r, *b)).collect();
    }
    struct Item {
        index: usize,
        native: RECT,
        shrink: i64,
        flags: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
    }
    let mut results = vec![true; ops.len()];
    let mut items: Vec<Item> = Vec::with_capacity(ops.len());
    for (index, (h, vis, b)) in ops.iter().enumerate() {
        let native = RECT {
            left: vis.left - b.0,
            top: vis.top - b.1,
            right: vis.right + b.2,
            bottom: vis.bottom + b.3,
        };
        let mut current = RECT::default();
        if unsafe { GetWindowRect(HWND(*h as *mut _), &mut current) }.is_err() {
            results[index] = false;
            continue;
        }
        if current == native {
            continue; // already there
        }
        let area = |r: &RECT| i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
        items.push(Item {
            index,
            native,
            shrink: area(&native) - area(&current),
            flags: unchanged_position_flags(&current, &native),
        });
    }
    if items.is_empty() {
        return results;
    }
    items.sort_by_key(|item| item.shrink); // most negative (shrinking) first
    if prof::on() {
        prof::PLACEMENTS.fetch_add(items.len() as u32, Ordering::Relaxed);
    }
    let batch = unsafe {
        BeginDeferWindowPos(items.len() as i32).and_then(|mut batch| {
            for item in &items {
                batch = DeferWindowPos(
                    batch,
                    HWND(ops[item.index].0 as *mut _),
                    None,
                    item.native.left,
                    item.native.top,
                    item.native.right - item.native.left,
                    item.native.bottom - item.native.top,
                    SWP_NOZORDER
                        | SWP_NOACTIVATE
                        | SWP_NOOWNERZORDER
                        | SWP_ASYNCWINDOWPOS
                        | item.flags,
                )?;
            }
            EndDeferWindowPos(batch)
        })
    };
    if let Err(error) = batch {
        log(format!("frame batch fallback: {error}"));
        for item in &items {
            let (h, vis, b) = &ops[item.index];
            results[item.index] = set_frame(*h, vis, *b);
        }
    }
    results
}

/// Moves a window to `target`: instantly, or - if `animate` and animations are enabled -
/// by gliding. Always cancels an animation that was already running for this window.
fn move_window(s: &mut State, hwnd: HWND, target: RECT, animate: bool) {
    let target = monitor::clamp_to_work(target);
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
    let busy = unsafe { IsZoomed(hwnd).as_bool() } || is_parked(hwnd) || animations_suspended(s);
    // if the frame timer could not be created, place windows instantly rather than freeze them
    if !animate || !on || ms == 0 || busy || s.anim_timer.is_invalid() {
        let before = visible_rect(hwnd);
        let succeeded = place(hwnd, target);
        note_placement_result(s, hwnd, target, before, succeeded);
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
        note_placement_result(s, hwnd, target, None, succeeded);
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
    let border = frame_insets(&win, &from).unwrap_or_default();
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
    if vw <= tw + 2 && vh <= th + 2 {
        return false;
    }
    let Some(request) = s
        .placements
        .get_mut(&h)
        .filter(|request| request.target == *target)
    else {
        return false;
    };
    if !request.ready_to_query(vis, Instant::now()) {
        if !request.queried {
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        }
        return false;
    }
    // Tracking minima are explicit client data, unlike the old large frame of a stalled
    // application. Successful queries happen once per generation; timed-out queries
    // can be retried during a bounded two-second responsiveness grace period.
    let Some(reported) = query_minimum(hwnd, &native, &vis) else {
        if request.submitted.elapsed() < Duration::from_secs(2) {
            request.sample = Some((vis, Instant::now()));
            s.enforce = true;
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
        } else {
            request.queried = true; // bounded stall, never a learned minimum
        }
        return false;
    };
    request.queried = true;
    let previous = s.min_size.get(&h).copied().unwrap_or((0, 0));
    let observed = confirmed_minimum(previous, (tw, th), (vw, vh), reported);
    if observed != previous {
        s.min_size.insert(h, observed);
        log(format!("minimum confirmed: hwnd={h:#x} target={tw}x{th} actual={vw}x{vh} reported={reported:?} minimum={observed:?}"));
        return true;
    }
    false
}

/// Ask a new window for its minimum size before it is laid out, so a layout that cannot hold it
/// is rejected up front instead of overlapping neighbours until a placement fails. Only sizes the
/// client itself reports above the system default (and that fit its monitor) are kept.
fn seed_min_size(s: &mut State, hwnd: HWND) {
    let h = hwnd.0 as isize;
    if s.min_size.contains_key(&h) {
        return;
    }
    let mut native = RECT::default();
    let Some(visible) = visible_rect(hwnd) else {
        return;
    };
    if unsafe { GetWindowRect(hwnd, &mut native) }.is_err() {
        return;
    }
    let (Some(reported), Some(b)) = (
        query_minimum(hwnd, &native, &visible),
        frame_insets(&native, &visible),
    ) else {
        return;
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let default = (
        (unsafe { GetSystemMetricsForDpi(SM_CXMINTRACK, dpi) } - b.0 - b.2).max(0),
        (unsafe { GetSystemMetricsForDpi(SM_CYMINTRACK, dpi) } - b.1 - b.3).max(0),
    );
    let area = monitor::work(monitor::of_window(hwnd));
    let axis = |reported: i32, default: i32, limit: i32| {
        if reported > default + 2 && reported <= limit {
            reported
        } else {
            0
        }
    };
    let min = (
        axis(reported.0, default.0, area.right - area.left),
        axis(reported.1, default.1, area.bottom - area.top),
    );
    if min != (0, 0) {
        s.min_size.insert(h, min);
        log(format!(
            "minimum reported up front: hwnd={h:#x} minimum={min:?}"
        ));
    }
}

fn query_minimum(hwnd: HWND, native: &RECT, visible: &RECT) -> Option<(i32, i32)> {
    let mut info = MINMAXINFO::default();
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    info.ptMinTrackSize = POINT {
        x: unsafe { GetSystemMetricsForDpi(SM_CXMINTRACK, dpi) },
        y: unsafe { GetSystemMetricsForDpi(SM_CYMINTRACK, dpi) },
    };
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETMINMAXINFO,
            WPARAM(0),
            LPARAM(&mut info as *mut MINMAXINFO as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            10,
            None,
        )
    };
    if result.0 == 0 {
        return None;
    }
    let borders = frame_insets(native, visible)?;
    Some((
        (info.ptMinTrackSize.x - borders.0 - borders.2).max(0),
        (info.ptMinTrackSize.y - borders.1 - borders.3).max(0),
    ))
}

fn confirmed_minimum(
    previous: (i32, i32),
    target: (i32, i32),
    actual: (i32, i32),
    reported: (i32, i32),
) -> (i32, i32) {
    let axis = |previous: i32, target: i32, actual: i32, reported: i32| {
        if actual > target + 2 && reported > target + 2 && reported <= actual + 2 {
            previous.max(reported)
        } else {
            previous
        }
    };
    (
        axis(previous.0, target.0, actual.0, reported.0),
        axis(previous.1, target.1, actual.1, reported.1),
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

/// Relative due time (negative, 100 ns units) of the frame that should be submitted just before
/// the next display refresh. `vblank` is any past or future refresh time and `period` the refresh
/// period, both in QPC ticks of frequency `freq`; `now` is the current QPC value. The frame is
/// submitted `lead` ticks before the refresh so the compositor picks it up for that refresh.
fn vsync_due_100ns(now: i64, vblank: i64, period: i64, freq: i64, lead: i64) -> Option<i64> {
    if period <= 0 || freq <= 0 {
        return None;
    }
    let earliest = now + freq / 1000; // never fire in less than 1 ms
    let elapsed = earliest + lead - vblank;
    let k = elapsed.div_euclid(period) + i64::from(elapsed.rem_euclid(period) != 0);
    let fire = vblank + k * period - lead;
    let ticks = (fire - now).max(0);
    Some(-(ticks as i128 * 10_000_000 / freq as i128) as i64)
}

/// The next vsync-aligned frame time, if the compositor reports its refresh timing.
fn vsync_frame_due() -> Option<i64> {
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let mut info = DWM_TIMING_INFO {
        cbSize: size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    let (mut now, mut freq) = (0i64, 0i64);
    unsafe {
        DwmGetCompositionTimingInfo(HWND::default(), &mut info).ok()?;
        QueryPerformanceCounter(&mut now).ok()?;
        QueryPerformanceFrequency(&mut freq).ok()?;
    }
    vsync_due_100ns(
        now,
        info.qpcVBlank as i64,
        info.qpcRefreshPeriod as i64,
        freq,
        freq * 2 / 1000, // 2 ms ahead of the refresh
    )
}

/// Frames follow the display refresh unless the user capped the frame rate below it.
fn frames_follow_vsync() -> bool {
    let _ = anim_frame_ms(); // fills the cached refresh rate
    let hz = DISPLAY_HZ.with(|cache| cache.get()).unwrap_or(60);
    config::animation_fps_limit() >= hz
}

/// After a vsync-aligned frame: schedule the next one for the next refresh. (A periodic timer
/// drifts against the refresh, which makes a frame get skipped every so often.)
fn rearm_frame_timer(s: &mut State) {
    if !s.frame_armed || !s.frame_oneshot || s.anim_timer.is_invalid() {
        return;
    }
    match vsync_frame_due() {
        Some(due) => unsafe {
            let _ = SetWaitableTimer(s.anim_timer, &due, 0, None, None, false);
        },
        None => {
            // timing info became unavailable: fall back to a plain periodic timer
            let frame = anim_frame_ms().max(1);
            let due = -(frame as i64) * 10_000;
            s.frame_oneshot = false;
            unsafe {
                let _ = SetWaitableTimer(s.anim_timer, &due, frame as i32, None, None, false);
            }
        }
    }
}

/// (Re)arms the frame timer. With vsync timing it fires once, just before the next display
/// refresh, and is re-armed after every frame; otherwise one frame interval, then periodically.
fn arm_anim_timer(s: &mut State) {
    if s.frame_armed {
        return;
    }
    let frame = anim_frame_ms().max(1);
    let synced = if frames_follow_vsync() {
        vsync_frame_due()
    } else {
        None
    };
    s.frame_oneshot = synced.is_some();
    let due = synced.unwrap_or(-(frame as i64) * 10_000); // relative time, in 100 ns units
    let period = if synced.is_some() { 0 } else { frame as i32 };
    unsafe {
        s.frame_armed = !s.anim_timer.is_invalid()
            && SetWaitableTimer(s.anim_timer, &due, period, None, None, false).is_ok();
        if !s.frame_armed {
            s.frame_fallback = SetTimer(None, s.frame_fallback, frame, None);
            s.frame_armed = s.frame_fallback != 0;
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
    let tick_started = Instant::now();
    let had_animations = STATE.with(|s| !s.borrow().anims.is_empty());
    divider::tick();
    if !divider::captured() && STATE.with(|s| s.borrow().divider.is_some()) {
        settle_divider();
    }
    let live = STATE.with(|s| std::mem::take(&mut s.borrow_mut().live));
    if live {
        live_resize();
    }
    STATE.with(|s| flush_live_resize(&mut s.borrow_mut()));
    STATE.with(|s| {
        let s = s.borrow();
        if let Some(gesture) = s.divider.as_ref() {
            let observed: Vec<_> = gesture
                .windows
                .iter()
                .enumerate()
                .map(|(i, h)| visible_rect(HWND(*h as *mut _)).unwrap_or(gesture.preview[i]))
                .collect();
            let handles: Vec<_> = divider::handles(
                gesture.handle.monitor,
                gesture.layout,
                &observed,
                &gesture.regions,
                gesture.gaps.inner,
            )
            .into_iter()
            .filter(|handle| handle.split == gesture.handle.split)
            .collect();
            divider::follow(divider::unobstructed(handles, &observed).into_iter().next());
        }
    });
    let profiling = prof::on();
    let easing = config::easing();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.anims.is_empty() {
            // The queue was emptied outside `anim_tick` (e.g. a drag started): stop the timer so
            // it does not keep firing at 60 Hz doing nothing.
            if s.divider.is_none() && !s.resizing {
                cancel_anim_timer(&mut s);
            }
            return;
        }
        let now = Instant::now();
        let mut keep = Vec::with_capacity(s.anims.len());
        let mut setpos = Duration::ZERO;
        let mut min_size_changed = false;
        // Pass 1: where is every window supposed to be in this frame?
        let mut frame: Vec<(Anim, RECT, bool, bool)> = Vec::new();
        for a in std::mem::take(&mut s.anims) {
            if !s.animation_current(a.hwnd) {
                continue;
            }
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
            let apply = a.last.as_ref().is_none_or(|l| !same_rect(l, &rect, 0));
            frame.push((a, rect, done, apply));
        }
        // Pass 2: submit all of them as ONE batch, so the whole layout changes in the same
        // composition frame instead of tile by tile.
        let ts = profiling.then(Instant::now);
        let befores: Vec<Option<RECT>> = frame
            .iter()
            .map(|(a, _, _, apply)| {
                apply
                    .then(|| visible_rect(HWND(a.hwnd as *mut _)))
                    .flatten()
            })
            .collect();
        let ops: Vec<(isize, RECT, FrameBorder)> = frame
            .iter()
            .filter(|(_, _, _, apply)| *apply)
            .map(|(a, rect, _, _)| (a.hwnd, *rect, a.border))
            .collect();
        let mut results = apply_frames(&ops).into_iter();
        if let Some(ts) = ts {
            setpos += ts.elapsed();
        }
        // Pass 3: book-keeping per window.
        for ((mut a, mut rect, mut done, apply), before) in frame.into_iter().zip(befores) {
            let mut can_observe_size = true;
            if apply {
                let applied = results.next().unwrap_or(false);
                note_placement_result(&mut s, HWND(a.hwnd as *mut _), rect, before, applied);
                if !applied {
                    // Do not keep posting frames to a window that rejected placement. Make one
                    // direct final attempt and let the delayed geometry verification reconcile it.
                    let before = visible_rect(HWND(a.hwnd as *mut _));
                    can_observe_size = place(HWND(a.hwnd as *mut _), a.to);
                    note_placement_result(
                        &mut s,
                        HWND(a.hwnd as *mut _),
                        a.to,
                        before,
                        can_observe_size,
                    );
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
            if s.divider.is_none() && !s.resizing {
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
    if had_animations && STATE.with(|s| s.borrow().anims.is_empty()) {
        sync_dividers();
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if had_animations {
            note_frame_cost(
                &mut s,
                tick_started.elapsed(),
                Duration::from_millis(anim_frame_ms().into()),
            );
        }
        rearm_frame_timer(&mut s);
        if s.anims.is_empty() {
            schedule_trim(&mut s);
        }
    });
}

/// Frames that repeatedly cost more than two frame intervals mean the machine cannot animate
/// smoothly: finish the running animations at once and move windows instantly for a while,
/// instead of stuttering through every layout change.
const SLOW_FRAMES_LIMIT: u32 = 3;
const ANIM_SUSPEND: Duration = Duration::from_secs(15);

fn note_frame_cost(s: &mut State, cost: Duration, interval: Duration) {
    if cost <= interval * 2 {
        s.slow_frames = 0;
        return;
    }
    s.slow_frames += 1;
    if s.slow_frames < SLOW_FRAMES_LIMIT {
        return;
    }
    s.slow_frames = 0;
    s.anim_suspended_until = Some(Instant::now() + ANIM_SUSPEND);
    for a in &mut s.anims {
        a.dur_ms = 1; // the next frame lands every window on its final tile
    }
    log(format!(
        "animations suspended for {}s: a frame took {cost:?} (interval {interval:?})",
        ANIM_SUSPEND.as_secs()
    ));
}

fn animations_suspended(s: &State) -> bool {
    s.anim_suspended_until
        .is_some_and(|until| Instant::now() < until)
}

/// Idle time after which the working set is trimmed.
const TRIM_AFTER_MS: u32 = 20_000;

/// Arms (or postpones) the one-shot working-set trim.
fn schedule_trim(s: &mut State) {
    s.trim_timer = unsafe { SetTimer(None, s.trim_timer, TRIM_AFTER_MS, None) };
}

/// Hands pages the manager is not using back to the OS (it sits idle almost all the time).
fn trim_working_set() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
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
    if s.focus_cur != Some(focus) && (s.ws.contains_key(&focus) || s.fs_ws.contains_key(&focus)) {
        s.focus_prev = s.focus_cur;
        s.focus_cur = Some(focus);
    }
    // a tiled window of the current workspace: in the applied layout, or (should the layout list
    // be momentarily stale) a known window that belongs to the current workspace
    let mi = s.mi(focus);
    let tiled = s.last[mi].contains(&focus) || s.ws.get(&focus) == Some(&s.cur[mi]);
    s.border_on = (config::border().is_some()
        && !s.paused
        && s.dragging.is_none()
        && s.divider.is_none()
        && tiled)
        .then_some(focus);
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
            || !s.last.iter().any(|windows| windows.contains(&fg))
        {
            return Vec::new();
        }
        (0..monitor::count().min(MON))
            .flat_map(|mi| {
                let frames: Vec<_> = s.last[mi]
                    .iter()
                    .filter_map(|h| visible_rect(HWND(*h as *mut _)))
                    .collect();
                divider::unobstructed(
                    divider::handles(
                        mi,
                        config::layout(),
                        &s.rects[mi],
                        &s.regions[mi],
                        config::inner_gap(),
                    ),
                    &frames,
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
            || !divider::handles(
                mi,
                config::layout(),
                &s.rects[mi],
                &s.regions[mi],
                config::inner_gap(),
            )
            .iter()
            .any(|candidate| {
                candidate.split == handle.split && candidate.vertical == handle.vertical
            })
        {
            return false;
        }
        let windows = s.last[mi].clone();
        for h in &windows {
            s.placement_failures.remove(h);
            s.geometry_retries.remove(h);
        }
        let sources: Vec<_> = windows
            .iter()
            .filter_map(|h| visible_rect(HWND(*h as *mut _)).map(|r| (*h, r)))
            .collect();
        if sources.len() != windows.len()
            || sources
                .iter()
                .zip(&s.rects[mi])
                .any(|((_, actual), target)| !same_rect(actual, target, 2))
        {
            return false;
        }
        s.live_borders.clear();
        capture_live_borders(&mut s, &windows);
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
        if !s.frame_armed {
            s.divider = None;
            s.live_borders.clear();
            return false;
        }
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
        {
            return false;
        }
        let constraints = s
            .live_resize
            .constrained_targets(Instant::now(), |h| visible_rect(HWND(h as *mut _)));
        let mut minimum_changed = false;
        for (h, target) in constraints {
            if observe_min_size(&mut s, HWND(h as *mut _), &target) {
                let minimum = s.min_size[&h];
                let gesture = s.divider.as_mut().unwrap();
                if let Some(i) = gesture.windows.iter().position(|window| *window == h) {
                    gesture.minimums[i].0 = gesture.minimums[i].0.max(minimum.0);
                    gesture.minimums[i].1 = gesture.minimums[i].1.max(minimum.1);
                    minimum_changed = true;
                }
            }
        }
        if !s.divider.as_mut().unwrap().pointer_changed(point) && !minimum_changed {
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
        s.live_resize.queue(&targets);
        s.divider.as_mut().unwrap().preview = tiles;
        true
    });
    if !valid {
        end_divider(false);
    }
}

fn end_divider(commit: bool) {
    finish_divider(if commit {
        DividerFinish::Pointer
    } else {
        DividerFinish::Cancel
    });
}

#[derive(Clone, Copy)]
enum DividerFinish {
    Cancel,
    Pointer,
    Preview,
}

pub fn settle_divider() {
    // Escape may cause native capture cancellation before the frame timer polls it.
    finish_divider(if unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE.0)) } < 0 {
        DividerFinish::Cancel
    } else {
        DividerFinish::Preview
    });
}

fn finish_divider(finish: DividerFinish) {
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
            && config::layout() == gesture.layout;
        s.live_resize.forget(&gesture.windows);
        let mut point = POINT::default();
        let result = valid
            .then(|| match finish {
                DividerFinish::Cancel => None,
                DividerFinish::Pointer if unsafe { GetCursorPos(&mut point) }.is_ok() => {
                    Some(gesture.target(point))
                }
                _ => Some(gesture.last_target()),
            })
            .flatten();
        let tiles = result
            .as_ref()
            .map_or(&gesture.tiles, |(_, tiles, _)| tiles);
        // Final placement bypasses the frame throttle. Cancellation restores the
        // original geometry, but must never resurrect or unpark a departed window.
        if s.cur[mi] == gesture.workspace {
            let targets: Vec<_> = gesture
                .windows
                .iter()
                .copied()
                .zip(tiles.iter().copied())
                .filter(|(h, _)| {
                    let hwnd = HWND(*h as *mut _);
                    s.ws.get(h) == Some(&gesture.workspace)
                        && s.mi(*h) == mi
                        && unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
                })
                .collect();
            place_batch(&mut s, &targets);
        }
        if let Some((ratios, tiles, regions)) = result {
            s.ratios[mi].insert(gesture.workspace, ratios);
            s.rects[mi] = tiles;
            s.regions[mi] = regions;
        }
        for h in &gesture.windows {
            s.live_borders.remove(h);
        }
        if !s.paused {
            s.enforce = true;
            if std::mem::take(&mut s.min_size_reflow) {
                s.reflow = true;
            }
            s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
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
    STATE.with(|s| {
        let s = s.borrow();
        for (mi, mon) in mons.iter().enumerate().take(MON) {
            let dpi = monitor::dpi(mi);
            let (w, h) = overlay::indicator_window_size(dpi);
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
            overlay::show_indicator(mi, rect, dpi, s.cur[mi], mask);
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
    let old_monitors = monitor::list();
    monitor::refresh();
    let new_monitors = monitor::list();
    reconcile_monitor_slots(&old_monitors, &new_monitors);
    if config::workspace_indicator() {
        let _ = overlay::sync_indicator_count(new_monitors.len());
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.invalidate_layout_constraints();
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
        Action::FocusMonitor(step) => {
            format!("focus_{}_monitor", if step >= 0 { "next" } else { "prev" })
        }
        Action::MoveToMonitor(step) => {
            format!(
                "move_to_{}_monitor",
                if step >= 0 { "next" } else { "prev" }
            )
        }
        Action::ReloadConfig => "reload_config".into(),
        Action::EditConfig => "edit_config".into(),
        Action::ToggleMonocle => "toggle_monocle".into(),
        Action::FocusLast => "focus_last".into(),
        Action::WorkspaceBack => "workspace_back_and_forth".into(),
        Action::MasterCount(step) => if step > 0 {
            "master_count_inc"
        } else {
            "master_count_dec"
        }
        .into(),
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
    let mut refused = String::new();
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
                    refused.push_str(&format!("{}\t{}\n", action_name(*action), spec));
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
    // the settings window reads this to mark the combinations Windows refused
    let _ = std::fs::write(config::hotkey_failures_path(), refused);
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
        || s.capacity_float.contains(&h)
        || s.identities.contains_key(&h)
        || s.placements.contains_key(&h)
        || s.expected.contains_key(&h)
        || s.geometry_retries.contains_key(&h)
        || s.follow.is_some_and(|(_, _, window)| window == h)
        || s.min_size.contains_key(&h)
        || s.placement_failures.contains_key(&h)
        || s.anims.iter().any(|animation| animation.hwnd == h)
        || s.border_on == Some(h)
        || s.order.iter().any(|order| order.contains(&h))
        || s.last.iter().any(|last| last.contains(&h));
    if !tracked {
        return false;
    }
    s.live_resize.forget(&[h]);
    s.live_borders.remove(&h);
    s.ws.remove(&h);
    s.mon.remove(&h);
    s.fs_ws.remove(&h);
    s.fs_mon.remove(&h);
    s.fs_hidden.remove(&h);
    s.manual_float.remove(&h);
    s.capacity_float.remove(&h);
    s.float_reasons.remove(&h);
    s.suspended.remove(&h);
    s.identities.remove(&h);
    s.min_size.remove(&h);
    s.placement_failures.remove(&h);
    s.geometry_retries.remove(&h);
    s.expected.remove(&h);
    s.placements.remove(&h);
    if s.follow.is_some_and(|(_, _, window)| window == h) {
        s.follow = None;
    }
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
        s.live_resize.clear();
        s.live_borders.clear();
        s.drop_dot = None;
        overlay::set_drop_target(None);
        overlay::raise_indicators(false);
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

fn native_identity(hwnd: HWND) -> Option<(u32, u32)> {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0 && thread != 0).then_some((pid, thread))
}

fn remember_identity(s: &mut State, hwnd: HWND) {
    if let Some(identity) = native_identity(hwnd) {
        s.identities.insert(hwnd.0 as isize, identity);
    }
}

fn temporarily_unavailable(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() || IsZoomed(hwnd).as_bool()
        {
            return true;
        }
        let mut cloaked: u32 = 0;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
    }
}

fn retain_assignments(
    s: &mut State,
    present: &HashSet<isize>,
    fullscreen: &HashSet<isize>,
    excluded: &HashSet<isize>,
) {
    let rules = config::rules();
    let retained: HashSet<_> =
        s.ws.keys()
            .copied()
            .filter(|h| {
                present.contains(h)
                    || (!excluded.contains(h)
                        && !fullscreen.contains(h)
                        && temporarily_unavailable(HWND(*h as *mut _))
                        && !window::policy_excludes(HWND(*h as *mut _), &rules))
            })
            .collect();
    s.ws.retain(|h, _| retained.contains(h));
    s.mon.retain(|h, _| retained.contains(h));
    s.suspended = retained.difference(present).copied().collect();
    if s.follow
        .is_some_and(|(_, _, h)| !s.ws.contains_key(&h) && !s.fs_ws.contains_key(&h))
    {
        s.follow = None;
    }
}

/// Remove cached identity before a missed destroy can transfer state to another process.
fn prune_identities(s: &mut State) {
    let stale: Vec<_> = s
        .identities
        .iter()
        .filter_map(|(h, identity)| {
            (native_identity(HWND(*h as *mut _)) != Some(*identity)).then_some(*h)
        })
        .collect();
    for h in stale {
        forget_window(s, h);
    }
}

fn float_for_capacity(s: &mut State, h: isize, reason: &str) {
    let mi = s
        .mon
        .get(&h)
        .copied()
        .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)).min(MON - 1));
    s.ws.remove(&h);
    s.mon.remove(&h);
    s.suspended.remove(&h);
    s.capacity_float.insert(h);
    s.float_reasons.insert(h, reason.to_owned());
    s.anims.retain(|animation| animation.hwnd != h);
    s.placements.remove(&h);
    s.expected.remove(&h);
    s.geometry_retries.remove(&h);
    if s.follow.is_some_and(|(_, _, window)| window == h) {
        s.follow = None;
    }
    s.live_resize.forget(&[h]);
    for order in &mut s.order {
        order.retain(|window| *window != h);
    }
    let hwnd = HWND(h as *mut _);
    if is_parked(hwnd) {
        let area = monitor_work(mi);
        let _ = apply_window_pos(
            hwnd,
            area.left + 20,
            area.top + 20,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            "capacity-float",
        );
    }
    remember_identity(s, hwnd);
    log(format!("floating: hwnd={h:#x} reason={reason}"));
}

/// Recheck old workspaces too: a newly confirmed minimum can invalidate an existing layout.
fn reconcile_capacity(s: &mut State) {
    for mi in 0..monitor::count().min(MON) {
        for ws in 0..WS_COUNT {
            loop {
                let windows = workspace_windows(s, mi, ws);
                if windows.is_empty() {
                    break;
                }
                let ratios = s.ratios[mi].get(&ws).map(Vec::as_slice).unwrap_or(&[]);
                let monocle = is_monocle(s, mi, ws);
                let (tiles, _) = fitted(monitor_work(mi), &windows, ratios, &s.min_size, monocle);
                if windows.len() <= tile_budget(mi)
                    && valid_layout(mi, &windows, &tiles, &s.min_size)
                {
                    break;
                }
                let h = *windows.last().unwrap();
                s.ws.remove(&h);
                let destination = config::auto_overflow()
                    .then(|| {
                        (1..WS_COUNT)
                            .map(|offset| (ws + offset) % WS_COUNT)
                            .find(|candidate| workspace_accepts(s, mi, *candidate, h))
                    })
                    .flatten();
                if let Some(destination) = destination {
                    s.ws.insert(h, destination);
                    if ws == s.cur[mi] {
                        s.follow = Some((mi, destination, h));
                    }
                    log(format!("overflow: hwnd={h:#x} monitor={} workspace={} -> {} reason=layout constraints", mi + 1, ws + 1, destination + 1));
                } else {
                    float_for_capacity(s, h, "no workspace satisfies window/tile minimums");
                }
            }
        }
    }
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
    let unrelated_divider_event = STATE.with(|state| {
        let s = state.borrow();
        s.divider
            .as_ref()
            .is_some_and(|gesture| !gesture.windows.contains(&h))
            && matches!(
                event,
                EVENT_OBJECT_CREATE
                    | EVENT_OBJECT_SHOW
                    | EVENT_OBJECT_UNCLOAKED
                    | EVENT_SYSTEM_MINIMIZEEND
            )
            && !matches!(
                window::classify_with_manual(hwnd, &config::rules(), &s.manual_float),
                Verdict::Tileable(_) | Verdict::Fullscreen(_)
            )
    });
    if unrelated_divider_event {
        return; // responsive client popups/tooltips do not change the tiled window set
    }
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
            | EVENT_SYSTEM_MENUPOPUPSTART => true,
            EVENT_SYSTEM_FOREGROUND => !gesture.windows.contains(&h),
            // A live client may clamp its size or report a delayed DWM frame.
            // Only explicit lifecycle/input changes cancel ownership of the gesture.
            EVENT_OBJECT_LOCATIONCHANGE => false,
            _ => false,
        }
    });
    if cancel_divider {
        if event == EVENT_SYSTEM_FOREGROUND || event == EVENT_SYSTEM_MENUPOPUPSTART {
            settle_divider();
        } else {
            end_divider(false);
        }
    }
    let mut resync = false;
    // Focus moved within the current workspace: the border target was just set to the event's
    // window, so it only has to be repositioned (no re-derivation from `GetForegroundWindow`,
    // which can still race with the click).
    let mut reposition = false;
    // Did this event ask for any actual work? Only then is the debounce timer armed: an event we
    // ignore (e.g. an unrelated window moving) must not cause a whole re-collect 40 ms later.
    let mut dirty = false;
    // A dragged window dropped onto a workspace dot: (window, monitor, workspace).
    let mut dot_drop: Option<(isize, usize, u8)> = None;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if event == EVENT_OBJECT_CREATE && s.identities.contains_key(&h) {
            // CREATE is a lifecycle boundary even when the old HWND was reused by
            // the same process/thread and its DESTROY notification was missed.
            forget_window(&mut s, h);
            s.windows_changed = true;
            dirty = true;
        }
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
                            s.drop_dot = None;
                            s.dragging = Some(h);
                            overlay::raise_indicators(true);
                            s.placement_failures.remove(&h);
                            s.geometry_retries.remove(&h);
                            s.drag_started = Some(Instant::now());
                            if s.drag_timer != 0 {
                                let _ = KillTimer(None, s.drag_timer);
                            }
                            s.drag_timer = SetTimer(None, 0, DRAG_WATCHDOG_MS, None);
                            let affected: HashSet<isize> = s.last[mi].iter().copied().collect();
                            s.anims.retain(|a| !affected.contains(&a.hwnd));
                            s.resizing = false;
                            s.live = false;
                            s.drag_preview = None;
                            // the overlay would otherwise chase the window (and its "whole monitor"
                            // guard makes it blink while the frame is in transition), so hide it
                            // for the duration of the drag and let the re-tile restore it
                            s.border_on = None;
                            overlay::hide_border();
                            divider::sync(Vec::new());
                            s.live_resize.clear();
                            s.live_borders.clear();
                            let windows = s.last[mi].clone();
                            capture_live_borders(&mut s, &windows);
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
                    let affected = s.last[old_mi].clone();
                    s.live_resize.forget(&affected);
                    overlay::set_drop_target(None);
                    overlay::raise_indicators(false);
                    let on_dot = s.drop_dot.take().filter(|_| !s.resizing);
                    if let Some((dot_mi, n)) = on_dot {
                        // dropped onto a workspace dot: the window goes to that workspace (see
                        // `send_to_workspace`); undo the swap preview and let it snap back first
                        if let Some((pi, ph)) = s.drag_preview.take() {
                            if pi < s.rects[old_mi].len() {
                                let back = s.rects[old_mi][pi];
                                move_window(&mut s, HWND(ph as *mut _), back, true);
                            }
                        }
                        s.resizing = false;
                        s.live = false;
                        dot_drop = Some((h, dot_mi, n));
                    } else if old_mi != new_mi {
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
                            let (tiles, regions) = layout_tiles(
                                config::layout(),
                                monitor_work(mi),
                                s.last[mi].len(),
                                &s.ratios[mi][&cur],
                                gaps(),
                            );
                            let targets: Vec<_> = s.last[mi]
                                .iter()
                                .copied()
                                .zip(tiles.iter().copied())
                                .collect();
                            place_batch(&mut s, &targets);
                            s.rects[mi] = tiles;
                            s.regions[mi] = regions;
                        }
                        s.resizing = false;
                        s.live = false;
                    } else {
                        let mi = old_mi;
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
                                tour_event("swap");
                            }
                        }
                    }
                    s.reflow = true;
                    for h in &affected {
                        s.live_borders.remove(h);
                    }
                    dirty = true;
                } else {
                    return;
                }
            }
            EVENT_OBJECT_LOCATIONCHANGE => {
                if s.paused {
                    return;
                }
                if s.suspended.contains(&h) && !temporarily_unavailable(hwnd) {
                    s.windows_changed = true;
                    dirty = true;
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
                if s.divider
                    .as_ref()
                    .is_some_and(|gesture| gesture.windows.contains(&h))
                {
                    // The gesture owns these geometry changes. Verification and
                    // adaptive fitting resume after release, not between frames.
                    return;
                }
                if !dirty
                    && s.dragging.is_none()
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
                            s.drop_dot = None;
                            overlay::set_drop_target(None);
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
                            // over a workspace dot: highlight it instead of previewing a swap
                            let mut p = POINT::default();
                            let dot = if GetCursorPos(&mut p).is_ok() {
                                overlay::dot_at_screen(p.x, p.y)
                            } else {
                                None
                            };
                            overlay::set_drop_target(dot);
                            s.drop_dot = dot;
                            if dot.is_some() {
                                let mi = s.mi(h);
                                if let Some((pi, ph)) = s.drag_preview.take() {
                                    if pi < s.rects[mi].len() {
                                        let back = s.rects[mi][pi];
                                        move_window(&mut s, HWND(ph as *mut _), back, true);
                                    }
                                }
                            } else {
                                update_drag_preview(&mut s, h);
                            }
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
                } else if s.suspended.contains(&h)
                    || s.fs_ws.contains_key(&h)
                    || h == GetForegroundWindow().0 as isize
                {
                    // a tracked fullscreen window changed size (a game left fullscreen), or the
                    // foreground window did: reconcile the window set
                    s.windows_changed = true;
                    dirty = true;
                }
            }
            EVENT_OBJECT_SHOW => {
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
            EVENT_OBJECT_NAMECHANGE => {
                if s.paused {
                    return;
                }
                let ignored = s.manual_float.union(&s.capacity_float).copied().collect();
                let verdict = window::classify_with_manual(hwnd, &config::rules(), &ignored);
                let changed = match verdict {
                    Verdict::Tileable(window) => {
                        let changed = !s.ws.contains_key(&h)
                            || s.suspended.contains(&h)
                            || s.fs_ws.contains_key(&h)
                            || window.rule_ws.is_some_and(|ws| s.ws.get(&h) != Some(&ws));
                        if let Some(ws) = window.rule_ws.filter(|_| s.ws.contains_key(&h)) {
                            s.ws.insert(h, ws);
                        }
                        changed
                    }
                    Verdict::Fullscreen(_) => !s.fs_ws.contains_key(&h),
                    _ => {
                        (s.ws.contains_key(&h) || s.fs_ws.contains_key(&h))
                            && !temporarily_unavailable(hwnd)
                    }
                };
                if !changed {
                    return;
                }
                s.windows_changed = true;
                dirty = true;
            }
            EVENT_SYSTEM_FOREGROUND => {
                update_border(&mut s, h);
                if s.suspended.contains(&h) && !temporarily_unavailable(hwnd) {
                    s.windows_changed = true;
                    dirty = true;
                }
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
                        if ws.is_none() && !s.paused {
                            let ignored =
                                s.manual_float.union(&s.capacity_float).copied().collect();
                            if matches!(
                                window::classify_with_manual(hwnd, &config::rules(), &ignored),
                                Verdict::Tileable(_) | Verdict::Fullscreen(_)
                            ) {
                                s.windows_changed = true;
                                dirty = true;
                            }
                        }
                        if !dirty {
                            return;
                        }
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
    if let Some((window, mi, n)) = dot_drop {
        send_to_workspace(window, mi, n);
    }
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
            rs[j] = (before + delta).clamp(MIN_RATIO.min(before), MAX_RATIO.max(before));
            if (rs[j] - before).abs() < 0.0005 {
                return; // already at the limit
            }
            rs.clone()
        };
        let monocle = is_monocle(&s, mi, cur);
        let (tiles, regions) = fitted(monitor_work(mi), &s.last[mi], &ratios, &s.min_size, monocle);
        if !valid_layout(mi, &s.last[mi], &tiles, &s.min_size) {
            s.reflow = true;
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
            return;
        }
        let last = s.last[mi].clone();
        let targets: Vec<_> = last.iter().copied().zip(tiles.iter().copied()).collect();
        // A held key commits all neighbours together without queuing animations.
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
            let others: Vec<_> = targets.into_iter().filter(|(hw, _)| *hw != h).collect();
            s.live_resize.queue(&others);
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
                if temporarily_unavailable(hw) {
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
                    continue;
                }
                if off_target(hw, r) {
                    if s.placements.get(h).is_some_and(|request| {
                        !request.queried && request.submitted.elapsed() < Duration::from_secs(2)
                    }) && visible_rect(hw).is_some_and(|actual| {
                        actual.right - actual.left > r.right - r.left + 2
                            || actual.bottom - actual.top > r.bottom - r.top + 2
                    }) {
                        s.enforce = true;
                        s.timer = unsafe { SetTimer(None, s.timer, VERIFY_MS, None) };
                        continue; // collect settled evidence before posting another resize
                    }
                    if !allow_geometry_retry(&mut s.geometry_retries, *h, *r) {
                        float_for_capacity(
                            &mut s,
                            *h,
                            "client repeatedly rejected placement without a confirmed minimum",
                        );
                        s.reflow = true;
                        s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
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
    if s.ws.contains_key(&h) || s.manual_float.contains(&h) || s.capacity_float.contains(&h) {
        return false; // known window shown again: the normal path decides
    }
    let mi = monitor::of_window(hwnd).min(MON - 1);
    if s.last[mi].len() >= tile_budget(mi) {
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
    let w = match window::classify_with_manual(hwnd, &rules, &s.manual_float) {
        Verdict::Tileable(w) => w,
        _ => return false, // not tileable (yet): e.g. title not set, dialog, tool window
    };
    let cur = s.cur[mi];
    if w.rule_ws.is_some_and(|n| n != cur) {
        return false; // belongs to another workspace: the normal path parks it
    }
    seed_min_size(s, hwnd);
    let mut layout = s.last[mi].clone();
    layout.push(h);
    let ratios = {
        let rs = s.ratios[mi].entry(cur).or_default();
        rs.resize(layout.len() - 1, 0.5);
        rs.clone()
    };
    let monocle = is_monocle(s, mi, cur);
    let (rects, regions) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size, monocle);
    if !valid_layout(mi, &layout, &rects, &s.min_size) {
        return false;
    }
    s.ws.insert(h, cur);
    s.mon.insert(h, mi);
    remember_identity(s, hwnd);
    if !s.order[mi].contains(&h) {
        s.order[mi].push(h);
    }
    for (i, hh) in layout.iter().enumerate() {
        // the new window appears in its tile instantly; the others glide to make room
        move_window(s, HWND(*hh as *mut _), rects[i], *hh != h);
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
    let manual = STATE.with(|s| {
        let mut s = s.borrow_mut();
        prune_identities(&mut s);
        s.manual_float.union(&s.capacity_float).copied().collect()
    });
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
                                   // Leaving fullscreen restores the previous assignment instead of admitting a new window.
        for window in &c.tileable {
            let h = window.hwnd.0 as isize;
            if let Some(ws) = s.fs_ws.get(&h).copied() {
                let mi = s
                    .fs_mon
                    .get(&h)
                    .copied()
                    .unwrap_or_else(|| monitor::of_window(window.hwnd).min(MON - 1));
                s.ws.insert(h, ws);
                s.mon.insert(h, mi);
            }
        }
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
        s.capacity_float
            .retain(|h| unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool());
        // forget windows that are gone; new windows join the current workspace, the workspace a
        // config rule asks for, or - if the current workspace is full - the next workspace
        let fullscreen: HashSet<_> = c
            .fullscreen
            .iter()
            .map(|window| window.hwnd.0 as isize)
            .collect();
        retain_assignments(&mut s, &present_set, &fullscreen, &manual);
        // Restore/migrate every known window's order before admitting any new one.
        // Otherwise a fullscreen exit later in the enumeration could change a
        // layout that an earlier new window already used for its capacity check.
        for w in &c.tileable {
            let h = w.hwnd.0 as isize;
            if !s.ws.contains_key(&h) {
                continue;
            }
            if !is_parked(w.hwnd) {
                s.mon.insert(h, monitor::of_window(w.hwnd).min(MON - 1));
            }
            let mi = s.mi(h);
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
        }
        let mut overflow: Option<(usize, u8, isize)> = None;
        for w in &c.tileable {
            let h = w.hwnd.0 as isize;
            remember_identity(&mut s, w.hwnd);
            if s.ws.contains_key(&h) {
                continue;
            }
            seed_min_size(&mut s, w.hwnd);
            let mi = monitor::of_window(w.hwnd).min(MON - 1);
            let cur = s.cur[mi];
            let ws = match w.rule_ws {
                Some(ws) if workspace_accepts(&s, mi, ws, h) => Some(ws),
                Some(_) => None,
                None => workspace_for_new_window(&s, mi, cur, h),
            };
            let Some(ws) = ws else {
                float_for_capacity(
                    &mut s,
                    h,
                    "new window cannot fit the requested workspace(s)",
                );
                continue;
            };
            s.ws.insert(h, ws);
            s.mon.insert(h, mi);
            // Commit admission order now: later windows in the same scan must be
            // validated against the exact Spiral order that will be placed.
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
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
                .filter(|h| s.ws.contains_key(h) && s.mon.get(h).copied() == Some(mi))
                .collect();
            let mut seen: HashSet<isize> = order.iter().copied().collect();
            for h in &present {
                if s.ws.contains_key(h) && s.mon.get(h).copied() == Some(mi) && seen.insert(*h) {
                    order.push(*h);
                }
            }
            s.order[mi] = order;
        }
        reconcile_capacity(&mut s);
        // make sure windows of other workspaces stay out of sight, and group the shown ones
        let mut layouts: Vec<Vec<isize>> = vec![Vec::new(); MON];
        for (mi, layout) in layouts.iter_mut().enumerate() {
            let order = s.order[mi].clone();
            for h in order {
                if !present_set.contains(&h) || s.suspended.contains(&h) {
                    continue;
                }
                if s.ws[&h] != s.cur[mi] {
                    park(&mut s, HWND(h as *mut _));
                } else {
                    layout.push(h);
                }
            }
        }
        sync_parked(&mut s);
        let mut changed = false;
        for (mi, layout) in layouts.iter().enumerate() {
            let layout = layout.clone();
            let cur = s.cur[mi];
            let ratios = {
                let rs = s.ratios[mi].entry(cur).or_default();
                rs.resize(layout.len().saturating_sub(1), 0.5);
                rs.clone()
            };
            let monocle = is_monocle(&s, mi, cur);
            let (rects, regions) = fitted(monitor_work(mi), &layout, &ratios, &s.min_size, monocle);
            if layout == s.last[mi] && rects == s.rects[mi] {
                // same windows and same tiles: leave them alone (respects manual tweaks)
                continue;
            }
            s.rects[mi] = rects.clone();
            s.regions[mi] = regions;
            for (h, r) in layout.iter().zip(rects) {
                if s.dragging == Some(*h) {
                    continue; // the user is dragging it right now: leave it under the cursor
                }
                let animate = animate_all && !fresh.contains(h);
                move_window(&mut s, HWND(*h as *mut _), r, animate);
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
        if s.follow.is_some() {
            s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
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

/// Returns whether a window was focused or swapped.
fn focus_or_swap(dir: Dir, swap: bool) -> bool {
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
        let to = if is_monocle(&s, mi, s.cur[mi]) {
            // all tiles coincide in monocle: left/up and right/down cycle through the windows
            if swap {
                return;
            }
            let n = s.last[mi].len() as i32;
            let step = if matches!(dir, Dir::Left | Dir::Up) {
                -1
            } else {
                1
            };
            (cur as i32 + step).rem_euclid(n) as usize
        } else {
            let Some(to) = neighbour(&s.rects[mi], cur, dir) else {
                return;
            };
            to
        };
        if swap {
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
    let acted = to_focus.is_some_and(|h| h != fg) || do_retile;
    if let Some(h) = to_focus {
        focus_hwnd(h);
    }
    if do_retile {
        retile();
    }
    acted
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
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if mi >= MON || s.paused || s.dragging.is_some() || n == s.cur[mi] || n >= WS_COUNT {
            return false;
        }
        s.prev_ws[mi] = Some(s.cur[mi]);
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
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let affected = s
            .mon
            .iter()
            .filter_map(|(h, monitor)| (*monitor == mi).then_some(*h))
            .collect();
        s.clear_placement_history_for(&affected);
    });
    retile();
    match focus {
        Some(h) => focus_hwnd(h),
        None => focus_workspace(mi, n),
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

/// A window dropped onto workspace dot `n` of monitor `mi` (or sent there from the dot's menu)
/// moves to that workspace. The user stays where they are, like dropping a file onto a folder.
fn send_to_workspace(h: isize, mi: usize, n: u8) {
    let hwnd = HWND(h as *mut _);
    let plan = STATE.with(|s| {
        let s = s.borrow();
        if s.paused || n >= WS_COUNT || mi >= MON || !s.ws.contains_key(&h) || s.fs_ws.contains_key(&h)
        {
            return None;
        }
        Some(s.mi(h))
    });
    let Some(from) = plan else {
        retile(); // whatever happened, the dropped window goes back to its tile
        return;
    };
    // where the window really is: a drag can leave it mostly on another monitor than its own
    let actual = monitor::of_window(hwnd).min(MON - 1);
    if actual != mi {
        // `retile` derives a window's monitor from where it is: put it on the target first
        let work = monitor::work(mi);
        let moved = unsafe {
            SetWindowPos(
                hwnd,
                None,
                work.left + 40,
                work.top + 40,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        };
        if moved.is_err() {
            retile();
            return;
        }
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if from != mi {
            s.order[from].retain(|window| *window != h);
            if !s.order[mi].contains(&h) {
                s.order[mi].push(h);
            }
            s.mon.insert(h, mi);
        }
        s.ws.insert(h, n);
        s.last[from].clear();
        s.last[actual].clear();
        s.last[mi].clear();
    });
    retile();
    tour_event("send");
    log(format!(
        "sent a window to workspace {} on monitor {}",
        n + 1,
        mi + 1
    ));
}

/// Right-click on a workspace dot: switch to it, or move the active window there.
fn dot_menu(mi: usize, n: u8) {
    // read before the menu takes the foreground
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let (tracked, from_mi) = STATE.with(|s| {
        let s = s.borrow();
        (s.ws.contains_key(&fg) && !s.fs_ws.contains_key(&fg), s.mi(fg))
    });
    let open = format!("Open workspace {}", n + 1);
    let items: Vec<&str> = if tracked {
        vec![&open, "Move the active window here"]
    } else {
        vec![&open]
    };
    match tray::pick(&items) {
        Some(0) => switch_ws_on_monitor(mi, n),
        Some(1) => {
            send_to_workspace(fg, mi, n);
            refocus_after_menu(fg, from_mi);
        }
        _ => refocus_after_menu(fg, from_mi),
    }
}

/// The menu took the foreground: give it back to `h`, or, when `h` just left the screen, to the
/// first window of its former monitor's (`from_mi`) visible workspace.
fn refocus_after_menu(h: isize, from_mi: usize) {
    let target = STATE.with(|s| {
        let s = s.borrow();
        if !unsafe { IsWindow(Some(HWND(h as *mut _))) }.as_bool() {
            return None;
        }
        let mi = s.mi(h);
        if s.ws.get(&h).is_none_or(|ws| *ws == s.cur[mi]) {
            Some(h)
        } else {
            s.last[from_mi.min(MON - 1)].first().copied()
        }
    });
    if let Some(target) = target {
        focus_hwnd(target);
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
    tour_event("send");
    log(format!("moved a window to workspace {}", n + 1));
}

/// Forget windows that are gone or no longer parked, and persist the list when it changed.
fn sync_parked(s: &mut State) {
    let before = s.parked.len();
    // Parking is asynchronous, so the real position is not checked here: an entry lives until its
    // window is gone or belongs to a visible workspace again.
    let drop: Vec<isize> = s
        .parked
        .keys()
        .copied()
        .filter(|h| {
            !unsafe { IsWindow(Some(HWND(*h as *mut _))) }.as_bool()
                || s.ws.get(h).is_none_or(|ws| *ws == s.cur[s.mi(*h)])
        })
        .collect();
    for h in drop {
        s.parked.remove(&h);
    }
    if s.parked_dirty || s.parked.len() != before {
        parked::save(&s.parked);
        s.parked_dirty = false;
    }
}

/// Windows left parked off-screen (e.g. after a crash) are brought back into view: first from the
/// persisted list (to their old positions), then any remaining ones found at a parking spot.
fn recover_parked() {
    let restored = parked::restore_persisted();
    if restored > 0 {
        log(format!(
            "recovered {restored} parked window(s) from the saved list"
        ));
    }
    let manual = STATE.with(|s| s.borrow().manual_float.clone());
    let c = collect(false, &manual);
    for w in &c.tileable {
        if parked::restore_unlisted(w.hwnd.0 as isize) {
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
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.parked.clear();
        parked::save(&s.parked);
    });
}

/// Pause / resume asked for by the settings window, which shows the state itself.
pub fn toggle_pause_quietly() {
    toggle_pause(false);
}

/// `announce`: show a notification when pausing (hotkey and tray menu).
fn toggle_pause(announce: bool) {
    end_divider(false);
    let paused = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.paused = !s.paused;
        if s.paused {
            s.anims.clear();
            s.live_resize.clear();
            s.live_borders.clear();
            cancel_anim_timer(&mut s);
        }
        s.paused
    });
    log(if paused { "paused" } else { "resumed" });
    register_hotkeys();
    tray::update();
    if paused && announce {
        tray::notify_paused();
    }
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
    reload(true);
}

/// Reload triggered by the settings window's autosave: refused hotkeys are shown there, so no
/// message box pops up after every edit.
pub fn reload_config_quiet() {
    reload(false);
}

fn reload(announce_failures: bool) {
    end_divider(false);
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
            overlay::set_indicator_accent(config::accent_color_value());
            overlay::set_border_color(config::border_color_value());
            let failed = register_hotkeys();
            STATE.with(|s| s.borrow_mut().invalidate_layout_constraints());
            retile();
            tray::update();
            clear_border(); // the colour (or the on/off switch) may have changed
            refresh_border();
            sync_indicator();
            apply_focus_follows_timer();
            let minimum = config::min_tile();
            log(format!(
                "config reloaded: minimum={}x{}; learned size constraints cleared",
                minimum.0, minimum.1
            ));
            if announce_failures && !failed.is_empty() {
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
        let was_manual = s.manual_float.remove(&h);
        let was_capacity = s.capacity_float.remove(&h);
        s.float_reasons.remove(&h);
        if was_manual || was_capacity {
            false
        } else {
            s.manual_float.insert(h);
            remember_identity(&mut s, fg);
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

/// The monitor slot `step` away from `from`, wrapping; `None` with a single monitor.
fn monitor_step(from: usize, step: i8) -> Option<usize> {
    let n = monitor::count().min(MON);
    (n >= 2).then(|| (from as i64 + i64::from(step)).rem_euclid(n as i64) as usize)
}

/// Hotkey: move keyboard focus to the next / previous monitor.
fn focus_monitor(step: i8) {
    let target = STATE.with(|s| {
        let s = s.borrow();
        let from = active_mon(&s);
        monitor_step(from, step).map(|to| (to, s.last[to].first().copied()))
    });
    let Some((to, first)) = target else {
        return;
    };
    match first {
        Some(h) => focus_hwnd(h),
        None => {
            // nothing tiled there: park the pointer on that monitor so it becomes the active one
            let work = monitor::work(to);
            unsafe {
                let _ = SetCursorPos(
                    work.left + (work.right - work.left) / 2,
                    work.top + (work.bottom - work.top) / 2,
                );
            }
            focus_hwnd(unsafe { GetShellWindow() }.0 as isize);
        }
    }
    log(format!("focused monitor {}", to + 1));
}

/// Hotkey: send the focused tiled window to the next / previous monitor, into the workspace that
/// monitor is showing, and keep it focused.
fn move_to_monitor(step: i8) {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let plan = STATE.with(|s| {
        let s = s.borrow();
        if s.paused || s.dragging.is_some() || !s.ws.contains_key(&fg) || s.fs_ws.contains_key(&fg)
        {
            return None;
        }
        monitor_step(s.mi(fg), step).map(|to| (to, s.cur[to]))
    });
    let Some((to, workspace)) = plan else {
        return;
    };
    let work = monitor::work(to);
    // `retile` re-derives a window's monitor from where it is, so move it there first
    let moved = unsafe {
        SetWindowPos(
            HWND(fg as *mut _),
            None,
            work.left + 40,
            work.top + 40,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    };
    if moved.is_err() {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let from = s.mi(fg);
        s.ws.insert(fg, workspace);
        s.mon.insert(fg, to);
        s.last[to].clear();
        s.last[from].clear();
    });
    retile();
    focus_hwnd(fg);
    log(format!("moved a window to monitor {}", to + 1));
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

/// Hotkey: show the focused window alone on the whole screen (the other windows of the workspace
/// stay stacked underneath and `focus_*` cycles through them); press again to restore the layout.
fn toggle_monocle() {
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    let on = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mi = s.mi(fg);
        let ws = s.cur[mi];
        if s.paused || !s.last[mi].contains(&fg) {
            return None;
        }
        let on = s.monocle[mi].insert(ws);
        if !on {
            s.monocle[mi].remove(&ws);
        }
        s.last[mi].clear(); // force a re-layout
        Some(on)
    });
    let Some(on) = on else { return };
    retile();
    focus_hwnd(fg);
    notify("tile-rs", if on { "Monocle on" } else { "Monocle off" });
}

/// Hotkey: focus the previously focused window, switching to its workspace if it is on another.
fn focus_last() {
    let target = STATE.with(|s| {
        let s = s.borrow();
        let h = s.focus_prev?;
        let ws = s.ws.get(&h).copied().or_else(|| s.fs_ws.get(&h).copied())?;
        let mi = s
            .mon
            .get(&h)
            .copied()
            .unwrap_or_else(|| monitor::of_window(HWND(h as *mut _)))
            .min(MON - 1);
        Some((h, ws, mi, ws != s.cur[mi]))
    });
    let Some((h, ws, mi, other)) = target else {
        return;
    };
    if !unsafe { IsWindow(Some(HWND(h as *mut _))) }.as_bool() {
        return;
    }
    if other {
        switch_ws(mi, ws, Some(h));
    } else {
        focus_hwnd(h);
    }
}

/// Hotkey: go back to the workspace the active monitor showed before the current one.
fn workspace_back() {
    let (mi, prev) = STATE.with(|s| {
        let s = s.borrow();
        let mi = active_mon(&s);
        (mi, s.prev_ws[mi])
    });
    if let Some(n) = prev {
        switch_ws(mi, n, None);
    }
}

/// Hotkey: add (`step` > 0) or remove a window in the master column of master-stack, and remember
/// the count in `config.toml`.
fn change_master_count(step: i8) {
    if config::layout() != Layout::MasterStack {
        notify("tile-rs", "Master count applies to the master-stack layout");
        return;
    }
    let now = config::master_count() as i32;
    let next = (now + step as i32).clamp(1, config::MAX_MASTER_COUNT as i32);
    if next == now {
        return;
    }
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = config::edit_top(&text, &[("master_count", next.to_string())]);
    if let Err(e) = config::save_atomic(&path, &new) {
        log(format!("could not save master_count to config.toml: {e}"));
        return;
    }
    reload_config();
    notify("tile-rs", &format!("Master windows: {next}"));
}

/// Marker file: the settings window's first-run tour is open and watching.
pub fn tour_marker() -> std::path::PathBuf {
    config::config_path().with_file_name("tour-active")
}

/// The tour's progress file: one line per step the user has done ("focus", "swap", "send").
pub fn tour_progress_path() -> std::path::PathBuf {
    config::config_path().with_file_name("tour-progress.txt")
}

/// Notes a tour step for the settings window, but only while its tour is open. A marker older
/// than an hour was left behind by a launcher that did not exit cleanly and is ignored.
fn tour_event(step: &str) {
    let fresh = std::fs::metadata(tour_marker())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(3600));
    if !fresh {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tour_progress_path())
    {
        let _ = writeln!(file, "{step}");
    }
}

pub fn dispatch(a: Action) {
    end_divider(false);
    match a {
        Action::Focus(d) => {
            if focus_or_swap(d, false) {
                tour_event("focus");
            }
        }
        Action::Swap(d) => {
            if focus_or_swap(d, true) {
                tour_event("swap");
            }
        }
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
        Action::Pause => toggle_pause(true),
        Action::Quit => unsafe { PostQuitMessage(0) },
        Action::Retile => {
            // also resets the sizes of every workspace to equal halves
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.invalidate_layout_constraints();
                for mi in 0..MON {
                    s.ratios[mi].clear();
                }
                s.clear_layouts();
            });
            retile();
        }
        Action::CycleLayout => cycle_layout(),
        Action::ToggleMonocle => toggle_monocle(),
        Action::FocusLast => focus_last(),
        Action::WorkspaceBack => workspace_back(),
        Action::MasterCount(step) => change_master_count(step),
        Action::FocusMonitor(step) => focus_monitor(step),
        Action::MoveToMonitor(step) => move_to_monitor(step),
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
    let trim = STATE.with(|s| s.borrow().trim_timer);
    if trim != 0 && wparam == trim {
        unsafe {
            let _ = KillTimer(None, trim);
        }
        STATE.with(|s| s.borrow_mut().trim_timer = 0);
        trim_working_set();
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
    // A panic must not strand windows of other workspaces off-screen: bring them back from the
    // persisted list (it needs no application state), then run the normal hook.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let restored = parked::restore_persisted();
        log(format!("panic: restored {restored} parked window(s)"));
        previous_hook(info);
    }));
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
        hook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE);
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
    overlay::set_indicator_accent(config::accent_color_value());
    overlay::set_indicator_click(switch_ws_on_monitor);
    overlay::set_indicator_menu(dot_menu);
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
    STATE.with(|s| schedule_trim(&mut s.borrow_mut()));
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

fn diagnostic_rect(rect: RECT) -> serde_json::Value {
    serde_json::json!([rect.left, rect.top, rect.right, rect.bottom])
}

fn diagnostic_report(include_titles: bool, manager_state_available: bool) -> serde_json::Value {
    let rules = config::rules();
    let (ignored, tracked) = STATE.with(|state| {
        let state = state.borrow();
        let ignored = state
            .manual_float
            .union(&state.capacity_float)
            .copied()
            .collect();
        let tracked: HashSet<_> = state
            .identities
            .keys()
            .chain(state.ws.keys())
            .chain(state.fs_ws.keys())
            .copied()
            .collect();
        (ignored, tracked)
    });
    let mut windows = window::diagnostic_snapshots(&rules, &ignored, include_titles);
    let visible: HashSet<_> = windows
        .iter()
        .filter_map(|window| window["hwnd"].as_i64().map(|h| h as isize))
        .collect();
    for h in tracked.difference(&visible) {
        windows.push(window::diagnostic_snapshot(
            HWND(*h as *mut _),
            &rules,
            &ignored,
            include_titles,
        ));
    }
    STATE.with(|state| {
        let state = state.borrow();
        for window in &mut windows {
            let Some(h) = window["hwnd"].as_i64().map(|h| h as isize) else {
                continue;
            };
            let mi = state
                .mon
                .get(&h)
                .copied()
                .or_else(|| state.fs_mon.get(&h).copied());
            let ws = state
                .ws
                .get(&h)
                .copied()
                .or_else(|| state.fs_ws.get(&h).copied());
            let target = mi.and_then(|mi| {
                state.last[mi]
                    .iter()
                    .position(|item| *item == h)
                    .and_then(|index| state.rects[mi].get(index))
                    .copied()
            });
            window["manager"] = serde_json::json!({
                "monitor": mi.map(|mi| mi + 1),
                "workspace": ws.map(|ws| ws + 1),
                "target": target.map(diagnostic_rect),
                "suspended": state.suspended.contains(&h),
                "manual_float": state.manual_float.contains(&h),
                "automatic_float_reason": state.float_reasons.get(&h),
                "learned_minimum": state.min_size.get(&h),
                "geometry_retries": state.geometry_retries.get(&h).map(|(_, attempts)| attempts),
                "placement": state.placements.get(&h).map(|request| serde_json::json!({
                    "generation": request.generation,
                    "before": request.before.map(diagnostic_rect),
                    "target": diagnostic_rect(request.target),
                    "age_ms": request.submitted.elapsed().as_millis() as u64,
                    "minimum_queried": request.queried,
                }))
            });
        }
    });
    let monitors: Vec<_> = monitor::list().iter().enumerate().map(|(mi, monitor)| serde_json::json!({
        "index": mi + 1, "device": monitor.device, "work_area": diagnostic_rect(monitor.work),
        "screen": diagnostic_rect(monitor.full),
    })).collect();
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "executable": std::env::current_exe().ok(),
        "captured_at_unix_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "manager_state_available": manager_state_available,
        "titles_included": include_titles,
        "config_path": config::config_path(),
        "layout": format!("{:?}", config::layout()),
        "minimum_tile": config::min_tile(),
        "max_tiles": config::max_tiles(),
        "auto_overflow": config::auto_overflow(),
        "monitors": monitors,
        "windows": windows,
    })
}

pub fn export_diagnostics(include_titles: bool) {
    let path = log_file().with_file_name("window-diagnostics.json");
    match write_diagnostics(&path, include_titles, true) {
        Ok(()) => notify(
            "tile-rs: window diagnostics",
            &format!("Saved to {}", path.display()),
        ),
        Err(error) => notify("tile-rs: diagnostics failed", &error.to_string()),
    }
}

fn write_diagnostics(
    path: &std::path::Path,
    include_titles: bool,
    manager: bool,
) -> std::io::Result<()> {
    let report = serde_json::to_vec_pretty(&diagnostic_report(include_titles, manager))?;
    std::fs::write(path, report)
}

fn console_out() -> Box<dyn Write> {
    if unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|handle| !handle.is_invalid()) {
        return Box::new(std::io::stdout());
    }
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
        Some("diagnose") => {
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            window::SKIP_ZOOMED.store(true, Ordering::Relaxed);
            let arguments: Vec<_> = std::env::args().skip(2).collect();
            let include_titles = arguments.iter().any(|argument| argument == "--titles");
            let mut output = None;
            let mut index = 0;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--titles" => {}
                    "--output" if index + 1 < arguments.len() => {
                        index += 1;
                        output = Some(std::path::PathBuf::from(&arguments[index]));
                    }
                    _ => {
                        eprintln!("usage: tile-rs diagnose [--titles] [--output PATH]");
                        return;
                    }
                }
                index += 1;
            }
            if output.is_none() && tray::request_manager_diagnostics(include_titles) {
                return;
            }
            if let Ok(config) = config::load() {
                config::set(config);
            }
            if let Some(path) = output {
                if let Err(error) = write_diagnostics(&path, include_titles, false) {
                    eprintln!("could not write diagnostics: {error}");
                    std::process::exit(1);
                }
            } else {
                let report = diagnostic_report(include_titles, false);
                let mut out = console_out();
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            }
        }
        Some("--restore-windows") => {
            // Rescue command for after a hard kill (Task Manager, power loss): bring back windows
            // that were left parked off-screen, without starting the window manager.
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            recover_parked();
            let mut out = console_out();
            let _ = writeln!(
                out,
                "Windows parked off-screen by tile-rs were brought back."
            );
        }
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
            let _ = writeln!(out, "  tile-rs.exe diagnose [--titles] [--output PATH]  export read-only window diagnostics");
            let _ = writeln!(
                out,
                "  tile-rs.exe tile         arrange the current windows once and exit"
            );
            let _ = writeln!(out, "  tile-rs.exe --restore-windows  bring back windows left off-screen after a crash or kill");
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
        assert_eq!(
            confirmed_minimum((0, 0), (2200, 600), (2200, 650), (200, 650)),
            (0, 650)
        );
        assert_eq!(
            confirmed_minimum((0, 0), (400, 1100), (480, 1100), (480, 200)),
            (480, 0)
        );
        assert_eq!(
            confirmed_minimum((480, 650), (2200, 800), (2201, 802), (200, 200)),
            (480, 650)
        );
        assert_eq!(
            confirmed_minimum((480, 650), (400, 600), (500, 700), (500, 700)),
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
    fn reloading_settings_allows_smaller_tiles_after_a_large_minimum_was_learned() {
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
        let windows = [11, 12];
        let ratios = vec![0.2];
        let mut state = State::default();
        state.min_size.insert(11, (1200, 500));
        state.min_size_reflow = true;
        state.expected.insert(11, (area, Instant::now()));
        state.ratios[0].insert(0, ratios.clone());
        state.last[0] = windows.to_vec();
        let before = place_fitted(
            Layout::Spiral,
            area,
            &windows,
            &ratios,
            &state.min_size,
            (350, 200),
            gaps,
        )
        .0;
        assert!(before[0].right - before[0].left >= 1200);
        state.invalidate_layout_constraints();
        let after = place_fitted(
            Layout::Spiral,
            area,
            &windows,
            &ratios,
            &state.min_size,
            (350, 200),
            gaps,
        )
        .0;
        assert!(after[0].right - after[0].left < 1200);
        assert!(after[0].right - after[0].left >= 350);
        assert!(state.min_size.is_empty() && state.expected.is_empty());
        assert!(!state.min_size_reflow);
        assert!(state.last[0].is_empty());
        assert_eq!(state.ratios[0][&0], ratios);
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
        // placements are kept inside the monitor's work area: make the target independent of it
        let target = monitor::clamp_to_work(RECT {
            left: 30,
            top: 40,
            right: 330,
            bottom: 240,
        });
        let mut state = State::default();
        let started = Instant::now();
        place_batch(&mut state, &[(h, target)]);
        let batch_elapsed = started.elapsed();
        let actual = owner.join().unwrap();
        release.join().unwrap();
        eprintln!(
            "native placement submission: single={}us batch={}us (client stalled 900ms)",
            individual_elapsed.as_micros(),
            batch_elapsed.as_micros()
        );
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
    fn cancelling_live_divider_restores_real_windows_and_discards_pending_motion() {
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_POPUP, WS_VISIBLE,
        };
        let area = RECT {
            left: -20000,
            top: -20000,
            right: -18800,
            bottom: -19000,
        };
        let gap = Gaps {
            inner: 10,
            outer: 10,
        };
        let (tiles, regions) = layout_tiles(Layout::Spiral, area, 2, &[0.5], gap);
        let windows: Vec<_> = tiles
            .iter()
            .map(|r| unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("STATIC"),
                    w!("tile-rs live resize regression"),
                    WS_POPUP | WS_VISIBLE,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap()
                .0 as isize
            })
            .collect();
        let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gap.inner)[0];
        let origin = POINT {
            x: handle.rect.left,
            y: handle.rect.top,
        };
        let gesture = divider::Resize {
            handle,
            layout: Layout::Spiral,
            area,
            gaps: gap,
            windows: windows.clone(),
            workspace: 0,
            ratios: vec![0.5],
            tiles: tiles.clone(),
            preview: tiles.clone(),
            regions: regions.clone(),
            minimums: vec![(100, 100); 2],
            origin,
            last_pointer_axis: origin.x,
        };
        let (_, changed, _) = gesture.target(POINT {
            x: origin.x + 150,
            ..origin
        });
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            *s = State::default();
            s.last[0] = windows.clone();
            s.rects[0] = tiles.clone();
            s.regions[0] = regions;
            for h in &windows {
                s.ws.insert(*h, 0);
                s.mon.insert(*h, 0);
            }
            let targets: Vec<_> = windows.iter().copied().zip(changed).collect();
            place_batch(&mut s, &targets);
            s.live_resize.queue(&targets);
            s.divider = Some(gesture);
        });
        end_divider(false);
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            assert!(s.divider.is_none());
            assert!(s
                .live_resize
                .take_ready(Instant::now(), |_| Some(area))
                .is_empty());
            for (h, original) in windows.iter().zip(tiles) {
                let mut actual = RECT::default();
                unsafe {
                    GetWindowRect(HWND(*h as *mut _), &mut actual).unwrap();
                }
                assert_eq!(actual, original);
            }
            if s.timer != 0 {
                unsafe {
                    let _ = KillTimer(None, s.timer);
                }
            }
            cancel_anim_timer(&mut s);
            *s = State::default();
        });
        for h in windows {
            unsafe {
                DestroyWindow(HWND(h as *mut _)).unwrap();
            }
        }
    }

    #[test]
    fn delayed_or_clamped_geometry_does_not_cancel_a_live_divider() {
        let area = RECT {
            left: 0,
            top: 0,
            right: 1600,
            bottom: 1000,
        };
        let gaps = Gaps {
            inner: 10,
            outer: 10,
        };
        let (tiles, regions) = layout_tiles(Layout::Spiral, area, 2, &[0.5], gaps);
        let handle = divider::handles(0, Layout::Spiral, &tiles, &regions, gaps.inner)[0];
        let origin = POINT {
            x: handle.rect.left,
            y: handle.rect.top,
        };
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            *s = State::default();
            s.ws.extend([(11, 0), (12, 0)]);
            s.last[0] = vec![11, 12];
            s.rects[0] = tiles.clone();
            s.divider = Some(divider::Resize {
                handle,
                layout: Layout::Spiral,
                area,
                gaps,
                windows: vec![11, 12],
                workspace: 0,
                ratios: vec![0.5],
                tiles: tiles.clone(),
                preview: tiles,
                regions,
                minimums: vec![(100, 100); 2],
                origin,
                last_pointer_axis: origin.x,
            });
        });
        // Even a temporarily unavailable DWM rectangle is not an explicit user
        // cancellation. The old code rolled the whole layout back in this case.
        unsafe {
            on_event(
                HWINEVENTHOOK(std::ptr::null_mut()),
                EVENT_OBJECT_LOCATIONCHANGE,
                HWND(11usize as *mut _),
                0,
                0,
                0,
                0,
            );
        }
        STATE.with(|state| {
            let mut s = state.borrow_mut();
            assert!(s.divider.is_some());
            assert!(!s.reflow);
            *s = State::default();
        });
    }

    #[test]
    fn moving_or_repeating_native_geometry_does_not_send_redundant_size_events() {
        use windows::Win32::Foundation::{LPARAM, LRESULT};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, SetWindowLongPtrW, GWLP_WNDPROC,
            WM_SIZE, WS_POPUP,
        };
        thread_local! { static SIZES: Cell<u32> = const { Cell::new(0) }; }
        unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if msg == WM_SIZE {
                SIZES.with(|sizes| sizes.set(sizes.get() + 1));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        unsafe {
            let hwnd = CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs size notification regression"),
                WS_POPUP,
                -20000,
                -20000,
                400,
                300,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let original = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as *const () as isize);
            SIZES.with(|sizes| sizes.set(0));
            let moved = apply_window_pos(
                hwnd,
                -19990,
                -19990,
                400,
                300,
                SWP_NOZORDER | SWP_NOACTIVATE,
                "test-move",
            );
            let repeated = apply_window_pos(
                hwnd,
                -19990,
                -19990,
                400,
                300,
                SWP_NOZORDER | SWP_NOACTIVATE,
                "test-repeat",
            );
            let move_sizes = SIZES.with(|sizes| sizes.get());
            let resized = apply_window_pos(
                hwnd,
                -19990,
                -19990,
                500,
                300,
                SWP_NOZORDER | SWP_NOACTIVATE,
                "test-size",
            );
            let final_sizes = SIZES.with(|sizes| sizes.get());
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, original);
            DestroyWindow(hwnd).unwrap();
            assert!(moved && repeated && resized);
            assert_eq!(move_sizes, 0);
            assert_eq!(final_sizes, 1);
        }
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
        let observed =
            confirmed_minimum((0, 0), (right_width, 600), (right_width, 650), (200, 650));
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
    fn automatic_tile_budget_grows_with_the_screen_and_is_capped() {
        // 1920x1080 work area, 480x320 comfortable tile: 1 + log2(13.5) = 4
        assert_eq!(auto_tile_budget(1920, 1040, 480, 320), 4);
        // a small screen still holds two, a huge one stops at eight
        assert_eq!(auto_tile_budget(800, 600, 480, 320), 2);
        assert_eq!(auto_tile_budget(7680, 4320, 480, 320), 8);
        // a degenerate area never panics
        assert_eq!(auto_tile_budget(0, 0, 0, 0), 2);
    }

    #[test]
    fn slow_frames_suspend_animations_and_finish_running_ones() {
        let interval = Duration::from_millis(16);
        let mut s = State::default();
        s.anims.push(Anim {
            hwnd: 11,
            from: RECT::default(),
            to: RECT::default(),
            border: (0, 0, 0, 0),
            t0: Instant::now(),
            dur_ms: 500,
            last: None,
        });
        // fast frames reset the streak
        note_frame_cost(&mut s, Duration::from_millis(40), interval);
        note_frame_cost(&mut s, Duration::from_millis(40), interval);
        note_frame_cost(&mut s, Duration::from_millis(5), interval);
        assert_eq!(s.slow_frames, 0);
        assert!(!animations_suspended(&s));
        for _ in 0..SLOW_FRAMES_LIMIT {
            note_frame_cost(&mut s, Duration::from_millis(40), interval);
        }
        assert!(animations_suspended(&s));
        assert_eq!(s.anims[0].dur_ms, 1);
    }

    #[test]
    fn vsync_due_lands_before_the_next_refresh() {
        let freq = 10_000_000; // 10 MHz QPC: 1 tick = 100 ns
        let period = 166_667; // 60 Hz
        let lead = 20_000; // 2 ms
        let to_ticks = |due: i64| -due; // 100 ns units == ticks here
                                        // refresh was 5 ms ago: next one is 11.67 ms away, we fire 2 ms earlier
        let due = vsync_due_100ns(1_000_000, 950_000, period, freq, lead).unwrap();
        assert_eq!(to_ticks(due), 950_000 + period - 20_000 - 1_000_000);
        // a refresh timestamp in the future works the same way
        let future = vsync_due_100ns(
            1_000_000,
            1_000_000 + 3 * period + 50_000,
            period,
            freq,
            lead,
        )
        .unwrap();
        assert!(to_ticks(future) >= freq / 1000, "never sooner than 1 ms");
        assert!(to_ticks(future) <= period);
        // the frame is never scheduled in the past, even right before a refresh
        let tight = vsync_due_100ns(1_000_000, 1_000_000 + 5_000, period, freq, lead).unwrap();
        assert!(tight <= -(freq / 1000));
        assert_eq!(vsync_due_100ns(0, 0, 0, freq, lead), None);
    }

    #[test]
    fn frame_batches_place_every_window_and_report_each_result() {
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WS_POPUP, WS_VISIBLE,
        };
        let make = |x: i32| unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("tile-rs frame batch"),
                WS_POPUP | WS_VISIBLE,
                x,
                -20000,
                300,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let (a, b) = (make(-20000), make(-19000));
        let ops = [
            (
                a.0 as isize,
                RECT {
                    left: -19900,
                    top: -19900,
                    right: -19500,
                    bottom: -19600,
                },
                (0, 0, 0, 0),
            ),
            (
                b.0 as isize,
                RECT {
                    left: -19000,
                    top: -19900,
                    right: -18800,
                    bottom: -19800,
                },
                (0, 0, 0, 0),
            ),
            (0, RECT::default(), (0, 0, 0, 0)), // not a window: only its own result is false
        ];
        let results = apply_frames(&ops);
        assert_eq!(results.len(), 3);
        // posted requests are applied once the owner thread pumps messages (this one does)
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut ok = false;
        while Instant::now() < deadline && !ok {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            let mut ra = RECT::default();
            let mut rb = RECT::default();
            unsafe {
                GetWindowRect(a, &mut ra).unwrap();
                GetWindowRect(b, &mut rb).unwrap();
            }
            ok = ra.left == -19900
                && ra.right == -19500
                && rb.right == -18800
                && rb.bottom == -19800;
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            let _ = DestroyWindow(a);
            let _ = DestroyWindow(b);
        }
        assert!(ok, "batched frame was not applied to both windows");
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
        let _config_lock = config::test_lock();
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
        let _config_lock = config::test_lock();
        config::set(config::parse("animations = true").unwrap());
        let mut state = State::default();
        let target = monitor::clamp_to_work(RECT {
            left: 0,
            top: 0,
            right: 500,
            bottom: 400,
        });
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
        state.live_borders.insert(12, (7, 0, 7, 7));
        state.live_resize.queue(&[(
            12,
            RECT {
                right: 100,
                bottom: 100,
                ..Default::default()
            },
        )]);
        assert!(forget_window(&mut state, 11));
        assert!(!state.ws.contains_key(&11));
        assert!(!state.expected.contains_key(&11));
        assert!(state.dragging.is_none());
        assert!(!state.resizing);
        assert!(state.live_borders.is_empty());
        assert!(state
            .live_resize
            .take_ready(Instant::now(), |_| Some(RECT::default()))
            .is_empty());
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
