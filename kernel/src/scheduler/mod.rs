/*
 * Preemptive Round-Robin Scheduler
 *
 * This module implements a preemptive round-robin scheduler for the CLUU kernel.
 * It provides full multitasking with automatic context switching via timer interrupts.
 *
 * PREEMPTIVE SCHEDULING EXPLAINED:
 * ================================
 *
 * The OS automatically switches between threads at regular intervals (every 10ms)
 * using timer interrupts. Threads don't need to cooperate - they're forcibly
 * preempted, ensuring fair CPU time distribution and preventing thread starvation.
 *
 * KEY FEATURES:
 * ============
 *
 * 1. AUTOMATIC SWITCHING: Timer interrupt (PIT, 100Hz) triggers context switches
 * 2. FAIR SCHEDULING: Round-robin ensures all threads get equal CPU time
 * 3. VOLUNTARY YIELDING: Threads can still yield early via yield_now() (INT 0x81)
 * 4. IRQ-SAFE: Uses atomic flags instead of mutexes in interrupt context
 * 5. UNIFIED MECHANISM: Both preemptive and voluntary use same interrupt-based switching
 *
 * THREAD LIFECYCLE:
 * ================
 *
 * Ready -> Running (preempted every 10ms) -> Ready
 *   ^                                           |
 *   |                                           v
 *   +--------- Blocked (future feature) --------+
 *
 * - Ready: Thread is waiting to be scheduled
 * - Running: Thread is currently executing on CPU
 * - Blocked: Thread is waiting for some event (future feature)
 * - Terminated: Thread has finished execution (future feature)
 *
 * SCHEDULING ALGORITHM:
 * ====================
 *
 * Round-robin with preemption:
 * 1. Timer interrupt fires every 10ms (IRQ0)
 * 2. Current thread is saved and moved to back of ready queue
 * 3. Next thread from front of ready queue becomes current
 * 4. Context switch via iretq resumes next thread
 *
 * Voluntary yielding (backward compatible):
 * 1. Thread calls yield_now()
 * 2. Software interrupt (INT 0x81) triggers same handler
 * 3. Identical context switch as timer preemption
 *
 * INTERRUPT-BASED CONTEXT SWITCHING:
 * ==================================
 *
 * Context switches are performed via interrupts (hardware timer or software INT):
 * 1. CPU automatically pushes interrupt frame (RIP, CS, RFLAGS, RSP, SS)
 * 2. Handler pushes all general-purpose registers (RAX-R15)
 * 3. Scheduler picks next thread
 * 4. Handler switches to next thread's saved context
 * 5. Pops all registers and uses iretq to resume execution
 *
 * This unified approach ensures:
 * - All registers are saved/restored correctly
 * - Interrupt flag is managed properly
 * - Stack switching works seamlessly
 * - No conflicts between preemptive and voluntary switches
 *
 * BUILT-IN IDLE THREAD:
 * ====================
 *
 * The scheduler automatically creates an idle thread that runs when no other
 * threads are ready. It halts the CPU to save power between interrupts.
 *
 * Key features:
 * - Preemptive multitasking with 100Hz timer
 * - Round-robin scheduling
 * - Interrupt-based context switching (iretq)
 * - Backward-compatible voluntary yielding
 * - IRQ-safe design with atomic flags
 * - Built-in idle thread
 * - Per-thread 64KB stacks
 */

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use spin::Mutex;

mod scheduler;

pub mod io_wait;
pub mod process;
pub mod scheduler_manager;
pub mod thread;

pub use io_wait::{IoChannel, wait_for_io, wake_io_waiters};
pub use process::{Process, ProcessId, ProcessManager};
pub use scheduler::InterruptContext;
pub use scheduler_manager::SchedulerManager;
pub use thread::{Thread, ThreadId, ThreadManager, ThreadState};

/// Scheduler operating mode
///
/// The scheduler operates in different modes during system lifecycle:
/// - Boot: Only critical system services run (VFS, memory server, etc.)
/// - Normal: All processes are scheduled according to their type/priority
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerMode {
    /// Boot mode: Only critical system services run
    /// Transitions to Normal once all critical services signal ready
    Boot {
        /// Total number of critical processes that must initialize
        critical_count: usize,
        /// Number of critical processes that have signaled ready
        ready_count: usize,
    },

    /// Normal mode: All processes scheduled according to priority
    Normal,
}

