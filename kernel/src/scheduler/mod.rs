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

use alloc::{collections::{BTreeMap, VecDeque}, vec::Vec};

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use spin::Mutex;

pub mod io_wait;
pub mod ipc;
pub mod process;
pub mod thread;

use crate::alloc::string::ToString;
pub use io_wait::{IoChannel, wait_for_io, wake_io_waiters};
pub use ipc::{
    IpcError, Message, PortId, port_create, port_destroy, port_recv, port_send,
};
pub use process::{Process, ProcessId, ProcessState};
pub use thread::{Thread, ThreadId, ThreadState};

/// Thread stack size (64 KiB per thread)
pub const THREAD_STACK_SIZE: usize = 64 * 1024;

/// Maximum number of threads
pub const MAX_THREADS: usize = 64;

/// Global scheduler instance
static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

/// Current running thread ID (atomic for IRQ safety)
static CURRENT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

/// Scheduler enabled flag
static SCHEDULER_ENABLED: AtomicBool = AtomicBool::new(false);

/// Preemption disabled flag (set during critical scheduler operations)
/// When true, timer interrupts will not perform context switches
static PREEMPTION_DISABLED: AtomicBool = AtomicBool::new(false);

/// Interrupt frame pushed by CPU during interrupt
///
/// When an interrupt occurs, the x86_64 CPU automatically pushes these registers
/// onto the stack in this exact order. This is the hardware-defined structure.
///
/// The #[repr(C)] ensures the struct layout matches what the CPU pushes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptFrame {
    pub rip: u64,    // Instruction pointer - where to resume execution
    pub cs: u64,     // Code segment selector
    pub rflags: u64, // CPU flags register
    pub rsp: u64,    // Stack pointer before interrupt
    pub ss: u64,     // Stack segment selector
}

impl Default for InterruptFrame {
    fn default() -> Self {
        Self {
            rip: 0,
            cs: 0x08,      // Kernel code segment (from GDT)
            rflags: 0x202, // IF=1 (interrupts enabled), reserved bit 1 always set
            rsp: 0,
            ss: 0x10, // Kernel data segment (from GDT)
        }
    }
}

/// Complete CPU context for interrupt-based context switching
///
/// This structure holds ALL registers that need to be saved during a
/// preemptive context switch triggered by a timer interrupt. It includes:
///
/// 1. INTERRUPT FRAME: CPU automatically pushes RIP, CS, RFLAGS, RSP, SS
/// 2. ALL GENERAL PURPOSE REGISTERS: We must manually save RAX-R15
///
/// The layout is designed to match what our assembly code expects when
/// performing context switches via iretq.
///
/// Memory layout (from high to low addresses on stack):
/// - Interrupt frame (pushed by CPU)
/// - General purpose registers (pushed by our code)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InterruptContext {
    // General purpose registers (pushed by our interrupt handler)
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    // Interrupt frame (pushed automatically by CPU)
    pub iret_frame: InterruptFrame,
}

impl Default for InterruptContext {
    fn default() -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rbp: 0,
            rdi: 0,
            rsi: 0,
            rdx: 0,
            rcx: 0,
            rbx: 0,
            rax: 0,
            iret_frame: InterruptFrame::default(),
        }
    }
}

/// Main scheduler structure
///
/// This is the core data structure that manages all threads in the system.
/// It maintains:
///
/// 1. THREAD STORAGE: All thread objects with their stacks and metadata
/// 2. READY QUEUE: FIFO queue of threads waiting to be scheduled
/// 3. ID ALLOCATION: Ensures each thread gets a unique identifier
///
/// DESIGN DECISIONS:
/// ================
///
/// - Vec<Thread>: Stores all threads, indexed by position (not ID)
/// - VecDeque<ThreadId>: Ready queue for O(1) push/pop operations
/// - ThreadId counter: Simple incrementing ID assignment
///
/// THREAD LOOKUP:
/// ==============
///
/// Threads are found by linear search through the Vec. This is acceptable
/// for a microkernel with a small number of threads. For better performance
/// with many threads, we could use a HashMap<ThreadId, Thread>.
pub struct Scheduler {
    threads: Vec<Thread>,             // All threads in the system
    ready_queue: VecDeque<ThreadId>,  // Queue of threads ready to run
    next_thread_id: ThreadId,         // Next ID to assign to new thread
    processes: BTreeMap<ProcessId, Process>, // All processes in the system
    next_process_id: ProcessId,       // Next ID to assign to new process
}

