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
}

impl Process {
    /// Create a new process with the specified address space
    ///
    /// This is the general constructor used for both kernel and userspace processes.
    /// The parent_id should be set separately after creation using set_parent().
    pub fn new(id: ProcessId, name: &str, address_space: AddressSpace) -> Self {
        Process {
            id,
            parent_id: None,
            name: String::from(name),
            state: ProcessState::Running,
            fd_table: FileDescriptorTable::new(),
            threads: Vec::new(),
            exit_code: None,
            address_space,
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
    pub fn new_kernel(id: ProcessId, name: String) -> Self {
        Process {
            id,
            parent_id: None,
            name,
            state: ProcessState::Running,
            fd_table: FileDescriptorTable::new(),
            threads: Vec::new(),
            exit_code: None,
            address_space: AddressSpace::new_kernel(),
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
