//! Priority Bitmap Scheduler
//!
//! This module implements an O(1) priority-based scheduler using a bitmap
//! to track which priority levels have ready threads.
//!
//! # Algorithm
//!
//! - 256 priority levels (0 = lowest, 255 = highest)
//! - Bitmap (4 x u64) tracks which priorities have ready threads
//! - Per-priority FIFO queues for round-robin within priority
//! - `pick_next()` finds highest set bit in O(1) time
//!
//! # Operations
//!
//! - `pick_next()`: O(1) - find highest priority ready thread
//! - `add()`: O(1) - add thread to ready queue
//! - `remove()`: O(n) worst case for queue search, typically O(1)
//! - `tick()`: O(1) - handle time slice expiration
//!
//! # Priority Bitmap Structure
//!
//! ```text
//! Priorities 0-255 mapped to 4 u64 words:
//! Word 0: priorities 0-63
//! Word 1: priorities 64-127
//! Word 2: priorities 128-191
//! Word 3: priorities 192-255
//!
//! Finding highest priority:
//! 1. Check word 3 first (highest priorities)
//! 2. Use leading_zeros() to find highest bit
//! 3. Calculate absolute priority number
//! ```

use crate::sched::thread::{Priority, ThreadId};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Scheduling policy trait
///
/// Defines the interface for scheduling algorithms.
/// Allows different implementations (round-robin, priority, etc.)
pub trait SchedulingPolicy: Send {
    /// Pick the next thread to run
    ///
    /// Returns the ThreadId of the highest priority ready thread,
    /// or None if no threads are ready.
    fn pick_next(&mut self) -> Option<ThreadId>;

    /// Add a thread to the ready queue
    ///
    /// The thread will be scheduled according to its priority.
    fn add(&mut self, thread_id: ThreadId, priority: Priority);

    /// Remove a thread from the scheduler
    ///
    /// Used when a thread blocks or terminates.
    fn remove(&mut self, thread_id: ThreadId);

    /// Update a thread's priority
    ///
    /// Moves the thread to the appropriate priority queue.
    fn set_priority(&mut self, thread_id: ThreadId, priority: Priority);

    /// Handle timer tick
    ///
    /// Called on each timer interrupt to handle time slice expiration
    /// and preemption.
    fn tick(&mut self);
}

/// Priority Bitmap Scheduler
///
/// O(1) scheduler using a 256-bit bitmap and per-priority FIFO queues.
///
/// # Structure
///
/// - **Bitmap**: 4 x u64 words covering 256 priorities
/// - **Queues**: Vec of 256 VecDeque (one per priority)
/// - **Current**: Currently running thread (if any)
pub struct PriorityBitmapScheduler {
    /// Bitmap of priorities with ready threads (256 bits = 4 x u64)
    /// bitmap[0] = priorities 0-63
    /// bitmap[1] = priorities 64-127
    /// bitmap[2] = priorities 128-191
    /// bitmap[3] = priorities 192-255
    bitmap: [u64; 4],

    /// Per-priority ready queues (FIFO within priority)
    /// queues[p] contains threads with priority p
    queues: Vec<VecDeque<ThreadId>>,

    /// Currently running thread (removed from queues)
    current: Option<ThreadId>,
}

impl PriorityBitmapScheduler {
    /// Create a new priority bitmap scheduler
    pub fn new() -> Self {
        // Create 256 empty queues (one per priority)
        let mut queues = Vec::with_capacity(256);
        for _ in 0..256 {
            queues.push(VecDeque::new());
        }

        Self {
            bitmap: [0; 4],
            queues,
            current: None,
        }
    }

    /// Find highest priority with ready threads
    ///
    /// Scans bitmap from highest to lowest priority.
    /// Returns None if no threads are ready.
    fn find_highest_priority(&self) -> Option<u8> {
        // Check from highest word to lowest (word 3 -> word 0)
        for word_idx in (0..4).rev() {
            let word = self.bitmap[word_idx];
            if word != 0 {
                // Find highest set bit in this word
                let bit = 63 - word.leading_zeros() as u8;
                let priority = (word_idx * 64) as u8 + bit;
                return Some(priority);
            }
        }
        None
    }

    /// Set a bit in the bitmap for the given priority
    fn set_bit(&mut self, priority: u8) {
        let word_idx = (priority / 64) as usize;
        let bit_idx = priority % 64;
        self.bitmap[word_idx] |= 1u64 << bit_idx;
    }

    /// Clear a bit in the bitmap for the given priority
    fn clear_bit(&mut self, priority: u8) {
        let word_idx = (priority / 64) as usize;
        let bit_idx = priority % 64;
        self.bitmap[word_idx] &= !(1u64 << bit_idx);
    }

    /// Check if a priority has any ready threads
    fn is_priority_ready(&self, priority: u8) -> bool {
        let word_idx = (priority / 64) as usize;
        let bit_idx = priority % 64;
        (self.bitmap[word_idx] & (1u64 << bit_idx)) != 0
    }

    /// Get the ready queue for a priority level
    fn queue(&self, priority: u8) -> &VecDeque<ThreadId> {
        &self.queues[priority as usize]
    }

    /// Get the mutable ready queue for a priority level
    fn queue_mut(&mut self, priority: u8) -> &mut VecDeque<ThreadId> {
        &mut self.queues[priority as usize]
    }
}

