//! Process Manager
//!
//! This module manages the global process table and process lifecycle.
//!
//! # Architecture
//!
//! The ProcessManager maintains:
//! - **Process table** - Maps ProcessId → Process
//! - **PID allocator** - Sequential PID allocation
//! - **Lifecycle methods** - Create, destroy, lookup processes
//!
//! # Design Principles (SOLID)
//!
//! - **Single Responsibility**: Only manages process table and PIDs
//! - **Dependency Inversion**: Depends on Process abstraction
//! - **Open/Closed**: Can extend with new process types
//!
//! # Thread Safety
//!
//! ProcessManager uses a Mutex for interior mutability, allowing
//! safe concurrent access from interrupts and syscalls.

use alloc::collections::BTreeMap;
use alloc::string::String;
use spin::Mutex;
use crate::mm::AddressSpace;
use super::process::{Process, ProcessId, ProcessState, ProcessType};

// ═══════════════════════════════════════════════════════════════════════════
// Process Table
// ═══════════════════════════════════════════════════════════════════════════

/// Global process table
///
/// Stores all processes in the system. Uses BTreeMap for:
/// - O(log n) lookup by PID
/// - Ordered iteration (useful for debugging)
/// - No pre-allocation needed
struct ProcessTable {
    /// Map of ProcessId → Process
    processes: BTreeMap<ProcessId, Process>,

    /// Next PID to allocate
    next_pid: usize,
}

impl ProcessTable {
    /// Create a new empty process table
    const fn new() -> Self {
        Self {
            processes: BTreeMap::new(),
            next_pid: 1, // PID 0 reserved for idle/kernel
        }
    }

    /// Allocate a new process ID
    ///
    /// PIDs are allocated sequentially and never reused within
    /// a single boot session.
    fn allocate_pid(&mut self) -> Option<ProcessId> {
        if self.next_pid >= usize::MAX {
            return None; // PID exhaustion (unlikely!)
        }

        let pid = ProcessId::new(self.next_pid);
        self.next_pid += 1;
        Some(pid)
    }

    /// Insert a process into the table
    fn insert(&mut self, process: Process) {
        self.processes.insert(process.id, process);
    }

    /// Get a process by ID (immutable)
    fn get(&self, pid: ProcessId) -> Option<&Process> {
        self.processes.get(&pid)
    }

    /// Get a process by ID (mutable)
    fn get_mut(&mut self, pid: ProcessId) -> Option<&mut Process> {
        self.processes.get_mut(&pid)
    }

    /// Remove a process from the table
    fn remove(&mut self, pid: ProcessId) -> Option<Process> {
        self.processes.remove(&pid)
    }

    /// Count total processes
    fn count(&self) -> usize {
        self.processes.len()
    }

    /// Count processes by state
    fn count_by_state(&self, state: ProcessState) -> usize {
        self.processes
            .values()
            .filter(|p| p.state == state)
            .count()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global Process Manager
// ═══════════════════════════════════════════════════════════════════════════

/// Global process manager instance
static PROCESS_MANAGER: Mutex<ProcessTable> = Mutex::new(ProcessTable::new());

/// Process Manager - handles process lifecycle
///
/// This is a Zero-Sized Type (ZST) that provides namespaced access
/// to the global process table. All methods are static.
pub struct ProcessManager;

impl ProcessManager {
    /// Create a new kernel process
    ///
    /// Kernel processes:
    /// - Use the kernel address space (shared)
    /// - Run in Ring 0
    /// - No parent process
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable process name
    /// * `process_type` - Process classification
    ///
    /// # Returns
    ///
    /// ProcessId of the newly created process
    pub fn spawn_kernel(name: &str, process_type: ProcessType) -> ProcessId {
        let mut table = PROCESS_MANAGER.lock();

        let pid = table
            .allocate_pid()
            .expect("PID exhaustion"); // Should never happen in practice

        let process = Process::new_kernel(pid, String::from(name), process_type);

        klibcluu::trace("Created kernel process: ");
        klibcluu::trace(name);
        klibcluu::trace(" (PID ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", pid.as_usize() as u64);
        klibcluu::trace(")");

        table.insert(process);
        pid
    }

    /// Create a new userspace process
    ///
    /// Userspace processes:
    /// - Get a fresh address space with own PML4
    /// - Run in Ring 3
    /// - Kernel mappings copied to high half
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable process name
    /// * `process_type` - Process classification
    ///
    /// # Returns
    ///
    /// * `Ok(ProcessId)` on success
    /// * `Err(&str)` if address space allocation fails
    pub fn spawn_user(name: &str, process_type: ProcessType) -> Result<ProcessId, &'static str> {
        // Create new userspace address space (outside lock)
        let address_space = AddressSpace::new_user()?;

        let mut table = PROCESS_MANAGER.lock();

        let pid = table
            .allocate_pid()
            .ok_or("PID exhaustion")?;

        let process = Process::new(pid, name, address_space, process_type);

        klibcluu::info("Created userspace process: ");
        klibcluu::info(name);
        klibcluu::info(" (PID ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", pid.as_usize() as u64);
        klibcluu::info(")");

        table.insert(process);
        Ok(pid)
    }

    /// Get a process by ID (immutable access)
    ///
    /// Executes a closure with immutable access to the process.
    ///
    /// # Arguments
    ///
    /// * `pid` - Process ID to lookup
    /// * `f` - Closure that receives &Process
    ///
    /// # Returns
    ///
    /// Result of closure, or None if process not found
    pub fn with_process<F, R>(pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&Process) -> R,
    {
        let table = PROCESS_MANAGER.lock();
        table.get(pid).map(f)
    }

    /// Get a process by ID (mutable access)
    ///
    /// Executes a closure with mutable access to the process.
    ///
    /// # Arguments
    ///
    /// * `pid` - Process ID to lookup
    /// * `f` - Closure that receives &mut Process
    ///
    /// # Returns
    ///
    /// Result of closure, or None if process not found
    pub fn with_process_mut<F, R>(pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut table = PROCESS_MANAGER.lock();
        table.get_mut(pid).map(f)
    }

    /// Reap a zombie process
    ///
    /// Removes a zombie process from the table, freeing all resources.
    /// Called by sys_waitpid after reading the exit code.
    ///
    /// # Arguments
    ///
    /// * `pid` - ProcessId of zombie to reap
    ///
    /// # Returns
    ///
    /// * `Ok(exit_code)` if process was reaped
    /// * `Err(&str)` if process not found or not a zombie
    pub fn reap(pid: ProcessId) -> Result<i32, &'static str> {
        let mut table = PROCESS_MANAGER.lock();

        // Verify process exists and is zombie
        let is_zombie = table
            .get(pid)
            .ok_or("Process not found")?
            .is_zombie();

        if !is_zombie {
            return Err("Process is not a zombie");
        }

        // Get exit code before removing
        let exit_code = table.get(pid).unwrap().exit_code();

        // Remove process (Drop will clean up resources)
        let process = table.remove(pid).unwrap();

        klibcluu::info("Reaped zombie process ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", pid.as_usize() as u64);
        klibcluu::info(" (");
        klibcluu::info(&process.name);
        klibcluu::info(") with exit code ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", exit_code as u64);

        drop(process);
        Ok(exit_code)
    }

