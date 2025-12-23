/*
 * Process Abstraction
 *
 * This module implements the Process abstraction for CLUU microkernel.
 * A Process represents a container for:
 * - Address space (page tables)
 * - File descriptor table
 * - One or more threads
 *
 * This follows the traditional Unix process model where:
 * - Processes own resources (memory, file descriptors)
 * - Threads execute code within a process context
 * - Threads within the same process share the address space and FD table
 *
 * Why this is important:
 * - Enables proper userspace support with isolated address spaces
 * - Provides POSIX-compliant file descriptor semantics (shared within process)
 * - Foundation for future fork/exec implementation
 * - Allows proper resource cleanup when process terminates
 */

use alloc::{string::String, vec::Vec};
use crate::io::FileDescriptorTable;
use crate::memory::AddressSpace;

/// Unique identifier for a process
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub usize);

impl ProcessId {
    /// Create a new ProcessId
    pub fn new(id: usize) -> Self {
        ProcessId(id)
    }

    /// Get the raw ID value
    pub fn as_usize(&self) -> usize {
        self.0
    }
}

/// Process state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Process is running (has at least one runnable thread)
    Running,
    /// Process has exited but not yet been reaped
    Zombie,
}

/// A process represents an isolated execution environment
///
/// Processes own:
/// - An address space (will be added in Phase 3)
/// - A file descriptor table (shared by all threads)
/// - One or more threads
///
/// Threads within a process:
/// - Share the same address space
/// - Share the same file descriptor table
/// - Have their own kernel stack and execution state
pub struct Process {
    /// Unique process identifier
    pub id: ProcessId,

    /// Parent process ID (None for kernel/init process)
    pub parent_id: Option<ProcessId>,

    /// Human-readable process name (for debugging)
    pub name: String,

    /// Current process state
    pub state: ProcessState,

    /// File descriptor table (shared by all threads in this process)
    pub fd_table: FileDescriptorTable,

    /// List of thread IDs belonging to this process
    pub threads: Vec<super::thread::ThreadId>,

    /// Exit code (valid only in Zombie state)
    pub exit_code: Option<i32>,

    /// Address space (page tables and memory regions)
    pub address_space: AddressSpace,

    /// Process type (Critical, System, User, RealTime)
    /// Determines scheduling priority and boot-time behavior
    pub process_type: super::ProcessType,

    /// Initialization state
    /// Tracks whether the process has completed initialization
    pub init_state: super::ProcessInitState,
}

impl Process {
    /// Create a new process with the specified address space
    ///
    /// This is the general constructor used for both kernel and userspace processes.
    /// The parent_id should be set separately after creation using set_parent().
    pub fn new(id: ProcessId, name: &str, address_space: AddressSpace, process_type: super::ProcessType) -> Self {
        Process {
            id,
            parent_id: None,
            name: String::from(name),
            state: ProcessState::Running,
            fd_table: FileDescriptorTable::new(),
            threads: Vec::new(),
            exit_code: None,
            address_space,
            process_type,
            init_state: super::ProcessInitState::Initializing,
        }
    }

    /// Create a new kernel process
    ///
    /// Kernel processes:
    /// - Run in Ring 0 (kernel mode)
    /// - Use the kernel address space
    /// - Have no user-accessible pages
    /// - Have no parent (parent_id = None)
    ///
    /// This is used for kernel threads that run during boot
    /// and for kernel services.
    pub fn new_kernel(id: ProcessId, name: String, process_type: super::ProcessType) -> Self {
        Process {
            id,
            parent_id: None,
            name,
            state: ProcessState::Running,
            fd_table: FileDescriptorTable::new(),
            threads: Vec::new(),
            exit_code: None,
            address_space: AddressSpace::new_kernel(),
            process_type,
            init_state: super::ProcessInitState::Initializing,
        }
    }

    /// Add a thread to this process
    ///
    /// Called when spawning a new thread within this process.
    pub fn add_thread(&mut self, thread_id: super::thread::ThreadId) {
        self.threads.push(thread_id);
    }