impl SchedulingPolicy for PriorityBitmapScheduler {
    fn pick_next(&mut self) -> Option<ThreadId> {
        // Find highest priority with ready threads
        let priority = self.find_highest_priority()?;

        // Get the next thread from that priority's queue (FIFO)
        let queue = self.queue_mut(priority);
        let thread_id = queue.pop_front()?;

        // If queue is now empty, clear the bitmap bit
        if queue.is_empty() {
            self.clear_bit(priority);
        }

        // Mark as current
        self.current = Some(thread_id);

        Some(thread_id)
    }

    fn add(&mut self, thread_id: ThreadId, priority: Priority) {
        let priority_val = priority.as_u8();

        // Add to priority queue
        self.queue_mut(priority_val).push_back(thread_id);

        // Set bitmap bit
        self.set_bit(priority_val);
    }

    fn remove(&mut self, thread_id: ThreadId) {
        // If it's the current thread, just clear current
        if self.current == Some(thread_id) {
            self.current = None;
            return;
        }

        // Search all queues for this thread (this is O(n) worst case)
        // In practice, we know the thread's priority, so this could be optimized
        for priority in 0..=255u8 {
            let queue = self.queue_mut(priority);

            if let Some(pos) = queue.iter().position(|&id| id == thread_id) {
                queue.remove(pos);

                // If queue is now empty, clear bitmap bit
                if queue.is_empty() {
                    self.clear_bit(priority);
                }

                return;
            }
        }
    }

    fn set_priority(&mut self, thread_id: ThreadId, new_priority: Priority) {
        // Remove from old queue
        self.remove(thread_id);

        // Add to new queue
        self.add(thread_id, new_priority);
    }

    fn tick(&mut self) {
        // For now, tick() doesn't do anything
        // In a full implementation with time slices, this would:
        // 1. Decrement current thread's time slice
        // 2. If exhausted, move current thread to back of its queue
        // 3. Call pick_next() to select new thread
        //
        // Since we don't have timer interrupts set up yet, we leave this empty
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
        assert_eq!(sched.bitmap, [0; 4]);
        assert_eq!(sched.queues.len(), 256);
        assert!(sched.current.is_none());
    }

    #[test]
    fn test_add_and_pick() {
        let mut sched = PriorityBitmapScheduler::new();

        // Add threads with different priorities
        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(200));
        sched.add(ThreadId::new(3), Priority::new(150));

        // Should pick highest priority first (200)
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));

        // Then 150
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));

        // Then 100
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));

        // No more threads
        assert_eq!(sched.pick_next(), None);
    }

    #[test]
    fn test_fifo_within_priority() {
        let mut sched = PriorityBitmapScheduler::new();

        // Add multiple threads at same priority
        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(100));
        sched.add(ThreadId::new(3), Priority::new(100));

        // Should pick in FIFO order
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
    }

    #[test]
    fn test_remove() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(100));
        sched.add(ThreadId::new(2), Priority::new(100));
        sched.add(ThreadId::new(3), Priority::new(100));

        // Remove middle thread
        sched.remove(ThreadId::new(2));

        // Should skip thread 2
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
        assert_eq!(sched.pick_next(), None);
    }

    #[test]
    fn test_bitmap_management() {
        let mut sched = PriorityBitmapScheduler::new();

        // Add thread at priority 100
        sched.add(ThreadId::new(1), Priority::new(100));
        assert!(sched.is_priority_ready(100));

        // Pick it (should clear bitmap)
        sched.pick_next();
        assert!(!sched.is_priority_ready(100));

        // Add thread at priority 255 (highest)
        sched.add(ThreadId::new(2), Priority::new(255));
        assert!(sched.is_priority_ready(255));

        // Add thread at priority 0 (lowest)
        sched.add(ThreadId::new(3), Priority::new(0));
        assert!(sched.is_priority_ready(0));

        // Highest priority should be picked first
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
        assert_eq!(sched.pick_next(), Some(ThreadId::new(3)));
    }

    #[test]
    fn test_set_priority() {
        let mut sched = PriorityBitmapScheduler::new();

        // Add thread at low priority
        sched.add(ThreadId::new(1), Priority::new(50));
        sched.add(ThreadId::new(2), Priority::new(100));

        // Boost thread 1's priority
        sched.set_priority(ThreadId::new(1), Priority::new(200));

        // Thread 1 should now be picked first
        assert_eq!(sched.pick_next(), Some(ThreadId::new(1)));
        assert_eq!(sched.pick_next(), Some(ThreadId::new(2)));
    }

    #[test]
    fn test_find_highest_priority() {
        let mut sched = PriorityBitmapScheduler::new();

        // No threads
        assert_eq!(sched.find_highest_priority(), None);

        // Add at various priorities
        sched.set_bit(50);
        assert_eq!(sched.find_highest_priority(), Some(50));

        sched.set_bit(200);
        assert_eq!(sched.find_highest_priority(), Some(200));

        sched.set_bit(255);
        assert_eq!(sched.find_highest_priority(), Some(255));

        // Clear highest
        sched.clear_bit(255);
        assert_eq!(sched.find_highest_priority(), Some(200));
    }

    #[test]
    fn test_empty_scheduler() {
        let mut sched = PriorityBitmapScheduler::new();
        assert_eq!(sched.pick_next(), None);
        assert_eq!(sched.find_highest_priority(), None);
    }

    #[test]
    fn test_current_thread_handling() {
        let mut sched = PriorityBitmapScheduler::new();

        sched.add(ThreadId::new(1), Priority::new(100));

        // Pick thread (becomes current)
        let thread = sched.pick_next();
        assert_eq!(thread, Some(ThreadId::new(1)));
        assert_eq!(sched.current, Some(ThreadId::new(1)));

        // Remove current thread
        sched.remove(ThreadId::new(1));
        assert_eq!(sched.current, None);
    }
}