impl Scheduler {
    fn new() -> Self {
        Self {
            threads: Vec::new(),
            ready_queue: VecDeque::new(),
            next_thread_id: ThreadId(1), // ID 0 reserved for kernel/idle
            processes: BTreeMap::new(),
            next_process_id: ProcessId(1), // ID 0 reserved for kernel
        }
    }

    /// Create a new thread
    ///
    /// This function sets up a new thread with its own stack and initial CPU context.
    /// The process involves several critical steps:
    ///
    /// THREAD CREATION PROCESS:
    /// =======================
    ///
    /// 1. ID ASSIGNMENT: Each thread gets a unique, incrementing ID
    /// 2. STACK ALLOCATION: 64KB stack allocated on the heap
    /// 3. CONTEXT SETUP: CPU registers initialized for first execution (both old and new style)
    /// 4. ENTRY POINT: Function pointer set in context
    /// 5. REGISTRATION: Thread added to scheduler's data structures
    ///
    /// STACK LAYOUT FOR COOPERATIVE:
    /// =============================
    ///
    /// High Address  [Stack Top]
    ///               [Entry Point Address] <- RSP points here initially
    ///               [Available Stack Space]
    ///               [...]
    /// Low Address   [Stack Bottom]
    ///
    /// INTERRUPT CONTEXT FOR PREEMPTIVE:
    /// =================================
    ///
    /// The interrupt context is set up as if the thread was interrupted:
    /// - RIP points to entry point function
    /// - RSP points to top of thread's stack
    /// - RFLAGS has interrupts enabled (IF=1)
    /// - CS/SS set to kernel segments
    ///
    /// When the thread first runs via iretq, it will jump to the entry point.
    /// Create a new thread
    ///
    /// This function sets up a new thread with its own stack and initial interrupt context.
    /// The thread will be ready to run via preemptive scheduling.
    ///
    /// THREAD CREATION PROCESS:
    /// =======================
    ///
    /// 1. ID ASSIGNMENT: Each thread gets a unique, incrementing ID
    /// 2. STACK ALLOCATION: 64KB stack allocated on the heap
    /// 3. INTERRUPT CONTEXT SETUP: Set up as if thread was interrupted
    ///    - RIP points to entry point function
    ///    - RSP points to top of thread's stack
    ///    - RFLAGS has interrupts enabled (IF=1)
    ///    - CS/SS set to kernel segments
    /// 4. REGISTRATION: Thread added to scheduler's ready queue
    ///
    /// When the thread first runs via iretq, it will jump to the trampoline
    /// which calls the entry point and ensures proper cleanup.
    fn create_thread(&mut self, entry_point: fn(), name: &str, process_id: ProcessId) -> ThreadId {
        // Assign unique ID and increment counter for next thread
        let thread_id = self.next_thread_id;
        self.next_thread_id.0 += 1;

        // Allocate a 64KB stack for this thread
        let stack = alloc::vec![0u8; THREAD_STACK_SIZE].into_boxed_slice();
        let stack_base = stack.as_ptr() as u64;
        let stack_top = stack_base + THREAD_STACK_SIZE as u64;

        // CRITICAL: Set up return address on stack for thread safety
        // When thread's entry function returns, it will "return" to thread_exit_trampoline
        // This prevents INVALID_OPCODE from executing garbage addresses
        let return_addr = thread_exit_trampoline as *const () as u64;
        let stack_ptr = (stack_top - 8) as *mut u64;
        unsafe {
            *stack_ptr = return_addr;
        }

        // Set up interrupt context for preemptive scheduling
        let mut interrupt_context = InterruptContext::default();

        // Set up interrupt frame to make it look like this thread was interrupted
        interrupt_context.iret_frame.rip = entry_point as u64; // Jump to entry point
        interrupt_context.iret_frame.cs = 0x08; // Kernel code segment
        interrupt_context.iret_frame.rflags = 0x202; // IF=1 (interrupts enabled), bit 1 always set
        interrupt_context.iret_frame.rsp = stack_top - 8; // RSP points to return address
        interrupt_context.iret_frame.ss = 0x10; // Kernel data segment

        // All general purpose registers initialized to 0 (from default())

        // Create the thread object and add it to our data structures
        let thread = Thread::new(thread_id, name.into(), stack, interrupt_context, process_id);
        self.threads.push(thread);

        // Add thread to its process
        if let Some(process) = self.processes.get_mut(&process_id) {
            process.add_thread(thread_id);
        }

        // New thread starts in Ready state, so add to ready queue
        self.ready_queue.push_back(thread_id);

        log::info!("Created thread '{}' (ID {:?}) in process {:?}", name, thread_id, process_id);
        thread_id
    }