    /// Remove a thread from this process
    ///
    /// Called when a thread terminates.
    /// If this was the last thread, the process transitions to Zombie state.
    pub fn remove_thread(&mut self, thread_id: super::thread::ThreadId) {
        self.threads.retain(|&id| id != thread_id);

        // If no threads remain, mark process as zombie
        if self.threads.is_empty() {
            self.state = ProcessState::Zombie;
        }
    }

    /// Mark process as exited with given exit code
    ///
    /// The process transitions to Zombie state and stores the exit code.
    /// It remains in memory until reaped by a parent process (future work).
    pub fn exit(&mut self, code: i32) {
        self.state = ProcessState::Zombie;
        self.exit_code = Some(code);
        // Note: We don't clear threads here - they'll be cleaned up by scheduler
    }

    /// Check if process is a zombie
    pub fn is_zombie(&self) -> bool {
        self.state == ProcessState::Zombie
    }

    /// Check if process has any threads
    pub fn has_threads(&self) -> bool {
        !self.threads.is_empty()
    }

    /// Get number of threads in this process
    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }

    /// Set the parent process ID
    ///
    /// This is called when spawning a child process to establish the
    /// parent-child relationship. Used for wait/waitpid semantics.
    pub fn set_parent(&mut self, parent_id: ProcessId) {
        self.parent_id = Some(parent_id);
    }

    /// Get the parent process ID
    ///
    /// Returns None if this is a kernel process or orphaned.
    pub fn parent(&self) -> Option<ProcessId> {
        self.parent_id
    }

    /// Get the exit code
    ///
    /// Returns the exit code if the process has terminated, or 0 if not set.
    pub fn exit_code(&self) -> i32 {
        self.exit_code.unwrap_or(0)
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Process")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("thread_count", &self.threads.len())
            .field("exit_code", &self.exit_code)
            .finish()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Process cleanup happens here when the process is destroyed
        // The address_space Drop implementation will handle:
        // - Unmapping all user pages
        // - Freeing page tables
        // - Returning physical frames to the allocator

        // FD table cleanup happens automatically when fd_table is dropped
        // All Arc<dyn Device> references are released

        // Note: We don't log here because this may run in IRQ context
        // Use IRQ-safe logging if needed:
        // use crate::utils::debug::irq_log;
        // irq_log::irq_log_str("Process dropped\n");
    }
}

// ================================================================================================
// PROCESS MANAGER
// ================================================================================================

use core::sync::atomic::Ordering;
use super::thread::ThreadId;

/// Process lifecycle management
///
/// This manager provides namespaced methods for process operations.
/// All methods internally access the global scheduler singleton using helper functions.
///
/// ProcessManager is a Zero-Sized Type (ZST) that provides organizational structure
/// without any runtime cost. It groups related process operations under a clear namespace.
pub struct ProcessManager;

impl ProcessManager {
    /// Create a new kernel process
    ///
    /// This creates a process with its own file descriptor table and resource
    /// management, but using the kernel address space (Ring 0).
    ///
    /// # Arguments
    /// * `name` - Human-readable process name
    /// * `process_type` - Process classification (Critical, System, User, RealTime)
    ///
    /// # Returns
    /// The ProcessId of the newly created process
    pub fn spawn_kernel(name: &str, process_type: super::ProcessType) -> ProcessId {
        super::with_scheduler_mut(|s| s.create_kernel_process(name, process_type))
    }

