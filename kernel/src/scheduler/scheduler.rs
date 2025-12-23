/*
 * Scheduler Implementation
 *
 * This module contains the core Scheduler struct and its implementation.
 * The Scheduler is responsible for managing all threads and processes,
 * maintaining the ready queue, and implementing the round-robin scheduling algorithm.
 *
 * This file is separated from mod.rs to make the OOP structure clear:
 * - scheduler.rs: Implementation details (this file)
 * - mod.rs: Public API layer and interrupt handlers
 */

use alloc::{
    collections::{BTreeMap, VecDeque},
    vec::Vec,
};

use super::{Thread, ThreadId, ThreadState, Process, ProcessId, ProcessType, ProcessInitState, SchedulerMode};

/// Thread stack size (64 KiB per thread)
pub const THREAD_STACK_SIZE: usize = 64 * 1024;

/// Maximum number of threads
pub const MAX_THREADS: usize = 64;

/// Maximum PID value before wrapping around
/// This matches typical Unix systems (32768)
const MAX_PID: usize = 32768;

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
/// This is the core data structure that manages all threads and processes in the system.
/// It maintains:
///
/// 1. THREAD STORAGE: All thread objects with their stacks and metadata
/// 2. READY QUEUE: FIFO queue of threads waiting to be scheduled
/// 3. ID ALLOCATION: Ensures each thread and process gets a unique identifier
/// 4. PROCESS MANAGEMENT: All processes with their resources
/// 5. MODE STATE: Boot mode vs Normal mode operation
///
/// DESIGN DECISIONS:
/// ================
///
/// - Vec<Thread>: Stores all threads, indexed by position (not ID)
/// - VecDeque<ThreadId>: Ready queue for O(1) push/pop operations
/// - BTreeMap<ProcessId, Process>: Processes indexed by PID
/// - ThreadId/ProcessId counters: Simple incrementing ID assignment
/// - mode: SchedulerMode: Encapsulated boot/normal mode state
///
/// THREAD LOOKUP:
/// ==============
///
/// Threads are found by linear search through the Vec. This is acceptable
/// for a microkernel with a small number of threads. For better performance
/// with many threads, we could use a HashMap<ThreadId, Thread>.
pub struct Scheduler {
    pub(super) threads: Vec<Thread>,                    // All threads in the system
    pub(super) ready_queue: VecDeque<ThreadId>,         // Queue of threads ready to run
    next_thread_id: ThreadId,                            // Next ID to assign to new thread
    pub(super) processes: BTreeMap<ProcessId, Process>, // All processes in the system
    next_process_id: ProcessId,                          // Next ID to assign to new process

