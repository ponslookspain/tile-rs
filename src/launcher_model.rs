//! Logic behind the settings launcher that does not depend on the UI toolkit: the hotkey table
//! (rows, key capture, conflicts) and the application rules text. The Slint layer in `launcher.rs`
//! only binds these to widgets, so a redesign of the UI never touches the rules below.

use std::collections::HashMap;

use crate::config::{self, Action, Rule};

/// One line of the hotkey table: an action and every key combination bound to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The name used in `config.toml`, e.g. `focus_left`.
    pub action: String,
    pub label: String,
    pub group: String,
    pub keys: Vec<String>,
}

/// Every action a hotkey can trigger, in display order: (config name, label, group).
fn catalog() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut add = |name: &str, label: &str, group: &str| {
        out.push((name.into(), label.into(), group.into()));
    };
    for (dir, label) in [
        ("left", "left"),
        ("down", "down"),
        ("up", "up"),
        ("right", "right"),
    ] {
        add(
            &format!("focus_{dir}"),
            &format!("Focus the window {label}"),
            "Windows",
        );
    }
    for (dir, label) in [
        ("left", "left"),
        ("down", "down"),
        ("up", "up"),
        ("right", "right"),
    ] {
        add(
            &format!("swap_{dir}"),
            &format!("Swap with the window {label}"),
            "Windows",
        );
    }
    add("grow_width", "Wider", "Size");
    add("shrink_width", "Narrower", "Size");
    add("grow_height", "Taller", "Size");
    add("shrink_height", "Shorter", "Size");
    add("minimize", "Minimize the window", "Windows");
    add(
        "toggle_float",
        "Float the window / tile it again",
        "Windows",
    );
    add("close", "Close the window", "Windows");
    add(
        "toggle_monocle",
        "Full screen: one window on the whole screen",
        "Layout",
    );
    add("cycle_layout", "Next layout", "Layout");
    add(
        "master_count_inc",
        "More windows in the main column",
        "Layout",
    );
    add(
        "master_count_dec",
        "Fewer windows in the main column",
        "Layout",
    );
    add("retile", "Re-arrange the windows", "Layout");
    for n in 1..=config::WS_COUNT {
        add(
            &format!("workspace_{n}"),
            &format!("Workspace {n}"),
            "Workspaces",
        );
    }
    for n in 1..=config::WS_COUNT {
        add(
            &format!("move_to_workspace_{n}"),
            &format!("Window to workspace {n}"),
            "Workspaces",
        );
    }
    add("workspace_next", "Next workspace", "Workspaces");
    add("workspace_prev", "Previous workspace", "Workspaces");
    add(
        "move_to_next_workspace",
        "Window to the next workspace",
        "Workspaces",
    );
    add(
        "move_to_prev_workspace",
        "Window to the previous workspace",
        "Workspaces",
    );
    add(
        "workspace_back_and_forth",
        "Back to the last workspace",
        "Workspaces",
    );
    add("focus_last", "Back to the last window", "Windows");
    add("focus_next_monitor", "Focus the next screen", "Screens");
    add("focus_prev_monitor", "Focus the previous screen", "Screens");
    add(
        "move_to_next_monitor",
        "Window to the next screen",
        "Screens",
    );
    add(
        "move_to_prev_monitor",
        "Window to the previous screen",
        "Screens",
    );
    add("pause", "Pause tile-rs", "tile-rs");
    add("reload_config", "Reload the config", "tile-rs");
    add("edit_config", "Open the config file", "tile-rs");
    add("quit", "Quit tile-rs", "tile-rs");
    out
}

/// All catalog actions, with the keys from `bindings` filled in. Bindings of actions the catalog
/// does not know (there are none today) are appended so they are never silently dropped.
pub fn rows_from_bindings(bindings: &[(Action, String)]) -> Vec<Row> {
    let mut by_action: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (action, spec) in bindings {
        let name = crate::action_name(*action);
        if !by_action.contains_key(&name) {
            order.push(name.clone());
        }
        by_action.entry(name).or_default().push(spec.clone());
    }
    let mut rows: Vec<Row> = catalog()
        .into_iter()
        .map(|(action, label, group)| Row {
            keys: by_action.remove(&action).unwrap_or_default(),
            action,
            label,
            group,
        })
        .collect();
    for name in order {
        if let Some(keys) = by_action.remove(&name) {
            rows.push(Row {
                label: name.clone(),
                action: name,
                group: "Other".into(),
                keys,
            });
        }
    }
    rows
}

