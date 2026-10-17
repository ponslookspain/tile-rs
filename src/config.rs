//! Configuration: %APPDATA%\tile-rs\config.toml (hotkeys, gap, window rules).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

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
    /// Grow / shrink the focused window's width (or height) by `resize_step` pixels.
    Resize { width: bool, grow: bool },
    Pause,
    Quit,
    Retile,
    ReloadConfig,
    EditConfig,
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
    gap: Option<i32>,
    animations: Option<bool>,
    animation_ms: Option<u32>,
    resize_step: Option<u32>,
    #[serde(default)]
    hotkeys: BTreeMap<String, OneOrMany>,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Clone, Default, Debug)]
pub struct Config {
    pub gap: i32,
    pub animations: bool,
    pub animation_ms: u32,
    /// Pixels per keypress for the resize hotkeys.
    pub resize_step: u32,
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
        r#"# tile-rs configuration.
# After editing: save the file, then tray icon -> "Reload config".

# Gap between windows and around the screen edges, in pixels.
gap = 10

# Smooth window movement when the layout changes (0..1000 ms; 0 or false = instant).
# If windows feel laggy on your PC, set animations = false.
animations = true
animation_ms = 140

# How many pixels the "grow/shrink" hotkeys change a window's width/height per keypress.
resize_step = 40

[hotkeys]
# Format: "modifiers+key". Modifiers: alt, ctrl, shift, win.
# Keys: a-z, 0-9, left, right, up, down, space, enter, tab, esc, f1-f12, minus, plus, comma, period.
# One binding or a list of bindings. Delete a line to disable that hotkey.
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
pause         = ["ctrl+alt+p", "alt+shift+p"]
quit          = "ctrl+alt+q"
# Optional (also available from the tray menu):
# reload_config = "ctrl+alt+shift+r"
# retile        = "alt+shift+t"
# edit_config   = "alt+shift+c"
"#,
    );
    for n in 1..=WS_COUNT {
        s.push_str(&format!("workspace_{n} = \"alt+{n}\"\n"));
    }
    for n in 1..=WS_COUNT {
        s.push_str(&format!("move_to_workspace_{n} = \"alt+shift+{n}\"\n"));
    }
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
            t.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
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
    if !has("animations") {
        top.push("# Smooth window movement (animation_ms: 0..1000; animations = false = instant).".into());
        top.push("animations = true".into());
    }
    if !has("animation_ms") {
        top.push("animation_ms = 140".into());
    }
    if !has("resize_step") {
        top.push("# Pixels per keypress for the grow/shrink hotkeys.".into());
        top.push("resize_step = 40".into());
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

/// Runs `migrate` on the config file (keeping a copy as config.toml.bak). True if it changed.
pub fn migrate_file() -> bool {
    let p = config_path();
    let Ok(text) = std::fs::read_to_string(&p) else { return false };
    let Some(new) = migrate(&text) else { return false };
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
    let ws = |s: &str| s.parse::<u8>().ok().filter(|n| (1..=WS_COUNT).contains(n)).map(|n| n - 1);
    if let Some(d) = n.strip_prefix("focus_") {
        return dir(d).map(Action::Focus);
    }
    if let Some(d) = n.strip_prefix("swap_") {
        return dir(d).map(Action::Swap);
    }
    if let Some(i) = n.strip_prefix("move_to_workspace_") {
        return ws(i).map(Action::MoveTo);
    }
    if let Some(i) = n.strip_prefix("workspace_") {
        return ws(i).map(Action::Workspace);
    }
    match n.as_str() {
        "grow_width" => Some(Action::Resize { width: true, grow: true }),
        "shrink_width" => Some(Action::Resize { width: true, grow: false }),
        "grow_height" => Some(Action::Resize { width: false, grow: true }),
        "shrink_height" => Some(Action::Resize { width: false, grow: false }),
        "pause" => Some(Action::Pause),
        "quit" => Some(Action::Quit),
        "retile" => Some(Action::Retile),
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
                key = Some(parse_key(other).ok_or_else(|| format!("unknown key \"{other}\" in \"{spec}\""))?);
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

pub fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    let gap = raw.gap.unwrap_or(10);
    if !(0..=200).contains(&gap) {
        return Err(format!("gap = {gap}: must be between 0 and 200"));
    }
    let animation_ms = raw.animation_ms.unwrap_or(140);
    if animation_ms > 1000 {
        return Err(format!("animation_ms = {animation_ms}: must be between 0 and 1000"));
    }
    let animations = raw.animations.unwrap_or(true) && animation_ms > 0;
    let resize_step = raw.resize_step.unwrap_or(40);
    if !(1..=500).contains(&resize_step) {
        return Err(format!("resize_step = {resize_step}: must be between 1 and 500"));
    }
    let mut bindings = Vec::new();
    for (name, v) in raw.hotkeys {
        let action = parse_action(&name).ok_or_else(|| format!("[hotkeys]: unknown action \"{name}\""))?;
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
                return Err(format!("rule #{}: workspace must be 1..{}", i + 1, WS_COUNT));
            }
        }
        if let Some(a) = &r.action {
            if a != "float" {
                return Err(format!("rule #{}: unknown action \"{a}\" (only \"float\")", i + 1));
            }
        }
        if r.process.is_none() && r.class.is_none() && r.title.is_none() {
            return Err(format!("rule #{}: needs at least one of process / class / title", i + 1));
        }
    }
    Ok(Config { gap, animations, animation_ms, resize_step, bindings, rules: raw.rules })
}

