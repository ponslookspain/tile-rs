//! Configuration: %APPDATA%\tile-rs\config.toml (hotkeys, gap, window rules).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

use crate::geometry::Easing;

pub const WS_COUNT: u8 = 9;

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
    ReloadConfig,
    EditConfig,
}

/// What to do with a window that runs fullscreen / borderless fullscreen (a game, a video
/// player): minimize it while its workspace is not shown, or leave it completely alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FullscreenWindows {
    /// Minimize on another workspace, restore when its workspace is shown (the default).
    #[default]
    Minimize,
    /// Never touch fullscreen windows.
    Ignore,
}

/// How to treat a window that has a photo or video open (a viewer / player showing a media file).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MediaWindows {
    /// Leave it as the app opened it (a movable floating window). The default.
    #[default]
    Float,
    /// Tile it like any other window.
    Tile,
}

/// Visual and clickable size of the workspace indicator.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IndicatorSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl IndicatorSize {
    pub fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

/// The tiling layout. Only `Spiral` is implemented so far; the others are placeholders (they are
/// shown in the launcher as "coming soon" and fall back to the spiral).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Layout {
    /// Spiral / dwindle (the default).
    #[default]
    Spiral,
    MasterStack,
    Grid,
    Columns,
}

impl Layout {
    /// The name used in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Spiral => "spiral",
            Layout::MasterStack => "master_stack",
            Layout::Grid => "grid",
            Layout::Columns => "columns",
        }
    }
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Rule {
    /// Executable name, e.g. "Telegram" or "Telegram.exe" (case-insensitive, exact).
    pub process: Option<String>,
    /// Window class (case-insensitive, exact).
    pub class: Option<String>,
    /// Part of the window title (case-insensitive).
    pub title: Option<String>,
    /// "float" = never tile this window.
    pub action: Option<String>,
    /// 1..=9: new windows of this app open on that workspace.
    pub workspace: Option<u8>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize, Default, Debug)]
struct RawConfig {
    layout: Option<String>,
    /// Legacy single gap (used for both inner and outer unless either is set).
    gap: Option<i32>,
    inner_gap: Option<i32>,
    outer_gap: Option<i32>,
    animations: Option<bool>,
    animation_ms: Option<u32>,
    easing: Option<String>,
    fullscreen_windows: Option<String>,
    media_windows: Option<String>,
    resize_step: Option<u32>,
    border: Option<bool>,
    border_color: Option<String>,
    border_width: Option<u32>,
    min_tile_width: Option<i32>,
    min_tile_height: Option<i32>,
    max_tiles: Option<u32>,
    auto_overflow: Option<bool>,
    workspace_indicator: Option<bool>,
    workspace_indicator_size: Option<String>,
    focus_follows_mouse: Option<bool>,
    #[serde(default)]
    hotkeys: BTreeMap<String, OneOrMany>,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Clone, Default, Debug)]
