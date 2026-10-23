//! Action names and parsing shared by configuration, settings and runtime dispatch.

use super::WS_COUNT;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Focus(Dir),
    Swap(Dir),
    /// 0-based workspace index
    Workspace(u8),
    MoveTo(u8),
    /// Switch to the next / previous workspace, wrapping around at the ends.
    WorkspaceNext,
    WorkspacePrev,
    /// Move the focused window to the next / previous workspace (wrapping) and follow it.
    MoveToNext,
    MoveToPrev,
    /// Grow / shrink the focused window's width (or height) by `resize_step` pixels.
    Resize {
        width: bool,
        grow: bool,
    },
    /// Minimize the focused window.
    Minimize,
    /// Toggle the focused window between tiled and floating.
    ToggleFloat,
    /// Close the focused window (like Alt+F4).
    Close,
    Pause,
    Quit,
    Retile,
    /// Switch to the next tiling layout (spiral -> master-stack -> grid -> columns -> spiral).
    CycleLayout,
    /// Move keyboard focus to the next / previous monitor.
    FocusMonitor(i8),
    /// Send the focused window to the next / previous monitor.
    MoveToMonitor(i8),
    ReloadConfig,
    EditConfig,
    /// Show the focused window alone on the whole screen (the rest stay stacked underneath);
    /// press again to go back to the normal layout of the workspace.
    ToggleMonocle,
    /// Focus the window that was focused before the current one (switching workspace if needed).
    FocusLast,
    /// Switch the active monitor back to the workspace it showed before the current one.
    WorkspaceBack,
    /// Add (+1) / remove (-1) a window in the master column of master-stack.
    MasterCount(i8),
}

pub(super) fn parse_action(name: &str) -> Option<Action> {
    let n = name.trim().to_ascii_lowercase();
    let dir = |s: &str| match s {
        "left" => Some(Dir::Left),
        "down" => Some(Dir::Down),
        "up" => Some(Dir::Up),
        "right" => Some(Dir::Right),
        _ => None,
    };
    let ws = |s: &str| {
        s.parse::<u8>()
            .ok()
            .filter(|n| (1..=WS_COUNT).contains(n))
            .map(|n| n - 1)
    };
    match n.as_str() {
        "workspace_next" => return Some(Action::WorkspaceNext),
        "focus_next_monitor" => return Some(Action::FocusMonitor(1)),
        "focus_prev_monitor" => return Some(Action::FocusMonitor(-1)),
        "move_to_next_monitor" => return Some(Action::MoveToMonitor(1)),
        "move_to_prev_monitor" => return Some(Action::MoveToMonitor(-1)),
        "workspace_prev" => return Some(Action::WorkspacePrev),
        // exact names must come before the `focus_` / `workspace_` prefix handlers below
        "focus_last" => return Some(Action::FocusLast),
        "workspace_back_and_forth" => return Some(Action::WorkspaceBack),
        "move_to_next_workspace" => return Some(Action::MoveToNext),
        "move_to_prev_workspace" => return Some(Action::MoveToPrev),
        _ => {}
    }
    if let Some(d) = n.strip_prefix("focus_") {
        return dir(d).map(Action::Focus);
    }
    if let Some(d) = n.strip_prefix("swap_") {
        return dir(d).map(Action::Swap);
    }
    // "next"/"prev" are not numbers, so they must be matched before the numbered prefixes
    if let Some(i) = n.strip_prefix("move_to_workspace_") {
        return ws(i).map(Action::MoveTo);
    }
    if let Some(i) = n.strip_prefix("workspace_") {
        return ws(i).map(Action::Workspace);
    }
    match n.as_str() {
        "grow_width" => Some(Action::Resize {
            width: true,
            grow: true,
        }),
        "shrink_width" => Some(Action::Resize {
            width: true,
            grow: false,
        }),
        "grow_height" => Some(Action::Resize {
            width: false,
            grow: true,
        }),
        "shrink_height" => Some(Action::Resize {
            width: false,
            grow: false,
        }),
        "minimize" => Some(Action::Minimize),
        "toggle_float" => Some(Action::ToggleFloat),
        "close" => Some(Action::Close),
        "pause" => Some(Action::Pause),
        "quit" => Some(Action::Quit),
        "retile" => Some(Action::Retile),
        "cycle_layout" => Some(Action::CycleLayout),
        "toggle_monocle" => Some(Action::ToggleMonocle),
        "master_count_inc" => Some(Action::MasterCount(1)),
        "master_count_dec" => Some(Action::MasterCount(-1)),
        "reload_config" => Some(Action::ReloadConfig),
        "edit_config" => Some(Action::EditConfig),
        _ => None,
    }
}

pub(crate) fn action_name(a: Action) -> String {
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
