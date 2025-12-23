/*
 * Thread Management
 *
 * This module defines the Thread structure and related types
 * for the preemptive scheduler.
 */

use alloc::{boxed::Box, string::String};
use alloc::string::ToString;
use core::fmt;

use super::{InterruptContext, process::ProcessId};

/// Thread identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadId(pub usize);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Thread({})", self.0)
    }
}

/// Thread state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Thread structure
///
/// Each thread has its own stack and interrupt context for preemptive scheduling.
/// The interrupt context stores all CPU registers + interrupt frame, allowing
/// threads to be switched at any time via timer interrupts or voluntary yields.
///
/// Threads belong to a Process and share that process's address space and
/// file descriptor table.
pub struct Thread {
    pub id: ThreadId,
    pub name: String,
    pub state: ThreadState,
    pub stack: Box<[u8]>,

    // Interrupt-based context for preemptive scheduling
    pub interrupt_context: InterruptContext,

    // CPU time tracking (in milliseconds)
    pub cpu_time_ms: u64,
    pub last_scheduled_time: u64,

    // Sleep tracking - if non-zero, thread is sleeping until this time
    pub sleep_until_ms: u64,

    // Process this thread belongs to
    pub process_id: ProcessId,

    // Exit code (set when thread terminates)
    // For a process's main/last thread, this becomes the process exit code
    pub exit_code: Option<i32>,
}

impl Thread {
    pub fn new(
        id: ThreadId,
        name: String,
        stack: Box<[u8]>,
        interrupt_context: InterruptContext,
        process_id: ProcessId,
    ) -> Self {
        Self {
            id,
            name,
            state: ThreadState::Ready,
            stack,
            interrupt_context,
            cpu_time_ms: 0,
            last_scheduled_time: 0,
            sleep_until_ms: 0,
            process_id,
            exit_code: None,
        }
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("stack_size", &self.stack.len())
            .finish()
    }
}

// ================================================================================================
// THREAD MANAGER
// ================================================================================================

use alloc::vec::Vec;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

/// Thread lifecycle management
///
/// This manager provides namespaced methods for thread operations.
/// All methods internally access the global scheduler singleton using helper functions.
///
/// ThreadManager is a Zero-Sized Type (ZST) that provides organizational structure
/// without any runtime cost. It groups related thread operations under a clear namespace.
pub struct ThreadManager;

impl ThreadManager {
    /// Spawn a new thread in the default kernel process
    ///
    /// This is a convenience function for creating kernel threads without
    /// explicitly managing processes. All threads created this way belong
    /// to the default kernel process (PID 0).
    ///
    /// # Arguments
    /// * `entry_point` - Function to execute in the new thread
    /// * `name` - Human-readable name for the thread
    ///
    /// # Returns
    /// The ThreadId of the newly created thread
    pub fn spawn(entry_point: fn(), name: &str) -> ThreadId {
        super::with_scheduler_mut(|s| s.create_thread(entry_point, name, ProcessId(0)))
    }

    /// Create a thread within a specific process
    ///
    /// This is the process-aware version of spawn, allowing you to
    /// specify which process the thread should belong to.
    ///
    /// # Arguments
    /// * `entry_point` - Function to execute in the new thread
    /// * `name` - Human-readable name for the thread
    /// * `process_id` - ID of the process the thread should belong to
    ///
    /// # Returns
    /// The ThreadId of the newly created thread
    pub fn spawn_in_process(
        entry_point: fn(),
        name: &str,
        process_id: ProcessId,
    ) -> ThreadId {
        super::with_scheduler_mut(|s| s.create_thread(entry_point, name, process_id))
    }