pub struct Config {
    /// The tiling layout (`spiral` for now).
    pub layout: Layout,
    /// Gap between neighbouring windows, in pixels.
    pub inner_gap: i32,
    /// Gap around the screen edges, in pixels.
    pub outer_gap: i32,
    pub animations: bool,
    pub animation_ms: u32,
    /// Easing curve of the animations.
    pub easing: Easing,
    /// How to handle windows that run fullscreen / borderless fullscreen.
    pub fullscreen_windows: FullscreenWindows,
    /// How to handle a window with a photo / video open.
    pub media_windows: MediaWindows,
    /// Pixels per keypress for the resize hotkeys.
    pub resize_step: u32,
    /// Draw a border around the focused tiled window.
    pub border: bool,
    /// Border colour as a Windows COLORREF (0x00BBGGRR).
    pub border_color: u32,
    /// Border thickness in pixels (0 = no border).
    pub border_width: u32,
    /// Comfortable minimum tile size; the spiral shrinks the master tile to keep tiles at least
    /// this big.
    pub min_tile_width: i32,
    pub min_tile_height: i32,
    /// Maximum number of tiles per workspace; `0` = choose automatically from the screen size.
    pub max_tiles: u32,
    /// When a workspace is full, open new windows on the next workspace and switch to it.
    pub auto_overflow: bool,
    /// Show the small workspace indicator.
    pub workspace_indicator: bool,
    pub workspace_indicator_size: IndicatorSize,
    /// Focus the window under the mouse pointer when it moves over a tiled window.
    pub focus_follows_mouse: bool,
    /// (action, "alt+shift+h")
    pub bindings: Vec<(Action, String)>,
    pub rules: Vec<Rule>,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tile-rs")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn default_config() -> String {
    let mut s = String::from(
        r##"# tile-rs configuration.
# After editing: save the file, then tray icon -> "Reload config".

# Tiling layout. "spiral" (dwindle) is the only one for now; master_stack, grid and columns
# are planned.
layout = "spiral"

# Gap between neighbouring windows (inner) and around the screen edges (outer), in pixels.
inner_gap = 10
outer_gap = 10

# Smooth window movement when the layout changes (0..1000 ms; 0 or false = instant).
# If windows feel laggy on your PC, set animations = false.
animations = true
animation_ms = 140

# Easing of the window animations: linear, out_quad, out_cubic (default), out_quart, in_out_cubic.
easing = "out_cubic"

# How many pixels the "grow/shrink" hotkeys change a window's width/height per keypress.
resize_step = 40

# Border around the focused tiled window (drawn by tile-rs itself).
border = true
border_color = "#8dbcff"
border_width = 2

# Comfortable minimum tile size: when windows would get smaller, tile-rs narrows the left (master)
# tile to make room, and once a workspace is full new windows go to the next workspace.
min_tile_width = 480
min_tile_height = 320
# Maximum tiles per workspace; 0 = pick automatically from the screen size.
max_tiles = 0
# When a workspace is full, open a new window on the next workspace and switch to it.
auto_overflow = true
# Small indicator at the top of the screen showing the workspaces (dots).
workspace_indicator = true
workspace_indicator_size = "medium" # small, medium, large

# Move focus to the window under the mouse pointer when it moves onto a tiled window.
focus_follows_mouse = false

# Windows that run fullscreen or borderless fullscreen (games, video players): "minimize" hides
# them while their workspace is not shown and restores them when you switch back; "ignore" leaves
# them completely alone.
fullscreen_windows = "minimize"

# Windows with a photo or video open (a viewer / player showing a media file): "float" leaves the
# window as the app opened it (a movable window you can put anywhere), "tile" treats it like any
# other window. Fullscreen playback is still handled by fullscreen_windows above.
media_windows = "float"

[hotkeys]
# Format: "modifiers+key". Modifiers: alt, ctrl, shift, win.
# Keys: a-z, 0-9, left, right, up, down, space, enter, tab, esc, f1-f12, minus, plus, comma, period.
# One binding or a list of bindings. Put # in front of a line to disable that hotkey.
focus_left  = ["alt+h", "alt+left"]
focus_down  = ["alt+j", "alt+down"]
focus_up    = ["alt+k", "alt+up"]
focus_right = ["alt+l", "alt+right"]
swap_left   = ["alt+shift+h", "alt+shift+left"]
swap_down   = ["alt+shift+j", "alt+shift+down"]
swap_up     = ["alt+shift+k", "alt+shift+up"]
swap_right  = ["alt+shift+l", "alt+shift+right"]
grow_width    = ["alt+plus", "alt+numpad_add"]
shrink_width  = ["alt+minus", "alt+numpad_subtract"]
# grow_height   = "alt+shift+plus"
# shrink_height = "alt+shift+minus"
minimize      = "alt+shift+m"
toggle_float  = "alt+shift+f"
close         = "alt+shift+q"
cycle_layout  = "ctrl+alt+l"
pause         = ["ctrl+alt+p", "alt+shift+p"]
quit          = "ctrl+alt+q"
# Optional (also available from the tray menu):
# reload_config = "ctrl+alt+shift+r"
# retile        = "alt+shift+t"
# edit_config   = "alt+shift+c"
"##,
    );
    for n in 1..=WS_COUNT {
        s.push_str(&format!("workspace_{n} = \"alt+{n}\"\n"));
    }
    for n in 1..=WS_COUNT {
        s.push_str(&format!("move_to_workspace_{n} = \"alt+shift+{n}\"\n"));
    }
    s.push_str("workspace_next = \"alt+shift+period\"\n");
    s.push_str("workspace_prev = \"alt+shift+comma\"\n");
    s.push_str("move_to_next_workspace = \"ctrl+alt+period\"\n");
    s.push_str("move_to_prev_workspace = \"ctrl+alt+comma\"\n");
    s.push_str(
        r#"
# Rules. A window matches when ALL given fields match
# (process and class: exact, title: contains; all case-insensitive).
#   action = "float"   -> never tile this window
#   workspace = 3      -> new windows of this app open on workspace 3
#
# [[rules]]
# process = "Telegram"
# workspace = 9
#
# [[rules]]
# title = "Settings"
# action = "float"

[[rules]]
process = "Taskmgr"
action = "float"
"#,
    );
    s
}

/// Creates the config file with defaults if it does not exist. Returns true if it was created.
pub fn ensure_default() -> bool {
    let p = config_path();
    if p.exists() {
        return false;
    }
    let _ = std::fs::create_dir_all(config_dir());
    std::fs::write(&p, default_config()).is_ok()
}

