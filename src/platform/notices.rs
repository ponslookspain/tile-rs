//! Messages for the user that must not pop up a window. The manager writes them to a small file;
//! the settings window (a separate process) shows them in a banner until they are dismissed.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Older notices are dropped beyond this many.
const MAX_NOTICES: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub text: String,
}

fn path() -> PathBuf {
    crate::diagnostics::log_file().with_file_name("notices.txt")
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(['\r', '\t'], " ")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn encode(notices: &[Notice]) -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    notices
        .iter()
        .map(|n| format!("{stamp}\t{}\t{}\n", escape(&n.title), escape(&n.text)))
        .collect()
}

fn decode(text: &str) -> Vec<Notice> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            parts.next()?;
            Some(Notice {
                title: unescape(parts.next()?),
                text: unescape(parts.next()?),
            })
        })
        .collect()
}

/// Notices waiting to be read, oldest first.
pub fn load() -> Vec<Notice> {
    std::fs::read_to_string(path())
        .map(|text| decode(&text))
        .unwrap_or_default()
}

/// Adds a notice. The same message again replaces its earlier copy instead of piling up.
pub fn push(title: &str, text: &str) {
    crate::diagnostics::log(format!("notice: {title}: {text}"));
    let mut notices = load();
    let new = Notice {
        title: title.to_string(),
        text: text.to_string(),
    };
    notices.retain(|n| *n != new);
    notices.push(new);
    let excess = notices.len().saturating_sub(MAX_NOTICES);
    notices.drain(..excess);
    let _ = std::fs::write(path(), encode(&notices));
}

pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_round_trip_with_awkward_text() {
        let notices = vec![
            Notice {
                title: "tile-rs: hotkeys".into(),
                text: "focus_last = alt+z\nswap = a\\b\tc".into(),
            },
            Notice {
                title: "x".into(),
                text: String::new(),
            },
        ];
        let decoded = decode(&encode(&notices));
        assert_eq!(decoded[0].title, "tile-rs: hotkeys");
        assert_eq!(decoded[0].text, "focus_last = alt+z\nswap = a\\b c");
        assert_eq!(decoded[1], notices[1]);
    }

    #[test]
    fn a_garbled_line_is_skipped() {
        assert!(decode("no tabs here\n").is_empty());
    }
}