pub fn load() -> Result<Config, String> {
    let p = config_path();
    let text = std::fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    parse(&text)
}

static CURRENT: OnceLock<RwLock<Config>> = OnceLock::new();

fn cell() -> &'static RwLock<Config> {
    CURRENT.get_or_init(|| RwLock::new(parse(&default_config()).unwrap_or_default()))
}

pub fn current() -> Config {
    cell().read().map(|c| c.clone()).unwrap_or_default()
}

pub fn set(c: Config) {
    if let Ok(mut w) = cell().write() {
        *w = c;
    }
}

pub fn gap() -> i32 {
    cell().read().map(|c| c.gap).unwrap_or(10)
}

/// (animations enabled, duration in ms)
pub fn animation() -> (bool, u32) {
    cell().read().map(|c| (c.animations, c.animation_ms)).unwrap_or((true, 140))
}

pub fn resize_step() -> i32 {
    cell().read().map(|c| c.resize_step as i32).unwrap_or(40)
}

/// Just the rules (cheaper than cloning the whole config on hot paths).
pub fn rules() -> Vec<Rule> {
    cell().read().map(|c| c.rules.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = parse(&default_config()).expect("default config must parse");
        assert_eq!(c.gap, 10);
        assert!(c.animations);
        assert_eq!(c.animation_ms, 140);
        assert_eq!(c.resize_step, 40);
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize { width: true, grow: true } && s == "alt+plus"
        }));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize { width: true, grow: false } && s == "alt+minus"
        }));
        // 16 direction bindings + 18 workspace bindings + 4 resize + pause x2 + quit
        assert!(c.bindings.len() >= 16 + 18 + 4 + 3);
        assert_eq!(c.rules.len(), 1);
        assert!(c.bindings.iter().any(|(a, s)| *a == Action::Workspace(0) && s == "alt+1"));
        assert!(c.bindings.iter().any(|(a, s)| *a == Action::MoveTo(8) && s == "alt+shift+9"));
    }


    /// What the first versions generated (no animations, no resize hotkeys).
    const OLD_CONFIG: &str = "# tile-rs configuration.\n\ngap = 10\n\n[hotkeys]\nfocus_left  = [\"alt+h\", \"alt+left\"]\npause = \"ctrl+alt+p\"\nquit = \"ctrl+alt+q\"\n\n[[rules]]\nprocess = \"Taskmgr\"\naction = \"float\"\n";

    #[test]
    fn migrate_adds_new_settings_to_an_old_config() {
        let new = migrate(OLD_CONFIG).expect("something must be added");
        let c = parse(&new).expect("migrated config must still parse");
        assert!(c.animations);
        assert_eq!(c.animation_ms, 140);
        assert_eq!(c.resize_step, 40);
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize { width: true, grow: true } && s == "alt+plus"
        }));
        assert!(c.bindings.iter().any(|(a, s)| {
            *a == Action::Resize { width: true, grow: false } && s == "alt+minus"
        }));
        // old content is untouched
        assert!(new.contains("focus_left  = [\"alt+h\", \"alt+left\"]"));
        assert!(new.contains("process = \"Taskmgr\""));
        assert_eq!(c.rules.len(), 1);
    }

    #[test]
    fn migrate_is_idempotent_and_respects_choices() {
        let once = migrate(OLD_CONFIG).unwrap();
        assert!(migrate(&once).is_none(), "second run must change nothing");
        // a key the user commented out or turned off is not re-added
        let custom = "gap = 5\nanimations = false\n# animation_ms = 99\nresize_step = 10\n\n[hotkeys]\n# grow_width = \"alt+plus\"\n# shrink_width = \"alt+minus\"\nquit = \"ctrl+alt+q\"\n";
        assert!(migrate(custom).is_none());
        // the freshly generated default never needs migrating
        assert!(migrate(&default_config()).is_none());
    }

    #[test]
    fn migrate_keeps_crlf_line_endings() {
        let crlf = OLD_CONFIG.replace('\n', "\r\n");
        let new = migrate(&crlf).unwrap();
        assert!(!new.replace("\r\n", "").contains('\n'), "only CRLF line breaks");
        assert!(parse(&new).is_ok());
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
        assert!(parse("[hotkeys]\nteleport = \"alt+t\"").unwrap_err().contains("teleport"));
        assert!(parse("[[rules]]\naction = \"float\"").is_err()); // no matcher
        assert!(parse("[[rules]]\nprocess = \"x\"\nworkspace = 12").is_err());
        assert!(parse("this is not toml").is_err());
    }
}