    /// Get the next thread to run
    fn get_next_thread(&mut self) -> Option<ThreadId> {
        let current_time = crate::utils::timer::uptime_ms();

        // Wake up any threads whose sleep time has expired
        for thread in &mut self.threads {
            if thread.sleep_until_ms > 0 && current_time >= thread.sleep_until_ms {
                // Sleep expired, wake up thread
                thread.sleep_until_ms = 0;
                if thread.state == ThreadState::Ready {
                    // Add back to ready queue if not already there
                    if !self.ready_queue.contains(&thread.id) {
                        self.ready_queue.push_back(thread.id);
                    }
                }
            }
        }

        // Find next thread that is not sleeping or terminated
        loop {
            let thread_id = self.ready_queue.pop_front()?;

            // Check thread state
            if let Some(thread) = self.threads.iter().find(|t| t.id == thread_id) {
                // Skip terminated threads
                if thread.state == ThreadState::Terminated {
                    continue;
                }

                // Skip sleeping threads
                if thread.sleep_until_ms > 0 && current_time < thread.sleep_until_ms {
                    // Thread is still sleeping, don't schedule it
                    // Don't put it back in ready queue
                    continue;
                }
            }

            // Thread is not sleeping or terminated, can be scheduled
            return Some(thread_id);
        }
    }

    /// Add thread back to ready queue
    fn make_ready(&mut self, thread_id: ThreadId) {
        if let Some(thread) = self.threads.iter_mut().find(|t| t.id == thread_id) {
            if thread.state == ThreadState::Running {
                thread.state = ThreadState::Ready;
                self.ready_queue.push_back(thread_id);
            }
        }
    }

    /// Get thread by ID
    fn get_thread_mut(&mut self, thread_id: ThreadId) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == thread_id)
    }

    /// Clean up terminated threads
    ///
    /// Removes all terminated threads from the scheduler, freeing their resources.
    /// This should be called periodically by the reaper thread.
    ///
    /// # Arguments
    /// * `current_thread_id` - The currently running thread (must not be cleaned up)
    /// * `log_cleanup` - Whether to log cleanup (should be false in IRQ context!)
    ///
    /// Returns the number of threads cleaned up.
    fn cleanup_terminated_threads(
        &mut self,
        current_thread_id: ThreadId,
        log_cleanup: bool,
    ) -> usize {
        // First, identify threads to clean up
        let mut to_remove = alloc::vec::Vec::new();
        for thread in &self.threads {
            if thread.state == ThreadState::Terminated
                && thread.id != current_thread_id
                && thread.id.0 != 0
            {
                to_remove.push((thread.id, thread.name.clone()));
            }
        }

        // CRITICAL: Only log if NOT in IRQ context (logging in IRQ can deadlock!)
        if log_cleanup {
            for (id, name) in &to_remove {
                log::info!("Reaper: Cleaning up thread {} ({})", id.0, name);
            }
        }

        // Now remove them (dropping Thread frees stack)
        let initial_count = self.threads.len();
        self.threads
            .retain(|t| !to_remove.iter().any(|(id, _)| t.id == *id));

        initial_count - self.threads.len()
    }

    /// Create a new kernel process
    ///
    /// Kernel processes run in Ring 0 and use the kernel address space.
    /// This is used for kernel threads that need isolated resource management.
    fn create_kernel_process(&mut self, name: &str) -> ProcessId {
        let process_id = self.next_process_id;
        self.next_process_id.0 += 1;

        let process = Process::new_kernel(process_id, name.into());
        self.processes.insert(process_id, process);

        log::info!("Created kernel process '{}' with ID {:?}", name, process_id);
        process_id
    }

    /// Get a process by ID (immutable)
    fn get_process(&self, process_id: ProcessId) -> Option<&Process> {
        self.processes.get(&process_id)
    }

    /// Get a process by ID (mutable)
    fn get_process_mut(&mut self, process_id: ProcessId) -> Option<&mut Process> {
        self.processes.get_mut(&process_id)
    }
}