/// Adds settings that newer versions introduced to an *existing* config. It only adds: nothing is
/// changed or removed, and a key you commented out (or deleted on purpose) is left alone only if
/// its name still appears in the file. Returns the new text if anything was added.
pub fn migrate(text: &str) -> Option<String> {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let has = |key: &str| {
        text.lines().any(|l| {
            let t = l.trim_start().trim_start_matches('#').trim_start();
            t.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
    };
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut changed = false;

    // new hotkeys go right under [hotkeys]
    let mut hot: Vec<String> = Vec::new();
    if !has("grow_width") {
        hot.push(r#"grow_width    = ["alt+plus", "alt+numpad_add"]"#.into());
    }
    if !has("shrink_width") {
        hot.push(r#"shrink_width  = ["alt+minus", "alt+numpad_subtract"]"#.into());
    }
    if !has("minimize") {
        hot.push(r#"minimize      = "alt+shift+m""#.into());
    }
    if !has("toggle_float") {
        hot.push(r#"toggle_float  = "alt+shift+f""#.into());
    }
    if !has("close") {
        hot.push(r#"close         = "alt+shift+q""#.into());
    }
    if !has("cycle_layout") {
        hot.push(r#"cycle_layout  = "ctrl+alt+l""#.into());
    }
    if !has("quit") {
        hot.push(r#"quit          = "ctrl+alt+q""#.into());
    }
    for (key, spec) in [
        ("workspace_next", "alt+shift+period"),
        ("workspace_prev", "alt+shift+comma"),
        ("move_to_next_workspace", "ctrl+alt+period"),
        ("move_to_prev_workspace", "ctrl+alt+comma"),
    ] {
        if !has(key) {
            hot.push(format!("{key} = \"{spec}\""));
        }
    }
    if !hot.is_empty() {
        if let Some(i) = lines.iter().position(|l| l.trim() == "[hotkeys]") {
            for (n, l) in hot.into_iter().enumerate() {
                lines.insert(i + 1 + n, l);
            }
            changed = true;
        }
    }

    // new top-level settings go before the first [table]
    let mut top: Vec<String> = Vec::new();
    if !has("layout") {
        top.push("# Tiling layout: spiral (the only one for now).".into());
        top.push("layout = \"spiral\"".into());
    }
    if !has("inner_gap") && !has("gap") {
        top.push("# Gap between neighbouring windows (inner) and around the edges (outer).".into());
        top.push("inner_gap = 10".into());
    }
    if !has("outer_gap") && !has("gap") {
        top.push("outer_gap = 10".into());
    }
    if !has("animations") {
        top.push(
            "# Smooth window movement (animation_ms: 0..1000; animations = false = instant)."
                .into(),
        );
        top.push("animations = true".into());
    }
    if !has("animation_ms") {
        top.push("animation_ms = 140".into());
    }
    if !has("easing") {
        top.push(
            "# Easing of the window animations: linear, out_quad, out_cubic (default), out_quart, in_out_cubic."
                .into(),
        );
        top.push("easing = \"out_cubic\"".into());
    }
    if !has("resize_step") {
        top.push("# Pixels per keypress for the grow/shrink hotkeys.".into());
        top.push("resize_step = 40".into());
    }
    if !has("border") {
        top.push("# Coloured border around the focused tiled window.".into());
        top.push("border = true".into());
    }
    if !has("border_color") {
        top.push("border_color = \"#8dbcff\"".into());
    }
    if !has("border_width") {
        top.push("border_width = 2".into());
    }
    if !has("min_tile_width") {
        top.push("# Comfortable minimum tile size (the master tile narrows to make room).".into());
        top.push("min_tile_width = 480".into());
    }
    if !has("min_tile_height") {
        top.push("min_tile_height = 320".into());
    }
    if !has("max_tiles") {
        top.push("# Maximum tiles per workspace; 0 = automatic (from the screen size).".into());
        top.push("max_tiles = 0".into());
    }
    if !has("auto_overflow") {
        top.push("# Open a new window on the next workspace when this one is full.".into());
        top.push("auto_overflow = true".into());
    }
    if !has("workspace_indicator") {
        top.push("workspace_indicator = true".into());
    }
    if !has("workspace_indicator_size") {
        top.push("workspace_indicator_size = \"medium\" # small, medium, large".into());
    }
    if !has("focus_follows_mouse") {
        top.push("# Focus the window under the mouse pointer when it moves onto it.".into());
        top.push("focus_follows_mouse = false".into());
    }
    if !has("fullscreen_windows") {
        top.push(
            "# Windows that run fullscreen / borderless fullscreen: \"minimize\" or \"ignore\"."
                .into(),
        );
        top.push("fullscreen_windows = \"minimize\"".into());
    }
    if !has("media_windows") {
        top.push(
            "# Windows with a photo/video open: \"float\" (as the app opened it) or \"tile\"."
                .into(),
        );
        top.push("media_windows = \"float\"".into());
    }
    if !top.is_empty() {
        let at = lines
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        top.push(String::new());
        for (n, l) in top.into_iter().enumerate() {
            lines.insert(at + n, l);
        }
        changed = true;
    }

    if !changed {
        return None;
    }
    let mut out = lines.join(nl);
    out.push_str(nl);
    Some(out)
}

/// Edits top-level `key = value` lines in `text` in place (the value is already formatted, e.g.
/// `"spiral"`, `10`, `true`). A missing key is inserted before the first `[section]`. Everything
/// else in the file (comments, hotkeys, rules) is left untouched.
pub fn edit_top(text: &str, pairs: &[(&str, String)]) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut at = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let mut inserted = 0;
    for (key, value) in pairs {
        let line = format!("{key} = {value}");
        let pos = lines.iter().position(|l| {
            let t = l.trim_start().trim_start_matches('#').trim_start();
            t.strip_prefix(*key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        });
        match pos {
            Some(i) => lines[i] = line,
            None => {
                lines.insert(at + inserted, line);
                inserted += 1;
                at += 0; // keep `at` fixed; `inserted` tracks the offset
            }
        }
    }
    let mut out = lines.join(nl);
    out.push_str(nl);
    out
}

/// Runs `migrate` on the config file (keeping a copy as config.toml.bak). True if it changed.
pub fn migrate_file() -> bool {
    let p = config_path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return false;
    };
    let Some(new) = migrate(&text) else {
        return false;
    };
    // never write something we cannot read back
    if parse(&new).is_err() {
        return false;
    }
    let _ = std::fs::write(p.with_extension("toml.bak"), &text);
    std::fs::write(&p, new).is_ok()
}

fn parse_action(name: &str) -> Option<Action> {
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
    if let Some(d) = n.strip_prefix("focus_") {
        return dir(d).map(Action::Focus);
    }
    if let Some(d) = n.strip_prefix("swap_") {
        return dir(d).map(Action::Swap);
    }
    // "next"/"prev" are not numbers, so they must be matched before the numbered prefixes
    match n.as_str() {
        "workspace_next" => return Some(Action::WorkspaceNext),
        "workspace_prev" => return Some(Action::WorkspacePrev),
        "move_to_next_workspace" => return Some(Action::MoveToNext),
        "move_to_prev_workspace" => return Some(Action::MoveToPrev),
        _ => {}
    }
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
        "reload_config" => Some(Action::ReloadConfig),
        "edit_config" => Some(Action::EditConfig),
        _ => None,
    }
}

/// "alt+shift+h" -> (modifiers, virtual-key code)
pub fn parse_hotkey(spec: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key: Option<u32> = None;
    for part in spec.split('+') {
        let p = part.trim().to_ascii_lowercase();
        match p.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            "" => return Err(format!("empty part in \"{spec}\"")),
            other => {
                if key.is_some() {
                    return Err(format!("\"{spec}\": more than one key"));
                }
                key = Some(
                    parse_key(other)
                        .ok_or_else(|| format!("unknown key \"{other}\" in \"{spec}\""))?,
                );
            }
        }
    }
    match key {
        Some(k) => Ok((mods, k)),
        None => Err(format!("\"{spec}\": no key")),
    }
}

fn parse_key(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.len() == 1 {
        let c = b[0];
        if c.is_ascii_lowercase() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=12).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    Some(match k {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "numpad_add" => 0x6B,
        "numpad_subtract" => 0x6D,
        "minus" => 0xBD,
        "plus" | "equal" => 0xBB,
        "comma" => 0xBC,
        "period" => 0xBE,
        _ => return None,
    })
}

/// "linear", "out_quad", "out_cubic", "out_quart", "in_out_cubic" -> `Easing`.
pub fn parse_easing(s: &str) -> Result<Easing, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "linear" => Ok(Easing::Linear),
        "out_quad" => Ok(Easing::OutQuad),
        "out_cubic" => Ok(Easing::OutCubic),
        "out_quart" => Ok(Easing::OutQuart),
        "in_out_cubic" => Ok(Easing::InOutCubic),
        other => Err(format!(
            "easing = \"{other}\": expected one of linear, out_quad, out_cubic, out_quart, in_out_cubic"
        )),
    }
}

/// "spiral", "master_stack", "grid", "columns" -> `Layout`.
pub fn parse_layout(s: &str) -> Result<Layout, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "spiral" => Ok(Layout::Spiral),
        "master_stack" => Ok(Layout::MasterStack),
        "grid" => Ok(Layout::Grid),
        "columns" => Ok(Layout::Columns),
        other => Err(format!(
            "layout = \"{other}\": expected one of spiral, master_stack, grid, columns"
        )),
    }
}

/// "minimize", "ignore" -> `FullscreenWindows`.
pub fn parse_fullscreen_windows(s: &str) -> Result<FullscreenWindows, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "minimize" => Ok(FullscreenWindows::Minimize),
        "ignore" => Ok(FullscreenWindows::Ignore),
        other => Err(format!(
            "fullscreen_windows = \"{other}\": expected \"minimize\" or \"ignore\""
        )),
    }
}

