//! Process Abstraction
//!
//! This module implements the Process abstraction for the CLUU microkernel.
//!
//! # Process Model
//!
//! A Process represents a container for:
//! - **Address space** (page tables and memory regions)
//! - **File descriptor table** (shared by all threads)
//! - **One or more threads** (execution units)
//!
//! This follows the traditional Unix process model:
//! - Processes own resources (memory, file descriptors)
//! - Threads execute code within a process context
//! - Threads within the same process share the address space and FD table
//!
//! # Design Principles (SOLID)
//!
//! - **Single Responsibility**: Process only manages process-level resources
//! - **Dependency Inversion**: Depends on AddressSpace abstraction, not concrete VMM
//! - **Open/Closed**: Can extend with new resource types without modifying core
//!
//! # Process Lifecycle
//!
//! ```text
//! new() → Running → exit() → Zombie → drop()
//!         ↑         (all threads terminated)
//!         └─── add_thread() / remove_thread()
//! ```

use super::thread::ThreadId;
use crate::mm::AddressSpace;
use alloc::string::String;
use alloc::vec::Vec;

// ═══════════════════════════════════════════════════════════════════════════
// Process Identifier
// ═══════════════════════════════════════════════════════════════════════════

/// Unique identifier for a process
///
/// ProcessIds are allocated sequentially and never reused within a single
/// boot session. This ensures references to old processes are always invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub usize);

impl ProcessId {
    /// Create a new ProcessId
    pub const fn new(id: usize) -> Self {
        ProcessId(id)
    }

    /// Get the raw ID value
    pub const fn as_usize(&self) -> usize {
        self.0
    }
}

impl core::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PID{}", self.0)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Process State
// ═══════════════════════════════════════════════════════════════════════════

/// Process state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Process is running (has at least one runnable thread)
    Running,
    /// Process has exited but not yet been reaped
    ///
    /// Zombie processes:
    /// - Have no runnable threads
    /// - Store exit code for parent to retrieve
    /// - Still occupy PID and memory until reaped
    Zombie,
}

// ═══════════════════════════════════════════════════════════════════════════
// Process Type
// ═══════════════════════════════════════════════════════════════════════════

/// Process type determines scheduling priority and boot behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessType {
    /// Critical system process (kernel services)
    /// - Highest priority
    /// - Started during boot
    /// - Termination causes kernel panic
    Critical,

    /// System process (core services like VFS, procmgr)
    /// - High priority
    /// - Started during boot
    System,

    /// Regular user process
    /// - Normal priority
    /// - Started on demand
    User,

    /// Real-time user process
    /// - Elevated priority for time-critical tasks
    /// - Requires special capability to create
    RealTime,
}

/// Process initialization state
///
/// Tracks whether a process has completed its initialization phase.
/// Used during boot to coordinate service startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInitState {
    /// Process is still initializing
    Initializing,
    /// Process has completed initialization and is ready
    Ready,
}

// ═══════════════════════════════════════════════════════════════════════════
// Process Structure
// ═══════════════════════════════════════════════════════════════════════════

/// A process represents an isolated execution environment
///
/// Processes own:
/// - An address space (page tables and memory regions)
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

    /// List of thread IDs belonging to this process
    pub threads: Vec<ThreadId>,

    /// Exit code (valid only in Zombie state)
    pub exit_code: Option<i32>,

    /// Address space (page tables and memory regions)
    pub address_space: AddressSpace,

    /// Process type (determines priority and boot behavior)
    pub process_type: ProcessType,

    /// Initialization state
    pub init_state: ProcessInitState,
}

