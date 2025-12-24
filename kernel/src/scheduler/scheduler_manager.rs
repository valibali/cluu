/*
 * Scheduler Manager
 *
 * This module provides the SchedulerManager ZST which offers a clean API for
 * controlling the scheduler and performing scheduling operations.
 */

use core::arch::asm;
use core::sync::atomic::Ordering;

use alloc::boxed::Box;

use super::{
    KernelComponent, Process, ProcessId, ProcessInitState, ProcessType, SchedulerMode, ThreadId,
    ThreadManager, ThreadState, CURRENT_THREAD_ID, SCHEDULER, SCHEDULER_CORE, SCHEDULER_ENABLED,
    SchedulerCore, RoundRobinPolicy, CpuId,
};

/// Scheduling control and system state
///
/// This manager provides namespaced methods for scheduler operations.
/// All methods internally access the global scheduler singleton using helper functions.
///
/// SchedulerManager is a Zero-Sized Type (ZST) that provides organizational structure
/// without any runtime cost. It groups scheduling control operations under a clear namespace.
///
/// Note: This is different from the internal `scheduler::Scheduler` struct which holds
/// the actual scheduler data. This manager provides the public API.
///
/// # Examples
///
/// ```rust
/// // Initialize the scheduler
/// SchedulerManager::init();
///
/// // Yield to another thread
/// SchedulerManager::yield_now();
///
/// // Sleep for 100ms
/// SchedulerManager::sleep_ms(100);
/// ```
pub struct SchedulerManager;

// ================================================================================================
// KERNEL COMPONENT TRAIT IMPLEMENTATION
// ================================================================================================

impl KernelComponent for SchedulerManager {
    /// Initialize the scheduler
    ///
    /// Creates the scheduler instance with boot mode enabled, creates the
    /// SchedulerCore with RoundRobinPolicy, and initializes the default
    /// kernel process (PID 0).
    fn init() {
        log::info!("Initializing preemptive scheduler with plugin architecture...");

        let mut scheduler = super::scheduler::Scheduler::new();

        // Create default kernel process (PID 0) for kernel threads
        // Kernel process is System type and always ready
        let mut kernel_process =
            Process::new_kernel(ProcessId(0), "kernel".into(), ProcessType::System);
        kernel_process.init_state = ProcessInitState::Ready; // Kernel is always ready
        scheduler.processes.insert(ProcessId(0), kernel_process);
        log::info!("Created default kernel process (PID 0, System type)");

        // Create the scheduling policy (Round-Robin)
        let policy: Box<dyn super::Scheduler> = Box::new(RoundRobinPolicy::new());
        let policy_name = policy.name();
        log::info!("Created scheduling policy: {}", policy_name);

        // Create the SchedulerCore with the policy
        let core = SchedulerCore::new(policy, 1); // 1 CPU for now

        // CRITICAL: Disable interrupts to prevent timer IRQ from trying to acquire lock
        x86_64::instructions::interrupts::without_interrupts(|| {
            *SCHEDULER.lock() = Some(scheduler);
            *SCHEDULER_CORE.lock() = Some(core);
        });

        // Initialize I/O wait queue system
        super::io_wait::init();

        log::info!("Scheduler initialized in BOOT MODE");
    }
}

// ================================================================================================
// MANAGER IMPLEMENTATIONS
// ================================================================================================

impl SchedulerManager {
    /// Initialize the scheduler
    ///
    /// Creates the scheduler instance with boot mode enabled and initializes
    /// the default kernel process (PID 0).
    ///
    /// This is a convenience wrapper around the trait implementation.
    pub fn init() {
        <Self as KernelComponent>::init()
    }

    /// Enable preemptive scheduling
    ///
    /// Spawns the built-in idle thread and enables timer-based context switching.
    /// After calling this, timer interrupts will start performing context switches.
    pub fn enable() {
        // Spawn the idle thread - it will run when no other threads are ready
        ThreadManager::spawn(super::idle_thread_main, "idle");
        log::info!("Idle thread created");
        log::info!("Scheduler enabled - preemptive multitasking active");
        log::info!("Terminated threads will be cleaned up immediately on context switch");

        // Enable preemptive scheduling
        // CRITICAL: This MUST be done AFTER all logging above!
        // Once enabled, timer interrupts can preempt us, and if another thread
        // tries to log while we're holding the log lock, we'll deadlock.
        SCHEDULER_ENABLED.store(true, Ordering::SeqCst);
    }