/// The rows of the built-in default configuration (for "reset hotkeys").
pub fn default_rows() -> Vec<Row> {
    let bindings = config::parse(&config::default_config())
        .map(|c| c.bindings)
        .unwrap_or_default();
    rows_from_bindings(&bindings)
}

/// Keys that Windows itself uses together with Win + Alt.
const WINDOWS_WIN_ALT_KEYS: [&str; 18] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "up", "down", "k", "l", "r", "g", "b", "d",
];

/// The default hotkeys on a different base modifier: 0 keeps Alt (as shipped), 1 moves every
/// plain Alt combination to Win + Alt, which applications almost never use. (Ctrl + Alt is not
/// offered: it is AltGr on many European layouts and would steal characters such as @ and |.)
/// A moved key that would land on a combination the defaults already use is dropped, unless it
/// is the action's only key, which then stays on Alt.
pub fn preset_rows(preset: i32) -> Vec<Row> {
    let mut rows = default_rows();
    if preset != 1 {
        return rows;
    }
    let taken: Vec<_> = rows
        .iter()
        .flat_map(|r| r.keys.iter())
        .filter_map(|k| config::parse_hotkey(k).ok())
        .collect();
    for row in &mut rows {
        let original = row.keys.clone();
        let moved: Vec<String> = original
            .iter()
            .filter_map(|key| {
                let lower = key.to_ascii_lowercase();
                let parts: Vec<&str> = lower.split('+').collect();
                let plain_alt = parts.contains(&"alt")
                    && !parts
                        .iter()
                        .any(|p| matches!(*p, "ctrl" | "control" | "win" | "super" | "meta"));
                if !plain_alt {
                    return Some(key.clone());
                }
                // Windows owns these Win + Alt keys (taskbar jump lists, snapping, Game Bar,
                // lock): moving there would only get the key refused, so it stays on Alt
                let last = parts.last().copied().unwrap_or_default();
                if WINDOWS_WIN_ALT_KEYS.contains(&last) {
                    return Some(key.clone());
                }
                let moved = format!("win+{lower}");
                let clashes = config::parse_hotkey(&moved).is_ok_and(|v| taken.contains(&v));
                (!clashes).then_some(moved)
            })
            .collect();
        row.keys = if moved.is_empty() { original } else { moved };
    }
    rows
}

/// `action = "keys"` lines for the `[hotkeys]` section; actions without keys are left out.
pub fn rows_to_lines(rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows.iter().filter(|r| !r.keys.is_empty()) {
        if row.keys.len() == 1 {
            out.push_str(&format!("{} = \"{}\"\n", row.action, row.keys[0]));
        } else {
            let values = row
                .keys
                .iter()
                .map(|k| format!("\"{k}\""))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("{} = [{values}]\n", row.action));
        }
    }
    out
}

/// For every row: whether one of its keys is also bound to a different row (such a combination
/// can be registered by Windows only once).
pub fn conflicts(rows: &[Row]) -> Vec<bool> {
    let mut owners: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        for key in &row.keys {
            if let Ok((mods, vk)) = config::parse_hotkey(key) {
                let owner = owners.entry((mods.0, vk)).or_default();
                if !owner.contains(&i) {
                    owner.push(i);
                }
            }
        }
    }
    let mut flags = vec![false; rows.len()];
    for owner in owners.values().filter(|o| o.len() > 1) {
        for &i in owner {
            flags[i] = true;
        }
    }
    flags
}

