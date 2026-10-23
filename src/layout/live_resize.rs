//! Latest-target scheduling for real window resizing. No pixels or application IPC.
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;

const RETRY_AFTER: Duration = Duration::from_millis(90);

#[derive(Default)]
pub struct Scheduler {
    pending: HashMap<isize, RECT>,
    sent: HashMap<isize, (RECT, RECT, Instant)>,
}

impl Scheduler {
    pub fn queue(&mut self, targets: &[(isize, RECT)]) {
        self.pending.extend(targets.iter().copied());
    }

    /// Returns at most one latest target per window. A client that has not yet
    /// applied its previous request gets time to catch up instead of a request
    /// on every display frame. The deadline also handles minimum-size rejection.
    pub fn take_ready(
        &mut self,
        now: Instant,
        mut observed: impl FnMut(isize) -> Option<RECT>,
    ) -> Vec<(isize, RECT)> {
        let mut ready = Vec::new();
        self.pending.retain(|h, target| {
            let Some(actual) = observed(*h) else {
                // DWM can temporarily fail while a live window is changing state.
                // Lifecycle cleanup removes destroyed/hidden members separately.
                return true;
            };
            if actual == *target
                && self
                    .sent
                    .get(h)
                    .is_none_or(|(previous, _, _)| previous == target)
            {
                self.sent.remove(h);
                return false;
            }
            if self.sent.get(h).is_some_and(|(previous, _, requested)| {
                !crate::layout::geometry::same_rect(previous, &actual, 2)
                    && now.saturating_duration_since(*requested) < RETRY_AFTER
            }) {
                return true;
            }
            self.sent.insert(*h, (*target, actual, now));
            ready.push((*h, *target));
            false
        });
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
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.sent.clear();
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