/// Initialize the scheduler
pub fn init() {
    log::info!("Initializing preemptive scheduler...");

    let mut scheduler = Scheduler::new();

    // Create default kernel process (PID 0) for kernel threads
    let kernel_process = Process::new_kernel(ProcessId(0), "kernel".into());
    scheduler.processes.insert(ProcessId(0), kernel_process);
    log::info!("Created default kernel process (PID 0)");

    // CRITICAL: Disable interrupts to prevent timer IRQ from trying to acquire lock
    x86_64::instructions::interrupts::without_interrupts(|| {
        *SCHEDULER.lock() = Some(scheduler);
    });

    // Initialize I/O wait queue system
    io_wait::init();

    log::info!("Scheduler initialized");
}

/// Create a new thread in the default kernel process
///
/// This is a convenience function for creating kernel threads without
/// explicitly managing processes. All threads created this way belong
/// to the default kernel process (PID 0).
pub fn spawn_thread(entry_point: fn(), name: &str) -> ThreadId {
    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_mut().expect("Scheduler not initialized");
        scheduler.create_thread(entry_point, name, ProcessId(0))
    })
}

/// Create a new kernel process
///
/// This creates a process with its own file descriptor table and resource
/// management, but using the kernel address space (Ring 0).
///
/// Returns the ProcessId of the newly created process.
pub fn spawn_kernel_process(name: &str) -> ProcessId {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_mut().expect("Scheduler not initialized");
        scheduler.create_kernel_process(name)
    })
}

/// Create a thread within a specific process
///
/// This is the process-aware version of spawn_thread, allowing you to
/// specify which process the thread should belong to.
pub fn spawn_thread_in_process(entry_point: fn(), name: &str, process_id: ProcessId) -> ThreadId {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_mut().expect("Scheduler not initialized");
        scheduler.create_thread(entry_point, name, process_id)
    })
}

/// Get the process ID for the currently running thread
///
/// Returns None if no thread is currently running or scheduler not initialized.
pub fn current_process_id() -> Option<ProcessId> {
    let current_tid = ThreadId(CURRENT_THREAD_ID.load(Ordering::Relaxed));
    if current_tid.0 == 0 {
        return None;
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_ref()?;
        scheduler.threads.iter()
            .find(|t| t.id == current_tid)
            .map(|t| t.process_id)
    })
}

/// Execute a closure with access to the current process (immutable)
///
/// This is a helper function for syscalls that need to access the current
/// process's state (e.g., file descriptor table).
pub fn with_current_process<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Process) -> R,
{
    let pid = current_process_id()?;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_ref()?;
        scheduler.get_process(pid).map(f)
    })
}

/// Execute a closure with access to the current process (mutable)
///
/// This is a helper function for syscalls that need to modify the current
/// process's state (e.g., modifying file descriptor table).
pub fn with_current_process_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut Process) -> R,
{
    let pid = current_process_id()?;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_mut()?;
        scheduler.get_process_mut(pid).map(f)
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
    use alloc::sync::Arc;

    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut scheduler_guard = SCHEDULER.lock();
        let scheduler = scheduler_guard.as_mut().expect("Scheduler not initialized");

        // Find thread by ID to get its process_id
        let process_id = scheduler.threads.iter()
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
            log::warn!("Cannot init std streams: thread {} not found", thread_id.0);
        }
    });
}

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

        // Halt CPU until next interrupt (power saving)
        x86_64::instructions::hlt();
    }
}

/// Enable the scheduler
///
/// This function:
/// 1. Spawns the built-in idle thread
/// 2. Enables preemptive scheduling
///
/// After calling this, timer interrupts will start performing context switches.
/// Terminated threads are cleaned up immediately during context switches.
pub fn enable() {
    // Spawn the idle thread - it will run when no other threads are ready
    spawn_thread(idle_thread_main, "idle");
    log::info!("Idle thread created");

    // Enable preemptive scheduling
    SCHEDULER_ENABLED.store(true, Ordering::SeqCst);
    log::info!("Scheduler enabled - preemptive multitasking active");
    log::info!("Terminated threads will be cleaned up immediately on context switch");
}

