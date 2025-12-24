/*
 * Scheduler Event Definitions
 *
 * This module defines the events that the scheduler mechanism reports to
 * scheduling policies. Policies react to these events to make scheduling
 * decisions.
 *
 * This design decouples the policy (what to schedule) from the mechanism
 * (how to perform context switches, manage threads, etc.).
 */

use super::{ThreadId, ProcessId, types::{BlockReason, CpuId, Priority}};

/// Events that the scheduler mechanism reports to policies
///
/// The SchedulerCore (mechanism) translates kernel operations (thread create,
/// wake, block, etc.) into these events and forwards them to the active policy.
/// The policy updates its internal state and makes scheduling decisions based
/// on these events.
#[derive(Debug, Clone)]
pub enum SchedEvent {
    /// A new thread was created and is ready to run
    ///
    /// The policy should add this thread to its ready structures.
    /// The mechanism has already allocated the thread's resources.
    ThreadCreated {
        tid: ThreadId,
        priority: Priority,
    },

    /// A blocked thread became runnable (I/O completed, woken up, etc.)
    ///
    /// The policy should move this thread back to its ready structures.
    /// This can be a priority inversion opportunity (e.g., priority boost).
    ThreadWoke {
        tid: ThreadId,
        was_blocked_on: BlockReason,
    },

    /// A running thread voluntarily gave up the CPU
    ///
    /// The policy decides whether to reschedule immediately or continue
    /// running the same thread (e.g., if no other threads are ready).
    ThreadYielded {
        tid: ThreadId,
    },

    /// A thread became blocked (waiting for I/O, IPC, lock, etc.)
    ///
    /// The policy should remove this thread from its ready structures
    /// and track the blocking reason for later accounting.
    ThreadBlocked {
        tid: ThreadId,
        reason: BlockReason,
    },

    /// A thread exited and will not run again
    ///
    /// The policy should remove this thread from all its structures.
    /// The mechanism will clean up resources after the policy processes this.
    ThreadExited {
        tid: ThreadId,
        exit_code: i32,
    },

    /// Timer interrupt (tick) occurred on a CPU
    ///
    /// The policy should update time accounting, check for quantum
    /// expiration, and decide whether to preempt the current thread.
    /// This is the primary event driving preemptive scheduling.
    Tick {
        cpu: CpuId,
        current_thread: Option<ThreadId>,
    },

    /// Thread priority was changed externally
    ///
    /// The policy should update its structures to reflect the new priority.
    /// This might trigger immediate rescheduling if priorities are inverted.
    PriorityChanged {
        tid: ThreadId,
        old_priority: Priority,
        new_priority: Priority,
    },

    /// A CPU came online (future SMP support)
    ///
    /// The policy can initialize per-CPU structures and balance load.
    CpuOnline {
        cpu: CpuId,
    },

    /// A CPU went offline (future SMP support)
    ///
    /// The policy should migrate threads from this CPU to others.
    CpuOffline {
        cpu: CpuId,
    },

    /// Scheduler mode changed (Boot -> Normal)
    ///
    /// The policy may adjust scheduling behavior based on the mode.
    /// In Boot mode, only critical processes run.
    ModeChanged {
        old_mode: super::SchedulerMode,
        new_mode: super::SchedulerMode,
    },

    /// A process signaled it's ready (for boot mode transition)
    ///
    /// The policy tracks how many critical processes are ready and
    /// may trigger transition to Normal mode.
    ProcessReady {
        pid: ProcessId,
    },
}

impl SchedEvent {
    /// Get a short name for logging
    pub fn name(&self) -> &'static str {
        match self {
            SchedEvent::ThreadCreated { .. } => "ThreadCreated",
            SchedEvent::ThreadWoke { .. } => "ThreadWoke",
            SchedEvent::ThreadYielded { .. } => "ThreadYielded",
            SchedEvent::ThreadBlocked { .. } => "ThreadBlocked",
            SchedEvent::ThreadExited { .. } => "ThreadExited",
            SchedEvent::Tick { .. } => "Tick",
            SchedEvent::PriorityChanged { .. } => "PriorityChanged",
            SchedEvent::CpuOnline { .. } => "CpuOnline",
            SchedEvent::CpuOffline { .. } => "CpuOffline",
            SchedEvent::ModeChanged { .. } => "ModeChanged",
            SchedEvent::ProcessReady { .. } => "ProcessReady",
        }
    }

    /// Check if this event should trigger immediate rescheduling
    ///
    /// Some events (like ThreadWoke with higher priority) should cause
    /// immediate preemption. Others (like Tick) are handled periodically.
    pub fn should_reschedule_immediately(&self) -> bool {
        matches!(
            self,
            SchedEvent::ThreadCreated { .. }
                | SchedEvent::ThreadWoke { .. }
                | SchedEvent::ThreadYielded { .. }
                | SchedEvent::PriorityChanged { .. }
        )
    }
}
