/*
 * Scheduler Core - Mechanism Layer
 *
 * This module implements SchedulerCore, the stable mechanism layer that:
 * 1. Holds the active scheduling policy (Box<dyn Scheduler>)
 * 2. Manages per-CPU scheduling state
 * 3. Provides the stable external API that the rest of the kernel uses
 * 4. Translates kernel operations into SchedEvents
 * 5. Drives context switches based on policy decisions
 *
 * The SchedulerCore separates "mechanism" (how to switch threads) from
 * "policy" (which thread to run next). This allows swapping scheduling
 * algorithms without changing any kernel code outside this module.
 */

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::{
    ThreadId, ProcessId,
    traits::{Scheduler, KernelSchedCtx},
    types::{CpuId, Priority, BlockReason},
    events::SchedEvent,
};

/// Per-CPU scheduling state
///
/// Maintains runtime state for each CPU core. Currently CLUU is single-core,
/// so there's only one instance (CPU 0), but this structure prepares for SMP.
#[derive(Debug)]
pub struct PerCpuSchedState {
    /// Which CPU this state belongs to
    pub cpu_id: CpuId,

    /// Currently running thread on this CPU
    pub current_thread: Option<ThreadId>,

    /// Whether this CPU needs to reschedule at next opportunity
    pub need_resched: bool,

    /// Ticks remaining in current thread's timeslice
    pub timeslice_remaining: u32,

    /// Total ticks this CPU has been running
    pub total_ticks: u64,

    /// Number of context switches performed
    pub context_switches: u64,
}

impl PerCpuSchedState {
    /// Create new per-CPU state
    pub fn new(cpu_id: CpuId) -> Self {
        Self {
            cpu_id,
            current_thread: None,
            need_resched: false,
            timeslice_remaining: 0,
            total_ticks: 0,
            context_switches: 0,
        }
    }

    /// Request that this CPU should reschedule
    pub fn request_reschedule(&mut self) {
        self.need_resched = true;
    }

    /// Check if this CPU needs to reschedule
    pub fn should_reschedule(&self) -> bool {
        self.need_resched || self.timeslice_remaining == 0
    }

    /// Reset reschedule flag
    pub fn clear_reschedule(&mut self) {
        self.need_resched = false;
    }
}

/// Scheduler Core - The Mechanism Layer
///
/// This is the stable interface between the rest of the kernel and the
/// scheduling policy. External code calls methods like thread_created(),
/// thread_woke(), on_tick(), etc., and SchedulerCore forwards them as
/// SchedEvents to the active policy.
///
/// The policy returns DispatchDecisions, and SchedulerCore executes them
/// by performing actual context switches.
pub struct SchedulerCore {
    /// The active scheduling policy (swappable at boot time)
    policy: Box<dyn Scheduler>,

    /// Per-CPU scheduling state
    per_cpu: Vec<PerCpuSchedState>,
}

impl SchedulerCore {
    /// Create a new SchedulerCore with the given policy
    ///
    /// # Arguments
    /// - `policy`: The scheduling policy to use (RoundRobin, MLFQ, etc.)
    /// - `cpu_count`: Number of CPUs in the system (currently always 1)
    pub fn new(policy: Box<dyn Scheduler>, cpu_count: usize) -> Self {
        let per_cpu = (0..cpu_count)
            .map(|i| PerCpuSchedState::new(CpuId(i as u32)))
            .collect();

        log::info!("SchedulerCore initialized with policy: {}", policy.name());
        log::info!("Managing {} CPU(s)", cpu_count);

        Self { policy, per_cpu }
    }

