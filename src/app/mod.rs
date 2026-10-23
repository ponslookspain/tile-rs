//! Window-manager state and orchestration. Submodules share this state on the message-loop thread.

mod actions;
mod animation;
mod border;
mod cli;
mod event_queue;
mod events;
mod interaction;
mod monitors;
mod placement;
mod report;
mod startup;
#[cfg(test)]
mod tests;
mod tiling;
#[cfg(test)]
mod window_lifecycle_tests;
mod workspaces;

use crate::config;
use crate::diagnostics::{
    anim_prof_finish, anim_prof_frame, anim_prof_reset, log, log_file, log_reset, prof, Timer,
};
use crate::layout::{divider, geometry, live_resize};
use crate::platform::autostart::set_autostart;
use crate::platform::dialogs::{msgbox, notify};
use crate::platform::{monitor, parked, update, window};
use crate::ui::{launcher, overlay, tray};

use actions::*;
use animation::*;
pub(crate) use cli::run;
use events::*;
use interaction::*;
use monitors::*;
use placement::*;
use report::*;
use startup::*;
use tiling::*;
use workspaces::*;

pub(crate) use actions::{
    dispatch, dispatch_quit, reload_config_quiet, show_launcher, toggle_pause_quietly, tour_marker,
    tour_progress_path,
};
pub(crate) use interaction::{
    begin_divider, end_divider, request_relayout, settle_divider, ui_state, update_divider,
};
pub(crate) use report::export_diagnostics;
pub(crate) use startup::{end_session, query_end_session};

use window::{collect, is_fullscreen, visible_rect, Verdict, Win};

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::mem::size_of;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, POINT, RECT,
    WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
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
    IsWindowVisible, IsZoomed, KillTimer, MsgWaitForMultipleObjectsEx, PeekMessageW, PostMessageW,
    PostQuitMessage, SendMessageTimeoutW, SetCursorPos, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindowAsync, TranslateMessage, WindowFromPoint, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MENUPOPUPEND, EVENT_SYSTEM_MENUPOPUPSTART, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, GA_ROOT,
    GUITHREADINFO, GUI_INMOVESIZE, GWL_STYLE, MINMAXINFO, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE,
    QS_ALLINPUT, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT, SM_CXMINTRACK, SM_CYMINTRACK,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE,
    SW_RESTORE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_GETMINMAXINFO,
    WM_HOTKEY, WM_QUIT, WM_TIMER, WS_CHILD,
};

use border::{
    border_window_shown, clear_border, place_border, refresh_border, sync_border, sync_border_to,
    update_border,
};
use config::{action_name, Action, Dir, FullscreenWindows, Layout, WS_COUNT};
use geometry::{
    anim_duration, apply_resize_for, ease, effective_step, fullscreen_action, layout_is_valid,
    lerp_rect, neighbour, place as layout_tiles, place_fitted, same_rect, split_for_layout,
    FsAction, Gaps, MAX_RATIO, MIN_RATIO, RESIZE_PX,
};

use parked::{PARK_X, PARK_Y};

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
struct TrackedWindow {
    /// PID/thread identity guards HWND reuse when a destroy notification is lost.
    identity: Option<(u32, u32)>,
    /// An accepted asynchronous move whose monitor geometry has not caught up yet.
    pending_monitor: Option<usize>,
    float_reason: Option<String>,
}

impl TrackedWindow {
    fn is_empty(&self) -> bool {
        self.identity.is_none() && self.pending_monitor.is_none() && self.float_reason.is_none()
    }
}

#[derive(Default)]
struct State {
    tracked: HashMap<isize, TrackedWindow>,
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
    /// Live but temporarily unavailable windows keep their workspace without taking a tile.
    suspended: HashSet<isize>,
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
    /// No windows may be parked while Windows is ending the session.
    session_ending: bool,
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
    /// Interval (ms) the frame timer was armed with: the fastest monitor that is animating.
    frame_ms: u32,
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
    static SESSION_ENDING: Cell<bool> = const { Cell::new(false) };
    static SESSION_ENDED: Cell<bool> = const { Cell::new(false) };
    static EVENT_HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
    static HIDDEN_FULLSCREEN: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    static STATE: RefCell<State> = RefCell::new(State::default());
    static PENDING_EVENTS: RefCell<event_queue::DeferredQueue<PendingEvent, isize>> = RefCell::new(event_queue::DeferredQueue::default());
    static PENDING_WAKE_POSTED: Cell<bool> = const { Cell::new(false) };
    /// Refresh rate of every monitor (same indices as `monitor::list`), cached until a display change.
    static DISPLAY_HZ: RefCell<Option<Vec<u32>>> = const { RefCell::new(None) };
}

const WM_PENDING_EVENTS: u32 = 0x8000 + 42;
const WM_SESSION_CANCELLED: u32 = 0x8000 + 43;

impl State {
    /// Field removal must not leave a new tracked entry that changes membership semantics.
    fn update_tracked(&mut self, h: isize, update: impl FnOnce(&mut TrackedWindow)) {
        let window = self.tracked.entry(h).or_default();
        update(window);
        if window.is_empty() {
            self.tracked.remove(&h);
        }
    }

    fn session_ending(&mut self) -> bool {
        self.session_ending = SESSION_ENDING.with(Cell::get);
        self.session_ending
    }

    /// A new configuration must be allowed to probe smaller sizes again.
    /// Old learned constraints and pending placements belong to the previous layout.
    fn invalidate_layout_constraints(&mut self) {
        self.min_size.clear();
        self.min_size_reflow = false;
        self.capacity_float.clear();
        self.tracked.retain(|_, window| {
            window.float_reason = None;
            !window.is_empty()
        });
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
        self.live_resize.forget(windows);
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
    /// Native geometry can lag an accepted asynchronous monitor move across several layout passes.
    fn observe_monitor(&mut self, h: isize, observed: usize) {
        let observed = observed.min(MON - 1);
        let assigned = match self
            .tracked
            .get(&h)
            .and_then(|window| window.pending_monitor)
        {
            Some(target) if target != observed => target,
            _ => {
                self.update_tracked(h, |window| window.pending_monitor = None);
                observed
            }
        };
        self.mon.insert(h, assigned);
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