    // Boot mode state (moved from module-level static)
    mode: SchedulerMode,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            threads: Vec::new(),
            ready_queue: VecDeque::new(),
            next_thread_id: ThreadId(1), // ID 0 reserved for kernel/idle
            processes: BTreeMap::new(),
            next_process_id: ProcessId(1), // ID 0 reserved for kernel
            mode: SchedulerMode::Boot {
                critical_count: 0,
                ready_count: 0,
            },
        }
    }

    /// Get the current scheduler mode
    pub fn mode(&self) -> SchedulerMode {
        self.mode
    }

    /// Register a critical process that must initialize before normal mode
    ///
    /// Critical processes are the only ones scheduled during boot mode.
    /// Once all critical processes signal ready, the scheduler transitions to normal mode.
    ///
    /// # Arguments
    /// * `pid` - Process ID of the critical process
    pub fn register_critical_process(&mut self, pid: ProcessId) {
        if let SchedulerMode::Boot { ref mut critical_count, .. } = self.mode {
            *critical_count += 1;

            // Get process name for logging
            let process_name = self.processes.get(&pid)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "?".into());

            log::info!("Registered critical process {} '{}' (total: {})",
                       pid.0,
                       process_name,
                       critical_count);
        } else {
            log::warn!("Cannot register critical process {} - scheduler already in normal mode", pid.0);
        }
    }

    /// Process signals it has completed initialization
    ///
    /// Called by critical processes when they have finished initialization
    /// and are ready to serve requests.
    ///
    /// When all critical processes have signaled ready, the scheduler automatically
    /// transitions from Boot mode to Normal mode, allowing user processes to run.
    ///
    /// # Arguments
    /// * `pid` - Process ID signaling ready
    ///
    /// # Returns
    /// Ok if signaled successfully, Err if process is not critical or already signaled
    pub fn signal_process_ready(&mut self, pid: ProcessId) -> Result<(), &'static str> {
        // Update process state
        let process = self.get_process_mut(pid).ok_or("Process not found")?;

        if process.process_type != ProcessType::Critical {
            return Err("Only critical processes can signal ready during boot");
        }

        if process.init_state == ProcessInitState::Ready {
            return Err("Process already signaled ready");
        }

        process.init_state = ProcessInitState::Ready;
        log::info!("Critical process {} '{}' signaled ready", pid.0, process.name);

        // Check if we should transition to normal mode
        if let SchedulerMode::Boot { critical_count, ref mut ready_count } = self.mode {
            *ready_count += 1;
            log::info!("Critical processes ready: {}/{}", ready_count, critical_count);

            if *ready_count == critical_count {
                log::info!("========================================");
                log::info!("All critical processes ready!");
                log::info!("Transitioning to NORMAL MODE");
                log::info!("========================================");
                self.mode = SchedulerMode::Normal;
            }
        }

        Ok(())
    }

    /// Allocate a new process ID
    ///
    /// This function allocates PIDs in a continuously increasing manner,
    /// wrapping around at MAX_PID and skipping PIDs that are still in use.
    /// This prevents immediate PID reuse and associated bugs.
    ///
    /// Returns None if all PIDs are exhausted (extremely unlikely).
    pub(super) fn allocate_pid(&mut self) -> Option<ProcessId> {
        let start_pid = self.next_process_id.0;

        // Try to find an unused PID, starting from next_process_id
        loop {
            let candidate = ProcessId(self.next_process_id.0);

            // Increment for next allocation (with wrapping)
            self.next_process_id.0 += 1;
            if self.next_process_id.0 >= MAX_PID {
                self.next_process_id.0 = 1; // Wrap around, skip PID 0 (kernel)
            }

            // Check if this PID is available
            if !self.processes.contains_key(&candidate) && candidate.0 != 0 {
                return Some(candidate);
            }

            // If we've wrapped all the way around, all PIDs are in use!
            if self.next_process_id.0 == start_pid {
                return None;
            }
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
    pub(super) fn create_thread(&mut self, entry_point: fn(), name: &str, process_id: ProcessId) -> ThreadId {
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

        log::info!(
            "Created thread '{}' (ID {:?}) in process {:?}",
            name,
            thread_id,
            process_id
        );
        thread_id
    }

    /// Get the next thread to run
    ///
    /// Respects scheduler mode:
    /// - Boot mode: Only schedules threads from Critical processes (+ kernel threads)
    /// - Normal mode: Schedules all threads by priority (RT > Critical > System > User)
    pub(super) fn get_next_thread(&mut self) -> Option<ThreadId> {
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
        // and respects scheduler mode restrictions
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

                // BOOT MODE FILTER: Only schedule critical processes (+ kernel threads)
                // Use self.mode instead of global SCHEDULER_MODE
                if let SchedulerMode::Boot { .. } = self.mode {
                    // Get process type for this thread
                    if let Some(process) = self.processes.get(&thread.process_id) {
                        // EXCEPTION: Always allow kernel process (PID 0) threads (includes idle)
                        if process.id.0 != 0 && process.process_type != ProcessType::Critical {
                            // Non-critical, non-kernel process in boot mode - put back at end of queue
                            self.ready_queue.push_back(thread_id);
                            continue;
                        }
                    }
                }
            }

            // Thread is valid and allowed to run in current mode
            return Some(thread_id);
        }
    }

    /// Add thread back to ready queue
    pub(super) fn make_ready(&mut self, thread_id: ThreadId) {
        if let Some(thread) = self.threads.iter_mut().find(|t| t.id == thread_id) {
            if thread.state == ThreadState::Running {
                thread.state = ThreadState::Ready;
                self.ready_queue.push_back(thread_id);
            }
        }
    }

    /// Get thread by ID
    pub(super) fn get_thread_mut(&mut self, thread_id: ThreadId) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == thread_id)
    }

    /// Clean up terminated threads
    ///
    /// Removes all terminated threads from the scheduler, freeing their resources.
    /// This is called automatically during context switches (yield_now) when a
    /// terminated thread is detected.
    ///
    /// For processes that have no remaining threads, marks them as zombie processes
    /// with their exit code. Zombie processes remain in the process table until
    /// reaped by sys_waitpid.
    ///
    /// # Arguments
    /// * `current_thread_id` - The currently running thread (must not be cleaned up)
    /// * `log_cleanup` - Whether to log cleanup (should be false in IRQ context!)
    ///
    /// Returns the number of threads cleaned up.
    pub(super) fn cleanup_terminated_threads(
        &mut self,
        current_thread_id: ThreadId,
        log_cleanup: bool,
    ) -> usize {
        // First, identify threads to clean up
        let mut to_remove = alloc::vec::Vec::new();
        let mut processes_to_check = alloc::collections::BTreeMap::new();

        for thread in &self.threads {
            if thread.state == ThreadState::Terminated
                && thread.id != current_thread_id
                && thread.id.0 != 0
            {
                to_remove.push((thread.id, thread.name.clone(), thread.process_id));
                // Store exit code for each process (last thread's exit code wins)
                processes_to_check.insert(thread.process_id, thread.exit_code.unwrap_or(0));
            }
        }

        // CRITICAL: Only log if NOT in IRQ context (logging in IRQ can deadlock!)
        if log_cleanup {
            for (id, name, _) in &to_remove {
                log::info!("Reaper: Cleaning up thread {} ({})", id.0, name);
            }
        }

        // Now remove them (dropping Thread frees stack)
        let initial_count = self.threads.len();
        self.threads
            .retain(|t| !to_remove.iter().any(|(id, _, _)| t.id == *id));

        // Check if any processes have no remaining threads and mark them as zombies
        for (process_id, exit_code) in processes_to_check {
            // Skip kernel process (PID 0)
            if process_id.0 == 0 {
                continue;
            }

            // Check if this process has any remaining threads
            let has_threads = self.threads.iter().any(|t| t.process_id == process_id);

            if !has_threads {
                // No threads left in this process - mark as zombie
                // The process will remain in the table until reaped by sys_waitpid
                if let Some(process) = self.processes.get_mut(&process_id) {
                    if !process.is_zombie() {
                        if log_cleanup {
                            log::info!(
                                "Reaper: Marking process {} ({}) as ZOMBIE with exit code {}",
                                process_id.0,
                                process.name,
                                exit_code
                            );
                        }
                        process.exit(exit_code); // Mark as zombie with thread's exit code
                    }
                }
            }
        }

        initial_count - self.threads.len()
    }

    /// Create a new kernel process
    ///
    /// Kernel processes run in Ring 0 and use the kernel address space.
    /// This is used for kernel threads that need isolated resource management.
    pub(super) fn create_kernel_process(&mut self, name: &str, process_type: ProcessType) -> ProcessId {
        let process_id = self.allocate_pid()
            .expect("Failed to allocate PID for kernel process");

        let process = Process::new_kernel(process_id, name.into(), process_type);
        self.processes.insert(process_id, process);

        log::info!("Created kernel process '{}' (type: {:?}) with ID {:?}", name, process_type, process_id);
        process_id
    }

    /// Get a process by ID (immutable)
    pub(super) fn get_process(&self, process_id: ProcessId) -> Option<&Process> {
        self.processes.get(&process_id)
    }

    /// Get a process by ID (mutable)
    pub(super) fn get_process_mut(&mut self, process_id: ProcessId) -> Option<&mut Process> {
        self.processes.get_mut(&process_id)
    }
}

/// Thread exit trampoline
///
/// This function is placed as the return address on every thread's stack.
/// If a thread's entry function returns (instead of calling exit_thread),
/// it will "return" here, and we'll properly clean it up.
///
/// This prevents INVALID_OPCODE exceptions from executing garbage addresses.
extern "C" fn thread_exit_trampoline() -> ! {
    // Thread returned instead of calling exit_thread() - clean up properly
    log::info!("!!! Thread returned to trampoline - calling ThreadManager::exit()");
    super::ThreadManager::exit(0); // Thread returned normally, exit with code 0
}
