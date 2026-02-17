//! Priority Bitmap Scheduler with Active/Expired Arrays
//!
//! This module implements an O(1) priority-based scheduler that guarantees
//! fairness through active/expired array swapping. Inspired by Linux 2.6 O(1).
//!
//! # Algorithm
//!
//! - 256 priority levels (0 = lowest, 255 = highest)
//! - Two sets of arrays: "active" and "expired"
//! - Threads are picked from active arrays (highest priority first)
//! - After running, threads move to expired arrays (same priority)
//! - When active is empty, swap active ↔ expired (O(1) pointer swap)
//!
//! # Fairness Guarantee
//!
//! Within one "epoch" (one complete swap cycle), every thread—regardless
//! of priority—gets exactly one timeslice. Higher priority threads still
//! run first within an epoch, but cannot starve lower priority threads
//! indefinitely.
//!
//! # Operations
//!
//! - `pick_next()`: O(1) - find highest priority ready thread
//! - `add()`: O(1) - add thread to active queue
//! - `expire()`: O(1) - move thread from active to expired
//! - `tick()`: O(1) - handle time slice expiration

use crate::sched::thread::{Priority, ThreadId};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Scheduling policy trait
pub trait SchedulingPolicy: Send {
    /// Pick the next thread to run
    fn pick_next(&mut self) -> Option<ThreadId>;

    /// Add a thread to the ready queue (active array)
    fn add(&mut self, thread_id: ThreadId, priority: Priority);

    /// Remove a thread from the scheduler entirely
    fn remove(&mut self, thread_id: ThreadId);

    /// Update a thread's priority
    fn set_priority(&mut self, thread_id: ThreadId, priority: Priority);

    /// Handle timer tick
    fn tick(&mut self);
}

/// A single priority array set (bitmap + per-priority queues)
struct PriorityArray {
    /// Bitmap of priorities with ready threads (256 bits = 4 x u64)
    bitmap: [u64; 4],
    /// Per-priority ready queues (FIFO within priority)
    queues: Vec<VecDeque<ThreadId>>,
}

impl PriorityArray {
    fn new() -> Self {
        let mut queues = Vec::with_capacity(256);
        for _ in 0..256 {
            queues.push(VecDeque::new());
        }
        Self {
            bitmap: [0; 4],
            queues,
        }
    }

    /// Find highest priority with ready threads
    #[inline(always)]
    fn find_highest_priority(&self) -> Option<u8> {
        for word_idx in (0..4).rev() {
            let word = self.bitmap[word_idx];
            if word != 0 {
                let bit = 63 - word.leading_zeros() as u8;
                let priority = (word_idx * 64) as u8 + bit;
                return Some(priority);
            }
        }
        None
    }

    /// Check if array is empty (no ready threads at any priority)
    fn is_empty(&self) -> bool {
        self.bitmap[0] == 0 && self.bitmap[1] == 0 && self.bitmap[2] == 0 && self.bitmap[3] == 0
    }

    /// Set a bit in the bitmap
    #[inline(always)]
    fn set_bit(&mut self, priority: u8) {
        let word_idx = (priority / 64) as usize;
        let bit_idx = priority % 64;
        self.bitmap[word_idx] |= 1u64 << bit_idx;
    }

    /// Clear a bit in the bitmap
    #[inline(always)]
    fn clear_bit(&mut self, priority: u8) {
        let word_idx = (priority / 64) as usize;
        let bit_idx = priority % 64;
        self.bitmap[word_idx] &= !(1u64 << bit_idx);
    }

    /// Add thread to this array at given priority
    fn add(&mut self, thread_id: ThreadId, priority: u8) {
        self.queues[priority as usize].push_back(thread_id);
        self.set_bit(priority);
    }

    /// Pop highest priority thread from this array
    fn pop_highest(&mut self) -> Option<(ThreadId, u8)> {
        let priority = self.find_highest_priority()?;
        let queue = &mut self.queues[priority as usize];
        let thread_id = queue.pop_front()?;
        if queue.is_empty() {
            self.clear_bit(priority);
        }
        Some((thread_id, priority))
    }

    /// Remove a specific thread from this array (O(n) search)
    fn remove(&mut self, thread_id: ThreadId) -> bool {
        for priority in 0..=255u8 {
            let queue = &mut self.queues[priority as usize];
            if let Some(pos) = queue.iter().position(|&id| id == thread_id) {
                queue.remove(pos);
                if queue.is_empty() {
                    self.clear_bit(priority);
                }
                return true;
            }
        }
        false
    }
}