/// Process type classification
///
/// Determines scheduling priority and whether the process runs during boot mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessType {
    /// Critical system service (VFS, memory_server, etc.)
    /// Only these run during boot mode
    /// High priority in normal mode
    Critical,

    /// Non-critical system service
    /// Medium priority in normal mode
    System,

    /// User process
    /// Low priority in normal mode
    User,

    /// Real-time process (future feature)
    /// Highest priority in normal mode
    RealTime { priority: u8 },
}

impl ProcessType {
    /// Get scheduling priority for this process type
    /// Higher value = higher priority
    pub fn priority(&self) -> usize {
        match self {
            ProcessType::RealTime { priority } => 1000 + (*priority as usize),
            ProcessType::Critical => 500,
            ProcessType::System => 100,
            ProcessType::User => 1,
        }
    }
}

/// Process initialization state
///
/// Tracks whether a process has completed its initialization phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInitState {
    /// Process created but not yet initialized
    Initializing,

    /// Process has signaled it's ready via sys_process_ready()
    Ready,

    /// Process failed initialization (future feature)
    Failed,
}

/// Global scheduler instance
static SCHEDULER: Mutex<Option<scheduler::Scheduler>> = Mutex::new(None);

/// Current running thread ID (atomic for IRQ safety)
static CURRENT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

/// Scheduler enabled flag
static SCHEDULER_ENABLED: AtomicBool = AtomicBool::new(false);

/// Preemption disabled flag (set during critical scheduler operations)
/// When true, timer interrupts will not perform context switches
static PREEMPTION_DISABLED: AtomicBool = AtomicBool::new(false);

// ================================================================================================
// INTERNAL HELPER FUNCTIONS
// ================================================================================================

/// Execute a closure with immutable access to the scheduler
///
/// This helper provides interrupt-safe access to the global scheduler singleton.
/// Interrupts are disabled during the closure execution to prevent timer IRQ deadlocks.
///
/// # Panics
/// Panics if the scheduler has not been initialized.
fn with_scheduler<F, R>(f: F) -> R
where
    F: FnOnce(&scheduler::Scheduler) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let sched_guard = SCHEDULER.lock();
        let scheduler = sched_guard.as_ref().expect("Scheduler not initialized");
        f(scheduler)
    })
}

/// Execute a closure with mutable access to the scheduler
///
/// This helper provides interrupt-safe mutable access to the global scheduler singleton.
/// Interrupts are disabled during the closure execution to prevent timer IRQ deadlocks.
///
/// # Panics
/// Panics if the scheduler has not been initialized.
fn with_scheduler_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut scheduler::Scheduler) -> R,
{
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched_guard = SCHEDULER.lock();
        let scheduler = sched_guard.as_mut().expect("Scheduler not initialized");
        f(scheduler)
    })
}

// ================================================================================================
// KERNEL COMPONENT TRAIT
// ================================================================================================

/// Trait for kernel components that require initialization
///
/// This trait enforces that critical kernel components must implement an `init()` function
/// to perform their initialization sequence during system boot.
///
/// Components implementing this trait typically:
/// - Set up internal state and data structures
/// - Initialize hardware or subsystems
/// - Register with other components
/// - Allocate resources needed for operation
///
/// # Example
///
/// ```rust
/// impl KernelComponent for MyComponent {
///     fn init() {
///         // Initialization logic here
///         log::info!("MyComponent initialized");
///     }
/// }
///
/// // Called during boot sequence
/// MyComponent::init();
/// ```
pub trait KernelComponent {
    /// Initialize the kernel component
    ///
    /// This function is called once during system boot to set up the component.
    /// It should perform all necessary initialization and leave the component
    /// in a ready-to-use state.
    ///
    /// # Panics
    ///
    /// Implementations may panic if initialization fails, as kernel components
    /// are critical to system operation and failure is typically unrecoverable.
    fn init();
}

// ================================================================================================
// MANAGER STRUCTS (Zero-Sized Types)
// ================================================================================================

// ================================================================================================
// INTERNAL HELPER FUNCTIONS
// ================================================================================================

/// Idle thread function
///
/// This is the built-in idle thread that runs when no other threads are ready.
/// It simply halts the CPU until the next interrupt, saving power.
fn idle_thread_main() {
    log::info!("Idle thread starting...");

    let mut idle_counter = 0u64;
    loop {
        // Log occasionally to show idle thread is running
        if idle_counter % 10000 == 0 {
            log::debug!("Idle thread tick: {}", idle_counter);
        }
        idle_counter = idle_counter.wrapping_add(1);

        // Flush log buffer to serial port
        // This ensures buffered log messages are written out
        crate::utils::debug::log_buffer::flush();

        // Halt CPU until next interrupt (power saving)
        x86_64::instructions::hlt();
    }
}