impl Process {
    /// Create a new process with the specified address space
    ///
    /// This is the general constructor used for both kernel and userspace processes.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique process identifier
    /// * `name` - Human-readable name for debugging
    /// * `address_space` - Address space (kernel or user)
    /// * `process_type` - Type determines priority and behavior
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let addr_space = AddressSpace::new_user()?;
    /// let process = Process::new(
    ///     ProcessId::new(1),
    ///     "init",
    ///     addr_space,
    ///     ProcessType::System
    /// );
    /// ```
    pub fn new(
        id: ProcessId,
        name: &str,
        address_space: AddressSpace,
        process_type: ProcessType,
    ) -> Self {
        Process {
            id,
            parent_id: None,
            name: String::from(name),
            state: ProcessState::Running,
            threads: Vec::new(),
            exit_code: None,
            address_space,
            process_type,
            init_state: ProcessInitState::Initializing,
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
    pub fn new_kernel(id: ProcessId, name: String, process_type: ProcessType) -> Self {
        Process {
            id,
            parent_id: None,
            name,
            state: ProcessState::Running,
            threads: Vec::new(),
            exit_code: None,
            address_space: AddressSpace::new_kernel(),
            process_type,
            init_state: ProcessInitState::Initializing,
        }
    }

    /// Add a thread to this process
    ///
    /// Called when spawning a new thread within this process.
    pub fn add_thread(&mut self, thread_id: ThreadId) {
        self.threads.push(thread_id);
    }

    /// Remove a thread from this process
    ///
    /// Called when a thread terminates.
    /// If this was the last thread, the process transitions to Zombie state.
    pub fn remove_thread(&mut self, thread_id: ThreadId) {
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

    /// Mark process as initialized and ready
    ///
    /// Called when a process completes its initialization phase.
    /// Used during boot to track service startup progress.
    pub fn mark_ready(&mut self) {
        self.init_state = ProcessInitState::Ready;
    }

    /// Check if process is ready (initialization complete)
    pub fn is_ready(&self) -> bool {
        self.init_state == ProcessInitState::Ready
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Process")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("type", &self.process_type)
            .field("thread_count", &self.threads.len())
            .field("exit_code", &self.exit_code)
            .field("init_state", &self.init_state)
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

        // Note: We don't log here because this may run in IRQ context
        // Use IRQ-safe logging if needed
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_id_creation() {
        let pid = ProcessId::new(42);
        assert_eq!(pid.as_usize(), 42);
    }

    #[test]
    fn test_process_id_ordering() {
        let pid1 = ProcessId::new(1);
        let pid2 = ProcessId::new(2);
        assert!(pid1 < pid2);
        assert_eq!(pid1, pid1);
    }

    #[test]
    fn test_process_state() {
        assert_eq!(ProcessState::Running, ProcessState::Running);
        assert_ne!(ProcessState::Running, ProcessState::Zombie);
    }

    #[test]
    fn test_process_type() {
        assert_eq!(ProcessType::Critical, ProcessType::Critical);
        assert_ne!(ProcessType::User, ProcessType::System);
    }

    #[test]
    fn test_process_creation_kernel() {
        let process = Process::new_kernel(
            ProcessId::new(0),
            String::from("kernel"),
            ProcessType::Critical,
        );

        assert_eq!(process.id, ProcessId::new(0));
        assert_eq!(process.name, "kernel");
        assert_eq!(process.state, ProcessState::Running);
        assert_eq!(process.thread_count(), 0);
        assert!(!process.is_zombie());
        assert_eq!(process.parent_id, None);
    }

    #[test]
    fn test_thread_management() {
        let mut process =
            Process::new_kernel(ProcessId::new(1), String::from("test"), ProcessType::System);

        // Add threads
        process.add_thread(ThreadId::new(10));
        process.add_thread(ThreadId::new(20));
        assert_eq!(process.thread_count(), 2);
        assert!(process.has_threads());

        // Remove one thread
        process.remove_thread(ThreadId::new(10));
        assert_eq!(process.thread_count(), 1);
        assert!(!process.is_zombie());

        // Remove last thread -> becomes zombie
        process.remove_thread(ThreadId::new(20));
        assert_eq!(process.thread_count(), 0);
        assert!(process.is_zombie());
    }

    #[test]
    fn test_process_exit() {
        let mut process = Process::new_kernel(
            ProcessId::new(2),
            String::from("exiting"),
            ProcessType::User,
        );

        process.exit(42);
        assert!(process.is_zombie());
        assert_eq!(process.exit_code(), 42);
    }

    #[test]
    fn test_parent_child_relationship() {
        let mut process =
            Process::new_kernel(ProcessId::new(3), String::from("child"), ProcessType::User);

        assert_eq!(process.parent(), None);

        process.set_parent(ProcessId::new(1));
        assert_eq!(process.parent(), Some(ProcessId::new(1)));
    }

    #[test]
    fn test_init_state() {
        let mut process = Process::new_kernel(
            ProcessId::new(4),
            String::from("service"),
            ProcessType::System,
        );

        assert!(!process.is_ready());
        process.mark_ready();
        assert!(process.is_ready());
    }
}