/// "float", "tile" -> `MediaWindows`.
pub fn parse_media_windows(s: &str) -> Result<MediaWindows, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "float" => Ok(MediaWindows::Float),
        "tile" => Ok(MediaWindows::Tile),
        other => Err(format!(
            "media_windows = \"{other}\": expected \"float\" or \"tile\""
        )),
    }
}

/// "#rrggbb" (or "rrggbb") -> Windows COLORREF (0x00BBGGRR)
pub fn parse_color(s: &str) -> Result<u32, String> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "border_color = \"{s}\": expected a colour like \"#8dbcff\""
        ));
    }
    let v = u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
    let (r, g, b) = ((v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF);
    Ok((b << 16) | (g << 8) | r)
}

pub fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    let layout = parse_layout(raw.layout.as_deref().unwrap_or("spiral"))?;
    let inner_gap = raw.inner_gap.or(raw.gap).unwrap_or(10);
    let outer_gap = raw.outer_gap.or(raw.gap).unwrap_or(10);
    if !(0..=200).contains(&inner_gap) {
        return Err(format!(
            "inner_gap = {inner_gap}: must be between 0 and 200"
        ));
    }
    if !(0..=200).contains(&outer_gap) {
        return Err(format!(
            "outer_gap = {outer_gap}: must be between 0 and 200"
        ));
    }
    let animation_ms = raw.animation_ms.unwrap_or(140);
    if animation_ms > 1000 {
        return Err(format!(
            "animation_ms = {animation_ms}: must be between 0 and 1000"
        ));
    }
    let animations = raw.animations.unwrap_or(true) && animation_ms > 0;
    let easing = parse_easing(raw.easing.as_deref().unwrap_or("out_cubic"))?;
    let fullscreen_windows =
        parse_fullscreen_windows(raw.fullscreen_windows.as_deref().unwrap_or("minimize"))?;
    let media_windows = parse_media_windows(raw.media_windows.as_deref().unwrap_or("float"))?;
    let resize_step = raw.resize_step.unwrap_or(40);
    if !(1..=500).contains(&resize_step) {
        return Err(format!(
            "resize_step = {resize_step}: must be between 1 and 500"
        ));
    }
    let border = raw.border.unwrap_or(true);
    let border_color = parse_color(raw.border_color.as_deref().unwrap_or("#8dbcff"))?;
    let border_width = raw.border_width.unwrap_or(2);
    if border_width > 20 {
        return Err(format!(
            "border_width = {border_width}: must be between 0 and 20"
        ));
    }
    let min_tile_width = raw.min_tile_width.unwrap_or(480);
    let min_tile_height = raw.min_tile_height.unwrap_or(320);
    if !(100..=4000).contains(&min_tile_width) || !(100..=4000).contains(&min_tile_height) {
        return Err(format!(
            "min_tile_width/height = {min_tile_width}/{min_tile_height}: must be between 100 and 4000"
        ));
    }
    let max_tiles = raw.max_tiles.unwrap_or(0);
    if max_tiles > 20 {
        return Err(format!("max_tiles = {max_tiles}: must be between 0 and 20"));
    }
    let auto_overflow = raw.auto_overflow.unwrap_or(true);
    let workspace_indicator = raw.workspace_indicator.unwrap_or(true);
    let workspace_indicator_size = match raw.workspace_indicator_size.as_deref().unwrap_or("medium")
    {
        "small" => IndicatorSize::Small,
        "medium" => IndicatorSize::Medium,
        "large" => IndicatorSize::Large,
        value => {
            return Err(format!(
                "workspace_indicator_size = \"{value}\": expected small, medium, or large"
            ))
        }
    };
    let focus_follows_mouse = raw.focus_follows_mouse.unwrap_or(false);
    let mut bindings = Vec::new();
    for (name, v) in raw.hotkeys {
        let action =
            parse_action(&name).ok_or_else(|| format!("[hotkeys]: unknown action \"{name}\""))?;
        let specs = match v {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        };
        for s in specs {
            parse_hotkey(&s).map_err(|e| format!("[hotkeys] {name}: {e}"))?;
            bindings.push((action, s));
        }
    }
    for (i, r) in raw.rules.iter().enumerate() {
        if let Some(w) = r.workspace {
            if !(1..=WS_COUNT).contains(&w) {
                return Err(format!(
                    "rule #{}: workspace must be 1..{}",
                    i + 1,
                    WS_COUNT
                ));
            }
        }
        if let Some(a) = &r.action {
            if a != "float" && a != "tile" {
                return Err(format!(
                    "rule #{}: unknown action \"{a}\" (only \"float\" or \"tile\")",
                    i + 1
                ));
            }
        }
        if r.process.is_none() && r.class.is_none() && r.title.is_none() {
            return Err(format!(
                "rule #{}: needs at least one of process / class / title",
                i + 1
            ));
        }
    }
    Ok(Config {
        layout,
        inner_gap,
        outer_gap,
        animations,
        animation_ms,
        easing,
        fullscreen_windows,
        media_windows,
        resize_step,
        border,
        border_color,
        border_width,
        min_tile_width,
        min_tile_height,
        max_tiles,
        auto_overflow,
        workspace_indicator,
        workspace_indicator_size,
        focus_follows_mouse,
        bindings,
        rules: raw.rules,
    })
}