/// Thread exit trampoline
///
/// This function is placed as a return address on the thread's initial stack.
/// If a thread's entry function returns (instead of calling exit_thread),
/// it will "return" here, and we'll properly clean it up.
///
/// This prevents INVALID_OPCODE exceptions from executing garbage addresses.
extern "C" fn thread_exit_trampoline() -> ! {
    // Thread returned instead of calling exit_thread() - clean up properly
    log::info!("!!! Thread returned to trampoline - calling ThreadManager::exit()");
    ThreadManager::exit(0); // Thread returned normally, exit with code 0
}

/// Get thread name by ID (for debugging)
fn get_thread_name(thread_id: ThreadId) -> Option<alloc::string::String> {
    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let sched_guard = SCHEDULER.lock();
        if let Some(scheduler) = sched_guard.as_ref() {
            scheduler
                .threads
                .iter()
                .find(|t| t.id == thread_id)
                .map(|t| t.name.clone())
        } else {
            None
        }
    })
}

/// Thread statistics for display
#[derive(Debug, Clone)]
pub struct ThreadStats {
    pub id: ThreadId,
    pub name: alloc::string::String,
    pub state: ThreadState,
    pub cpu_time_ms: u64,
    pub cpu_percent: u64,
}

// ================================================================================================
// INTERRUPT HANDLERS (MUST remain module-level for IDT registration)
// ================================================================================================

/// Software interrupt handler for voluntary yielding (INT 0x81)
///
/// This is the entry point for voluntary context switches triggered by
/// yield_now(). It performs the same steps as the timer interrupt handler
/// but is triggered by software instead of hardware.
///
/// Identical to timer handler except:
/// - Triggered by `int 0x81` instruction instead of hardware timer
/// - No EOI needed (software interrupts don't use PIC)
#[unsafe(naked)]
pub unsafe extern "C" fn yield_interrupt_handler() {
    core::arch::naked_asm!(
        // Save all general purpose registers (same as timer handler)
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Call scheduler with current context
        "mov rdi, rsp",
        "call {schedule_fn}",

        // Switch to next thread's context
        "mov rsp, rax",

        // Restore all registers
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",

        // No EOI needed for software interrupts!

        // Return via iretq
        "iretq",

        schedule_fn = sym schedule_from_interrupt,
    )
}

/// Preemptive timer interrupt handler (naked assembly wrapper)
///
/// This is the entry point for preemptive context switches triggered by
/// timer interrupts. It performs the following steps:
///
/// 1. SAVE ALL REGISTERS: Creates a complete InterruptContext on the stack
/// 2. CALL SCHEDULER: Invokes schedule_from_interrupt() to pick next thread
/// 3. RESTORE REGISTERS: Loads next thread's context from returned pointer
/// 4. RETURN VIA IRETQ: Resumes execution in the next thread
///
/// STACK LAYOUT AFTER INTERRUPT:
/// =============================
///
/// High Address  [SS]           <- Pushed by CPU
///               [RSP]          <- Pushed by CPU
///               [RFLAGS]       <- Pushed by CPU
///               [CS]           <- Pushed by CPU
///               [RIP]          <- Pushed by CPU (this is the interrupt frame)
///               [RAX]          <- We push these
///               [RBX]          <- (general purpose registers)
///               [RCX]
///               [RDX]
///               [RSI]
///               [RDI]
///               [RBP]
///               [R8-R15]
/// Low Address   [...]          <- RSP after all pushes
///
/// This creates an InterruptContext structure on the stack.
#[unsafe(naked)]
pub unsafe extern "C" fn preemptive_timer_interrupt_handler() {
    core::arch::naked_asm!(
        // Save all general purpose registers to create InterruptContext on stack
        // The CPU has already pushed: SS, RSP, RFLAGS, CS, RIP (interrupt frame)
        //
        // STACK GROWS DOWNWARD! When we push:
        // - First push goes to higher address (bottom of what we're pushing)
        // - Last push goes to lower address (top, where RSP points)
        //
        // We want RSP to point to r15 (first field of struct), so push RAX first, R15 last:
        // After pushing: [iret_frame at high addr] [rax] [rbx] ... [r15 at RSP]

        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Now RSP points to a complete InterruptContext structure
        // Call the scheduler with pointer to current context
        "mov rdi, rsp",                    // First argument: pointer to current context
        "call {schedule_fn}",              // Call schedule_from_interrupt(current_ctx)
        // RAX now contains pointer to next thread's InterruptContext

        // Switch to next thread's context
        "mov rsp, rax",                    // Switch stack to next thread's context

        // Restore all general purpose registers from next thread's context
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",

        // Send EOI to PIC before iret
        "push rax",                        // Save RAX
        "mov al, 0x20",                    // EOI command
        "out 0x20, al",                    // Send to master PIC
        "pop rax",                         // Restore RAX

        // Return via iretq - CPU will pop RIP, CS, RFLAGS, RSP, SS
        "iretq",

        schedule_fn = sym schedule_from_interrupt,
    )
}