    /// Check if the scheduler is enabled
    ///
    /// Returns true if the scheduler has been initialized and is running.
    /// During early boot (before scheduler initialization), this returns false.
    pub fn is_enabled() -> bool {
        SCHEDULER_ENABLED.load(Ordering::SeqCst)
    }

    /// Voluntarily yield the CPU to the next ready thread
    ///
    /// With preemptive scheduling, this function triggers a software interrupt (INT 0x81)
    /// that performs the same context switch as the timer interrupt, but voluntarily.
    /// This provides backward compatibility while using the interrupt-based mechanism.
    ///
    /// # When to Use
    ///
    /// - Threads can call yield_now() to voluntarily give up CPU
    /// - Useful in busy-wait loops to let other threads run
    /// - Will be preempted anyway if they don't yield
    /// - Kernel idle loop can use this before enabling preemption
    pub fn yield_now() {
        // Early exit if scheduler is not yet enabled (during boot)
        if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
            crate::utils::debug::irq_log::irq_log_simple("[YIELD] scheduler not enabled");
            return;
        }

        // Don't yield if interrupts are disabled - this could indicate we're
        // in a critical section that shouldn't be interrupted
        if !crate::arch::x86_64::interrupts::are_enabled() {
            crate::utils::debug::irq_log::irq_log_simple("[YIELD] interrupts disabled!");
            return;
        }

        crate::utils::debug::irq_log::irq_log_simple("[YIELD] yielding...");

        // Flush log buffer before yielding to ensure logs appear promptly
        crate::utils::debug::log_buffer::flush();

