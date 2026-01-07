//! Process Spawning
//!
//! This module integrates ELF loading with process creation to spawn
//! userspace processes from ELF binaries.
//!
//! # Flow
//!
//! ```text
//! ELF binary → load_elf() → Process → Thread → Scheduler
//!   (bytes)     (parse)    (create)  (spawn)   (schedule)
//! ```
//!
//! # Design
//!
//! This is the glue layer that connects:
//! - **ProcessManager** - Process lifecycle
//! - **ELF Loader** - Binary parsing and loading
//! - **Address Space** - Memory management
//! - **Thread** (future) - Execution units

use super::process::{ProcessId, ProcessType};
use super::process_manager::ProcessManager;
use crate::elf;
use crate::mm::AddressSpace;

/// Spawn a userspace process from an ELF binary
///
/// This is the high-level function that:
/// 1. Creates a new userspace process (with address space)
/// 2. Loads the ELF binary into the address space
/// 3. Returns the ProcessId
///
/// # Arguments
///
/// * `name` - Process name
/// * `elf_data` - Raw ELF binary bytes
/// * `process_type` - Process classification
///
/// # Returns
///
/// * `Ok(ProcessId)` - Process created and loaded successfully
/// * `Err(&str)` - Error during creation or loading
///
/// # Example
///
/// ```rust,no_run
/// let init_binary = include_bytes!("../initrd/sys/init");
/// let pid = spawn_elf_process("init", init_binary, ProcessType::System)?;
/// ```
pub fn spawn_elf_process(
    name: &str,
    elf_data: &[u8],
    process_type: ProcessType,
) -> Result<ProcessId, &'static str> {
    klibcluu::trace("Spawning ELF process: ");
    klibcluu::trace(name);

    // Step 1: Create userspace process (allocates address space)
    let pid = ProcessManager::spawn_user(name, process_type)?;

    // Step 2: Load ELF binary into process address space
    let result = ProcessManager::with_process_mut(pid, |process| {
        // Load ELF segments into address space
        let binary = elf::load_elf(elf_data, &mut process.address_space)
            .map_err(|_| "ELF loading failed")?;

        Ok::<_, &'static str>(binary)
    });

    // Check if loading succeeded
    match result {
        Some(Ok(_binary)) => {
            klibcluu::trace("Successfully loaded ELF process: ");
            klibcluu::trace(name);
            Ok(pid)
        }
        Some(Err(e)) => {
            // Loading failed - clean up by reaping the process
            let _ = ProcessManager::reap(pid);
            Err(e)
        }
        None => {
            // Process not found (shouldn't happen)
            Err("Process disappeared during loading")
        }
    }
}

/// Spawn a kernel process
///
/// Simpler than ELF spawning - just creates the process with
/// kernel address space. The caller is responsible for creating
/// threads with appropriate entry points.
///
/// # Arguments
///
/// * `name` - Process name
/// * `process_type` - Process classification
///
/// # Returns
///
/// ProcessId of the new kernel process
pub fn spawn_kernel_process(name: &str, process_type: ProcessType) -> ProcessId {
    ProcessManager::spawn_kernel(name, process_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_kernel_process() {
        let pid = spawn_kernel_process("test_kernel", ProcessType::System);
        assert!(pid.as_usize() > 0);

        // Verify process exists
        let found = ProcessManager::with_process(pid, |p| p.name.clone());
        assert_eq!(found, Some("test_kernel".into()));
    }

    // Note: Can't easily test spawn_elf_process without a valid ELF binary
    // That would be an integration test with actual ELF files
}
