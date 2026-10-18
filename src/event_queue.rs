//! Deferred callback events with bounded replay and motion coalescing.
//!
//! A barrier preserves all events before it. Within a segment, repeated motion for the
//! same key replaces its first pending slot; different keys retain first-seen order.
//! This deliberately does not promise chronological order for coalesced motion.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

pub struct DeferredQueue<T, K> {
    events: VecDeque<T>,
    motion: HashMap<K, u64>,
    head: u64,
}

impl<T, K> Default for DeferredQueue<T, K> {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            motion: HashMap::new(),
            head: 0,
        }
    }
}

impl<T, K: Eq + Hash> DeferredQueue<T, K> {
    /// `Some(key)` may replace pending motion in this segment. `None` is a FIFO
    /// barrier: use it for every lifecycle event and move/size boundary.
    pub fn push(&mut self, event: T, key: Option<K>) {
        if let Some(key) = key {
            if let Some(sequence) = self.motion.get(&key) {
                let index = sequence.wrapping_sub(self.head);
                if index < self.events.len() as u64 {
                    self.events[index as usize] = event;
                    return;
                }
            }
            let sequence = self.head.wrapping_add(self.events.len() as u64);
            self.motion.insert(key, sequence);
        } else {
            self.motion.clear();
        }
        self.events.push_back(event);
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn pop_front(&mut self) -> Option<T> {
        let event = self.events.pop_front()?;
        self.head = self.head.wrapping_add(1);
        if self.events.is_empty() {
            self.motion.clear();
        } else if self.motion.len() > self.events.len().saturating_mul(2).saturating_add(64) {
            // A queue that never reaches idle must not retain every consumed HWND.
            let head = self.head;
            let len = self.events.len() as u64;
            self.motion
                .retain(|_, sequence| sequence.wrapping_sub(head) < len);
        }
        Some(event)
    }

    /// Take a finite batch before calling foreign APIs. Newly delivered callbacks
    /// cannot extend this batch and must wait for the next replay opportunity.
    #[cfg(test)]
    pub fn take_batch(&mut self, limit: usize) -> Vec<T> {
        let count = limit.min(self.events.len());
        let batch = self.events.drain(..count).collect();
        self.head = self.head.wrapping_add(count as u64);
        // Entries for consumed motion are harmless while the queue is nonempty:
        // the wrapping distance check in push rejects them. Clear at idle to
        // release keys from windows that no longer produce callbacks.
        if self.events.is_empty() {
            self.motion.clear();
        }
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::DeferredQueue;

    #[test]
    fn consumed_motion_keys_remain_bounded_without_idle() {
        let mut q = DeferredQueue::default();
        q.push(0, None);
        for key in 1..20_000 {
            q.push(key, Some(key));
            q.pop_front();
            assert_eq!(q.len(), 1);
            assert!(q.motion.len() <= 66);
        }
        assert_eq!(q.pop_front(), Some(19_999));
        assert!(q.motion.is_empty());
    }

    #[test]
    fn motion_coalesces_but_cannot_cross_a_lifecycle_barrier() {
        let mut q = DeferredQueue::default();
        q.push("move 1", Some(7));
        q.push("move 2", Some(7));
        q.push("destroy", None);
        q.push("create reused hwnd", None);
        q.push("move new 1", Some(7));
        q.push("move new 2", Some(7));
        assert_eq!(q.len(), 4);
        assert_eq!(
            q.take_batch(100),
            ["move 2", "destroy", "create reused hwnd", "move new 2"]
        );
        assert!(q.is_empty());
    }

    #[test]
    fn structural_events_remain_fifo_and_replay_has_a_fixed_budget() {
        let mut q: DeferredQueue<_, u32> = DeferredQueue::default();
        for n in 0..100 {
            q.push(n, None);
        }
        let batch = q.take_batch(8);
        // Events arriving during the caller's replay remain for a later turn.
        q.push(100, None);
        assert_eq!(batch, (0..8).collect::<Vec<_>>());
        assert_eq!(q.len(), 93);
        assert!(q.take_batch(0).is_empty());
        assert_eq!(q.take_batch(100), (8..=100).collect::<Vec<_>>());
    }

    #[test]
    fn consumed_slot_is_never_replaced_and_other_keys_are_preserved() {
        let mut q = DeferredQueue::default();
        q.push((1, 10), Some(1));
        q.push((2, 20), Some(2));
        assert_eq!(q.take_batch(1), [(1, 10)]);
        q.push((1, 11), Some(1));
        q.push((2, 21), Some(2));
        assert_eq!(q.take_batch(10), [(2, 21), (1, 11)]);
    }

    #[test]
    fn a_motion_storm_keeps_only_the_latest_frame_per_window() {
        let mut q = DeferredQueue::default();
        for frame in 0..20_000 {
            for hwnd in 0..10 {
                q.push((hwnd, frame), Some(hwnd));
            }
        }
        assert_eq!(q.len(), 10);
        for hwnd in 0..10 {
            assert_eq!(q.pop_front(), Some((hwnd, 19_999)));
        }
        assert!(q.is_empty());
    }
}
