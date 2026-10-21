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
        ("left", "влево"),
        ("down", "вниз"),
        ("up", "вверх"),
        ("right", "вправо"),
    ] {
        add(&format!("focus_{dir}"), &format!("Фокус {label}"), "Окна");
    }
    for (dir, label) in [
        ("left", "влево"),
        ("down", "вниз"),
        ("up", "вверх"),
        ("right", "вправо"),
    ] {
        add(
            &format!("swap_{dir}"),
            &format!("Поменять окно {label}"),
            "Окна",
        );
    }
    add("grow_width", "Шире", "Размер");
    add("shrink_width", "Уже", "Размер");
    add("grow_height", "Выше", "Размер");
    add("shrink_height", "Ниже", "Размер");
    add("minimize", "Свернуть окно", "Окна");
    add("toggle_float", "Плавающее / в раскладке", "Окна");
    add("close", "Закрыть окно", "Окна");
    add("toggle_monocle", "Monocle: окно на весь экран", "Раскладка");
    add("cycle_layout", "Следующая раскладка", "Раскладка");
    add("master_count_inc", "Больше окон в master", "Раскладка");
    add("master_count_dec", "Меньше окон в master", "Раскладка");
    add("retile", "Перестроить окна", "Раскладка");
    for n in 1..=config::WS_COUNT {
        add(
            &format!("workspace_{n}"),
            &format!("Рабочий стол {n}"),
            "Рабочие столы",
        );
    }
    for n in 1..=config::WS_COUNT {
        add(
            &format!("move_to_workspace_{n}"),
            &format!("Окно на рабочий стол {n}"),
            "Рабочие столы",
        );
    }
    add("workspace_next", "Следующий рабочий стол", "Рабочие столы");
    add("workspace_prev", "Предыдущий рабочий стол", "Рабочие столы");
    add(
        "move_to_next_workspace",
        "Окно на следующий стол",
        "Рабочие столы",
    );
    add(
        "move_to_prev_workspace",
        "Окно на предыдущий стол",
        "Рабочие столы",
    );
    add(
        "workspace_back_and_forth",
        "Вернуться на прошлый стол",
        "Рабочие столы",
    );
    add("focus_last", "Прошлое окно", "Окна");
    add(
        "focus_next_monitor",
        "Фокус на следующий монитор",
        "Мониторы",
    );
    add("focus_prev_monitor", "Фокус на прошлый монитор", "Мониторы");
    add(
        "move_to_next_monitor",
        "Окно на следующий монитор",
        "Мониторы",
    );
    add(
        "move_to_prev_monitor",
        "Окно на прошлый монитор",
        "Мониторы",
    );
    add("pause", "Пауза tile-rs", "Программа");
    add("reload_config", "Перечитать конфиг", "Программа");
    add("edit_config", "Открыть конфиг", "Программа");
    add("quit", "Выйти из tile-rs", "Программа");
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
                group: "Прочее".into(),
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
        // Ctrl+letter arrives as a control code
        '\u{1}'..='\u{1a}' => ((b'a' + (c as u8 - 1)) as char).to_string(),
        c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        _ => return Capture::Reject("Эту клавишу нельзя использовать в сочетании".into()),
    };
    let function_key = key.len() >= 2 && key.starts_with('f') && key[1..].parse::<u32>().is_ok();
    if !any_modifier && !function_key {
        return Capture::Reject("Добавьте Alt, Ctrl, Shift или Win к клавише".into());
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
        Err(_) => Capture::Reject(format!("Сочетание «{spec}» не поддерживается")),
    }
}

/// A short human description of a rule, for the list in the launcher.
pub fn rule_label(rule: &Rule) -> String {
    let mut who: Vec<String> = Vec::new();
    if let Some(p) = &rule.process {
        who.push(format!("программа {p}"));
    }
    if let Some(c) = &rule.class {
        who.push(format!("класс {c}"));
    }
    if let Some(t) = &rule.title {
        who.push(format!("заголовок «{t}»"));
    }
    let who = if who.is_empty() {
        "любое окно".to_string()
    } else {
        who.join(", ")
    };
    let mut what: Vec<String> = Vec::new();
    match rule.action.as_deref() {
        Some("float") => what.push("не трогать (плавающее)".into()),
        Some("tile") => what.push("всегда в раскладке".into()),
        Some("ignore") => what.push("игнорировать".into()),
        Some(other) => what.push(other.to_string()),
        None => {}
    }
    if let Some(ws) = rule.workspace {
        what.push(format!("открывать на столе {ws}"));
    }
    if what.is_empty() {
        what.push("без действия".into());
    }
    format!("{who} → {}", what.join(", "))
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