pub fn load() -> Result<Config, String> {
    let p = config_path();
    let text =
        std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    parse(&text)
}

static CURRENT: OnceLock<RwLock<Config>> = OnceLock::new();

fn cell() -> &'static RwLock<Config> {
    CURRENT.get_or_init(|| RwLock::new(parse(&default_config()).unwrap_or_default()))
}

thread_local! {
    /// A snapshot of `CURRENT` so the hot paths (animation frames, layout) do not take the lock
    /// once per setting they read. `set` invalidates it; every read happens on the main thread.
    static SNAPSHOT: RefCell<Option<Config>> = const { RefCell::new(None) };
}

/// Runs `f` with the current config, taking the lock only when the snapshot is stale. Must not be
/// called re-entrantly (the accessors below only read plain fields).
fn with_cfg<R>(f: impl FnOnce(&Config) -> R) -> R {
    SNAPSHOT.with(|s| {
        let mut snap = s.borrow_mut();
        if snap.is_none() {
            *snap = Some(cell().read().map(|c| c.clone()).unwrap_or_default());
        }
        f(snap.as_ref().unwrap())
    })
}

pub fn current() -> Config {
    with_cfg(|c| c.clone())
}

pub fn set(c: Config) {
    if let Ok(mut w) = cell().write() {
        *w = c;
    }
    SNAPSHOT.with(|s| *s.borrow_mut() = None);
}