    /// Create a new userspace process with dedicated page tables
    ///
    /// This allocates a fresh address space with its own PML4 and copies
    /// kernel mappings for syscall handling.
    ///
    /// # Arguments
    /// * `name` - Human-readable process name
    /// * `process_type` - Process classification (Critical, System, User, RealTime)
    ///
    /// # Returns
    /// * `Ok(ProcessId)` if process was created successfully
    /// * `Err(&str)` if process creation failed
    pub fn spawn_user(
        name: &str,
        process_type: super::ProcessType,
    ) -> Result<ProcessId, &'static str> {
        super::with_scheduler_mut(|scheduler| {
            // Create new userspace address space
            let address_space = AddressSpace::new_user()?;

            // Create process with userspace address space
            let process_id = scheduler
                .allocate_pid()
                .ok_or("Failed to allocate PID - all PIDs in use")?;

            let process = Process::new(process_id, name, address_space, process_type);
            scheduler.processes.insert(process_id, process);

            log::info!(
                "Created userspace process '{}' (type: {:?}) with ID {:?}",
                name,
                process_type,
                process_id
            );
            Ok(process_id)
        })
    }

    /// Get the process ID for the currently running thread
    ///
    /// Returns None if no thread is currently running or scheduler not initialized.
    pub fn current_id() -> Option<ProcessId> {
        let current_tid = ThreadId(super::CURRENT_THREAD_ID.load(Ordering::Relaxed));
        if current_tid.0 == 0 {
            return None;
        }

        super::with_scheduler(|scheduler| {
            scheduler
                .threads
                .iter()
                .find(|t| t.id == current_tid)
                .map(|t| t.process_id)
        })
    }

    /// Execute a closure with access to the current process (immutable)
    ///
    /// This is a helper function for syscalls that need to access the current
    /// process's state (e.g., file descriptor table).
    ///
    /// # Arguments
    /// * `f` - Closure that receives a reference to the current process
    ///
    /// # Returns
    /// The result of the closure, or None if process not found
    pub fn with_current<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&Process) -> R,
    {
        let pid = Self::current_id()?;
        super::with_scheduler(|s| s.get_process(pid).map(f))
    }

    /// Execute a closure with access to the current process (mutable)
    ///
    /// This is a helper function for syscalls that need to modify the current
    /// process's state (e.g., modifying file descriptor table).
    ///
    /// # Arguments
    /// * `f` - Closure that receives a mutable reference to the current process
    ///
    /// # Returns
    /// The result of the closure, or None if process not found
    pub fn with_current_mut<F, R>(f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let pid = Self::current_id()?;
        super::with_scheduler_mut(|s| s.get_process_mut(pid).map(f))
    }

    /// Execute a closure with access to a specific process (mutable)
    ///
    /// This is a helper function for loading binaries or modifying a process's
    /// state by process ID.
    ///
    /// # Arguments
    /// * `process_id` - The ID of the process to access
    /// * `f` - Closure that receives mutable access to the process
    ///
    /// # Returns
    /// The result of the closure, or None if the process doesn't exist.
    pub fn with_mut<F, R>(process_id: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        super::with_scheduler_mut(|s| s.get_process_mut(process_id).map(f))
    }

    /// Reap a zombie process (remove it from the process table)
    ///
    /// This is called by sys_waitpid after reading the exit code of a zombie process.
    /// The process and all its resources (address space, file descriptors) are freed.
    ///
    /// # Arguments
    /// * `process_id` - The PID of the zombie process to reap
    ///
    /// # Returns
    /// * `Ok(exit_code)` if the process was reaped successfully
    /// * `Err(&str)` if the process doesn't exist or is not a zombie
    pub fn reap(process_id: ProcessId) -> Result<i32, &'static str> {
        super::with_scheduler_mut(|scheduler| {
            // Check if process exists and is a zombie
            let process = scheduler
                .processes
                .get(&process_id)
                .ok_or("Process not found")?;

            if !process.is_zombie() {
                return Err("Process is not a zombie");
            }

            let exit_code = process.exit_code();

            // Remove the process from the table
            // The Process Drop implementation will clean up:
            // - Address space (page tables, mapped pages)
            // - File descriptors
            if let Some(process) = scheduler.processes.remove(&process_id) {
                log::info!(
                    "Reaped zombie process {} ({}) with exit code {}",
                    process_id.0,
                    process.name,
                    exit_code
                );
                drop(process);
                Ok(exit_code)
            } else {
                Err("Failed to remove process")
            }
        })
    }
}
