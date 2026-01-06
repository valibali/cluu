//! Thread Manager - Global Thread Scheduling
//!
//! This module provides the global scheduler instance and thread management.
//!
//! # Scheduler Modes
//!
//! - **INITMODE**: Cooperative scheduling for critical processes during boot
//!   - Threads with COOPERATIVE flag don't preempt each other
//!   - No timer-based preemption
//!   - Used until all critical processes signal initialization complete
//!
//! - **NORMALMODE**: Preemptive scheduling for normal operation
//!   - Timer interrupts (APIC 250Hz) trigger preemption
//!   - All threads can be preempted
//!   - Activated after all critical processes initialized

use crate::sched::{PriorityBitmapScheduler, SchedulingPolicy, Thread, ThreadId, ThreadRepository};
use crate::architecture::x86_64::gdt::set_tss_rsp0;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

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

    /// Get current scheduler mode
    pub fn mode() -> SchedulerMode {
        if SCHEDULER_MODE.load(Ordering::Acquire) {
            SchedulerMode::Normal
        } else {
            SchedulerMode::Init
        }
    }

    /// Register a critical process (call during bootstrap)
    pub fn register_critical_thread(thread_id: ThreadId) {
        CRITICAL_PROCESS_COUNT.fetch_add(thread_id.as_u64() as usize, Ordering::SeqCst);
    }

    /// Signal that a critical process has completed initialization
    ///
    /// When the last critical process signals, switches to NORMALMODE.
    ///
    /// Returns true if this was the last critical process and mode switched.
    pub fn signal_critical_process_ready() -> bool {
        let remaining = CRITICAL_PROCESS_COUNT.fetch_sub(1, Ordering::SeqCst);

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

    /// Called from syscall_entry.asm after sys_yield
    ///
    /// Saves current thread's context and returns pointer to next thread's context.
    ///
    /// # Arguments
    ///
    /// * `current_ctx_ptr` - Pointer to Context structure on kernel stack
    ///
    /// # Returns
    ///
    /// Pointer to next thread's Context, or null if no switch needed
    ///
    /// # Safety
    ///
    /// Must be called from syscall_entry.asm with valid Context pointer.
    #[no_mangle]
    pub unsafe extern "C" fn schedule_and_switch(
        current_ctx_ptr: *const crate::sched::Context,
    ) -> *const crate::sched::Context {
        use crate::sched::{Context, Priority};

        klibcluu::trace("=== schedule_and_switch called ===");

        // Get current thread ID
        let current_id = match Self::current() {
            Some(id) => id,
            None => {
                // No current thread? This shouldn't happen
                klibcluu::error("schedule_and_switch: No current thread!");
                return core::ptr::null();
            }
        };

        klibcluu::trace("Current thread: ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", current_id.as_u64());

        // Save current thread's context
        if !current_ctx_ptr.is_null() {
            let ctx = &*current_ctx_ptr;

            klibcluu::trace("Saving current thread context:");
            klibcluu::trace("  RIP:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", ctx.rip);
            klibcluu::trace("  RSP:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", ctx.rsp);
            klibcluu::trace("  CR3:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", ctx.cr3);
            klibcluu::trace("  RFLAGS: 0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", ctx.rflags);

            Self::with_thread_mut(current_id, |thread| {
                // Copy context from kernel stack to thread structure
                thread.context = *current_ctx_ptr;
                klibcluu::trace("Context saved to thread ");
                klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread.id.as_u64());
            });
        }

        // Add current thread back to scheduler for next scheduling round
        let priority = Self::with_thread(current_id, |t| t.priority).unwrap_or(Priority(100));
        {
            let mut scheduler = SCHEDULER.lock();
            scheduler.add(current_id, priority);
        }

        // Pick next thread
        let next_id = match Self::pick_next() {
            Some(id) => id,
            None => {
                // No threads ready? Go back to current thread (shouldn't happen)
                klibcluu::warn("schedule_and_switch: No threads ready, returning to current");
                return Self::with_thread(current_id, |t| &t.context as *const Context)
                    .unwrap_or(core::ptr::null());
            }
        };

        // If next thread is same as current, no need to switch
        if next_id == current_id {
            klibcluu::trace("No context switch needed (same thread)");
            return core::ptr::null(); // Signal no switch needed
        }

        klibcluu::trace("Context switch: ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, " -> ", current_id.as_u64());
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", next_id.as_u64());

        // Set next thread as current
        Self::set_current(next_id);

        // Return pointer to next thread's context
        Self::with_thread(next_id, |thread| {
            klibcluu::trace("Loading next thread context:");
            klibcluu::trace("  Thread: ");
            klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread.id.as_u64());
            klibcluu::trace("  RIP:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.rip);
            klibcluu::trace("  RSP:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.rsp);
            klibcluu::trace("  CR3:    0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.cr3);
            klibcluu::trace("  RFLAGS: 0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.rflags);
            klibcluu::trace("  CS:     0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.cs);
            klibcluu::trace("  SS:     0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread.context.ss);
            klibcluu::trace("Returning context pointer, will SYSRET to thread");
            &thread.context as *const Context
        })
        .unwrap_or(core::ptr::null())
    }
}

/// Jump to a thread's context (initial userspace entry)
///
/// This uses iretq to switch from kernel mode to user mode.
/// Sets up the stack for iretq and jumps.
///
/// # Safety
///
/// - Must be called with interrupts disabled
/// - Context must be valid
unsafe fn jump_to_thread(context: &crate::sched::Context) -> ! {
    use x86_64::registers::control::{Cr3, Cr3Flags};
    use x86_64::structures::paging::PhysFrame;
    use x86_64::PhysAddr;

    klibcluu::trace("jump_to_thread: Preparing to jump to userspace");
    klibcluu::trace("  RIP:    0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.rip);
    klibcluu::trace("  RSP:    0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.rsp);
    klibcluu::trace("  CR3:    0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.cr3);
    klibcluu::trace("  RFLAGS: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.rflags);
    klibcluu::trace("  CS:     0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.cs);
    klibcluu::trace("  SS:     0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.ss);

    // ═══════════════════════════════════════════════════════════════════════
    // CRITICAL: Set up per-thread kernel stack (SMP-ready design)
    // ═══════════════════════════════════════════════════════════════════════
    //
    // When userspace executes SYSCALL or triggers an exception, the CPU needs
    // a kernel stack to handle it:
    //
    // 1. SYSCALL path:
    //    - syscall_entry.asm executes: mov rsp, [gs:0x08]
    //    - Loads kernel stack from PerCpuData.kernel_rsp
    //
    // 2. Exception/Interrupt path:
    //    - CPU loads TSS.RSP0 automatically
    //    - Both must point to the same stack
    //
    // SMP Design:
    // - Each CPU has its own PerCpuData (GS points to it)
    // - Each thread has its own kernel stack
    // - Before running a thread: Update PerCpuData.kernel_rsp to thread's stack
    //
    // Current (Single CPU):
    // - One global PER_CPU_DATA
    // - All threads share BSP_STACK (TODO Phase 8: per-thread stacks)
    // ═══════════════════════════════════════════════════════════════════════

    klibcluu::trace("Setting up per-thread kernel stack...");

    // Get this thread's kernel stack top
    // TODO Phase 8: Each thread should have thread.kernel_stack_top
    // For now: Use BSP_STACK for all threads (single-threaded testing)
    extern "C" {
        static BSP_STACK: u8;
    }
    let thread_kernel_stack_top = (&raw const BSP_STACK as u64) + (64 * 1024);

    // Update PerCpuData.kernel_rsp (used by syscall_entry.asm)
    unsafe {
        crate::architecture::x86_64::syscall::set_current_thread_kernel_stack(
            thread_kernel_stack_top
        );
    }

    // Update TSS.RSP0 (used by interrupt/exception handlers)
    set_tss_rsp0(thread_kernel_stack_top);

    klibcluu::trace("  Thread kernel stack top = 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", thread_kernel_stack_top);

    // ═══════════════════════════════════════════════════════════════════════
    // Verify PerCpuData.kernel_rsp is set correctly
    // ═══════════════════════════════════════════════════════════════════════

    klibcluu::trace("Verifying PerCpuData.kernel_rsp via GS:[8]...");
    let verified_kernel_rsp = unsafe {
        crate::architecture::x86_64::syscall::get_current_kernel_stack()
    };

    klibcluu::trace("  PerCpuData.kernel_rsp = 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", verified_kernel_rsp);

    // CRITICAL: Verify it's not zero (would crash on SYSCALL)
    if verified_kernel_rsp == 0 {
        klibcluu::error("FATAL: PerCpuData.kernel_rsp is 0!");
        klibcluu::error("  SYSCALL will fail - userspace cannot call kernel!");
        loop {
            x86_64::instructions::hlt();
        }
    }

    // CRITICAL: Verify it matches what we just set
    if verified_kernel_rsp != thread_kernel_stack_top {
        klibcluu::error("FATAL: PerCpuData.kernel_rsp mismatch!");
        klibcluu::error("  Expected: 0x");
        klibcluu::log_hex(klibcluu::LogLevel::Error, "", thread_kernel_stack_top);
        klibcluu::error("  Got:      0x");
        klibcluu::log_hex(klibcluu::LogLevel::Error, "", verified_kernel_rsp);
        loop {
            x86_64::instructions::hlt();
        }
    }

    klibcluu::trace("Kernel stack verification PASSED");

    // Load thread's page table (CR3)
    klibcluu::trace("Loading CR3...");
    let frame = PhysFrame::containing_address(PhysAddr::new(context.cr3));
    Cr3::write(frame, Cr3Flags::empty());
    klibcluu::trace("CR3 loaded successfully");

    // Verify we can still access kernel stack after CR3 switch
    klibcluu::trace("Verifying kernel stack accessibility after CR3 switch...");
    let mut stack_test: u64 = 0x12345678;
    core::ptr::write_volatile(&mut stack_test, 0xABCDEF);
    let verify = core::ptr::read_volatile(&stack_test);
    if verify != 0xABCDEF {
        klibcluu::error("Stack not accessible after CR3 switch!");
    } else {
        klibcluu::trace("Stack still accessible - kernel mappings OK");
    }

    // Verify user code is accessible at entry point
    klibcluu::trace("Verifying user code accessibility at RIP...");
    let code_ptr = context.rip as *const u8;
    let first_byte = core::ptr::read_volatile(code_ptr);
    klibcluu::trace("First instruction byte at entry: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", first_byte as u64);

    // Verify user stack is accessible
    klibcluu::trace("Verifying user stack accessibility...");
    let stack_ptr = (context.rsp - 8) as *const u64;
    let _ = core::ptr::read_volatile(stack_ptr);
    klibcluu::trace("User stack accessible");

    // Use iretq to jump to userspace
    // Stack layout for iretq (from top to bottom):
    // - SS (user data segment)
    // - RSP (user stack pointer)
    // - RFLAGS
    // - CS (user code segment)
    // - RIP (instruction pointer)

    klibcluu::trace("Executing iretq...");

    // CRITICAL: Must match hardware interrupt frame layout EXACTLY
    // Values must be 64-bit to match x86_64 interrupt frame
    let user_ss: u64 = context.ss;
    let user_rsp: u64 = context.rsp;
    let user_rflags: u64 = context.rflags;
    let user_cs: u64 = context.cs;
    let user_rip: u64 = context.rip;

    // Log all values before iretq
    klibcluu::trace("IRETQ frame values:");
    klibcluu::trace("  user_rip:    ");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", user_rip);
    klibcluu::trace("  user_cs:     ");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", user_cs);
    klibcluu::trace("  user_rflags: ");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", user_rflags);
    klibcluu::trace("  user_rsp:    ");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", user_rsp);
    klibcluu::trace("  user_ss:     ");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", user_ss);

    core::arch::asm!(
        // Push interrupt frame in reverse order (SS, RSP, RFLAGS, CS, RIP)
        "push {0}",      // SS
        "push {1}",      // RSP
        "push {2}",      // RFLAGS
        "push {3}",      // CS
        "push {4}",      // RIP
        "iretq",
        in(reg) user_ss,
        in(reg) user_rsp,
        in(reg) user_rflags,
        in(reg) user_cs,
        in(reg) user_rip,
        options(noreturn)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::{PhysAddr, VirtAddr};

    #[test]
    fn test_mode_tracking() {
        assert_eq!(ThreadManager::mode(), SchedulerMode::Init);
        assert!(ThreadManager::is_init_mode());
        assert!(!ThreadManager::is_normal_mode());
    }

    #[test]
    fn test_critical_process_counting() {
        ThreadManager::register_critical_process();
        ThreadManager::register_critical_process();
        ThreadManager::register_critical_process();

        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(ThreadManager::signal_critical_process_ready()); // Last one

        assert!(ThreadManager::is_normal_mode());
    }
}