/// The configured tiling layout (only `Spiral` is implemented).
pub fn layout() -> Layout {
    with_cfg(|c| c.layout)
}

/// Gap between neighbouring windows.
pub fn inner_gap() -> i32 {
    with_cfg(|c| c.inner_gap)
}

/// Gap around the screen edges.
pub fn outer_gap() -> i32 {
    with_cfg(|c| c.outer_gap)
}

/// (animations enabled, duration in ms)
pub fn animation() -> (bool, u32) {
    with_cfg(|c| (c.animations, c.animation_ms))
}

/// Easing curve of the animations.
pub fn easing() -> Easing {
    with_cfg(|c| c.easing)
}

/// How to handle windows that run fullscreen / borderless fullscreen.
pub fn fullscreen_windows() -> FullscreenWindows {
    with_cfg(|c| c.fullscreen_windows)
}

/// How to treat a window with a photo / video open.
pub fn media_windows() -> MediaWindows {
    with_cfg(|c| c.media_windows)
}

/// The border colour (COLORREF) if the border is enabled.
pub fn border() -> Option<u32> {
    with_cfg(|c| c.border.then_some(c.border_color))
}

/// The configured border colour (COLORREF), whether or not the border is enabled.
pub fn border_color_value() -> u32 {
    with_cfg(|c| c.border_color)
}

pub fn resize_step() -> i32 {
    with_cfg(|c| c.resize_step as i32)
}

pub fn border_width() -> i32 {
    with_cfg(|c| c.border_width as i32)
}

/// (min tile width, min tile height)
pub fn min_tile() -> (i32, i32) {
    with_cfg(|c| (c.min_tile_width, c.min_tile_height))
}

/// Configured maximum tiles per workspace (0 = automatic).
pub fn max_tiles() -> u32 {
    with_cfg(|c| c.max_tiles)
}

pub fn auto_overflow() -> bool {
    with_cfg(|c| c.auto_overflow)
}

pub fn workspace_indicator() -> bool {
    with_cfg(|c| c.workspace_indicator)
}

/// Whether moving the pointer over a tiled window focuses it.
pub fn focus_follows_mouse() -> bool {
    with_cfg(|c| c.focus_follows_mouse)
}