/// A combination as people read it: `alt+shift+left` -> `Alt + Shift + ←`.
pub fn display_spec(spec: &str) -> String {
    spec.split('+')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "alt" => "Alt".to_string(),
            "ctrl" | "control" => "Ctrl".to_string(),
            "shift" => "Shift".to_string(),
            "win" | "super" | "meta" => "Win".to_string(),
            "left" => "←".to_string(),
            "right" => "→".to_string(),
            "up" => "↑".to_string(),
            "down" => "↓".to_string(),
            "plus" => "+".to_string(),
            "minus" => "−".to_string(),
            "comma" => ",".to_string(),
            "period" => ".".to_string(),
            "semicolon" => ";".to_string(),
            "slash" => "/".to_string(),
            "grave" => "`".to_string(),
            "bracketleft" => "[".to_string(),
            "backslash" => "\\".to_string(),
            "bracketright" => "]".to_string(),
            "quote" => "'".to_string(),
            "numpad_add" => "Num +".to_string(),
            "numpad_subtract" => "Num −".to_string(),
            "esc" | "escape" => "Esc".to_string(),
            "enter" | "return" => "Enter".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// The key to show for an action. For directional actions an arrow key is preferred over
/// letters like H/J/K/L, which mean nothing to someone new to tiling.
fn first_key(rows: &[Row], action: &str) -> Option<String> {
    let keys = &rows.iter().find(|r| r.action == action)?.keys;
    let arrow = ["left", "down", "up", "right"]
        .into_iter()
        .find(|d| action.ends_with(&format!("_{d}")))
        .and_then(|d| {
            keys.iter()
                .find(|k| k.to_ascii_lowercase().ends_with(&format!("+{d}")))
        });
    arrow.or(keys.first()).map(|k| display_spec(k))
}

/// `Alt + H J K L` for four keys sharing their modifiers; `Alt + 1 … 9` when `range`.
fn compact(keys: &[String], range: bool) -> String {
    let parts: Vec<Vec<&str>> = keys.iter().map(|k| k.split(" + ").collect()).collect();
    let Some(first) = parts.first() else {
        return "Not set".into();
    };
    let prefix = &first[..first.len() - 1];
    if parts
        .iter()
        .any(|p| p.len() != first.len() || p[..p.len() - 1] != *prefix)
    {
        return keys.join(" / ");
    }
    let lasts: Vec<&str> = parts.iter().map(|p| p[p.len() - 1]).collect();
    let tail = if range && lasts.len() > 2 {
        format!("{} … {}", lasts[0], lasts[lasts.len() - 1])
    } else {
        lasts.join(" ")
    };
    if prefix.is_empty() {
        tail
    } else {
        format!("{} + {tail}", prefix.join(" + "))
    }
}

/// The welcome card's (keys, meaning) lines, built from the current hotkey table.
pub fn welcome_items(rows: &[Row]) -> Vec<(String, String)> {
    let dirs = ["left", "up", "down", "right"];
    let numbered = |prefix: &str| -> Vec<String> {
        (1..=config::WS_COUNT)
            .map(|n| format!("{prefix}{n}"))
            .collect()
    };
    let groups: Vec<(&str, Vec<String>, bool)> = vec![
        (
            "Move to the next window",
            dirs.iter().map(|d| format!("focus_{d}")).collect(),
            false,
        ),
        (
            "Swap two windows",
            dirs.iter().map(|d| format!("swap_{d}")).collect(),
            false,
        ),
        ("Open a workspace", numbered("workspace_"), true),
        (
            "Send the window to a workspace",
            numbered("move_to_workspace_"),
            true,
        ),
        ("Full screen and back", vec!["toggle_monocle".into()], false),
        ("Back to the last window", vec!["focus_last".into()], false),
        ("Change the layout", vec!["cycle_layout".into()], false),
    ];
    groups
        .into_iter()
        .map(|(what, actions, range)| {
            let keys: Vec<String> = actions.iter().filter_map(|a| first_key(rows, a)).collect();
            (compact(&keys, range), what.to_string())
        })
        .collect()
}

/// The first-run tour's three steps, with the keys from the hotkey table.
pub fn tour_texts(rows: &[Row]) -> [String; 3] {
    let items = welcome_items(rows);
    let keys = |i: usize| items.get(i).map(|(k, _)| k.clone()).unwrap_or_default();
    [
        format!("Move between windows: press {}", keys(0)),
        format!(
            "Swap two windows: drag a window onto another one (or press {})",
            keys(1)
        ),
        "Send a window to another workspace: drag it onto a dot at the top of the screen".into(),
    ]
}

/// Tour steps the manager reported as done, from its progress file (one step name per line).
pub fn tour_progress(text: &str) -> [bool; 3] {
    let seen = |name: &str| text.lines().any(|l| l.trim() == name);
    [seen("focus"), seen("swap"), seen("send")]
}

/// "Also Alt + . and Alt + ," for the master-column setting; empty when either key is unbound.
pub fn master_hint(rows: &[Row]) -> String {
    match (
        first_key(rows, "master_count_inc"),
        first_key(rows, "master_count_dec"),
    ) {
        (Some(inc), Some(dec)) => format!("Also {inc} and {dec}"),
        _ => String::new(),
    }
}

/// What a key press means while a hotkey is being recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Escape: stop recording, keep the old keys.
    Cancel,
    /// Backspace / Delete without modifiers: remove the keys of this action.
    Clear,
    /// A complete combination, in the `config.toml` notation.
    Spec(String),
    /// A modifier on its own (or something that is not a key): keep waiting.
    Wait,
    /// A key that cannot be used; the text says why.
    Reject(String),
}

