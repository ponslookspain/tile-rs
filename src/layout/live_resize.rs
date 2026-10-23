//! Latest-target scheduling for real window resizing. No pixels or application IPC.
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;

const RETRY_AFTER: Duration = Duration::from_millis(90);
/// How often a window away from the dragged separator is resized during a gesture. Every resize
/// makes the application lay out and repaint its content, and these windows only change in
/// proportion: about 30 updates a second look the same and cost a fraction.
const FAR_INTERVAL: Duration = Duration::from_millis(33);
/// How long the windows beside the separator wait for a partner that has not applied its last
/// size: two frames keep the edge together for responsive apps, while a slow one (a browser
/// re-laying out a page) cannot hold the others back for long.
const NEAR_WAIT: Duration = Duration::from_millis(34);

/// How a queued window is scheduled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Class {
    /// Updated as soon as it has applied its previous size.
    #[default]
    Free,
    /// Beside the dragged separator: all of these move together, so the separator never pulls
    /// apart into a gap or an overlap while one of them is still busy.
    Near,
    /// Away from the separator: updated at most every `FAR_INTERVAL`.
    Far,
}

#[derive(Default)]
pub struct Scheduler {
    pending: HashMap<isize, RECT>,
    sent: HashMap<isize, (RECT, RECT, Instant)>,
    class: HashMap<isize, Class>,
    far_sent: HashMap<isize, Instant>,
}

impl Scheduler {
    pub fn queue(&mut self, targets: &[(isize, RECT)]) {
        for (h, target) in targets {
            self.pending.insert(*h, *target);
            self.class.insert(*h, Class::Free);
        }
    }

    /// Queues one frame of a separator drag (see `Class`).
    pub fn queue_frame(&mut self, targets: &[(isize, RECT, Class)]) {
        for (h, target, class) in targets {
            self.pending.insert(*h, *target);
            self.class.insert(*h, *class);
        }
    }

    /// The previous request of `h` has not been applied yet and is still within its deadline.
    fn in_flight(&self, h: isize, actual: &RECT, now: Instant) -> bool {
        self.sent.get(&h).is_some_and(|(previous, _, requested)| {
            !crate::layout::geometry::same_rect(previous, actual, 2)
                && now.saturating_duration_since(*requested) < RETRY_AFTER
        })
    }

    /// Returns at most one latest target per window. A client that has not yet
    /// applied its previous request gets time to catch up instead of a request
    /// on every display frame. The deadline also handles minimum-size rejection.
    pub fn take_ready(
        &mut self,
        now: Instant,
        mut observed: impl FnMut(isize) -> Option<RECT>,
    ) -> Vec<(isize, RECT)> {
        let mut seen: HashMap<isize, Option<RECT>> = HashMap::new();
        let mut look = |h: isize| *seen.entry(h).or_insert_with(|| observed(h));
        let near: Vec<isize> = self
            .class
            .iter()
            .filter(|(_, class)| **class == Class::Near)
            .map(|(h, _)| *h)
            .collect();
        let hold_near = near.into_iter().any(|h| {
            look(h).is_some_and(|actual| {
                self.in_flight(h, &actual, now)
                    && self.sent.get(&h).is_some_and(|(_, _, requested)| {
                        now.saturating_duration_since(*requested) < NEAR_WAIT
                    })
            })
        });
        let mut pending: Vec<(isize, RECT)> = self.pending.iter().map(|(h, t)| (*h, *t)).collect();
        pending.sort_by_key(|(h, _)| *h);
        let mut ready = Vec::new();
        for (h, target) in pending {
            let Some(actual) = look(h) else {
                // DWM can temporarily fail while a live window is changing state.
                // Lifecycle cleanup removes destroyed/hidden members separately.
                continue;
            };
            if actual == target
                && self
                    .sent
                    .get(&h)
                    .is_none_or(|(previous, _, _)| *previous == target)
            {
                self.sent.remove(&h);
                self.pending.remove(&h);
                continue;
            }
            if self.in_flight(h, &actual, now) {
                continue;
            }
            match self.class.get(&h).copied().unwrap_or_default() {
                Class::Near if hold_near => continue,
                Class::Far
                    if self
                        .far_sent
                        .get(&h)
                        .is_some_and(|at| now.saturating_duration_since(*at) < FAR_INTERVAL) =>
                {
                    continue
                }
                Class::Far => {
                    self.far_sent.insert(h, now);
                }
                _ => {}
            }
            self.sent.insert(h, (target, actual, now));
            self.pending.remove(&h);
            ready.push((h, target));
        }
        ready
    }

    pub fn constrained_targets(
        &self,
        now: Instant,
        mut observed: impl FnMut(isize) -> Option<RECT>,
    ) -> Vec<(isize, RECT)> {
        self.sent
            .iter()
            .filter_map(|(h, (target, before, requested))| {
                if now.saturating_duration_since(*requested) < RETRY_AFTER {
                    return None;
                }
                let actual = observed(*h)?;
                // An entirely unchanged frame can belong to a stalled application.
                // Learn only after observing progress, never from the initial large size.
                (!crate::layout::geometry::same_rect(&actual, before, 2)
                    && (actual.right - actual.left > target.right - target.left + 2
                        || actual.bottom - actual.top > target.bottom - target.top + 2))
                    .then_some((*h, *target))
            })
            .collect()
    }

