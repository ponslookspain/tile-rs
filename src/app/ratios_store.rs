//! Split ratios survive a restart: they are saved per display and workspace next to the log and
//! read back on start. A display is identified by its device name, so the sizes follow it even
//! when Windows enumerates the monitors in another order.
use super::*;
use crate::layout::geometry::{MAX_RATIO, MIN_RATIO};
use std::path::PathBuf;

/// Quiet period after the last change before the file is written: a held resize key or a drag
/// changes the ratios many times a second.
const SAVE_DELAY_MS: u32 = 1500;
/// More splits than any workspace can hold; a longer entry in the file is damaged.
const MAX_SPLITS: usize = 64;

/// (display device, 0-based workspace) -> ratios.
pub(super) type Stored = HashMap<(String, u8), Vec<f32>>;

fn path() -> PathBuf {
    crate::diagnostics::log_file().with_file_name("ratios.txt")
}

/// One `device<TAB>workspace<TAB>ratios` line per entry. Entries at the default (all equal
/// halves) are left out. `offline` holds displays that are not connected right now.
fn encode(
    ids: &[Option<String>; MON],
    ratios: &[HashMap<u8, Vec<f32>>; MON],
    offline: &Stored,
) -> String {
    let mut entries: Stored = offline
        .iter()
        .filter(|((device, _), _)| !ids.iter().flatten().any(|id| id == device))
        .map(|(key, rs)| (key.clone(), rs.clone()))
        .collect();
    for (id, per_workspace) in ids.iter().zip(ratios) {
        let Some(device) = id else { continue };
        for (ws, rs) in per_workspace {
            entries.insert((device.clone(), *ws), rs.clone());
        }
    }
    let mut lines: Vec<String> = entries
        .into_iter()
        .filter(|(_, rs)| rs.iter().any(|r| (r - 0.5).abs() >= 0.0005))
        .map(|((device, ws), rs)| {
            let values: Vec<String> = rs.iter().map(|r| format!("{r:.4}")).collect();
            format!("{device}\t{ws}\t{}", values.join(" "))
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// Lines that do not parse are skipped; values are forced into the range the layouts allow.
fn decode(text: &str) -> Stored {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (device, ws, values) = (parts.next()?, parts.next()?, parts.next()?);
            if device.is_empty() || parts.next().is_some() {
                return None;
            }
            let ws: u8 = ws.parse().ok().filter(|ws| *ws < config::WS_COUNT)?;
            let ratios: Vec<f32> = values
                .split_whitespace()
                .take(MAX_SPLITS)
                .map(|v| {
                    v.parse::<f32>()
                        .ok()
                        .filter(|v| v.is_finite())
                        .map(|v| v.clamp(MIN_RATIO, MAX_RATIO))
                })
                .collect::<Option<_>>()?;
            (!ratios.is_empty()).then(|| ((device.to_string(), ws), ratios))
        })
        .collect()
}

impl State {
    /// A ratio was changed by the user: save the ratios shortly.
    pub(super) fn touch_ratios(&mut self) {
        self.ratios_dirty = true;
        self.save_timer = unsafe { SetTimer(None, self.save_timer, SAVE_DELAY_MS, None) };
    }

    /// Give every connected display the saved ratios of its device (for workspaces that have
    /// none yet).
    pub(super) fn adopt_offline_ratios(&mut self) {
        for mi in 0..MON {
            let Some(device) = self.monitor_ids[mi].clone() else {
                continue;
            };
            for ws in 0..config::WS_COUNT {
                if self.ratios[mi].contains_key(&ws) {
                    continue;
                }
                if let Some(rs) = self.ratios_offline.remove(&(device.clone(), ws)) {
                    self.ratios[mi].insert(ws, rs);
                }
            }
        }
    }
}

/// Read the saved ratios (start-up, after `remember_monitor_slots`).
pub(super) fn load() {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return;
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.ratios_offline = decode(&text);
        s.adopt_offline_ratios();
        s.ratios_saved = encode(&s.monitor_ids, &s.ratios, &s.ratios_offline);
    });
}

/// Write the ratios now if the user changed them since the last write.
pub(super) fn flush(s: &mut State) {
    if s.save_timer != 0 {
        unsafe {
            let _ = KillTimer(None, s.save_timer);
        }
        s.save_timer = 0;
    }
    if !std::mem::take(&mut s.ratios_dirty) {
        return;
    }
    let text = encode(&s.monitor_ids, &s.ratios, &s.ratios_offline);
    if text == s.ratios_saved {
        return;
    }
    let p = path();
    let result = if text.is_empty() {
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        config::save_atomic(&p, &text)
    };
    match result {
        Ok(()) => s.ratios_saved = text,
        Err(e) => {
            s.ratios_dirty = true; // try again with the next change or on exit
            log(format!("could not save the split ratios: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(first: &str) -> [Option<String>; MON] {
        let mut ids: [Option<String>; MON] = std::array::from_fn(|_| None);
        ids[0] = Some(first.to_string());
        ids
    }

    fn ratios(ws: u8, rs: &[f32]) -> [HashMap<u8, Vec<f32>>; MON] {
        let mut out: [HashMap<u8, Vec<f32>>; MON] = std::array::from_fn(|_| HashMap::new());
        out[0].insert(ws, rs.to_vec());
        out
    }

    #[test]
    fn ratios_round_trip_per_display_and_workspace() {
        let text = encode(
            &ids(r"\\.\DISPLAY1"),
            &ratios(2, &[0.3, 0.65]),
            &Stored::new(),
        );
        let back = decode(&text);
        let rs = &back[&(r"\\.\DISPLAY1".to_string(), 2)];
        assert_eq!(rs.len(), 2);
        assert!((rs[0] - 0.3).abs() < 0.001 && (rs[1] - 0.65).abs() < 0.001);
    }

    #[test]
    fn default_ratios_are_not_stored() {
        assert!(encode(&ids("A"), &ratios(0, &[0.5, 0.5]), &Stored::new()).is_empty());
        assert!(encode(&ids("A"), &ratios(0, &[]), &Stored::new()).is_empty());
    }

    #[test]
    fn disconnected_displays_keep_their_saved_ratios() {
        let mut offline = Stored::new();
        offline.insert(("B".to_string(), 0), vec![0.3]);
        // B is not connected: its entry stays in the file
        let text = encode(&ids("A"), &ratios(0, &[0.4]), &offline);
        assert_eq!(decode(&text).len(), 2);
        // B is connected now and its live value wins over the stale offline one
        let mut both = ids("A");
        both[1] = Some("B".to_string());
        let text = encode(&both, &ratios(0, &[0.4]), &offline);
        assert!(!decode(&text).contains_key(&("B".to_string(), 0)));
    }

    #[test]
    fn damaged_lines_are_skipped_and_values_clamped() {
        let text =
            "A\t0\t0.3 nan\nA\t99\t0.3\nA\t1\t0.01 0.99\nA\t2\t\n\nA\t3\t0.4\textra\nA\t4\t0.4";
        let back = decode(text);
        assert_eq!(back.len(), 2);
        assert_eq!(back[&("A".to_string(), 1)], vec![MIN_RATIO, MAX_RATIO]);
        assert_eq!(back[&("A".to_string(), 4)], vec![0.4]);
    }

    #[test]
    fn an_oversized_entry_is_cut_to_the_split_limit() {
        let values = vec!["0.4"; MAX_SPLITS + 10].join(" ");
        let back = decode(&format!("A\t0\t{values}"));
        assert_eq!(back[&("A".to_string(), 0)].len(), MAX_SPLITS);
    }
}