        // Trigger software interrupt to perform context switch
        // This uses the same interrupt-based mechanism as timer preemption
        // INT 0x81 is handled by yield_interrupt_handler() which:
        // 1. Saves all registers + interrupt frame
        // 2. Calls schedule_from_interrupt()
        // 3. Restores next thread's context
        // 4. Returns via iretq
        unsafe {
            asm!("int 0x81", options(nostack));
        }
    }

    /// Sleep for a number of milliseconds (blocking)
    ///
    /// This function implements true blocking sleep by marking the thread
    /// as sleeping and removing it from active scheduling. The sleeping
    /// thread consumes 0% CPU during sleep.
    ///
    /// The actual sleep time may be slightly longer than requested due to:
    /// - Timer resolution (currently 10ms)
    /// - Scheduling overhead
    ///
    /// # Arguments
    /// * `ms` - Number of milliseconds to sleep
    pub fn sleep_ms(ms: u64) {
        if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
            // Scheduler not enabled, fall back to busy-wait with hlt
            let start = crate::utils::timer::uptime_ms();
            while crate::utils::timer::uptime_ms() - start < ms {
                x86_64::instructions::hlt();
            }
            return;
        }

        let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));
        if current_id.0 == 0 {
            // Can't sleep in kernel/idle thread context
            return;
        }

        // Set the thread's sleep timer
        // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut sched_guard = SCHEDULER.lock();
            if let Some(scheduler) = sched_guard.as_mut() {
                if let Some(thread) = scheduler
                    .threads
                    .iter_mut()
                    .find(|t| t.id == current_id)
                {
                    let wake_time = crate::utils::timer::uptime_ms() + ms;
                    thread.sleep_until_ms = wake_time;
                }
            }
        });

        // Yield to switch to another thread
        // The scheduler will not reschedule us until sleep time expires
        Self::yield_now();
    }

    /// Block the current thread
    ///
    /// Sets the current thread's state to Blocked and notifies the scheduling policy.
    /// The thread will not be scheduled again until wake() is called.
    ///
    /// This is typically used for blocking I/O operations where a thread needs to
    /// wait for an external event (like keyboard input or timer expiry).
    ///
    /// # Safety
    /// The caller must ensure that some mechanism exists to eventually wake this thread,
    /// otherwise it will be blocked forever.
    pub fn block_current() {
        if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
            return;
        }

        let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));

        if current_id.0 == 0 {
            panic!("Cannot block idle thread");
        }

        super::with_scheduler_and_core(|scheduler, core| {
            // Mark as blocked
            if let Some(thread) = scheduler.threads.iter_mut().find(|t| t.id == current_id) {
                thread.state = ThreadState::Blocked;
            }

            // Notify policy
            let mut ctx = super::SchedContext::new(scheduler, CpuId::BSP);
            core.thread_blocked(&mut ctx, current_id, super::BlockReason::Other);
        });
    }

    /// Wake a blocked thread
    ///
    /// Moves the specified thread from Blocked state to Ready state and notifies
    /// the scheduling policy. If the thread is not blocked, this is a no-op.
    ///
    /// This function is IRQ-safe and can be called from interrupt handlers.
    ///
    /// # Arguments
    /// * `thread_id` - The ID of the thread to wake up
    pub fn wake(thread_id: ThreadId) {
        if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
            return;
        }

        super::with_scheduler_and_core(|scheduler, core| {
            // Check if thread is blocked and mark as ready if so
            let was_blocked = if let Some(thread) = scheduler.threads.iter_mut().find(|t| t.id == thread_id) {
                if thread.state == ThreadState::Blocked {
                    log::debug!("wake: Waking thread {} ({})", thread_id.0, thread.name);
                    thread.state = ThreadState::Ready;
                    true
                } else {
                    log::debug!("wake: Thread {} already in state {:?}", thread_id.0, thread.state);
                    false
                }
            } else {
                log::warn!("wake: Thread {} not found", thread_id.0);
                false
            };

            // Notify policy if thread was woken
            if was_blocked {
                let mut ctx = super::SchedContext::new(scheduler, CpuId::BSP);
                core.thread_woke(&mut ctx, thread_id, super::BlockReason::Other);
                log::debug!("wake: Notified policy about thread {}", thread_id.0);
            }
        });
    }

    /// Get the current scheduler operating mode
    ///
    /// Returns the current mode (Boot or Normal).
    pub fn mode() -> SchedulerMode {
        x86_64::instructions::interrupts::without_interrupts(|| {
            SCHEDULER
                .lock()
                .as_ref()
                .map(|s| s.mode())
                .unwrap_or(SchedulerMode::Boot {
                    critical_count: 0,
                    ready_count: 0,
                })
        })
    }

    /// Register a critical process that must initialize before normal mode
    ///
    /// Critical processes are the only ones scheduled during boot mode.
    /// Once all critical processes signal ready, the scheduler transitions to normal mode.
    ///
    /// This should be called immediately after spawning a critical process.
    ///
    /// # Arguments
    /// * `process_id` - Process ID of the critical process
    pub fn register_critical(process_id: ProcessId) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            if let Some(scheduler) = SCHEDULER.lock().as_mut() {
                scheduler.register_critical_process(process_id);
            }
        })
    }

    /// Process signals it has completed initialization
    ///
    /// Called by critical processes via sys_process_ready() when they have finished
    /// initialization and are ready to serve requests.
    ///
    /// When all critical processes have signaled ready, the scheduler automatically
    /// transitions from Boot mode to Normal mode, allowing user processes to run.
    ///
    /// # Arguments
    /// * `process_id` - Process ID signaling ready
    ///
    /// # Returns
    /// Ok if signaled successfully, Err if process is not critical or already signaled
    pub fn signal_ready(process_id: ProcessId) -> Result<(), &'static str> {
        x86_64::instructions::interrupts::without_interrupts(|| {
            // Get old mode before signaling
            let old_mode = SCHEDULER
                .lock()
                .as_ref()
                .map(|s| s.mode())
                .ok_or("Scheduler not initialized")?;

            // Signal process ready (may trigger mode transition)
            SCHEDULER
                .lock()
                .as_mut()
                .ok_or("Scheduler not initialized")?
                .signal_process_ready(process_id)?;

            // Get new mode after signaling
            let new_mode = SCHEDULER
                .lock()
                .as_ref()
                .map(|s| s.mode())
                .ok_or("Scheduler not initialized")?;

            // If mode changed, notify SchedulerCore
            if old_mode != new_mode {
                super::with_scheduler_and_core(|scheduler, core| {
                    let mut ctx = super::SchedContext::new(scheduler, CpuId::BSP);
                    core.mode_changed(&mut ctx, old_mode, new_mode);
                });
            }

            Ok(())
        })
    }
}