/// Voluntarily yield the CPU to the next ready thread
///
/// This is the heart of cooperative scheduling. When a thread calls this function,
/// it gives up the CPU and allows another thread to run. The process involves:
///
/// YIELD PROCESS:
/// =============
///
/// 1. SAFETY CHECKS: Ensure scheduler is enabled and interrupts are on
/// 2. DISABLE INTERRUPTS: Prevent race conditions during context switch
/// 3. FIND NEXT THREAD: Get next thread from ready queue
/// 4. UPDATE QUEUES: Move current thread to back of ready queue
/// 5. CONTEXT SWITCH: Save current state, load next thread's state
/// 6. RESUME EXECUTION: Next thread continues from where it last yielded
///
/// CRITICAL SECTION:
/// ================
///
/// The scheduler mutex is held only briefly to:
/// - Read/modify thread queues
/// - Get pointers to thread contexts
/// - Update thread states
///
/// The mutex is released BEFORE the actual context switch to prevent
/// deadlocks if the new thread tries to access the scheduler.
///
/// INTERRUPT HANDLING:
/// ==================
///
/// Interrupts are disabled during context switch to prevent:
/// - Timer interrupts from interfering with register save/restore
/// - Race conditions in scheduler data structures
/// - Corruption of thread contexts
///
/// WHY COOPERATIVE?
/// ===============
///
/// Cooperative scheduling is simpler and more predictable than preemptive:
/// - Threads run until they choose to yield
/// - No complex timer-based preemption
/// - Easier to reason about thread interactions
/// - Better for kernel-level code that needs atomicity
/// Voluntarily yield the CPU to the next ready thread
///
/// With preemptive scheduling, this function triggers a software interrupt (INT 0x81)
/// that performs the same context switch as the timer interrupt, but voluntarily.
/// This provides backward compatibility while using the interrupt-based mechanism.
///
/// INTERRUPT-BASED YIELDING:
/// ========================
///
/// yield_now() now uses INT 0x81 to trigger a context switch. This:
/// - Uses the same InterruptContext mechanism as timer preemption
/// - Saves/restores all registers via the interrupt handler
/// - Works seamlessly with preemptive scheduling
/// - Provides true backward compatibility
///
/// WHEN TO USE:
/// ===========
///
/// - Threads can call yield_now() to voluntarily give up CPU
/// - Useful in busy-wait loops to let other threads run
/// - Will be preempted anyway if they don't yield
/// - Kernel idle loop can use this before enabling preemption
pub fn yield_now() {
    // Early exit if scheduler is not yet enabled (during boot)
    if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // Don't yield if interrupts are disabled - this could indicate we're
    // in a critical section that shouldn't be interrupted
    if !crate::arch::x86_64::interrupts::are_enabled() {
        return;
    }

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

/// Get current thread ID
pub fn current_thread_id() -> ThreadId {
    ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst))
}

/// Execute a closure with access to the current thread
///
/// Provides safe read-only access to the current thread's data.
/// Returns None if the scheduler is not initialized or thread not found.
pub fn with_current_thread<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Thread) -> R,
{
    let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));
    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let sched_guard = SCHEDULER.lock();
        if let Some(scheduler) = sched_guard.as_ref() {
            if let Some(thread) = scheduler.threads.iter().find(|t| t.id == current_id) {
                return Some(f(thread));
            }
        }
        None
    })
}

/// Block the current thread
///
/// Removes the current thread from the ready queue and sets its state to Blocked.
/// The thread will not be scheduled again until wake_thread() is called.
///
/// This is typically used for blocking I/O operations where a thread needs to
/// wait for an external event (like keyboard input or timer expiry).
///
/// # Safety
/// The caller must ensure that some mechanism exists to eventually wake this thread,
/// otherwise it will be blocked forever.
pub fn block_current_thread() {
    if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));
    if current_id.0 == 0 {
        // Cannot block kernel/idle thread
        return;
    }

    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched_guard = SCHEDULER.lock();
        if let Some(scheduler) = sched_guard.as_mut() {
            if let Some(thread) = scheduler.get_thread_mut(current_id) {
                thread.state = ThreadState::Blocked;
                // Thread is already not in ready queue since it's currently running
                // When it yields, schedule_from_interrupt won't add it back because state is Blocked
            }
        }
    });
}