    /// Terminate the current thread with an exit code
    ///
    /// Marks the current thread as Terminated, stores the exit code, and yields.
    /// The thread will not be scheduled again. This is the proper way for a thread
    /// to exit.
    ///
    /// **Cleanup:** Thread resources (stack, etc.) are freed on the next context
    /// switch when cleanup_terminated_threads() runs. If this is the last thread
    /// in a process, the process is marked as zombie with this thread's exit code.
    /// The zombie process remains in memory until reaped by sys_waitpid.
    ///
    /// # Arguments
    /// * `exit_code` - The exit code for the thread (becomes process exit code if last thread)
    ///
    /// # Panics
    /// Panics if called from the idle thread (thread 0).
    pub fn exit(exit_code: i32) -> ! {
        let current_id = ThreadId(super::CURRENT_THREAD_ID.load(Ordering::SeqCst));

        if current_id.0 == 0 {
            panic!("Cannot exit idle thread");
        }

        log::info!(
            "Thread {} ({}) terminating with exit code {}",
            current_id.0,
            super::get_thread_name(current_id).unwrap_or_else(|| "unknown".to_string()),
            exit_code
        );

        // Mark thread as terminated and store exit code
        // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut sched_guard = super::SCHEDULER.lock();
            if let Some(scheduler) = sched_guard.as_mut() {
                if let Some(thread) = scheduler.threads.iter_mut().find(|t| t.id == current_id) {
                    thread.state = ThreadState::Terminated;
                    thread.exit_code = Some(exit_code);

                    // CRITICAL: Remove this thread from ready queue!
                    // The thread may have been added to the ready queue in previous
                    // scheduling cycles. If we don't remove it now, the scheduler
                    // will try to run the terminated thread, causing a page fault
                    // when accessing initrd from the wrong address space.
                    scheduler.ready_queue.retain(|&tid| tid != current_id);
                }
            }
        });

        // CRITICAL: Enable interrupts before yielding!
        // If called from syscall context (via sys_exit), interrupts are disabled by SYSCALL instruction.
        // yield_now() requires interrupts to be enabled to trigger the context switch.
        x86_64::instructions::interrupts::enable();

        // Yield to switch to another thread
        // We will never return here
        super::SchedulerManager::yield_now();

        // Should never reach here
        use crate::utils::debug::irq_log;
        irq_log::irq_log_str("exit_thread: RETURNED FROM yield_now() - THIS IS A BUG!\n");
        loop {
            x86_64::instructions::hlt();
        }
    }

    /// Get current thread ID
    ///
    /// Returns the ThreadId of the currently executing thread.
    pub fn current_id() -> ThreadId {
        ThreadId(super::CURRENT_THREAD_ID.load(Ordering::SeqCst))
    }

    /// Execute a closure with access to the current thread
    ///
    /// Provides safe read-only access to the current thread's data.
    /// Returns None if the scheduler is not initialized or thread not found.
    ///
    /// # Arguments
    /// * `f` - Closure that receives a reference to the current thread
    ///
    /// # Returns
    /// The result of the closure, or None if thread not found
    pub fn with_current<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&Thread) -> R,
    {
        let current_id = ThreadId(super::CURRENT_THREAD_ID.load(Ordering::SeqCst));
        super::with_scheduler(|s| s.threads.iter().find(|t| t.id == current_id).map(f))
    }

    /// Set up a thread to enter userspace at a specific entry point
    ///
    /// This function modifies the thread's interrupt context to transition
    /// from Ring 0 (kernel) to Ring 3 (userspace) when the thread is scheduled.
    ///
    /// # Arguments
    /// * `thread_id` - ID of the thread to configure
    /// * `entry_point` - Virtual address to start execution (ELF entry point)
    /// * `user_stack_top` - Top of user stack (RSP value)
    ///
    /// # Returns
    /// * `Ok(())` if thread was configured successfully
    /// * `Err(&str)` if thread not found or configuration failed
    pub fn setup_userspace(
        thread_id: ThreadId,
        entry_point: x86_64::VirtAddr,
        user_stack_top: x86_64::VirtAddr,
    ) -> Result<(), &'static str> {
        super::with_scheduler_mut(|scheduler| {
            let thread = scheduler
                .threads
                .iter_mut()
                .find(|t| t.id == thread_id)
                .ok_or("Thread not found")?;

            // Set up interrupt context for userspace entry
            // When this thread is scheduled, it will "return" to userspace via IRETQ

            // Set up interrupt frame for Ring 3 entry
            // Get user segment selectors from GDT and ensure RPL=3 is set
            let user_cs = crate::arch::x86_64::gdt::user_code_selector();
            let user_ss = crate::arch::x86_64::gdt::user_data_selector();

            // Ensure RPL (Request Privilege Level) bits are set to 3 for Ring 3
            // The low 2 bits of the selector are the RPL
            // Cast to u64 for interrupt frame (which stores segment selectors as 64-bit values)
            let user_cs_with_rpl = (user_cs.0 | 3) as u64;
            let user_ss_with_rpl = (user_ss.0 | 3) as u64;

            thread.interrupt_context.iret_frame.rip = entry_point.as_u64();
            thread.interrupt_context.iret_frame.cs = user_cs_with_rpl; // User code segment with RPL=3
            thread.interrupt_context.iret_frame.rflags = 0x202; // IF=1 (interrupts enabled), bit 1 always set
            thread.interrupt_context.iret_frame.rsp = user_stack_top.as_u64();
            thread.interrupt_context.iret_frame.ss = user_ss_with_rpl; // User data segment with RPL=3

            log::info!(
                "setup_userspace_thread: Set RSP=0x{:x}, RIP=0x{:x}",
                user_stack_top.as_u64(),
                entry_point.as_u64()
            );

            // Clear all general purpose registers for security
            thread.interrupt_context.rax = 0;
            thread.interrupt_context.rbx = 0;
            thread.interrupt_context.rcx = 0;
            thread.interrupt_context.rdx = 0;
            thread.interrupt_context.rsi = 0;
            thread.interrupt_context.rdi = 0;
            thread.interrupt_context.rbp = 0;
            thread.interrupt_context.r8 = 0;
            thread.interrupt_context.r9 = 0;
            thread.interrupt_context.r10 = 0;
            thread.interrupt_context.r11 = 0;
            thread.interrupt_context.r12 = 0;
            thread.interrupt_context.r13 = 0;
            thread.interrupt_context.r14 = 0;
            thread.interrupt_context.r15 = 0;

            log::debug!(
                "Thread {:?} configured for userspace entry at 0x{:x} (stack: 0x{:x})",
                thread_id,
                entry_point.as_u64(),
                user_stack_top.as_u64()
            );

            Ok(())
        })
    }

    /// Initialize standard streams (stdin/stdout/stderr) for a process
    ///
    /// This sets up the file descriptor table for the process that owns the
    /// given thread, with FDs 0, 1, 2 all pointing to TTY0 (the console).
    ///
    /// # Arguments
    /// * `thread_id` - A thread belonging to the process to initialize
    pub fn init_std_streams(thread_id: ThreadId) {
        use crate::io::TtyDevice;

        super::with_scheduler_mut(|scheduler| {
            // Find thread by ID to get its process_id
            let process_id = scheduler
                .threads
                .iter()
                .find(|t| t.id == thread_id)
                .map(|t| t.process_id);

            if let Some(pid) = process_id {
                if let Some(process) = scheduler.processes.get_mut(&pid) {
                    // Create TTY device for console (TTY0)
                    let tty = Arc::new(TtyDevice::new(0));

                    // Initialize stdin, stdout, stderr (all point to same TTY)
                    process.fd_table.insert(0, tty.clone()); // stdin
                    process.fd_table.insert(1, tty.clone()); // stdout
                    process.fd_table.insert(2, tty); // stderr

                    log::debug!("Initialized standard streams for process {:?}", pid);
                } else {
                    log::warn!("Cannot init std streams: process {:?} not found", pid);
                }
            } else {
                log::warn!(
                    "Cannot init std streams: thread {} not found",
                    thread_id.0
                );
            }
        })
    }

    /// Get statistics for all threads
    ///
    /// Returns a vector of ThreadStats with information about each thread
    /// including CPU time and usage percentage.
    pub fn stats() -> Vec<super::ThreadStats> {
        super::with_scheduler(|scheduler| {
            let total_uptime = crate::utils::timer::uptime_ms();
            if total_uptime == 0 {
                return Vec::new();
            }

            let current_id = ThreadId(super::CURRENT_THREAD_ID.load(Ordering::SeqCst));

            let mut stats = Vec::new();
            for thread in &scheduler.threads {
                let mut cpu_time = thread.cpu_time_ms;

                // If this is the currently running thread, add elapsed time since last scheduled
                if thread.id == current_id && thread.last_scheduled_time > 0 {
                    let current_time = crate::utils::timer::uptime_ms();
                    let elapsed = current_time.saturating_sub(thread.last_scheduled_time);
                    cpu_time = cpu_time.saturating_add(elapsed);
                }

                // Calculate CPU percentage
                let cpu_percent = if total_uptime > 0 {
                    (cpu_time * 100) / total_uptime
                } else {
                    0
                };

                stats.push(super::ThreadStats {
                    id: thread.id,
                    name: thread.name.clone(),
                    state: thread.state,
                    cpu_time_ms: cpu_time,
                    cpu_percent,
                });
            }

            stats
        })
    }
}
