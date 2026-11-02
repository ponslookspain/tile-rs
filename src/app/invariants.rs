//! Consistency checks of the layout bookkeeping, run after every full re-tile.
//!
//! The per-window maps (`ws`, `mon`, `order`, `last`, `rects`) are updated in many places. A slip
//! in one of them shows up much later as a window that is "stuck" or tiled twice, so the sums are
//! checked where they are rebuilt, and the cheap cases are repaired on the spot.

use super::*;

/// Minimum time between two self-heal re-tiles, so a check that keeps failing cannot spin.
const HEAL_INTERVAL: Duration = Duration::from_secs(10);

thread_local! {
    static LAST_HEAL: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn has_duplicates(windows: &[isize]) -> bool {
    let mut seen = HashSet::with_capacity(windows.len());
    windows.iter().any(|h| !seen.insert(*h))
}

/// Every broken invariant of the current state, as short messages (empty when it is consistent).
pub(super) fn violations(s: &State) -> Vec<String> {
    let mut found = Vec::new();
    for mi in 0..MON {
        if has_duplicates(&s.order[mi]) {
            found.push(format!("monitor {mi}: a window appears twice in the order"));
        }
        for h in &s.order[mi] {
            if !s.ws.contains_key(h) || s.mon.get(h) != Some(&mi) {
                found.push(format!(
                    "monitor {mi}: ordered window {h:#x} is not assigned here"
                ));
            }
        }
        if s.last[mi].len() != s.rects[mi].len() {
            found.push(format!(
                "monitor {mi}: {} laid out windows but {} tiles",
                s.last[mi].len(),
                s.rects[mi].len()
            ));
        }
        if has_duplicates(&s.last[mi]) {
            found.push(format!("monitor {mi}: a window is laid out twice"));
        }
        for h in &s.last[mi] {
            if s.ws.get(h) != Some(&s.cur[mi]) || s.mon.get(h) != Some(&mi) {
                found.push(format!(
                    "monitor {mi}: laid out window {h:#x} does not belong to workspace {}",
                    s.cur[mi] + 1
                ));
            }
        }
    }
    found
}

/// Repairs what can be repaired in place, logs the rest and asks for one re-tile (at most once per
/// `HEAL_INTERVAL`). Debug builds stop instead: a violation there is a bug to fix, not to hide.
pub(super) fn check(s: &mut State) {
    for order in s.order.iter_mut() {
        if has_duplicates(order) {
            let mut seen = HashSet::new();
            order.retain(|h| seen.insert(*h));
        }
    }
    let found = violations(s);
    if found.is_empty() {
        return;
    }
    debug_assert!(false, "layout state is inconsistent: {found:?}");
    for message in found.iter().take(5) {
        log(format!("layout state check failed: {message}"));
    }
    let now = Instant::now();
    if LAST_HEAL
        .with(Cell::get)
        .is_some_and(|at| now.duration_since(at) < HEAL_INTERVAL)
    {
        return;
    }
    LAST_HEAL.with(|at| at.set(Some(now)));
    s.reflow = true;
    s.timer = unsafe { SetTimer(None, s.timer, DEBOUNCE_MS, None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_one_window() -> State {
        let mut s = State::default();
        s.ws.insert(1, 0);
        s.mon.insert(1, 0);
        s.order[0].push(1);
        s.last[0].push(1);
        s.rects[0].push(RECT::default());
        s
    }

    #[test]
    fn a_consistent_state_has_no_violations() {
        assert!(violations(&State::default()).is_empty());
        assert!(violations(&state_with_one_window()).is_empty());
    }

    #[test]
    fn broken_bookkeeping_is_reported() {
        let mut s = state_with_one_window();
        s.rects[0].clear();
        assert_eq!(violations(&s).len(), 1, "tile count");

        let mut s = state_with_one_window();
        s.mon.insert(1, 1);
        assert_eq!(
            violations(&s).len(),
            2,
            "wrong monitor: ordered and laid out"
        );

        let mut s = state_with_one_window();
        s.cur[0] = 3;
        assert_eq!(violations(&s).len(), 1, "laid out on another workspace");

        let mut s = state_with_one_window();
        s.ws.remove(&1);
        assert_eq!(
            violations(&s).len(),
            2,
            "forgotten window still ordered and laid out"
        );
    }

    #[test]
    fn duplicates_in_the_order_are_repaired_in_place() {
        let mut s = state_with_one_window();
        s.order[0].push(1);
        assert!(!violations(&s).is_empty());
        check(&mut s);
        assert_eq!(s.order[0], vec![1]);
        assert!(violations(&s).is_empty());
    }
}
