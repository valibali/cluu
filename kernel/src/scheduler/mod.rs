/*
 * Cooperative Scheduler
 *
 * This module implements a simple cooperative scheduler for the CLUU kernel.
 * It provides basic thread management with voluntary yielding.
 *
 * COOPERATIVE SCHEDULING EXPLAINED:
 * ================================
 * 
 * Unlike preemptive scheduling where the OS forcibly switches between threads,
 * cooperative scheduling relies on threads voluntarily giving up the CPU by
 * calling yield_now(). This has several advantages for a microkernel:
 * 
 * 1. SIMPLICITY: No complex timer-based preemption logic needed
 * 2. PREDICTABILITY: Threads run until they explicitly yield
 * 3. REDUCED COMPLEXITY: No need to handle arbitrary preemption points
 * 4. BETTER FOR KERNEL: Kernel threads can complete critical sections atomically
 * 
 * THREAD LIFECYCLE:
 * ================
 * 
 * Ready -> Running -> Ready (via yield_now())
 *   ^                   |
 *   |                   v
 *   +-- Blocked --------+
 * 
 * - Ready: Thread is waiting to be scheduled
 * - Running: Thread is currently executing on CPU
 * - Blocked: Thread is waiting for some event (future feature)
 * - Terminated: Thread has finished execution (future feature)
 * 
 * SCHEDULING ALGORITHM:
 * ====================
 * 
 * Simple round-robin: threads are scheduled in the order they yield.
 * When a thread calls yield_now():
 * 1. Current thread is moved to back of ready queue
 * 2. Next thread from front of ready queue becomes current
 * 3. Context switch occurs to new thread
 * 
 * CONTEXT SWITCHING:
 * =================
 * 
 * Context switching involves saving the current thread's CPU state
 * (registers, stack pointer) and loading the next thread's state.
 * This is done in assembly for maximum efficiency and control.
 * 
 * Key features:
 * - Cooperative multitasking (threads must yield voluntarily)
 * - Round-robin scheduling
 * - IRQ-safe thread switching
 * - Simple thread states (Ready, Running, Blocked)
 * - Stack management for each thread
 */

use alloc::{collections::VecDeque, vec::Vec};

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use spin::Mutex;

pub mod thread;

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

/// CPU context for thread switching
/// 
/// This structure holds the minimal set of CPU registers that need to be
/// saved and restored during a context switch. We only save callee-saved
/// registers because:
/// 
/// 1. CALLER-SAVED REGISTERS (RAX, RCX, RDX, RSI, RDI, R8-R11):
///    These are automatically saved by the calling function before
///    calling yield_now(), so we don't need to save them here.
/// 
/// 2. CALLEE-SAVED REGISTERS (RBX, RBP, R12-R15, RSP):
///    These must be preserved across function calls, so we save them
///    to ensure the thread can continue correctly after context switch.
/// 
/// 3. STACK POINTER (RSP):
///    Critical for maintaining each thread's separate stack.
/// 
/// 4. BASE POINTER (RBP):
///    Used for stack frame management and debugging.
/// 
/// The #[repr(C)] ensures the struct layout matches what our assembly
/// code expects, with fields in the exact order we access them.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuContext {
    pub rsp: u64,  // Stack pointer - points to thread's current stack position
    pub rbp: u64,  // Base pointer - used for stack frame management
    pub rbx: u64,  // General purpose register (callee-saved)
    pub r12: u64,  // General purpose register (callee-saved)
    pub r13: u64,  // General purpose register (callee-saved)
    pub r14: u64,  // General purpose register (callee-saved)
    pub r15: u64,  // General purpose register (callee-saved)
}