/// The layout-independent character for a held virtual key: letters, digits and the punctuation
/// hotkeys can use. Slint's `text` follows the keyboard layout and Shift (Alt+Shift+1 arrives as
/// `!`, Alt+H as `р` on a Russian layout), so the physical key is preferred when one is held.
pub fn vk_char(vk: u16) -> Option<char> {
    match vk {
        0x30..=0x39 => Some(vk as u8 as char),
        0x41..=0x5a => Some((vk as u8).to_ascii_lowercase() as char),
        0xbb => Some('+'),
        0xbc => Some(','),
        0xbd => Some('-'),
        0xbe => Some('.'),
        0xba => Some(';'),
        0xbf => Some('/'),
        0xc0 => Some('`'),
        0xdb => Some('['),
        0xdc => Some('\\'),
        0xdd => Some(']'),
        0xde => Some('\''),
        _ => None,
    }
}

/// The text to record for a key event: the physically held key (`held`, from `vk_char`) when the
/// event is an ordinary character, so the result does not depend on the layout or on Shift.
/// Arrows, F-keys and Escape keep Slint's text, as does a bare Backspace/Delete (it clears the
/// keys); Ctrl+letter arrives as a control code and is resolved through the held key too.
pub fn resolve_text(text: &str, held: &[char], any_modifier: bool) -> String {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return text.to_string();
    };
    let special = matches!(c, '\u{1b}' | '\u{10}'..='\u{18}' | '\u{f700}'..='\u{f8ff}')
        || (matches!(c, '\u{8}' | '\u{7f}') && !any_modifier);
    if special || held.is_empty() {
        return text.to_string();
    }
    // several keys down (a rolled key press): the one matching the typed character wins
    let lower = c.to_lowercase().next().unwrap_or(c);
    let pick = held.iter().find(|h| **h == lower).unwrap_or(&held[0]);
    pick.to_string()
}

/// Translates a Slint key event (its `text` and the modifier flags) into a hotkey.
pub fn capture_key(text: &str, alt: bool, ctrl: bool, shift: bool, meta: bool) -> Capture {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Capture::Wait;
    };
    let any_modifier = alt || ctrl || shift || meta;
    let key: String = match c {
        // modifier keys on their own (Slint's private key codes)
        '\u{10}'..='\u{18}' => return Capture::Wait,
        '\u{1b}' => return Capture::Cancel,
        '\u{8}' | '\u{7f}' if !any_modifier => return Capture::Clear,
        '\u{8}' | '\u{7f}' => "backspace".into(),
        '\u{f700}' => "up".into(),
        '\u{f701}' => "down".into(),
        '\u{f702}' => "left".into(),
        '\u{f703}' => "right".into(),
        '\u{f704}'..='\u{f70f}' => format!("f{}", c as u32 - 0xf704 + 1),
        '\n' | '\r' => "enter".into(),
        '\t' => "tab".into(),
        ' ' => "space".into(),
        '+' => "plus".into(),
        '-' => "minus".into(),
        ',' => "comma".into(),
        '.' => "period".into(),
        ';' => "semicolon".into(),
        '/' => "slash".into(),
        '`' => "grave".into(),
        '[' => "bracketleft".into(),
        '\\' => "backslash".into(),
        ']' => "bracketright".into(),
        '\'' => "quote".into(),
        // Ctrl+letter arrives as a control code
        '\u{1}'..='\u{1a}' => ((b'a' + (c as u8 - 1)) as char).to_string(),
        c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        _ => return Capture::Reject("This key cannot be used in a combination".into()),
    };
    let function_key = key.len() >= 2 && key.starts_with('f') && key[1..].parse::<u32>().is_ok();
    if !any_modifier && !function_key {
        return Capture::Reject("Add Alt, Ctrl, Shift or Win to the key".into());
    }
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if meta {
        parts.push("win");
    }
    parts.push(&key);
    let spec = parts.join("+");
    match config::parse_hotkey(&spec) {
        Ok(_) => Capture::Spec(spec),
        Err(_) => Capture::Reject(format!("The combination {spec} is not supported")),
    }
}