/// Schedule next thread from interrupt context
///
/// This function is called from the naked timer interrupt handler.
/// It receives a pointer to the current thread's InterruptContext on the stack,
/// picks the next thread, saves the current context, and returns a pointer to
/// the next thread's context.
///
/// CRITICAL: This function runs in interrupt context with interrupts disabled.
/// It must be quick and cannot block or sleep.
///
/// # Arguments
/// * `current_ctx_ptr` - Pointer to current thread's InterruptContext on the stack
///
/// # Returns
/// Pointer to next thread's InterruptContext (to be loaded into RSP)
#[unsafe(no_mangle)]
pub extern "C" fn schedule_from_interrupt(
    current_ctx_ptr: *const InterruptContext,
) -> *const InterruptContext {
    // Early exit if scheduler is not enabled
    if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
        // Scheduler not enabled yet, just return current context
        return current_ctx_ptr;
    }

    // Check if preemption is disabled (critical section in progress)
    if PREEMPTION_DISABLED.load(Ordering::SeqCst) {
        // Preemption disabled, skip scheduling this tick
        // Still update timer for accurate uptime tracking
        crate::utils::timer::on_timer_interrupt();
        return current_ctx_ptr;
    }

    // Update uptime and scheduler ticks (timer interrupt functionality)
    crate::utils::timer::on_timer_interrupt();

    // Access scheduler to pick next thread
    let mut sched_guard = SCHEDULER.lock();
    let scheduler = match sched_guard.as_mut() {
        Some(s) => s,
        None => {
            // Scheduler not initialized, return current context
            return current_ctx_ptr;
        }
    };

    // Get current thread ID
    let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));

    // Try to get next thread from ready queue
    let next_id = match scheduler.get_next_thread() {
        Some(id) => id,
        None => {
            // No threads ready to run
            // CRITICAL: If current thread is terminated, we can't return to it!
            // This should never happen (idle thread should always be available)
            if current_id.0 != 0 {
                if let Some(current_thread) = scheduler.get_thread_mut(current_id) {
                    if current_thread.state == ThreadState::Terminated {
                        // Terminated thread with no other threads available!
                        // This is a critical error - idle thread should always be ready
                        log::error!("SCHEDULER PANIC: No ready threads and current is terminated!");
                        log::error!(
                            "  Current thread: {} ({})",
                            current_id.0,
                            current_thread.name
                        );
                        log::error!("  Ready queue is empty!");
                        log::error!("  Total threads: {}", scheduler.threads.len());

                        // Try to find idle thread and add it to queue as last resort
                        if let Some(idle) = scheduler.threads.iter().find(|t| t.id.0 == 0) {
                            log::error!("  Found idle thread in state: {:?}", idle.state);
                        }

                        panic!("Scheduler deadlock: no threads available!");
                    }
                }
            }

            // Return current context (safe if not terminated)
            return current_ctx_ptr;
        }
    };

    // If current thread is the same as next, just return
    if current_id == next_id && current_id.0 != 0 {
        // Put thread back in ready queue
        scheduler.ready_queue.push_back(current_id);
        return current_ctx_ptr;
    }

    // Get current system uptime for CPU time tracking
    let current_time = crate::utils::timer::uptime_ms();

    // Save current thread's context (if we have a current thread)
    if current_id.0 != 0 {
        if let Some(current_thread) = scheduler.get_thread_mut(current_id) {
            // Copy context from stack to thread's storage
            unsafe {
                current_thread.interrupt_context = *current_ctx_ptr;
            }

            // Update CPU time: add time elapsed since last scheduled
            if current_thread.last_scheduled_time > 0 {
                let elapsed = current_time.saturating_sub(current_thread.last_scheduled_time);
                current_thread.cpu_time_ms = current_thread.cpu_time_ms.saturating_add(elapsed);
            }

            // Move current thread to ready queue (unless it's sleeping, blocked, or terminated)
            if current_thread.state != ThreadState::Blocked
                && current_thread.state != ThreadState::Terminated
            {
                current_thread.state = ThreadState::Ready;
            }

            // Only add to ready queue if not sleeping, not blocked, and not terminated
            if current_thread.state == ThreadState::Ready {
                if current_thread.sleep_until_ms == 0
                    || current_time >= current_thread.sleep_until_ms
                {
                    // Thread is not sleeping (or sleep expired), not blocked, and not terminated
                    scheduler.ready_queue.push_back(current_id);
                }
            }
            // If sleeping, blocked, or terminated, thread is NOT added to ready queue
            // Sleeping threads are woken by get_next_thread() when sleep expires
            // Blocked threads are woken by wake_thread() when event occurs
            // Terminated threads are never scheduled again
        }
    }

    // CRITICAL BUG FIX: Cleanup MUST happen BEFORE getting next_ctx_ptr!
    // If we cleanup after getting the pointer, Vec::retain() can reallocate
    // the Vec, moving all threads and making next_ctx_ptr DANGLING!
    // This caused INVALID_OPCODE crashes when returning from IRQ.

    // Check if cleanup needed (before getting pointer!)
    let should_cleanup = if current_id.0 != 0 {
        scheduler
            .get_thread_mut(current_id)
            .map(|t| t.state == ThreadState::Terminated)
            .unwrap_or(false)
    } else {
        false
    };

    // Do cleanup NOW if needed (before getting next_ctx_ptr)
    if should_cleanup {
        scheduler.cleanup_terminated_threads(next_id, false);
    }

    // NOW get the pointer to next thread (after cleanup, so it won't be invalidated)
    let (next_ctx_ptr, next_process_id, next_stack_top) =
        if let Some(next_thread) = scheduler.get_thread_mut(next_id) {
            next_thread.state = ThreadState::Running;
            next_thread.last_scheduled_time = current_time;

            // Calculate kernel stack top for syscall entry
            // Stack top = base + size
            let stack_base = next_thread.stack.as_ptr() as u64;
            let stack_top = stack_base + next_thread.stack.len() as u64;

            (
                &next_thread.interrupt_context as *const InterruptContext,
                next_thread.process_id,
                stack_top,
            )
        } else {
            // Thread not found, return current context
            return current_ctx_ptr;
        };

    // Get current thread's process ID for comparison
    let current_process_id = if current_id.0 != 0 {
        scheduler
            .get_thread_mut(current_id)
            .map(|t| t.process_id)
            .unwrap_or(ProcessId(0))
    } else {
        ProcessId(0)
    };

    // If switching to a different process, update address space (CR3 register)
    // CRITICAL: If we just cleaned up a terminated thread (should_cleanup == true),
    // we MUST switch CR3 even if process IDs appear equal, because the current CR3
    // might still point to the terminated process's page tables!
    if next_process_id != current_process_id || should_cleanup {
        if let Some(next_process) = scheduler.get_process(next_process_id) {
            // Switch to next process's address space
            // This updates CR3 register, which invalidates TLB (~100 cycles to refill)
            next_process.address_space.switch_to();
        }
    }

    // Update current thread ID
    CURRENT_THREAD_ID.store(next_id.0, Ordering::SeqCst);

    // CRITICAL: Release scheduler lock BEFORE calling set_kernel_stack()
    // set_kernel_stack() verifies interrupts are disabled, which they are
    // (we're in an interrupt handler), but we must not hold the lock
    drop(sched_guard);

    // Update kernel stack pointer for SYSCALL entry
    // This MUST be done after releasing the scheduler lock to prevent potential deadlock
    // if set_kernel_stack() needs to log (though it shouldn't in IRQ context)
    //
    // Safety: We're in an interrupt handler, so interrupts are already disabled
    // The stack pointer is guaranteed valid because we just retrieved it from the thread
    crate::syscall::set_kernel_stack(next_stack_top);

    // CRITICAL: Also update TSS RSP0 for Ring 3 → Ring 0 transitions
    // When a userspace thread (Ring 3) gets interrupted or makes a syscall,
    // the CPU needs to know where the kernel stack is. For SYSCALL, we use
    // set_kernel_stack() above. For interrupts/exceptions, the CPU uses TSS RSP0.
    crate::arch::x86_64::gdt::set_tss_rsp0(next_stack_top);

    // Removed IRQ logging to avoid deadlocks

    // Return pointer to next thread's context (guaranteed valid)
    next_ctx_ptr
}