impl Default for CpuContext {
    fn default() -> Self {
        Self {
            rsp: 0,
            rbp: 0,
            rbx: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
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
    threads: Vec<Thread>,           // All threads in the system
    ready_queue: VecDeque<ThreadId>, // Queue of threads ready to run
    next_thread_id: ThreadId,       // Next ID to assign to new thread
}

impl Scheduler {
    fn new() -> Self {
        Self {
            threads: Vec::new(),
            ready_queue: VecDeque::new(),
            next_thread_id: ThreadId(1), // ID 0 reserved for kernel/idle
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
    /// 3. CONTEXT SETUP: CPU registers initialized for first execution
    /// 4. ENTRY POINT: Function pointer placed on stack as return address
    /// 5. REGISTRATION: Thread added to scheduler's data structures
    /// 
    /// STACK LAYOUT:
    /// ============
    /// 
    /// High Address  [Stack Top]
    ///               [Entry Point Address] <- RSP points here initially
    ///               [Available Stack Space]
    ///               [...]
    /// Low Address   [Stack Bottom]
    /// 
    /// When the thread first runs, it will "return" to the entry point function.
    fn create_thread(&mut self, entry_point: fn(), name: &str) -> ThreadId {
        // Assign unique ID and increment counter for next thread
        let thread_id = self.next_thread_id;
        self.next_thread_id.0 += 1;

        // Allocate a 64KB stack for this thread
        // Each thread needs its own stack to maintain independent execution context
        let stack = alloc::vec![0u8; THREAD_STACK_SIZE].into_boxed_slice();
        let stack_top = stack.as_ptr() as u64 + THREAD_STACK_SIZE as u64;

        // Set up initial CPU context for the new thread
        let mut context = CpuContext::default();
        
        // Stack pointer starts at top of stack, minus 8 bytes for return address
        // This is because x86_64 calling convention expects return address on stack
        context.rsp = stack_top - 8;

        // Place the entry point function address on the stack
        // When context_switch() does a 'ret' instruction, it will jump to this address
        unsafe {
            let return_addr_ptr = context.rsp as *mut u64;
            *return_addr_ptr = entry_point as u64;
        }

        // Create the thread object and add it to our data structures
        let thread = Thread::new(thread_id, name.into(), stack, context);
        self.threads.push(thread);
        
        // New thread starts in Ready state, so add to ready queue
        self.ready_queue.push_back(thread_id);

        log::info!("Created thread '{}' with ID {:?}", name, thread_id);
        thread_id
    }

    /// Get the next thread to run
    fn get_next_thread(&mut self) -> Option<ThreadId> {
        self.ready_queue.pop_front()
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
}

/// Initialize the scheduler
pub fn init() {
    log::info!("Initializing cooperative scheduler...");

    let scheduler = Scheduler::new();
    *SCHEDULER.lock() = Some(scheduler);

    log::info!("Scheduler initialized");
}

/// Create a new thread
pub fn spawn_thread(entry_point: fn(), name: &str) -> ThreadId {
    let mut scheduler = SCHEDULER.lock();
    let scheduler = scheduler.as_mut().expect("Scheduler not initialized");
    scheduler.create_thread(entry_point, name)
}

/// Enable the scheduler
pub fn enable() {
    SCHEDULER_ENABLED.store(true, Ordering::SeqCst);
    log::info!("Scheduler enabled");
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

    // Disable interrupts to create atomic context switch section
    // This prevents timer interrupts from interfering with register save/restore
    crate::arch::x86_64::interrupts::disable();

    // Critical section: access scheduler data structures
    // We extract all needed information and release the lock BEFORE context switching
    let (current_ctx, next_ctx, first_switch) = {
        let mut sched_guard = SCHEDULER.lock();
        let scheduler = sched_guard.as_mut().expect("Scheduler not initialized");

        // Get current thread ID (0 means no current thread - first boot)
        let current_id = ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst));

        // Try to get next thread from ready queue
        let next_id = match scheduler.get_next_thread() {
            Some(id) => id,
            None => {
                // No threads ready to run - re-enable interrupts and return
                // This can happen if all threads are blocked or only idle thread exists
                crate::arch::x86_64::interrupts::enable();
                return;
            }
        };

        // If we have a current thread, move it back to ready queue
        // (Thread 0 is special - represents kernel/idle, not a real thread)
        if current_id.0 != 0 {
            scheduler.make_ready(current_id);
        }

        // Get pointer to next thread's context and mark it as running
        let next_ctx: *const CpuContext = {
            let next = scheduler
                .get_thread_mut(next_id)
                .expect("next thread missing");
            next.state = ThreadState::Running;
            &next.context as *const CpuContext
        };

        // Get pointer to current thread's context (if any)
        let current_ctx: *mut CpuContext = if current_id.0 != 0 {
            let cur = scheduler
                .get_thread_mut(current_id)
                .expect("current thread missing");
            &mut cur.context as *mut CpuContext
        } else {
            core::ptr::null_mut()
        };

        // Update global current thread ID
        CURRENT_THREAD_ID.store(next_id.0, Ordering::SeqCst);

        // Return context pointers and whether this is a first-time switch
        (current_ctx, next_ctx, current_ctx.is_null())
        // Scheduler mutex is automatically released here!
    };

    // Perform the actual context switch with interrupts disabled
    // This is the critical moment where we switch between threads
    unsafe {
        if first_switch {
            // First time switching to this thread - just load its context
            load_context(next_ctx);
        } else {
            // Normal context switch - save current, load next
            context_switch(current_ctx, next_ctx);
        }
    }

    // This line should never be reached in normal operation
    // If we get here, something went wrong with the context switch
    crate::arch::x86_64::interrupts::enable();
}

#[inline(always)]
fn return_unlock_and_enable() {
    // helper if you want; or inline logic above
    crate::arch::x86_64::interrupts::enable();
}

fn switch_to_thread(scheduler: &mut Scheduler, current_id: ThreadId, next_id: ThreadId) {
    // Get next thread context pointer + mark Running
    let next_ctx: *const CpuContext = {
        let next = match scheduler.get_thread_mut(next_id) {
            Some(t) => t,
            None => {
                log::error!("Thread {:?} not found", next_id);
                return;
            }
        };
        next.state = ThreadState::Running;
        &next.context as *const CpuContext
    };

    // Get current thread context pointer (if any)
    let current_ctx: *mut CpuContext = if current_id.0 != 0 {
        match scheduler.get_thread_mut(current_id) {
            Some(cur) => &mut cur.context as *mut CpuContext,
            None => core::ptr::null_mut(),
        }
    } else {
        core::ptr::null_mut()
    };

    CURRENT_THREAD_ID.store(next_id.0, Ordering::SeqCst);

    unsafe {
        if !current_ctx.is_null() {
            context_switch(current_ctx, next_ctx);
        } else {
            load_context(next_ctx);
        }
    }
}

/// Low-level context switch between threads
/// 
/// This is the most critical function in the scheduler - it performs the actual
/// CPU context switch between two threads. The process involves:
/// 
/// CONTEXT SWITCH PROCESS:
/// ======================
/// 
/// 1. SAVE CURRENT STATE: Store all callee-saved registers to current thread's context
/// 2. SAVE FLAGS: Store CPU flags register (RFLAGS) with sanitization
/// 3. LOAD NEXT STATE: Restore all registers from next thread's context
/// 4. RESTORE FLAGS: Restore CPU flags to resume next thread's execution
/// 
/// REGISTER LAYOUT IN CONTEXT:
/// ===========================
/// 
/// Offset | Register | Purpose
/// -------|----------|--------
///   0    |   RSP    | Stack pointer
///   8    |   RBP    | Base pointer
///  16    |   RBX    | General purpose (callee-saved)
///  24    |   R12    | General purpose (callee-saved)
///  32    |   R13    | General purpose (callee-saved)
///  40    |   R14    | General purpose (callee-saved)
///  48    |   R15    | General purpose (callee-saved)
///  56    |  RFLAGS  | CPU flags (added for completeness)
/// 
/// RFLAGS SANITIZATION:
/// ===================
/// 
/// We sanitize the saved RFLAGS to ensure:
/// - IF (Interrupt Flag) is always set (enable interrupts)
/// - TF (Trap Flag) is cleared (disable single-step debugging)
/// - DF (Direction Flag) is cleared (ensure string ops work correctly)
/// 
/// This prevents threads from accidentally disabling interrupts permanently
/// or leaving the CPU in an unexpected state.
/// 
/// CALLING CONVENTION:
/// ==================
/// 
/// - RDI: Pointer to current thread's context (to save into)
/// - RSI: Pointer to next thread's context (to load from)
/// 
/// The function never returns to the caller in the normal sense - instead,
/// execution continues in the next thread from where it last yielded.
unsafe fn context_switch(current_ctx: *mut CpuContext, next_ctx: *const CpuContext) {
    unsafe {
        asm!(
            // === SAVE CURRENT THREAD'S STATE ===
            
            // Save all callee-saved general purpose registers
            // These must be preserved across function calls per x86_64 ABI
            "mov [rdi + 0], rsp",    // Save stack pointer
            "mov [rdi + 8], rbp",    // Save base pointer
            "mov [rdi + 16], rbx",   // Save RBX
            "mov [rdi + 24], r12",   // Save R12
            "mov [rdi + 32], r13",   // Save R13
            "mov [rdi + 40], r14",   // Save R14
            "mov [rdi + 48], r15",   // Save R15

            // Save CPU flags register
            "pushfq",                        // Push RFLAGS onto stack
            "pop qword ptr [rdi + 56]",      // Pop into context structure

            // Sanitize the saved flags for safety
            "or  qword ptr [rdi + 56], 0x200",           // Force IF=1 (enable interrupts)
            "and qword ptr [rdi + 56], 0xfffffffffffffaff", // Clear TF and DF flags

            // === LOAD NEXT THREAD'S STATE ===
            
            // Restore all callee-saved general purpose registers
            "mov rsp, [rsi + 0]",    // Load stack pointer (critical!)
            "mov rbp, [rsi + 8]",    // Load base pointer
            "mov rbx, [rsi + 16]",   // Load RBX
            "mov r12, [rsi + 24]",   // Load R12
            "mov r13, [rsi + 32]",   // Load R13
            "mov r14, [rsi + 40]",   // Load R14
            "mov r15, [rsi + 48]",   // Load R15

            // Restore CPU flags register (already sanitized when saved)
            "push qword ptr [rsi + 56]",     // Push saved RFLAGS onto stack
            "popfq",                         // Pop into RFLAGS register

            // At this point, we're running on the next thread's stack with its context
            // When this function "returns", it returns to where the next thread yielded

            in("rdi") current_ctx,  // RDI = pointer to current context
            in("rsi") next_ctx,     // RSI = pointer to next context
        );
    }
}

/// Load context for first-time thread switch
/// 
/// This function is used when switching to a thread for the very first time.
/// Unlike context_switch(), we don't need to save any current state because
/// there's no previous thread context to preserve.
/// 
/// FIRST-TIME THREAD STARTUP:
/// ==========================
/// 
/// When a thread is created, its stack is set up like this:
/// 
/// High Address  [Stack Top]
///               [Entry Point Address] <- RSP points here
///               [Available Stack Space]
///               [...]
/// Low Address   [Stack Bottom]
/// 
/// The 'ret' instruction at the end pops the entry point address from the
/// stack and jumps to it, effectively starting the thread's execution.
/// 
/// WHY NORETURN:
/// ============
/// 
/// This function never returns to its caller because:
/// 1. We switch to the new thread's stack
/// 2. We jump to the thread's entry point function
/// 3. The thread runs independently from this point
/// 
/// The options(noreturn) tells the compiler that this function never returns,
/// which helps with optimization and prevents warnings about unreachable code.
/// 
/// REGISTER INITIALIZATION:
/// =======================
/// 
/// We load all callee-saved registers from the context, even though they're
/// initially zero. This ensures consistent behavior and makes debugging easier.
/// 
/// FLAGS SANITIZATION:
/// ==================
/// 
/// We sanitize RFLAGS to ensure the new thread starts with:
/// - Interrupts enabled (IF=1)
/// - No single-step debugging (TF=0)
/// - Correct string operation direction (DF=0)
unsafe fn load_context(ctx: *const CpuContext) -> ! {
    unsafe {
        asm!(
            // === LOAD NEW THREAD'S INITIAL STATE ===
            
            // Load all callee-saved registers from context
            "mov rsp, [rdi + 0]",    // Load stack pointer (switches to thread's stack!)
            "mov rbp, [rdi + 8]",    // Load base pointer
            "mov rbx, [rdi + 16]",   // Load RBX (initially 0)
            "mov r12, [rdi + 24]",   // Load R12 (initially 0)
            "mov r13, [rdi + 32]",   // Load R13 (initially 0)
            "mov r14, [rdi + 40]",   // Load R14 (initially 0)
            "mov r15, [rdi + 48]",   // Load R15 (initially 0)

            // Sanitize and load CPU flags
            "mov rax, [rdi + 56]",               // Load saved RFLAGS into RAX
            "or  rax, 0x200",                    // Force IF=1 (enable interrupts)
            "and rax, 0xfffffffffffffaff",       // Clear TF and DF flags
            "push rax",                          // Push sanitized flags onto stack
            "popfq",                             // Pop into RFLAGS register

            // Jump to thread's entry point
            // The entry point address was placed on the stack during thread creation
            // 'ret' pops this address and jumps to it, starting the thread
            "ret",

            in("rdi") ctx,          // RDI = pointer to thread context
            options(noreturn),      // This function never returns
        );
    }
}

/// Get current thread ID
pub fn current_thread_id() -> ThreadId {
    ThreadId(CURRENT_THREAD_ID.load(Ordering::SeqCst))
}

/// Sleep for a number of milliseconds (cooperative)
/// 
/// This function implements cooperative sleeping by repeatedly yielding
/// the CPU until the specified time has elapsed. This is different from
/// preemptive sleep where the thread would be blocked and automatically
/// woken up by the scheduler.
/// 
/// COOPERATIVE SLEEP PROCESS:
/// =========================
/// 
/// 1. RECORD START TIME: Note current system uptime
/// 2. YIELD LOOP: Repeatedly call yield_now() to give CPU to other threads
/// 3. CHECK TIME: After each yield, check if enough time has passed
/// 4. CONTINUE: Keep yielding until target time is reached
/// 
/// WHY COOPERATIVE SLEEP?
/// =====================
/// 
/// In a cooperative scheduler, we can't just block a thread and have the
/// scheduler wake it up later (that would require preemptive features).
/// Instead, the thread must actively check if it's time to wake up.
/// 
/// ADVANTAGES:
/// - Simple implementation
/// - No need for timer-based wakeup mechanism
/// - Thread remains responsive to other events
/// 
/// DISADVANTAGES:
/// - Less precise timing (depends on other threads yielding)
/// - Thread continues to consume some CPU time
/// - Not suitable for hard real-time requirements
/// 
/// USAGE EXAMPLE:
/// =============
/// 
/// ```rust
/// // Sleep for 1 second
/// scheduler::sleep_ms(1000);
/// 
/// // Sleep for 100 milliseconds
/// scheduler::sleep_ms(100);
/// ```
/// 
/// The actual sleep time may be slightly longer than requested due to:
/// - Other threads running between yields
/// - Timer resolution (currently 10ms)
/// - Scheduling overhead
pub fn sleep_ms(ms: u64) {
    // Record the time when sleep started
    let start_time = crate::utils::timer::uptime_ms();
    
    // Keep yielding until enough time has passed
    while crate::utils::timer::uptime_ms() - start_time < ms {
        yield_now();
    }
}