    pub fn forget<'a>(&mut self, windows: impl IntoIterator<Item = &'a isize>) {
        for h in windows {
            self.pending.remove(h);
            self.sent.remove(h);
            self.class.remove(h);
            self.far_sent.remove(h);
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.sent.clear();
        self.class.clear();
        self.far_sent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: i32) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: 600,
        }
    }

    #[test]
    fn minimum_feedback_requires_observed_progress_and_time_to_apply() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue(&[(1, rect(400))]);
        scheduler.take_ready(now, |_| Some(rect(1000)));
        assert!(scheduler
            .constrained_targets(now + Duration::from_millis(17), |_| Some(rect(600)))
            .is_empty());
        assert!(scheduler
            .constrained_targets(now + RETRY_AFTER, |_| Some(rect(1000)))
            .is_empty());
        assert_eq!(
            scheduler.constrained_targets(now + RETRY_AFTER, |_| Some(rect(600))),
            vec![(1, rect(400))]
        );
    }

    #[test]
    fn pointer_bursts_keep_only_latest_target_and_do_not_repeat_unchanged_windows() {
        let mut scheduler = Scheduler::default();
        for width in 800..1800 {
            scheduler.queue(&[(1, rect(width)), (2, rect(600))]);
        }
        let now = Instant::now();
        assert_eq!(
            scheduler.take_ready(now, |h| Some(rect(if h == 1 { 800 } else { 600 }))),
            vec![(1, rect(1799))]
        );
        assert!(scheduler.take_ready(now, |_| Some(rect(800))).is_empty());
    }

    #[test]
    fn busy_window_is_throttled_while_other_windows_continue() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue(&[(1, rect(900))]);
        scheduler.take_ready(now, |_| Some(rect(800)));
        for width in 901..1000 {
            scheduler.queue(&[(1, rect(width))]);
        }
        scheduler.queue(&[(2, rect(700))]);
        assert_eq!(
            scheduler.take_ready(now + Duration::from_millis(17), |_| Some(rect(800))),
            vec![(2, rect(700))]
        );
        assert_eq!(
            scheduler.take_ready(now + RETRY_AFTER, |_| Some(rect(800))),
            vec![(1, rect(999))]
        );
    }

    #[test]
    fn responsive_clients_update_next_frame_and_cancel_discards_pending_targets() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue(&[(1, rect(900))]);
        scheduler.take_ready(now, |_| Some(rect(800)));
        scheduler.queue(&[(1, rect(1000))]);
        assert_eq!(
            scheduler.take_ready(now + Duration::from_millis(17), |_| Some(rect(900))),
            vec![(1, rect(1000))]
        );
        scheduler.queue(&[(1, rect(1100))]);
        scheduler.forget(&[1]);
        assert!(scheduler
            .take_ready(now + RETRY_AFTER, |_| Some(rect(1000)))
            .is_empty());
    }

    #[test]
    fn windows_beside_the_separator_wait_for_a_busy_partner() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue_frame(&[(1, rect(900), Class::Near), (2, rect(500), Class::Near)]);
        assert_eq!(scheduler.take_ready(now, |_| Some(rect(800))).len(), 2);
        // window 1 applied its size, window 2 is still busy: 1 must not run ahead
        scheduler.queue_frame(&[(1, rect(950), Class::Near), (2, rect(450), Class::Near)]);
        let observed = |h: isize| Some(rect(if h == 1 { 900 } else { 800 }));
        assert!(scheduler
            .take_ready(now + Duration::from_millis(17), observed)
            .is_empty());
        // once 2 has caught up, both go out in the same frame
        let caught_up = |h: isize| Some(rect(if h == 1 { 900 } else { 500 }));
        let mut ready = scheduler.take_ready(now + Duration::from_millis(34), caught_up);
        ready.sort_by_key(|(h, _)| *h);
        assert_eq!(ready, vec![(1, rect(950)), (2, rect(450))]);
    }

    #[test]
    fn a_stalled_partner_holds_the_separator_only_briefly() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue_frame(&[(1, rect(900), Class::Near), (2, rect(500), Class::Near)]);
        scheduler.take_ready(now, |_| Some(rect(800)));
        scheduler.queue_frame(&[(1, rect(950), Class::Near)]);
        let stalled = |h: isize| Some(rect(if h == 1 { 900 } else { 800 }));
        assert!(scheduler
            .take_ready(now + Duration::from_millis(17), stalled)
            .is_empty());
        assert_eq!(
            scheduler.take_ready(now + NEAR_WAIT, stalled),
            vec![(1, rect(950))]
        );
    }

    #[test]
    fn far_windows_are_resized_at_a_lower_rate() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue_frame(&[(1, rect(900), Class::Far)]);
        assert_eq!(scheduler.take_ready(now, |_| Some(rect(800))).len(), 1);
        scheduler.queue_frame(&[(1, rect(910), Class::Far)]);
        // applied at once, but the next update still waits for the far interval
        assert!(scheduler
            .take_ready(now + Duration::from_millis(17), |_| Some(rect(900)))
            .is_empty());
        assert_eq!(
            scheduler.take_ready(now + FAR_INTERVAL, |_| Some(rect(900))),
            vec![(1, rect(910))]
        );
    }

    #[test]
    fn reversing_to_current_bounds_does_not_drop_the_target_behind_an_inflight_resize() {
        let mut scheduler = Scheduler::default();
        let now = Instant::now();
        scheduler.queue(&[(1, rect(900))]);
        scheduler.take_ready(now, |_| Some(rect(800)));
        scheduler.queue(&[(1, rect(800))]);
        assert!(scheduler
            .take_ready(now + Duration::from_millis(17), |_| Some(rect(800)))
            .is_empty());
        // The old request arrives after the reversal. The newest target must
        // still be pending, even though it matched bounds in the previous frame.
        assert_eq!(
            scheduler.take_ready(now + Duration::from_millis(34), |_| Some(rect(900))),
            vec![(1, rect(800))]
        );
    }
}