/// Priority Bitmap Scheduler with Active/Expired Arrays
///
/// O(1) scheduler that guarantees all threads get CPU time.
pub struct PriorityBitmapScheduler {
    /// Active array - threads are picked from here
    active: PriorityArray,
    /// Expired array - threads move here after running
    expired: PriorityArray,
    /// Currently running thread (if any)
    current: Option<(ThreadId, u8)>, // (thread_id, priority)
    /// Epoch counter (increments on each swap)
    epoch: u64,
}

impl PriorityBitmapScheduler {
    /// Create a new priority bitmap scheduler
    pub fn new() -> Self {
        Self {
            active: PriorityArray::new(),
            expired: PriorityArray::new(),
            current: None,
            epoch: 0,
        }
    }

    /// Swap active and expired arrays when active is exhausted
    fn swap_arrays(&mut self) {
        core::mem::swap(&mut self.active, &mut self.expired);
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Move current thread to expired array
    /// Called when a thread yields or uses its timeslice
    pub fn expire_current(&mut self) {
        if let Some((thread_id, priority)) = self.current.take() {
            self.expired.add(thread_id, priority);
        }
    }

    /// Get current epoch number
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl SchedulingPolicy for PriorityBitmapScheduler {
    fn pick_next(&mut self) -> Option<ThreadId> {
        // If active is empty, swap with expired
        if self.active.is_empty() {
            if self.expired.is_empty() {
                return None; // No threads at all
            }
            self.swap_arrays();
        }

        // Pick from active array
        let (thread_id, priority) = self.active.pop_highest()?;
        self.current = Some((thread_id, priority));
        Some(thread_id)
    }

    fn add(&mut self, thread_id: ThreadId, priority: Priority) {
        // New/woken threads go to active array so they can run this epoch
        self.active.add(thread_id, priority.as_u8());
    }

    fn remove(&mut self, thread_id: ThreadId) {
        // If it's the current thread, just clear current
        if let Some((current_id, _)) = self.current {
            if current_id == thread_id {
                self.current = None;
                return;
            }
        }

        // Try to remove from both arrays
        if !self.active.remove(thread_id) {
            self.expired.remove(thread_id);
        }
    }

    fn set_priority(&mut self, thread_id: ThreadId, new_priority: Priority) {
        self.remove(thread_id);
        self.add(thread_id, new_priority);
    }

    fn tick(&mut self) {
        // Called on timer tick - could implement time slice tracking here
    }
}

impl Default for PriorityBitmapScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_new() {
        let sched = PriorityBitmapScheduler::new();
        assert!(sched.active.is_empty());
        assert!(sched.expired.is_empty());
        assert!(sched.current.is_none());
    }

    #[test]
    fn test_add_and_pick() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(200));
        sched.add(ThreadId::new(3), Priority::new(150));

        // Should pick highest priority first (200)
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
        sched.expire_current();

        // Then 150
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
        sched.expire_current();

        // Then 100
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        sched.expire_current();

        // Active now empty, should swap and pick from expired
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
    }

    #[test]
    fn test_fairness_guarantee() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(255)); // highest
        sched.add(ThreadId::new(2), Priority::new(1)); // very low

        // First epoch: high priority runs first
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        sched.expire_current();

        // Low priority WILL run in same epoch (no starvation)
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
        sched.expire_current();

        // Now both in expired, swap happens on next pick
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
    }

    #[test]
    fn test_fifo_within_priority() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(100));
        sched.add(ThreadId::new(3), Priority::new(100));

        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        sched.expire_current();
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
        sched.expire_current();
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
    }

    #[test]
    fn test_remove() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(100));
        sched.add(ThreadId::new(3), Priority::new(100));

        sched.remove(ThreadId::new(2));

        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        sched.expire_current();
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
    }

    #[test]
    fn test_epoch_swap() {
        let mut sched = PriorityBitmapScheduler::new();
        assert_eq!(sched.epoch, 0);

        sched.add(ThreadId::new(1), Priority::new(100));

        sched.pick_next();
        sched.expire_current();

        // Next pick should trigger swap
        sched.pick_next();
        assert_eq!(sched.epoch, 1);
    }

    #[test]
    fn test_empty_scheduler() {
        let mut sched = PriorityBitmapScheduler::new();
        assert_eq!(sched.pick_next(), None);
    }
}
