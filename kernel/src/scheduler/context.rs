/*
 * Scheduler Context - KernelSchedCtx Implementation
 *
 * This module provides SchedContext, the bridge between scheduling policies
 * and the actual kernel state. It implements the KernelSchedCtx trait,
 * providing a safe, controlled interface for policies to query and modify
 * kernel scheduling state without direct access to internal structures.
 *
 * This separation provides:
 * - Safety: Policies can't corrupt kernel data structures
 * - Testability: Can create mock implementations for unit tests
 * - Clarity: Well-defined API shows exactly what policies can do
 */

use alloc::vec::Vec;

use super::{
    ThreadId, ThreadState, ProcessId, ProcessType,
    traits::KernelSchedCtx,
    types::{CpuId, Priority, SchedClass},
    sched_core::SchedulerCore,
    SchedulerMode,
};

/// Scheduling context for policy access
///
/// This struct provides the implementation of KernelSchedCtx trait,
/// giving policies controlled access to kernel scheduling state.
///
/// # Lifetime
/// The 'a lifetime ensures that the context cannot outlive the
/// reference it holds to the scheduler.
pub struct SchedContext<'a> {
    /// Reference to the internal scheduler data (threads, processes)
    scheduler: &'a mut super::scheduler::Scheduler,

    /// Which CPU this context is for (always CPU 0 for now)
    current_cpu: CpuId,

    /// Cached reference to SchedulerCore (optional, for accessing per-CPU state)
    core: Option<&'a SchedulerCore>,
}

impl<'a> SchedContext<'a> {
    /// Create a new scheduling context
    ///
    /// # Arguments
    /// - `scheduler`: Mutable reference to the internal scheduler data
    /// - `current_cpu`: Which CPU this context represents
    pub fn new(
        scheduler: &'a mut super::scheduler::Scheduler,
        current_cpu: CpuId,
    ) -> Self {
        Self {
            scheduler,
            current_cpu,
            core: None,
        }
    }

    /// Create a context with core reference (for accessing per-CPU state)
    pub fn with_core(
        scheduler: &'a mut super::scheduler::Scheduler,
        core: &'a SchedulerCore,
        current_cpu: CpuId,
    ) -> Self {
        Self {
            scheduler,
            current_cpu,
            core: Some(core),
        }
    }

    /// Find a thread by ID
    fn find_thread(&self, tid: ThreadId) -> Option<&super::Thread> {
        self.scheduler.threads.iter().find(|t| t.id == tid)
    }

    /// Find a mutable thread by ID
    fn find_thread_mut(&mut self, tid: ThreadId) -> Option<&mut super::Thread> {
        self.scheduler.threads.iter_mut().find(|t| t.id == tid)
    }

    /// Find a process by ID
    fn find_process(&self, pid: ProcessId) -> Option<&super::Process> {
        self.scheduler.processes.get(&pid)
    }
}

impl<'a> KernelSchedCtx for SchedContext<'a> {
    // ========================================================================
    // QUERY OPERATIONS
    // ========================================================================

    fn thread_state(&self, tid: ThreadId) -> Option<ThreadState> {
        self.find_thread(tid).map(|t| t.state)
    }

    fn is_runnable(&self, tid: ThreadId) -> bool {
        if let Some(thread) = self.find_thread(tid) {
            // Check if thread is in Ready state
            if thread.state != ThreadState::Ready {
                return false;
            }

            // Check if thread is sleeping
            if thread.sleep_until_ms > 0 {
                let now = crate::utils::timer::uptime_ms();
                if now < thread.sleep_until_ms {
                    return false; // Still sleeping
                }
            }

            true
        } else {
            false
        }
    }

    fn current_thread(&self, cpu: CpuId) -> Option<ThreadId> {
        self.core.map(|c| c.current_thread(cpu)).flatten()
    }

    fn thread_priority(&self, tid: ThreadId) -> Option<Priority> {
        // Get priority from the thread's process type
        if let Some(thread) = self.find_thread(tid) {
            if let Some(process) = self.find_process(thread.process_id) {
                let priority = process.process_type.priority();
                return Some(Priority(priority as i32));
            }
        }
        None
    }

