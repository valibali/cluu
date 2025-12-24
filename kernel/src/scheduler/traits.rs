/*
 * Scheduler Trait Definitions
 *
 * This module defines the traits that separate scheduling policy from mechanism:
 *
 * - Scheduler: The policy interface that different algorithms implement
 * - KernelSchedCtx: The mechanism interface that policies use to interact with the kernel
 *
 * This separation allows:
 * 1. Swapping scheduling algorithms without changing kernel code
 * 2. Testing policies in isolation
 * 3. Clear ownership boundaries (policies never touch Thread structs directly)
 */

use super::{
    ThreadId, ThreadState, ProcessId,
    types::{CpuId, DispatchDecision, Priority, SchedClass},
    events::SchedEvent,
};

/// Scheduling policy trait
///
/// Different scheduling algorithms (RR, MLFQ, EDF, etc.) implement this trait.
/// The SchedulerCore (mechanism) holds a Box<dyn Scheduler> and forwards events to it.
///
/// Policies are responsible for:
/// - Deciding which thread to run next (pick_next)
/// - Reacting to system events (on_event)
/// - Managing internal structures (ready queues, priorities, etc.)
///
/// Policies interact with the kernel ONLY through KernelSchedCtx methods.
/// They never access Thread/Process objects directly, ensuring clean separation.
pub trait Scheduler: Send {
    /// React to a scheduling event
    ///
    /// The mechanism calls this when something happens in the system:
    /// threads are created/woken/blocked/exited, timer ticks occur, etc.
    ///
    /// The policy updates its internal state based on the event.
    ///
    /// # Arguments
    /// - `ctx`: Access to kernel state (thread info, current thread, etc.)
    /// - `event`: The event that occurred
    fn on_event(&mut self, ctx: &mut dyn KernelSchedCtx, event: SchedEvent);

    /// Choose the next thread to run
    ///
    /// Called by the mechanism when it needs to perform a context switch.
    /// The policy examines its ready structures and returns a dispatch decision.
    ///
    /// # Arguments
    /// - `ctx`: Access to kernel state
    /// - `cpu`: Which CPU needs a thread (for SMP; currently always CpuId::BSP)
    ///
    /// # Returns
    /// A DispatchDecision containing:
    /// - Which thread to run (or None for idle)
    /// - How long to run it before the next preemption
    fn pick_next(&mut self, ctx: &mut dyn KernelSchedCtx, cpu: CpuId) -> DispatchDecision;

    /// Notification that a context switch completed
    ///
    /// Called after the mechanism switches from `prev` to `next` thread.
    /// Useful for accounting (track how long threads ran, context switch overhead, etc.)
    ///
    /// # Arguments
    /// - `ctx`: Access to kernel state
    /// - `cpu`: Which CPU performed the switch
    /// - `prev`: Thread that was running (or None if idle)
    /// - `next`: Thread that is now running (or None if idle)
    fn on_switched(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        cpu: CpuId,
        prev: Option<ThreadId>,
        next: Option<ThreadId>,
    );

    /// Get the policy name for debugging
    fn name(&self) -> &'static str;
}

/// Kernel context interface for scheduling policies
///
/// This trait is the ONLY way policies can query or modify kernel state.
/// It acts as a capability-based security boundary:
/// - Policies can't corrupt kernel structures
/// - Policies can't access memory outside their domain
/// - Clear API makes testing easier
///
/// The mechanism (SchedulerCore) provides an implementation of this trait
/// that safely accesses the actual kernel state.
pub trait KernelSchedCtx {
    // ========== QUERY OPERATIONS ==========

    /// Get the state of a thread
    fn thread_state(&self, tid: ThreadId) -> Option<ThreadState>;

    /// Check if a thread is runnable (Ready state and not blocked)
    fn is_runnable(&self, tid: ThreadId) -> bool;

    /// Get the currently running thread on a CPU
    fn current_thread(&self, cpu: CpuId) -> Option<ThreadId>;

    /// Get a thread's priority
    fn thread_priority(&self, tid: ThreadId) -> Option<Priority>;

    /// Get a thread's scheduling class
    fn thread_class(&self, tid: ThreadId) -> Option<SchedClass>;

    /// Get the process ID that owns a thread
    fn thread_process(&self, tid: ThreadId) -> Option<ProcessId>;

    /// Check if a process is critical (runs in boot mode)
    fn is_critical_process(&self, pid: ProcessId) -> bool;

    /// Get current scheduler mode (Boot or Normal)
    fn current_mode(&self) -> super::SchedulerMode;

    /// Get current tick count (for time-based scheduling)
    fn now_ticks(&self) -> u64;

    /// Get number of CPUs in the system
    fn cpu_count(&self) -> usize;

    /// Get all thread IDs (for policy initialization)
    fn all_threads(&self) -> alloc::vec::Vec<ThreadId>;

    // ========== STATE MODIFICATION ==========

    /// Mark a thread as runnable (transition from Blocked to Ready)
    ///
    /// The mechanism handles the actual state transition; the policy
    /// should update its ready structures in response to ThreadWoke event.
    fn make_runnable(&mut self, tid: ThreadId);

    /// Request that a CPU should reschedule at the next opportunity
    ///
    /// Sets a per-CPU flag that causes the next interrupt to trigger a context switch.
    /// For remote CPUs (SMP), may send an IPI.
    fn request_reschedule(&mut self, cpu: CpuId);

    /// Set a thread's scheduling class
    ///
    /// Allows policies to categorize threads and apply different algorithms.
    fn set_thread_class(&mut self, tid: ThreadId, class: SchedClass);

    /// Update a thread's priority
    ///
    /// The policy usually manages priorities internally, but this allows
    /// external priority changes (e.g., from syscalls).
    fn set_thread_priority(&mut self, tid: ThreadId, priority: Priority);

    // Note: Policy-private data (per-thread metadata) is omitted from this trait
    // to maintain dyn compatibility. Policies that need per-thread state should
    // maintain their own HashMap<ThreadId, PolicyData> internally.

    // ========== DEBUGGING ==========

    /// Log a message from the policy
    ///
    /// Policies should use this instead of log::info! directly to allow
    /// mechanism to control verbosity and routing.
    fn log(&self, level: log::Level, message: &str);
}
