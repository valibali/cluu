/*
 * Scheduler Type Definitions
 *
 * This module defines the core types used throughout the scheduler subsystem.
 * These types are designed to be lightweight, Copy-able, and suitable for
 * use in both policy and mechanism layers.
 */

use super::{ThreadId, ProcessId};

/// CPU identifier
///
/// Represents a logical CPU core. Currently CLUU is single-core,
/// so this is always CpuId(0), but the type prepares for future SMP support.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct CpuId(pub u32);

impl CpuId {
    /// Bootstrap processor (CPU 0)
    pub const BSP: CpuId = CpuId(0);

    /// Get the CPU ID as a usize for indexing
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Thread priority
///
/// Higher values indicate higher priority. Policies use this to make
/// scheduling decisions. The kernel provides default priorities based
/// on ProcessType, but policies can override or adjust them.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub i32);

impl Priority {
    /// Minimum priority (idle threads)
    pub const MIN: Priority = Priority(0);

    /// Normal priority (user threads)
    pub const NORMAL: Priority = Priority(100);

    /// System priority (system services)
    pub const SYSTEM: Priority = Priority(500);

    /// Critical priority (boot services)
    pub const CRITICAL: Priority = Priority(1000);

    /// Real-time base priority
    pub const REALTIME_BASE: Priority = Priority(2000);
}

/// Time slice duration in timer ticks
///
/// The policy specifies how long a thread should run before being preempted.
/// With a 100Hz timer (10ms per tick), TimeSliceTicks(1) = 10ms.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TimeSliceTicks(pub u32);

impl TimeSliceTicks {
    /// Default time slice (10 ticks = 100ms @ 100Hz)
    pub const DEFAULT: TimeSliceTicks = TimeSliceTicks(10);

    /// Short time slice for interactive threads (2 ticks = 20ms)
    pub const SHORT: TimeSliceTicks = TimeSliceTicks(2);

    /// Long time slice for batch threads (50 ticks = 500ms)
    pub const LONG: TimeSliceTicks = TimeSliceTicks(50);

    /// Get the value as u32
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Dispatch decision made by a scheduling policy
///
/// After evaluating the current system state, a policy returns this
/// decision indicating what thread should run next and for how long.
#[derive(Debug, Clone)]
pub struct DispatchDecision {
    /// Thread to schedule next (None = idle/halt CPU)
    pub next: Option<ThreadId>,

    /// Time slice for the selected thread
    pub timeslice: TimeSliceTicks,

    /// Optional: CPU affinity hint for SMP (future)
    pub cpu_hint: Option<CpuId>,
}

impl DispatchDecision {
    /// Create a decision to run a specific thread
    pub fn run_thread(tid: ThreadId, timeslice: TimeSliceTicks) -> Self {
        Self {
            next: Some(tid),
            timeslice,
            cpu_hint: None,
        }
    }

    /// Create a decision to idle the CPU
    pub fn idle() -> Self {
        Self {
            next: None,
            timeslice: TimeSliceTicks::DEFAULT,
            cpu_hint: None,
        }
    }
}

/// Reason why a thread was blocked
///
/// Policies may use this information to implement different wakeup strategies
/// or priority adjustments when threads are unblocked.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// Waiting for I/O operation to complete
    WaitingForIo { channel: u32 },

    /// Sleeping for a specific duration
    Sleeping { until_tick: u64 },

    /// Waiting for IPC message
    WaitingForIpc { port_id: usize },

    /// Waiting for a mutex/lock
    WaitingForLock { lock_id: usize },

    /// Waiting for child process to exit
    WaitingForChild { pid: ProcessId },

    /// Generic blocking (reason not specified)
    Other,
}

/// Thread class/category
///
/// Policies can use this to group threads and apply different
/// scheduling algorithms to different classes.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SchedClass {
    /// Interactive threads (shells, UI, etc.) - prefer responsiveness
    Interactive,

    /// Batch/background threads - prefer throughput
    Batch,

    /// Real-time threads - strict timing guarantees
    RealTime,

    /// Idle threads - run only when nothing else is runnable
    Idle,

    /// Normal threads - balanced scheduling
    Normal,
}

impl Default for SchedClass {
    fn default() -> Self {
        SchedClass::Normal
    }
}