    /// Get process count
    pub fn count() -> usize {
        PROCESS_MANAGER.lock().count()
    }

    /// Get count of running processes
    pub fn count_running() -> usize {
        PROCESS_MANAGER.lock().count_by_state(ProcessState::Running)
    }

    /// Get count of zombie processes
    pub fn count_zombies() -> usize {
        PROCESS_MANAGER.lock().count_by_state(ProcessState::Zombie)
    }

    /// Execute closure for each process (for iteration/debugging)
    ///
    /// # Arguments
    ///
    /// * `f` - Closure that receives &Process for each process
    pub fn for_each<F>(mut f: F)
    where
        F: FnMut(&Process),
    {
        let table = PROCESS_MANAGER.lock();
        for process in table.processes.values() {
            f(process);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Initialization
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize the process manager
///
/// This should be called during kernel initialization.
/// Creates the initial kernel process (PID 0).
pub fn init() {
    klibcluu::info("Initializing process manager...");

    // The first process will get PID 1 automatically
    // (PID 0 is reserved for idle)

    klibcluu::info("Process manager initialized");
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_table_pid_allocation() {
        let mut table = ProcessTable::new();

        let pid1 = table.allocate_pid().unwrap();
        let pid2 = table.allocate_pid().unwrap();
        let pid3 = table.allocate_pid().unwrap();

        assert_eq!(pid1, ProcessId::new(1));
        assert_eq!(pid2, ProcessId::new(2));
        assert_eq!(pid3, ProcessId::new(3));
    }

    #[test]
    fn test_process_table_insert_get() {
        let mut table = ProcessTable::new();

        let pid = ProcessId::new(42);
        let process = Process::new_kernel(pid, String::from("test"), ProcessType::System);

        table.insert(process);

        let found = table.get(pid);
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "test");
    }

    #[test]
    fn test_process_table_remove() {
        let mut table = ProcessTable::new();

        let pid = ProcessId::new(1);
        let process = Process::new_kernel(pid, String::from("test"), ProcessType::User);

        table.insert(process);
        assert_eq!(table.count(), 1);

        let removed = table.remove(pid);
        assert!(removed.is_some());
        assert_eq!(table.count(), 0);
    }

    #[test]
    fn test_process_table_count_by_state() {
        let mut table = ProcessTable::new();

        let mut p1 = Process::new_kernel(
            ProcessId::new(1),
            String::from("running"),
            ProcessType::User,
        );
        let mut p2 = Process::new_kernel(
            ProcessId::new(2),
            String::from("zombie"),
            ProcessType::User,
        );

        p2.exit(0); // Mark as zombie

        table.insert(p1);
        table.insert(p2);

        assert_eq!(table.count_by_state(ProcessState::Running), 1);
        assert_eq!(table.count_by_state(ProcessState::Zombie), 1);
    }
}