/// A short human description of a rule, for the list in the launcher.
pub fn rule_label(rule: &Rule) -> String {
    let mut who: Vec<String> = Vec::new();
    if let Some(p) = &rule.process {
        who.push(format!("{p}.exe"));
    }
    if let Some(c) = &rule.class {
        who.push(format!("windows of class {c}"));
    }
    if let Some(t) = &rule.title {
        who.push(format!("windows titled \"{t}\""));
    }
    let who = if who.is_empty() {
        "Any window".to_string()
    } else {
        who.join(", ")
    };
    let mut what: Vec<String> = Vec::new();
    match rule.action.as_deref() {
        Some("float") => what.push("always floating".into()),
        Some("tile") => what.push("always tiled".into()),
        Some("ignore") => what.push("left alone".into()),
        Some(other) => what.push(other.to_string()),
        None => {}
    }
    if let Some(ws) = rule.workspace {
        what.push(format!("always on workspace {ws}"));
    }
    if what.is_empty() {
        what.push("no action".into());
    }
    format!("{who}  ·  {}", what.join(", "))
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `[[rules]]` tables for `rules`.
pub fn rules_to_toml(rules: &[Rule]) -> String {
    let mut out = String::new();
    for rule in rules {
        out.push_str("[[rules]]\n");
        if let Some(v) = &rule.process {
            out.push_str(&format!("process = {}\n", quoted(v)));
        }
        if let Some(v) = &rule.class {
            out.push_str(&format!("class = {}\n", quoted(v)));
        }
        if let Some(v) = &rule.title {
            out.push_str(&format!("title = {}\n", quoted(v)));
        }
        if let Some(v) = &rule.action {
            out.push_str(&format!("action = {}\n", quoted(v)));
        }
        if let Some(v) = rule.workspace {
            out.push_str(&format!("workspace = {v}\n"));
        }
        out.push('\n');
    }
    out
}

/// Replaces every `[[rules]]` table in `text` with `rules`, leaving comments and everything else
/// untouched. The new tables go to the end of the file.
pub fn edit_rules(text: &str, rules: &[Rule]) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        let t = line.trim();
        if t == "[[rules]]" {
            skipping = true;
            continue;
        }
        if skipping {
            // a table body is a run of `key = value` lines
            if !t.is_empty() && !t.starts_with('#') && !t.starts_with('[') && t.contains('=') {
                continue;
            }
            skipping = false;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join(nl);
    out.push_str(nl);
    let block = rules_to_toml(rules);
    if !block.is_empty() {
        out.push_str(nl);
        out.push_str(block.replace('\n', nl).trim_end());
        out.push_str(nl);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(process: &str, action: &str) -> Rule {
        Rule {
            process: Some(process.into()),
            action: Some(action.into()),
            ..Rule::default()
        }
    }

    #[test]
    fn default_rows_cover_the_catalog_and_every_default_key() {
        let rows = default_rows();
        let bound = config::parse(&config::default_config())
            .unwrap()
            .bindings
            .len();
        assert_eq!(rows.iter().map(|r| r.keys.len()).sum::<usize>(), bound);
        assert!(rows.iter().any(|r| r.action == "toggle_monocle"));
        // a row survives a round trip through the config text
        let text = format!("[hotkeys]\n{}", rows_to_lines(&rows));
        let back = config::parse(&text).expect("generated hotkeys parse");
        assert_eq!(back.bindings.len(), bound);
        assert!(
            conflicts(&rows).iter().all(|c| !c),
            "defaults must not clash"
        );
    }

    #[test]
    fn combinations_are_shown_readably() {
        assert_eq!(display_spec("alt+shift+left"), "Alt + Shift + ←");
        assert_eq!(display_spec("ctrl+alt+l"), "Ctrl + Alt + L");
        assert_eq!(display_spec("alt+numpad_add"), "Alt + Num +");
        assert_eq!(display_spec("f5"), "F5");
    }

    #[test]
    fn conflicts_flag_both_rows_even_with_different_spelling() {
        let rows = vec![
            Row {
                action: "close".into(),
                label: String::new(),
                group: String::new(),
                keys: vec!["Alt+Shift+Q".into()],
            },
            Row {
                action: "quit".into(),
                label: String::new(),
                group: String::new(),
                keys: vec!["shift+alt+q".into()],
            },
            Row {
                action: "pause".into(),
                label: String::new(),
                group: String::new(),
                keys: vec!["ctrl+alt+p".into()],
            },
        ];
        assert_eq!(conflicts(&rows), vec![true, true, false]);
    }

    #[test]
    fn key_capture_builds_config_notation() {
        assert_eq!(
            capture_key("h", true, false, false, false),
            Capture::Spec("alt+h".into())
        );
        // Shift+letter arrives as an upper-case letter
        assert_eq!(
            capture_key("H", true, false, true, false),
            Capture::Spec("alt+shift+h".into())
        );
        // Ctrl+Alt+L: a control code for the letter
        assert_eq!(
            capture_key("\u{c}", true, true, false, false),
            Capture::Spec("ctrl+alt+l".into())
        );
        assert_eq!(
            capture_key("\u{f702}", true, false, false, false),
            Capture::Spec("alt+left".into())
        );
        assert_eq!(
            capture_key(".", true, false, true, false),
            Capture::Spec("alt+shift+period".into())
        );
        assert_eq!(
            capture_key("\u{f704}", false, false, false, false),
            Capture::Spec("f1".into())
        );
        assert_eq!(
            capture_key("\u{1b}", false, false, false, false),
            Capture::Cancel
        );
        assert_eq!(
            capture_key("\u{8}", false, false, false, false),
            Capture::Clear
        );
        assert_eq!(
            capture_key("\u{12}", true, false, false, false),
            Capture::Wait
        );
        assert_eq!(capture_key("", true, false, false, false), Capture::Wait);
        assert!(matches!(
            capture_key("h", false, false, false, false),
            Capture::Reject(_)
        ));
        assert!(matches!(
            capture_key("!", true, false, true, false),
            Capture::Reject(_)
        ));
    }

    #[test]
    fn win_alt_preset_moves_keys_without_clashes() {
        let rows = preset_rows(1);
        assert!(conflicts(&rows).iter().all(|c| !c), "preset must not clash");
        let focus_left = rows.iter().find(|r| r.action == "focus_left").unwrap();
        assert!(focus_left.keys.contains(&"win+alt+left".to_string()));
        // nothing lands on a Win + Alt key that Windows owns (Win + Alt + 1 opens a jump list)
        for key in rows.iter().flat_map(|r| r.keys.iter()) {
            let parts: Vec<&str> = key.split('+').collect();
            let windows_owned = parts.len() == 3
                && parts[0] == "win"
                && parts[1] == "alt"
                && WINDOWS_WIN_ALT_KEYS.contains(&parts[2]);
            assert!(!windows_owned, "{key}");
        }
        let workspace_1 = rows.iter().find(|r| r.action == "workspace_1").unwrap();
        assert_eq!(workspace_1.keys, vec!["alt+1".to_string()]);
        // no action that had keys loses all of them
        for (before, after) in default_rows().iter().zip(&rows) {
            assert!(
                before.keys.is_empty() || !after.keys.is_empty(),
                "{}",
                before.action
            );
        }
        assert!(rows
            .iter()
            .flat_map(|r| r.keys.iter())
            .all(|k| !k.starts_with("ctrl+alt+")
                || default_rows().iter().any(|r| r.keys.contains(k))));
        // the round trip through the config text keeps every key
        let text = format!("[hotkeys]\n{}", rows_to_lines(&rows));
        assert!(config::parse(&text).is_ok());
        assert_eq!(preset_rows(0), default_rows());
    }

    #[test]
    fn held_keys_only_replace_ordinary_characters() {
        // Shift+1 on any layout, Alt+H on a Russian layout, Ctrl+H as a control code
        assert_eq!(resolve_text("!", &['1'], true), "1");
        assert_eq!(resolve_text("р", &['h'], true), "h");
        assert_eq!(resolve_text("\u{8}", &['h'], true), "h");
        // a letter still held does not hijack arrows, F-keys, Escape or a bare Backspace
        assert_eq!(resolve_text("\u{f702}", &['a'], true), "\u{f702}");
        assert_eq!(resolve_text("\u{f704}", &['a'], false), "\u{f704}");
        assert_eq!(resolve_text("\u{1b}", &['a'], false), "\u{1b}");
        assert_eq!(resolve_text("\u{8}", &['a'], false), "\u{8}");
        // rolled keys: the one matching the typed character
        assert_eq!(resolve_text("K", &['j', 'k'], true), "k");
        // OEM keys are named the way the config parser expects
        let semi = resolve_text("ж", &[vk_char(0xba).unwrap()], true);
        assert_eq!(
            capture_key(&semi, true, false, false, false),
            Capture::Spec("alt+semicolon".into())
        );
        assert!(config::parse_hotkey("alt+bracketleft").is_ok());
    }

    #[test]
    fn hints_follow_the_hotkey_table() {
        let mut rows = default_rows();
        let items = welcome_items(&rows);
        assert_eq!(items[0].0, "Alt + ← ↑ ↓ →");
        assert_eq!(items[1].0, "Alt + Shift + ← ↑ ↓ →");
        assert_eq!(items[2].0, "Alt + 1 … 9");
        assert_eq!(master_hint(&rows), "Also Alt + . and Alt + ,");
        rows.iter_mut()
            .find(|r| r.action == "master_count_inc")
            .unwrap()
            .keys
            .clear();
        assert_eq!(master_hint(&rows), "");
        rows.iter_mut()
            .find(|r| r.action == "toggle_monocle")
            .unwrap()
            .keys = vec!["ctrl+f".into()];
        assert_eq!(welcome_items(&rows)[4].0, "Ctrl + F");
    }

    #[test]
    fn physical_keys_ignore_layout_and_shift() {
        assert_eq!(vk_char(0x31), Some('1'));
        assert_eq!(vk_char(0x48), Some('h'));
        assert_eq!(vk_char(0xbe), Some('.'));
        assert_eq!(vk_char(0x25), None); // arrows come from the Slint text
        let one = vk_char(0x31).unwrap().to_string();
        assert_eq!(
            capture_key(&one, true, false, true, false),
            Capture::Spec("alt+shift+1".into())
        );
    }

    #[test]
    fn rules_replace_only_the_rules_tables() {
        let text = "gap = 5\n# keep me\n\n[hotkeys]\nquit = \"ctrl+alt+q\"\n\n# a comment\n[[rules]]\nprocess = \"Taskmgr\"\naction = \"float\"\n\n[[rules]]\ntitle = \"Settings\"\naction = \"float\"\n";
        let rules = vec![
            rule("Telegram", "float"),
            Rule {
                process: Some("Chrome".into()),
                workspace: Some(2),
                ..Rule::default()
            },
        ];
        let out = edit_rules(text, &rules);
        assert!(out.contains("# keep me") && out.contains("# a comment"));
        assert!(out.contains("quit = \"ctrl+alt+q\""));
        assert!(!out.contains("Taskmgr") && !out.contains("Settings"));
        let parsed = config::parse(&out).expect("edited config parses");
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.rules[1].workspace, Some(2));
        // removing every rule leaves a valid file without tables
        let none = edit_rules(&out, &[]);
        assert!(!none.contains("[[rules]]"));
        assert_eq!(config::parse(&none).unwrap().rules.len(), 0);
        // idempotent
        assert_eq!(edit_rules(&out, &rules), out);
    }

    #[test]
    fn rule_text_is_escaped_and_described() {
        let mut r = rule("a\"b", "float");
        r.title = Some("C:\\x".into());
        let toml = rules_to_toml(&[r.clone()]);
        let parsed = config::parse(&toml).expect("escaped rule parses");
        assert_eq!(parsed.rules[0].process.as_deref(), Some("a\"b"));
        assert_eq!(parsed.rules[0].title.as_deref(), Some("C:\\x"));
        assert!(rule_label(&rule("Telegram", "float")).contains("Telegram"));
    }
}