/// Wake a blocked thread
///
/// Moves the specified thread from Blocked state to Ready state and adds it
/// to the ready queue. If the thread is not blocked, this is a no-op.
///
/// This function is IRQ-safe and can be called from interrupt handlers.
///
/// # Arguments
/// * `thread_id` - The ID of the thread to wake up
pub fn wake_thread(thread_id: ThreadId) {
    if !SCHEDULER_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched_guard = SCHEDULER.lock();
        if let Some(scheduler) = sched_guard.as_mut() {
            if let Some(thread) = scheduler.get_thread_mut(thread_id) {
                if thread.state == ThreadState::Blocked {
                    thread.state = ThreadState::Ready;
                    // Add to ready queue
                    scheduler.ready_queue.push_back(thread_id);
                }
            }
        }
    });
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
    log::info!("!!! Thread returned to trampoline - calling exit_thread()");
    exit_thread();
}

/// Terminate the current thread
///
/// Marks the current thread as Terminated and yields. The thread will not
/// be scheduled again. This is the proper way for a thread to exit.
///
/// **Important:** The thread's resources (stack, etc.) are not immediately freed.
/// In a full implementation, you would need a reaper thread to clean up
/// terminated threads. For now, they remain in the thread list as Terminated.
///
/// # Panics
/// Panics if called from the idle thread (thread 0).
pub fn exit_thread() -> ! {
    let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));

    if current_id.0 == 0 {
        panic!("Cannot exit idle thread");
    }

    log::info!(
        "Thread {} ({}) terminating",
        current_id.0,
        get_thread_name(current_id).unwrap_or_else(|| "unknown".to_string())
    );

    // Mark thread as terminated
    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut sched_guard = SCHEDULER.lock();
        if let Some(scheduler) = sched_guard.as_mut() {
            if let Some(thread) = scheduler.get_thread_mut(current_id) {
                thread.state = ThreadState::Terminated;
                // Thread will not be added back to ready queue
            }
        }
    });

    // Yield to switch to another thread
    // We will never return here
    yield_now();

    // Should never reach here
    loop {
        x86_64::instructions::hlt();
    }
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

/// Get statistics for all threads
///
/// Returns a vector of ThreadStats with information about each thread
/// including CPU time and usage percentage.
pub fn get_thread_stats() -> Vec<ThreadStats> {
    // CRITICAL: Disable interrupts to prevent timer IRQ deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        let sched_guard = SCHEDULER.lock();
        let scheduler = match sched_guard.as_ref() {
            Some(s) => s,
            None => return Vec::new(),
        };

        let total_uptime = crate::utils::timer::uptime_ms();
        if total_uptime == 0 {
            return Vec::new();
        }

        let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));

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

            stats.push(ThreadStats {
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
            // No threads ready to run, return current context
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
    let (next_ctx_ptr, next_process_id, next_stack_top) = if let Some(next_thread) = scheduler.get_thread_mut(next_id) {
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
    if next_process_id != current_process_id {
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

    // Return pointer to next thread's context (guaranteed valid)
    next_ctx_ptr
}

/// Sleep for a number of milliseconds (blocking)
///
/// This function implements true blocking sleep by marking the thread
/// as sleeping and removing it from active scheduling. The sleeping
/// thread consumes 0% CPU during sleep.
///
/// BLOCKING SLEEP PROCESS:
/// =======================
///
/// 1. SET SLEEP TIMER: Mark thread's sleep_until_ms field
/// 2. YIELD CPU: Call yield_now() to switch to another thread
/// 3. AUTOMATIC WAKEUP: Scheduler checks sleep timers and reschedules when ready
/// 4. THREAD RESUMES: Execution continues after sleep time expires
///
/// ADVANTAGES:
/// - Zero CPU consumption during sleep
/// - Accurate CPU usage statistics
/// - Power efficient (idle thread can halt CPU)
/// - Proper thread blocking semantics
///
/// DISADVANTAGES:
/// - Slightly more complex scheduler logic
/// - Resolution limited by timer frequency (10ms)
///
/// USAGE EXAMPLE:
/// =============
///
/// ```rust
/// // Sleep for 1 second (thread uses 0% CPU during this time)
/// scheduler::sleep_ms(1000);
///
/// // Sleep for 100 milliseconds
/// scheduler::sleep_ms(100);
/// ```
///
/// The actual sleep time may be slightly longer than requested due to:
/// - Timer resolution (currently 10ms)
/// - Scheduling overhead
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
            if let Some(thread) = scheduler.get_thread_mut(current_id) {
                let wake_time = crate::utils::timer::uptime_ms() + ms;
                thread.sleep_until_ms = wake_time;
            }
        }
    });

    // Yield to switch to another thread
    // The scheduler will not reschedule us until sleep time expires
    yield_now();
}
