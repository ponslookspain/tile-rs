//! Windows of inactive workspaces are moved off-screen ("parked"). The list of parked windows and
//! their original rectangles is persisted so a crash cannot leave them stranded: the next start
//! (and the panic hook) puts them back.
use std::collections::HashMap;
use std::path::PathBuf;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsIconic, IsWindow, SetWindowPos, SET_WINDOW_POS_FLAGS, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
};

/// Parking spot. Deliberately not (-32000, -32000): Windows uses that spot for minimized windows,
/// so a minimized window must never be mistaken for a parked one.
pub const PARK_X: i32 = -24000;
pub const PARK_Y: i32 = -24000;
/// The old parking spot, still recognised so windows parked by an older build are recovered.
const LEGACY_PARK_X: i32 = -32000;

pub type Saved = HashMap<isize, RECT>;

fn path() -> PathBuf {
    crate::diagnostics::log_file().with_file_name("parked.txt")
}

/// True when the window sits at a parking spot. Minimized windows report -32000 too, so they are
/// excluded explicitly.
pub fn at_park_spot(hwnd: HWND) -> bool {
    let mut r = RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut r).is_err() || IsIconic(hwnd).as_bool() {
            return false;
        }
    }
    is_park_x(r.left)
}

fn is_park_x(left: i32) -> bool {
    (PARK_X..PARK_X + 1000).contains(&left) || (LEGACY_PARK_X..LEGACY_PARK_X + 1000).contains(&left)
}

fn encode(saved: &Saved) -> String {
    let mut lines: Vec<String> = saved
        .iter()
        .map(|(h, r)| format!("{h} {} {} {} {}", r.left, r.top, r.right, r.bottom))
        .collect();
    lines.sort();
    lines.join("\n")
}

fn decode(text: &str) -> Saved {
    text.lines()
        .filter_map(|line| {
            let v: Vec<i64> = line
                .split_whitespace()
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            let [h, l, t, r, b] = v[..] else { return None };
            Some((
                h as isize,
                RECT {
                    left: l as i32,
                    top: t as i32,
                    right: r as i32,
                    bottom: b as i32,
                },
            ))
        })
        .collect()
}

pub fn save(saved: &Saved) {
    let p = path();
    if saved.is_empty() {
        let _ = std::fs::remove_file(p);
    } else {
        let _ = crate::config::save_atomic(&p, &encode(saved));
    }
}

pub fn load() -> Saved {
    std::fs::read_to_string(path())
        .map(|t| decode(&t))
        .unwrap_or_default()
}

/// Put a parked window back: at its saved position when that is still on a monitor, otherwise
/// near the top-left corner of the primary area.
fn restore_flags(synchronous: bool) -> SET_WINDOW_POS_FLAGS {
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
    if synchronous {
        flags
    } else {
        flags | SWP_ASYNCWINDOWPOS
    }
}

fn restore_one(h: isize, saved: Option<RECT>, synchronous: bool) -> windows::core::Result<bool> {
    let hwnd = HWND(h as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || !at_park_spot(hwnd) {
            return Ok(false);
        }
        let (x, y) = match saved {
            Some(r) if !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() => (r.left, r.top),
            _ => (100, 100),
        };
        SetWindowPos(hwnd, None, x, y, 0, 0, restore_flags(synchronous))?;
    }
    Ok(true)
}

/// Restore every window recorded in the persisted list that is still parked, then forget the
/// list. Touches no application state, so it is safe from a panic hook. Returns how many windows
/// were brought back.
pub fn restore_persisted() -> usize {
    restore_saved(&load(), false)
}

/// Session shutdown must finish placing windows before other applications exit.
pub fn restore_saved_sync(saved: &Saved) -> usize {
    restore_saved(saved, true)
}

pub fn restore_persisted_sync() -> usize {
    restore_saved_sync(&load())
}

fn restore_saved(saved: &Saved, synchronous: bool) -> usize {
    let mut restored = 0;
    let mut failed = Saved::new();
    for (h, r) in saved {
        match restore_one(*h, Some(*r), synchronous) {
            Ok(true) => restored += 1,
            Ok(false) => {}
            Err(error) => {
                crate::diagnostics::log(format!("could not restore parked window {h:#x}: {error}"));
                failed.insert(*h, *r);
            }
        }
    }
    if !saved.is_empty() {
        save(&failed);
    }
    restored
}

/// Restore a parked window that is not in the persisted list (parked by an older build).
pub fn restore_unlisted(h: isize) -> bool {
    restore_one(h, None, false).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_restore_is_synchronous_but_recovery_stays_asynchronous() {
        assert!(!restore_flags(true).contains(SWP_ASYNCWINDOWPOS));
        assert!(restore_flags(false).contains(SWP_ASYNCWINDOWPOS));
        assert!(restore_flags(true).contains(SWP_NOACTIVATE | SWP_NOZORDER));
    }

    #[test]
    fn minimized_spot_is_not_a_park_spot_but_legacy_park_is_recovered() {
        assert!(is_park_x(PARK_X));
        assert!(is_park_x(LEGACY_PARK_X));
        assert!(!is_park_x(-3000));
        assert!(!is_park_x(0));
        assert_ne!(PARK_X, LEGACY_PARK_X);
    }

    #[test]
    fn persisted_list_round_trips_and_skips_garbage() {
        let mut saved = Saved::new();
        saved.insert(
            0x1234,
            RECT {
                left: -10,
                top: 5,
                right: 700,
                bottom: 600,
            },
        );
        let mut text = encode(&saved);
        text.push_str("\nnot a record\n1 2 3");
        let back = decode(&text);
        assert_eq!(back.len(), 1);
        let r = back[&0x1234];
        assert_eq!((r.left, r.top, r.right, r.bottom), (-10, 5, 700, 600));
    }
}