    /// Get the name of the active policy
    pub fn policy_name(&self) -> &'static str {
        self.policy.name()
    }

    // ========================================================================
    // EXTERNAL API - What the rest of the kernel calls
    // ========================================================================

    /// Notify that a new thread was created
    ///
    /// Called by ThreadManager::spawn() after allocating thread resources.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: The new thread's ID
    /// - `priority`: Initial priority for the thread
    pub fn thread_created(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        tid: ThreadId,
        priority: Priority,
    ) {
        let event = SchedEvent::ThreadCreated { tid, priority };
        self.policy.on_event(ctx, event.clone());

        // Check if we should reschedule immediately
        if event.should_reschedule_immediately() {
            self.per_cpu[0].request_reschedule();
        }
    }

    /// Notify that a blocked thread became runnable
    ///
    /// Called when I/O completes, IPC message arrives, lock is released, etc.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: Thread that became runnable
    /// - `reason`: Why it was blocked (for accounting/priority boost)
    pub fn thread_woke(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        tid: ThreadId,
        reason: BlockReason,
    ) {
        let event = SchedEvent::ThreadWoke {
            tid,
            was_blocked_on: reason,
        };
        self.policy.on_event(ctx, event.clone());

        // High-priority thread waking might preempt current thread
        if event.should_reschedule_immediately() {
            self.per_cpu[0].request_reschedule();
        }
    }

    /// Notify that a thread voluntarily yielded the CPU
    ///
    /// Called by SchedulerManager::yield_now() or when a thread
    /// explicitly calls yield.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: Thread that yielded
    pub fn thread_yielded(&mut self, ctx: &mut dyn KernelSchedCtx, tid: ThreadId) {
        let event = SchedEvent::ThreadYielded { tid };
        self.policy.on_event(ctx, event);

        // Always reschedule after yield
        self.per_cpu[0].request_reschedule();
    }

    /// Notify that a thread became blocked
    ///
    /// Called when a thread waits for I/O, IPC, locks, etc.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: Thread that blocked
    /// - `reason`: What it's waiting for
    pub fn thread_blocked(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        tid: ThreadId,
        reason: BlockReason,
    ) {
        let event = SchedEvent::ThreadBlocked { tid, reason };
        self.policy.on_event(ctx, event);

        // If current thread blocked, must reschedule
        if Some(tid) == self.per_cpu[0].current_thread {
            self.per_cpu[0].request_reschedule();
        }
    }

    /// Notify that a thread exited
    ///
    /// Called by ThreadManager::exit() before cleaning up resources.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: Thread that exited
    /// - `exit_code`: Exit status
    pub fn thread_exited(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        tid: ThreadId,
        exit_code: i32,
    ) {
        let event = SchedEvent::ThreadExited { tid, exit_code };
        self.policy.on_event(ctx, event);

        // If current thread exited, must reschedule
        if Some(tid) == self.per_cpu[0].current_thread {
            self.per_cpu[0].request_reschedule();
        }
    }

    /// Notify that a thread's priority changed
    ///
    /// Called by syscalls or kernel code that adjusts thread priorities.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `tid`: Thread whose priority changed
    /// - `old_priority`: Previous priority
    /// - `new_priority`: New priority
    pub fn thread_priority_changed(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        tid: ThreadId,
        old_priority: Priority,
        new_priority: Priority,
    ) {
        let event = SchedEvent::PriorityChanged {
            tid,
            old_priority,
            new_priority,
        };
        self.policy.on_event(ctx, event.clone());

        // Priority inversion might require immediate reschedule
        if event.should_reschedule_immediately() {
            self.per_cpu[0].request_reschedule();
        }
    }

    /// Notify that a critical process signaled it's ready
    ///
    /// Called by sys_process_ready() during boot mode.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `pid`: Process that became ready
    pub fn process_ready(&mut self, ctx: &mut dyn KernelSchedCtx, pid: ProcessId) {
        let event = SchedEvent::ProcessReady { pid };
        self.policy.on_event(ctx, event);
    }

    /// Notify that scheduler mode changed
    ///
    /// Called when transitioning from Boot to Normal mode.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `old_mode`: Previous mode
    /// - `new_mode`: New mode
    pub fn mode_changed(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        old_mode: super::SchedulerMode,
        new_mode: super::SchedulerMode,
    ) {
        let event = SchedEvent::ModeChanged { old_mode, new_mode };
        self.policy.on_event(ctx, event);

        // Mode change might affect what threads can run
        self.per_cpu[0].request_reschedule();
    }

    // ========================================================================
    // TIMER INTERRUPT HANDLING
    // ========================================================================

    /// Handle timer interrupt (tick)
    ///
    /// Called by the timer interrupt handler (IRQ0) every ~10ms.
    /// Updates timeslice accounting and asks policy if we should preempt.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `cpu`: Which CPU had the timer interrupt (always CPU 0 for now)
    ///
    /// # Returns
    /// true if a reschedule should occur, false otherwise
    pub fn on_tick(&mut self, ctx: &mut dyn KernelSchedCtx, cpu: CpuId) -> bool {
        let cpu_idx = cpu.as_usize();
        let cpu_state = &mut self.per_cpu[cpu_idx];

        cpu_state.total_ticks += 1;

        // Decrement timeslice if a thread is running
        if cpu_state.current_thread.is_some() && cpu_state.timeslice_remaining > 0 {
            cpu_state.timeslice_remaining -= 1;
        }

        // Notify policy about tick
        let event = SchedEvent::Tick {
            cpu,
            current_thread: cpu_state.current_thread,
        };
        self.policy.on_event(ctx, event);

        // Check if we should reschedule
        cpu_state.should_reschedule()
    }

    // ========================================================================
    // CONTEXT SWITCH EXECUTION
    // ========================================================================

    /// Perform a context switch
    ///
    /// Asks the policy which thread to run next, then updates the
    /// current thread pointer and timeslice.
    ///
    /// The actual assembly-level context switch is performed by the caller
    /// (usually the interrupt handler) using the returned ThreadId.
    ///
    /// # Arguments
    /// - `ctx`: Kernel context for policy to query state
    /// - `cpu`: Which CPU is switching (always CPU 0 for now)
    ///
    /// # Returns
    /// - Some(ThreadId): Switch to this thread
    /// - None: No runnable threads, CPU should idle
    pub fn reschedule(
        &mut self,
        ctx: &mut dyn KernelSchedCtx,
        cpu: CpuId,
    ) -> Option<ThreadId> {
        let cpu_idx = cpu.as_usize();
        let cpu_state = &mut self.per_cpu[cpu_idx];

        // Clear the reschedule flag
        cpu_state.clear_reschedule();

        // Save the previous thread
        let prev_thread = cpu_state.current_thread;

        // Ask policy which thread to run next
        let decision = self.policy.pick_next(ctx, cpu);

        // Update current thread and timeslice
        cpu_state.current_thread = decision.next;
        cpu_state.timeslice_remaining = decision.timeslice.get();

        // Notify policy that switch completed
        if prev_thread != decision.next {
            cpu_state.context_switches += 1;
            self.policy
                .on_switched(ctx, cpu, prev_thread, decision.next);
        }

        decision.next
    }

    /// Get the current thread running on a CPU
    ///
    /// # Arguments
    /// - `cpu`: Which CPU to query (always CPU 0 for now)
    ///
    /// # Returns
    /// The currently running thread, or None if idle
    pub fn current_thread(&self, cpu: CpuId) -> Option<ThreadId> {
        self.per_cpu[cpu.as_usize()].current_thread
    }

    /// Check if a CPU needs to reschedule
    ///
    /// # Arguments
    /// - `cpu`: Which CPU to query (always CPU 0 for now)
    ///
    /// # Returns
    /// true if reschedule is needed, false otherwise
    pub fn should_reschedule(&self, cpu: CpuId) -> bool {
        self.per_cpu[cpu.as_usize()].should_reschedule()
    }

    // ========================================================================
    // STATISTICS AND DEBUGGING
    // ========================================================================

    /// Get total context switches performed
    pub fn context_switch_count(&self, cpu: CpuId) -> u64 {
        self.per_cpu[cpu.as_usize()].context_switches
    }

    /// Get total ticks elapsed
    pub fn total_ticks(&self, cpu: CpuId) -> u64 {
        self.per_cpu[cpu.as_usize()].total_ticks
    }

    /// Get timeslice remaining for current thread
    pub fn timeslice_remaining(&self, cpu: CpuId) -> u32 {
        self.per_cpu[cpu.as_usize()].timeslice_remaining
    }
}

// ============================================================================
// DEBUG IMPLEMENTATION
// ============================================================================

impl core::fmt::Debug for SchedulerCore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SchedulerCore")
            .field("policy", &self.policy.name())
            .field("cpu_count", &self.per_cpu.len())
            .field("per_cpu", &self.per_cpu)
            .finish()
    }
}