/// Just the rules (cheaper than cloning the whole config on hot paths).
pub fn rules() -> Vec<Rule> {
    with_cfg(|c| c.rules.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = parse(&default_config()).expect("default config must parse");
        assert_eq!(c.layout, Layout::Spiral);
        assert_eq!((c.inner_gap, c.outer_gap), (10, 10));
        assert!(c.animations);
        assert_eq!(c.animation_ms, 140);
        assert_eq!(c.easing, Easing::OutCubic);
        assert_eq!(c.fullscreen_windows, FullscreenWindows::Minimize);
        assert_eq!(c.media_windows, MediaWindows::Float);
        assert_eq!(c.resize_step, 40);
        assert!(c.border);
        assert_eq!(c.border_color, 0xFFBC8D); // #8dbcff as COLORREF (BGR)
        assert_eq!(c.border_width, 2);
        assert_eq!((c.min_tile_width, c.min_tile_height), (480, 320));
        assert_eq!(c.max_tiles, 0);
        assert!(c.auto_overflow);
        assert!(c.workspace_indicator);
        assert!(!c.focus_follows_mouse);
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Minimize && s == "alt+shift+m"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Close && s == "alt+shift+q"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::CycleLayout && s == "ctrl+alt+l"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::ToggleFloat && s == "alt+shift+f"));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize {
                width: true,
                grow: true,
            } && s == "alt+plus"
        }));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize {
                width: true,
                grow: false,
            } && s == "alt+minus"
        }));
        // 16 direction + 18 workspace + 4 resize + 4 misc (pause x2, toggle_float, close,
        // cycle_layout, quit) + 4 next/prev switch & move
        assert!(c.bindings.len() >= 16 + 18 + 4 + 10);
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::WorkspaceNext && s == "alt+shift+period"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::MoveToPrev && s == "ctrl+alt+comma"));
        assert_eq!(c.rules.len(), 1);
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Workspace(0) && s == "alt+1"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::MoveTo(8) && s == "alt+shift+9"));
    }

    /// What the first versions generated (no animations, no resize hotkeys).
    const OLD_CONFIG: &str = "# tile-rs configuration.\n\ngap = 10\n\n[hotkeys]\nfocus_left  = [\"alt+h\", \"alt+left\"]\npause = \"ctrl+alt+p\"\nquit = \"ctrl+alt+q\"\n\n[[rules]]\nprocess = \"Taskmgr\"\naction = \"float\"\n";

    #[test]
    fn migrate_adds_new_settings_to_an_old_config() {
        let new = migrate(OLD_CONFIG).expect("something must be added");
        let c = parse(&new).expect("migrated config must still parse");
        assert!(c.animations);
        assert_eq!(c.animation_ms, 140);
        assert_eq!(c.easing, Easing::OutCubic);
        assert_eq!(c.fullscreen_windows, FullscreenWindows::Minimize);
        assert_eq!(c.media_windows, MediaWindows::Float);
        assert_eq!(c.resize_step, 40);
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Minimize && s == "alt+shift+m"));
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Close && s == "alt+shift+q"));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize {
                width: true,
                grow: true,
            } && s == "alt+plus"
        }));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize {
                width: true,
                grow: false,
            } && s == "alt+minus"
        }));
        // old content is untouched
        assert!(new.contains("focus_left  = [\"alt+h\", \"alt+left\"]"));
        assert!(new.contains("process = \"Taskmgr\""));
        assert_eq!(c.rules.len(), 1);
    }

    #[test]
    fn migrate_adds_the_quit_hotkey_to_a_config_that_lacks_it() {
        let old = "[hotkeys]\nfocus_left = \"alt+h\"\n";
        let new = migrate(old).expect("quit must be added");
        let c = parse(&new).unwrap();
        assert!(c
            .bindings
            .iter()
            .any(|(a, s)| *a == Action::Quit && s == "ctrl+alt+q"));
    }

    #[test]
    fn migrate_is_idempotent_and_respects_choices() {
        let once = migrate(OLD_CONFIG).unwrap();
        assert!(migrate(&once).is_none(), "second run must change nothing");
        // a key the user commented out or turned off is not re-added
        let custom = "layout = \"spiral\"\ngap = 5\nanimations = false\n# animation_ms = 99\neasing = \"linear\"\nfullscreen_windows = \"ignore\"\nmedia_windows = \"tile\"\nresize_step = 10\nborder = false\n# border_color = \"#112233\"\nborder_width = 3\nmin_tile_width = 400\nmin_tile_height = 300\nmax_tiles = 6\nauto_overflow = false\nworkspace_indicator = false\nfocus_follows_mouse = true\n\n[hotkeys]\n# grow_width = \"alt+plus\"\n# shrink_width = \"alt+minus\"\nminimize = \"alt+shift+m\"\ntoggle_float = \"alt+shift+f\"\nclose = \"alt+shift+q\"\ncycle_layout = \"ctrl+alt+l\"\nquit = \"ctrl+alt+q\"\nworkspace_next = \"alt+shift+period\"\nworkspace_prev = \"alt+shift+comma\"\nmove_to_next_workspace = \"ctrl+alt+period\"\nmove_to_prev_workspace = \"ctrl+alt+comma\"\n";
        assert!(migrate(custom).is_none());
        // the freshly generated default never needs migrating
        assert!(migrate(&default_config()).is_none());
    }

    #[test]
    fn migrate_keeps_crlf_line_endings() {
        let crlf = OLD_CONFIG.replace('\n', "\r\n");
        let new = migrate(&crlf).unwrap();
        assert!(
            !new.replace("\r\n", "").contains('\n'),
            "only CRLF line breaks"
        );
        assert!(parse(&new).is_ok());
    }

    #[test]
    fn gaps_split_from_the_legacy_single_gap() {
        // the old `gap` still sets both
        let c = parse("gap = 4").unwrap();
        assert_eq!((c.inner_gap, c.outer_gap), (4, 4));
        // separate values win
        let c = parse("inner_gap = 2\nouter_gap = 8").unwrap();
        assert_eq!((c.inner_gap, c.outer_gap), (2, 8));
        // one set, the other falls back to the legacy gap
        let c = parse("gap = 6\ninner_gap = 1").unwrap();
        assert_eq!((c.inner_gap, c.outer_gap), (1, 6));
        // out of range is rejected
        assert!(parse("inner_gap = 999").is_err());
        assert!(parse("outer_gap = -1").is_err());
    }

    #[test]
    fn colors_parse() {
        assert_eq!(parse_color("#ff0000").unwrap(), 0x0000FF); // red in COLORREF
        assert_eq!(parse_color("00FF00").unwrap(), 0x00FF00);
        assert_eq!(parse_color(" #8DBCFF ").unwrap(), 0xFFBC8D);
        assert!(parse_color("#fff").is_err());
        assert!(parse_color("#gg0000").is_err());
        assert!(parse("border_color = \"nope\"").is_err());
        assert!(!parse("border = false").unwrap().border);
    }

    #[test]
    fn easing_parses_and_rejects_unknown() {
        assert_eq!(parse_easing("linear").unwrap(), Easing::Linear);
        assert_eq!(parse_easing("OUT_QUAD").unwrap(), Easing::OutQuad);
        assert_eq!(parse_easing(" out_cubic ").unwrap(), Easing::OutCubic);
        assert_eq!(parse_easing("out_quart").unwrap(), Easing::OutQuart);
        assert_eq!(parse_easing("in_out_cubic").unwrap(), Easing::InOutCubic);
        let err = parse_easing("wobble").unwrap_err();
        assert!(err.contains("out_cubic"), "{err}");
        assert!(parse("easing = \"wobble\"").is_err());
        assert_eq!(parse("easing = \"linear\"").unwrap().easing, Easing::Linear);
    }

    #[test]
    fn layout_parses_and_rejects_unknown() {
        assert_eq!(parse_layout("spiral").unwrap(), Layout::Spiral);
        assert_eq!(parse_layout(" GRID ").unwrap(), Layout::Grid);
        assert_eq!(parse_layout("master_stack").unwrap(), Layout::MasterStack);
        assert_eq!(parse_layout("columns").unwrap(), Layout::Columns);
        assert!(parse_layout("stacked").is_err());
        assert!(parse("layout = \"stacked\"").is_err());
        assert_eq!(parse("layout = \"grid\"").unwrap().layout, Layout::Grid);
    }

    #[test]
    fn fullscreen_windows_parses_and_rejects_unknown() {
        assert_eq!(
            parse_fullscreen_windows("minimize").unwrap(),
            FullscreenWindows::Minimize
        );
        assert_eq!(
            parse_fullscreen_windows("IGNORE").unwrap(),
            FullscreenWindows::Ignore
        );
        assert_eq!(
            parse_fullscreen_windows(" ignore ").unwrap(),
            FullscreenWindows::Ignore
        );
        assert!(parse_fullscreen_windows("hide").is_err());
        assert!(parse("fullscreen_windows = \"hide\"").is_err());
        assert_eq!(
            parse("fullscreen_windows = \"ignore\"")
                .unwrap()
                .fullscreen_windows,
            FullscreenWindows::Ignore
        );
    }

    #[test]
    fn media_windows_parses_and_rejects_unknown() {
        assert_eq!(parse_media_windows("float").unwrap(), MediaWindows::Float);
        assert_eq!(parse_media_windows("TILE").unwrap(), MediaWindows::Tile);
        assert_eq!(parse_media_windows(" tile ").unwrap(), MediaWindows::Tile);
        assert!(parse_media_windows("hide").is_err());
        assert!(parse("media_windows = \"hide\"").is_err());
        assert_eq!(
            parse("media_windows = \"tile\"").unwrap().media_windows,
            MediaWindows::Tile
        );
        // a rule may force a media window back into the tiling
        let c = parse("media_windows = \"float\"\n[[rules]]\nprocess = \"vlc\"\naction = \"tile\"")
            .unwrap();
        assert_eq!(c.media_windows, MediaWindows::Float);
        assert_eq!(c.rules[0].action.as_deref(), Some("tile"));
    }

    #[test]
    fn hotkeys_parse() {
        let (m, k) = parse_hotkey("Alt+Shift+H").unwrap();
        assert_eq!(k, b'H' as u32);
        assert_eq!(m, MOD_ALT | MOD_SHIFT);
        assert_eq!(parse_hotkey("ctrl+alt+left").unwrap().1, 0x25);
        assert_eq!(parse_hotkey("win+f5").unwrap().1, 0x74);
        assert_eq!(parse_hotkey("alt+plus").unwrap().1, 0xBB);
        assert_eq!(parse_hotkey("alt+minus").unwrap().1, 0xBD);
        assert_eq!(parse_hotkey("alt+numpad_add").unwrap().1, 0x6B);
        assert!(parse_hotkey("alt+").is_err());
        assert!(parse_hotkey("alt+h+j").is_err());
        assert!(parse_hotkey("alt+banana").is_err());
    }

    #[test]
    fn bad_configs_are_rejected_with_a_message() {
        assert!(parse("gap = 9999").is_err());
        assert!(parse("animation_ms = 5000").is_err());
        assert!(parse("resize_step = 0").is_err());
        assert!(parse("resize_step = 900").is_err());
        assert!(!parse("animations = false").unwrap().animations);
        assert!(!parse("animation_ms = 0").unwrap().animations);
        assert!(parse("[hotkeys]\nteleport = \"alt+t\"")
            .unwrap_err()
            .contains("teleport"));
        assert!(parse("[[rules]]\naction = \"float\"").is_err()); // no matcher
        assert!(parse("[[rules]]\nprocess = \"x\"\nworkspace = 12").is_err());
        assert!(parse("this is not toml").is_err());
    }
}
