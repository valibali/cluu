//! Thread Manager - Global Thread Scheduling
//!
//! This module provides the global scheduler instance and thread management.
//!
//! # Scheduler Modes
//!
//! - **INITMODE**: Cooperative scheduling for critical processes during boot
//! - **NORMALMODE**: Preemptive scheduling for normal operation

use crate::architecture::x86_64::gdt::set_tss_rsp0;
use crate::sched::{
    Context, Priority, PriorityBitmapScheduler, SchedulingPolicy, Thread, ThreadId,
    ThreadRepository,
};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::PhysAddr;

// ═══════════════════════════════════════════════════════════════════════════
// Scheduler Mode
// ═══════════════════════════════════════════════════════════════════════════

/// Scheduler execution mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerMode {
    /// Cooperative scheduling during boot (no preemption)
    Init,
    /// Preemptive scheduling during normal operation
    Normal,
}

// ═══════════════════════════════════════════════════════════════════════════
// Global State
// ═══════════════════════════════════════════════════════════════════════════

lazy_static! {
    /// Global thread repository
    static ref THREAD_REPOSITORY: Mutex<ThreadRepository> = Mutex::new(ThreadRepository::new());

    /// Global scheduler instance
    static ref SCHEDULER: Mutex<PriorityBitmapScheduler> = Mutex::new(PriorityBitmapScheduler::new());

    /// Currently running thread
    static ref CURRENT_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);
}

/// Current scheduler mode (starts in INITMODE)
static SCHEDULER_MODE: AtomicBool = AtomicBool::new(false); // false = INIT, true = NORMAL

/// Number of critical processes still initializing
static CRITICAL_PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);

// ═══════════════════════════════════════════════════════════════════════════
// Thread Manager API
// ═══════════════════════════════════════════════════════════════════════════

/// Thread Manager - Zero-Sized Type for namespaced access
pub struct ThreadManager;

impl ThreadManager {
    /// Create a new thread and add to repository
    ///
    /// # Arguments
    ///
    /// * `thread` - Thread to add
    ///
    /// # Returns
    ///
    /// ThreadId of the added thread
    pub fn add_thread(thread: Thread) -> ThreadId {
        let thread_id = thread.id;
        let priority = thread.priority;

        // Insert into repository
        let mut repo = THREAD_REPOSITORY.lock();
        repo.insert(thread).expect("Failed to insert thread");
        drop(repo);

        // Add to scheduler
        let mut scheduler = SCHEDULER.lock();
        scheduler.add(thread_id, priority);

        thread_id
    }

    /// Access a thread by ID (immutable)
    pub fn with_thread<F, R>(id: ThreadId, f: F) -> Option<R>
    where
        F: FnOnce(&Thread) -> R,
    {
        let repo = THREAD_REPOSITORY.lock();
        repo.get(id).map(f)
    }

    /// Modify a thread by ID
    pub fn with_thread_mut<F, R>(id: ThreadId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Thread) -> R,
    {
        let mut repo = THREAD_REPOSITORY.lock();
        repo.get_mut(id).map(f)
    }

    /// Allocate a new ThreadId
    pub fn alloc_thread_id() -> ThreadId {
        let mut repo = THREAD_REPOSITORY.lock();
        repo.alloc_id()
    }

    /// Pick the next thread to run
    ///
    /// Returns None if no threads are ready.
    pub fn pick_next() -> Option<ThreadId> {
        let mut scheduler = SCHEDULER.lock();
        scheduler.pick_next()
    }

    /// Yield current thread (add back to scheduler)
    pub fn yield_current() {
        let current = {
            let mut current_lock = CURRENT_THREAD.lock();
            current_lock.take()
        };

        if let Some(thread_id) = current {
            // Get thread priority and add back to scheduler
            let priority = {
                let repo = THREAD_REPOSITORY.lock();
                repo.get(thread_id).map(|t| t.priority)
            };

            if let Some(priority) = priority {
                let mut scheduler = SCHEDULER.lock();
                scheduler.add(thread_id, priority);
            }
        }
    }

    /// Set the currently running thread
    pub fn set_current(thread_id: ThreadId) {
        let mut current = CURRENT_THREAD.lock();
        *current = Some(thread_id);
    }

    /// Get the currently running thread
    pub fn current() -> Option<ThreadId> {
        let current = CURRENT_THREAD.lock();
        *current
    }

