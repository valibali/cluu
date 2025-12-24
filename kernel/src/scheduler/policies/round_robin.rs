/*
 * Round-Robin Scheduling Policy
 *
 * This module implements a simple preemptive round-robin scheduling policy.
 * It maintains a FIFO queue of ready threads and rotates through them,
 * giving each thread an equal time slice.
 *
 * Features:
 * - Simple FIFO ready queue
 * - Equal time slices for all threads
 * - Boot mode support (only critical processes run during boot)
 * - Preemptive (threads are rotated on timer ticks)
 *
 * This is the current default policy for CLUU.
 */

use alloc::collections::VecDeque;

use super::super::{
    SchedulerMode, ThreadId,
    events::SchedEvent,
    traits::{KernelSchedCtx, Scheduler},
    types::{CpuId, DispatchDecision, TimeSliceTicks},
};

/// Round-Robin scheduling policy
///
/// Maintains a simple FIFO queue of ready threads. On each scheduling
/// decision (pick_next), it pops the front thread, runs it, and pushes
/// it to the back of the queue.
pub struct RoundRobinPolicy {
    /// FIFO queue of threads ready to run
    ready_queue: VecDeque<ThreadId>,

    /// Current scheduler mode (Boot or Normal)
    mode: SchedulerMode,

    /// Boot mode: number of critical processes expected
    boot_critical_count: usize,

    /// Boot mode: number of critical processes that signaled ready
    boot_ready_count: usize,
}

impl RoundRobinPolicy {
    /// Create a new Round-Robin policy
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            mode: SchedulerMode::Boot {
                critical_count: 0,
                ready_count: 0,
            },
            boot_critical_count: 0,
            boot_ready_count: 0,
        }
    }
}

impl Scheduler for RoundRobinPolicy {
    fn on_event(&mut self, _ctx: &mut dyn KernelSchedCtx, event: SchedEvent) {
        match event {
            SchedEvent::ThreadCreated { tid, .. } => {
                // Add new thread to ready queue
                self.ready_queue.push_back(tid);
            }

            SchedEvent::ThreadWoke { tid, .. } => {
                // Thread became runnable, add to ready queue
                // (only if not already there - policy tracks its own queue)
                if !self.ready_queue.contains(&tid) {
                    log::debug!("[RR Policy] ThreadWoke: Adding thread {} to ready queue", tid.0);
                    self.ready_queue.push_back(tid);
                } else {
                    log::debug!("[RR Policy] ThreadWoke: Thread {} already in queue", tid.0);
                }
            }

            SchedEvent::ThreadYielded { tid } => {
                // Thread voluntarily gave up CPU
                // In round-robin, yielding thread goes to back of queue
                // (It was removed from queue when picked, so re-add it)
                if !self.ready_queue.contains(&tid) {
                    log::debug!("[RR Policy] ThreadYielded: Adding thread {} to ready queue", tid.0);
                    self.ready_queue.push_back(tid);
                } else {
                    log::debug!("[RR Policy] ThreadYielded: Thread {} already in queue", tid.0);
                }
            }

            SchedEvent::ThreadBlocked { tid, .. } | SchedEvent::ThreadExited { tid, .. } => {
                // Remove thread from ready queue
                self.ready_queue.retain(|&id| id != tid);
            }

            SchedEvent::Tick { .. } => {
                // RR rotates on every tick (timeslice expiration)
                // No action needed here - pick_next will rotate
            }

            SchedEvent::ModeChanged { new_mode, .. } => {
                // Update our mode
                self.mode = new_mode;
            }

            SchedEvent::ProcessReady { pid } => {
                // Track boot mode progress
                if matches!(self.mode, SchedulerMode::Boot { .. }) {
                    self.boot_ready_count += 1;
                    // TODO: Check if we should transition to Normal mode
                    // This requires querying ctx.is_critical_process(pid)
                    let _ = pid; // Suppress warning for now
                }
            }

            _ => {
                // Ignore other events for now
            }
        }
    }

    fn pick_next(&mut self, ctx: &mut dyn KernelSchedCtx, _cpu: CpuId) -> DispatchDecision {
        // Boot mode: only schedule critical processes
        if matches!(self.mode, SchedulerMode::Boot { .. }) {
            // Try each thread in the ready queue
            let mut checked = 0;
            let queue_len = self.ready_queue.len();

            while checked < queue_len {
                if let Some(tid) = self.ready_queue.pop_front() {
                    checked += 1;

                    // Check if this thread belongs to a critical process
                    if let Some(pid) = ctx.thread_process(tid) {
                        if ctx.is_critical_process(pid) && ctx.is_runnable(tid) {
                            // Found a critical thread that's runnable
                            self.ready_queue.push_back(tid); // Re-add to back
                            return DispatchDecision::run_thread(tid, TimeSliceTicks::DEFAULT);
                        }
                    }

                    // Not critical or not runnable, put back at end
                    self.ready_queue.push_back(tid);
                }
            }

            // No critical threads ready, idle
            return DispatchDecision::idle();
        }

        // Normal mode: simple round-robin
        // Try each thread in queue until we find a runnable one
        let mut attempts = 0;
        let max_attempts = self.ready_queue.len();

        while attempts < max_attempts {
            if let Some(tid) = self.ready_queue.pop_front() {
                attempts += 1;

                if ctx.is_runnable(tid) {
                    // Found a runnable thread
                    self.ready_queue.push_back(tid); // Re-add to back
                    return DispatchDecision::run_thread(tid, TimeSliceTicks::DEFAULT);
                }

                // Not runnable (might be sleeping, blocked, etc.)
                // Don't re-add to queue - it will be added when it wakes
            } else {
                break;
            }
        }

        // No runnable threads, idle
        DispatchDecision::idle()
    }

    fn on_switched(
        &mut self,
        _ctx: &mut dyn KernelSchedCtx,
        _cpu: CpuId,
        _prev: Option<ThreadId>,
        _next: Option<ThreadId>,
    ) {
        // Round-robin doesn't need to track context switch accounting
        // More sophisticated policies (like MLFQ) would track CPU time here
    }

    fn name(&self) -> &'static str {
        "Round-Robin"
    }
}

impl Default for RoundRobinPolicy {
    fn default() -> Self {
        Self::new()
    }
}