    fn thread_class(&self, _tid: ThreadId) -> Option<SchedClass> {
        // For now, all threads are Normal class
        // Future: Add SchedClass field to Thread struct
        Some(SchedClass::Normal)
    }

    fn thread_process(&self, tid: ThreadId) -> Option<ProcessId> {
        self.find_thread(tid).map(|t| t.process_id)
    }

    fn is_critical_process(&self, pid: ProcessId) -> bool {
        self.find_process(pid)
            .map(|p| p.process_type == ProcessType::Critical)
            .unwrap_or(false)
    }

    fn current_mode(&self) -> SchedulerMode {
        self.scheduler.mode()
    }

    fn now_ticks(&self) -> u64 {
        self.core
            .map(|c| c.total_ticks(self.current_cpu))
            .unwrap_or(0)
    }

    fn cpu_count(&self) -> usize {
        1 // CLUU is currently single-core
    }

    fn all_threads(&self) -> Vec<ThreadId> {
        self.scheduler.threads.iter().map(|t| t.id).collect()
    }

    // ========================================================================
    // STATE MODIFICATION
    // ========================================================================

    fn make_runnable(&mut self, tid: ThreadId) {
        if let Some(thread) = self.find_thread_mut(tid) {
            if thread.state == ThreadState::Blocked {
                thread.state = ThreadState::Ready;
            }
            // Also clear sleep timer if thread was sleeping
            thread.sleep_until_ms = 0;
        }
    }

    fn request_reschedule(&mut self, cpu: CpuId) {
        // Access the per-CPU state through SchedulerCore would require
        // adding a method to expose it. For now, this is handled by
        // SchedulerCore internally when events are processed.
        let _ = cpu; // Suppress unused warning
    }

    fn set_thread_class(&mut self, _tid: ThreadId, _class: SchedClass) {
        // Future: Add SchedClass field to Thread struct and set it here
    }

    fn set_thread_priority(&mut self, tid: ThreadId, priority: Priority) {
        // Priority is determined by ProcessType, so we'd need to either:
        // 1. Add a per-thread priority override field
        // 2. Change the process type (not recommended)
        // For now, log that this isn't fully implemented
        let _ = (tid, priority); // Suppress unused warning
    }

    // ========================================================================
    // DEBUGGING
    // ========================================================================

    fn log(&self, level: log::Level, message: &str) {
        let policy_name = self.core
            .map(|c| c.policy_name())
            .unwrap_or("unknown");

        match level {
            log::Level::Error => log::error!("[Policy:{}] {}", policy_name, message),
            log::Level::Warn => log::warn!("[Policy:{}] {}", policy_name, message),
            log::Level::Info => log::info!("[Policy:{}] {}", policy_name, message),
            log::Level::Debug => log::debug!("[Policy:{}] {}", policy_name, message),
            log::Level::Trace => log::trace!("[Policy:{}] {}", policy_name, message),
        }
    }
}

// ============================================================================
// HELPER FUNCTIONS FOR CREATING CONTEXTS
// ============================================================================

/// Create a temporary scheduling context for policy calls
///
/// This is a convenience function that can be used when you have
/// references to both the scheduler and core.
///
/// # Example
/// ```rust
/// with_scheduler_and_core(|scheduler, core| {
///     let mut ctx = create_sched_context(scheduler, core, CpuId::BSP);
///     core.thread_created(&mut ctx, tid, priority);
/// });
/// ```
pub fn create_sched_context<'a>(
    scheduler: &'a mut super::scheduler::Scheduler,
    core: &'a SchedulerCore,
    cpu: CpuId,
) -> SchedContext<'a> {
    SchedContext::with_core(scheduler, core, cpu)
}

// ============================================================================
// DEBUG IMPLEMENTATION
// ============================================================================

impl<'a> core::fmt::Debug for SchedContext<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SchedContext")
            .field("current_cpu", &self.current_cpu)
            .field("thread_count", &self.scheduler.threads.len())
            .field("process_count", &self.scheduler.processes.len())
            .field("mode", &self.scheduler.mode())
            .finish()
    }
}