    /// Get page table root (CR3) of currently running thread
    pub fn current_page_table_root() -> Option<PhysAddr> {
        let thread_id = Self::current()?;
        let repo = THREAD_REPOSITORY.lock();
        repo.get(thread_id).map(|t| t.page_table_root)
    }

    /// Get current scheduler mode
    pub fn mode() -> SchedulerMode {
        if SCHEDULER_MODE.load(Ordering::Acquire) {
            SchedulerMode::Normal
        } else {
            SchedulerMode::Init
        }
    }

    /// Register a critical process (call during bootstrap)
    pub fn register_critical_thread(_thread_id: ThreadId) {
        CRITICAL_PROCESS_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Signal that a critical process has completed initialization
    ///
    /// When the last critical process signals, switches to NORMALMODE.
    ///
    /// Returns true if this was the last critical process and mode switched.
    pub fn signal_critical_process_ready() -> bool {
        let prev =
            CRITICAL_PROCESS_COUNT.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                if count == 0 {
                    None
                } else {
                    Some(count - 1)
                }
            });

        match prev {
            Ok(remaining) => {
                if remaining == 1 {
                    // This was the last critical process
                    klibcluu::info("========================================");
                    klibcluu::info("All critical processes initialized");
                    klibcluu::info("Switching to NORMALMODE (preemptive)");
                    klibcluu::info("========================================");

                    SCHEDULER_MODE.store(true, Ordering::Release);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Get number of critical processes still initializing
    pub fn critical_processes_remaining() -> usize {
        CRITICAL_PROCESS_COUNT.load(Ordering::SeqCst)
    }

    /// Check if running in INITMODE (cooperative)
    pub fn is_init_mode() -> bool {
        Self::mode() == SchedulerMode::Init
    }

    /// Check if running in NORMALMODE (preemptive)
    pub fn is_normal_mode() -> bool {
        Self::mode() == SchedulerMode::Normal
    }

    /// Handle timer tick (only in NORMALMODE)
    pub fn tick() {
        if Self::is_normal_mode() {
            let mut scheduler = SCHEDULER.lock();
            scheduler.tick();
        }
    }

    /// Start the scheduler and jump to the first thread
    ///
    /// This function never returns - it jumps to userspace.
    ///
    /// # Safety
    ///
    /// Must be called with interrupts disabled.
    /// Must have at least one thread in the scheduler.
    pub unsafe fn start() -> ! {
        klibcluu::info("Starting scheduler...");

        // Pick the first thread to run
        let thread_id = Self::pick_next().expect("No threads to schedule");

        klibcluu::info("Jumping to first thread: ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", thread_id.as_u64());

        // Set as current
        Self::set_current(thread_id);

        // Get thread context
        let context =
            Self::with_thread(thread_id, |t| t.context.clone()).expect("Thread disappeared");

        // Jump to thread context
        jump_to_thread(&context);
    }

    /// Called from syscall_entry.asm to perform context switch
    ///
    /// Saves current thread's context and returns pointer to next thread's context.
    ///
    /// # Returns
    ///
    /// Pointer to next thread's Context, or null if no switch needed
    #[no_mangle]
    pub unsafe extern "C" fn schedule_and_switch(
        current_ctx_ptr: *const Context,
    ) -> *const Context {
        // Get current thread ID
        let current_id = match Self::current() {
            Some(id) => id,
            None => {
                klibcluu::error("schedule_and_switch: No current thread!");
                return core::ptr::null();
            }
        };

        // Save current thread's context
        if !current_ctx_ptr.is_null() {
            Self::save_context(current_id, &*current_ctx_ptr);
        }

        // Re-queue current thread for next scheduling round
        Self::requeue_thread(current_id);

        // Pick next thread
        let next_id = match Self::pick_next() {
            Some(id) => id,
            None => {
                klibcluu::warn("No threads ready, returning to current");
                return Self::get_context_ptr(current_id);
            }
        };

        // No switch needed if same thread
        if next_id == current_id {
            return core::ptr::null();
        }

        klibcluu::trace("Context switch: thread ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, " -> ", current_id.as_u64());
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", next_id.as_u64());

        // Set next thread as current
        Self::set_current(next_id);

        // Return pointer to next thread's context
        Self::get_context_ptr(next_id)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Private Helper Functions
    // ═══════════════════════════════════════════════════════════════════════

    /// Save context to thread structure
    fn save_context(thread_id: ThreadId, context: &Context) {
        Self::with_thread_mut(thread_id, |thread| {
            thread.context = *context;
        });
    }

    /// Re-queue thread back to scheduler
    fn requeue_thread(thread_id: ThreadId) {
        let priority = Self::with_thread(thread_id, |t| t.priority).unwrap_or(Priority(100));
        let mut scheduler = SCHEDULER.lock();
        scheduler.add(thread_id, priority);
    }

    /// Get pointer to thread's context
    fn get_context_ptr(thread_id: ThreadId) -> *const Context {
        Self::with_thread(thread_id, |thread| &thread.context as *const Context)
            .unwrap_or(core::ptr::null())
    }
}

/// Jump to a thread's context (initial userspace entry via iretq)
///
/// # Safety
///
/// Must be called with interrupts disabled and valid context
unsafe fn jump_to_thread(context: &Context) -> ! {
    klibcluu::trace("jump_to_thread: entering userspace at RIP 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.rip);

    setup_kernel_stack();
    load_address_space(context.cr3);
    klibcluu::info("Entering userspace...");
    enter_userspace(context);
}

/// Setup kernel stack for SYSCALL and interrupts
///
/// Updates PerCpuData.kernel_rsp (for SYSCALL) and TSS.RSP0 (for interrupts)
/// to point to the current thread's kernel stack.
///
/// # SMP Design
///
/// - Each CPU has its own PerCpuData (GS points to it)
/// - Each thread will have its own kernel stack (TODO: Phase 8)
/// - Before running a thread: Update PerCpuData.kernel_rsp to thread's stack
///
/// # Current Implementation
///
/// All threads share BSP_STACK (single-CPU, single kernel stack)
unsafe fn setup_kernel_stack() {
    extern "C" {
        static BSP_STACK: u8;
    }
    let kernel_stack_top = (&raw const BSP_STACK as u64) + (64 * 1024);

    // Update PerCpuData.kernel_rsp (for SYSCALL path)
    crate::architecture::x86_64::syscall::set_current_thread_kernel_stack(kernel_stack_top);

    // Update TSS.RSP0 (for interrupt/exception path)
    set_tss_rsp0(kernel_stack_top);

    // Verify kernel stack is set correctly
    let verified = crate::architecture::x86_64::syscall::get_current_kernel_stack();
    if verified != kernel_stack_top {
        klibcluu::error("FATAL: Kernel stack setup failed!");
        loop {
            x86_64::instructions::hlt();
        }
    }
}

/// Load address space by switching CR3
///
/// Switches to the thread's page table. Kernel mappings must be present
/// in the new address space for continued execution.
unsafe fn load_address_space(cr3: u64) {
    use x86_64::registers::control::{Cr3, Cr3Flags};
    use x86_64::structures::paging::PhysFrame;
    use x86_64::PhysAddr;

    let frame = PhysFrame::containing_address(PhysAddr::new(cr3));
    Cr3::write(frame, Cr3Flags::empty());
}

/// Enter userspace via iretq
///
/// Builds interrupt frame on stack and executes iretq to switch to Ring 3.
unsafe fn enter_userspace(context: &Context) -> ! {
    klibcluu::trace("Executing iretq to userspace");

    core::arch::asm!(
        "push {0}",      // SS
        "push {1}",      // RSP
        "push {2}",      // RFLAGS
        "push {3}",      // CS
        "push {4}",      // RIP
        "iretq",
        in(reg) context.ss,
        in(reg) context.rsp,
        in(reg) context.rflags,
        in(reg) context.cs,
        in(reg) context.rip,
        options(noreturn)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_tracking() {
        assert_eq!(ThreadManager::mode(), SchedulerMode::Init);
        assert!(ThreadManager::is_init_mode());
        assert!(!ThreadManager::is_normal_mode());
    }

    #[test]
    fn test_critical_process_counting() {
        let tid1 = ThreadId::new(1);
        let tid2 = ThreadId::new(2);
        let tid3 = ThreadId::new(3);

        ThreadManager::register_critical_thread(tid1);
        ThreadManager::register_critical_thread(tid2);
        ThreadManager::register_critical_thread(tid3);

        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(ThreadManager::signal_critical_process_ready()); // Last one

        assert!(ThreadManager::is_normal_mode());
    }
}
