/*
 * Userspace Hello World Test
 *
 * This module embeds the userspace hello world binary and provides
 * a function to spawn it as a process.
 */

use crate::loaders::elf;
use crate::scheduler;

/// Embedded userspace hello world binary (built from userspace/hello/)
///
/// To rebuild: cd userspace/hello && make
/// Then update kernel build to include this binary
static HELLO_BINARY: &[u8] = include_bytes!("../../../userspace/hello/hello");

/// Spawn the userspace hello world process
///
/// This function:
/// 1. Creates a new process with fresh address space
/// 2. Loads the embedded ELF binary
/// 3. Initializes stdin/stdout/stderr
/// 4. Creates initial thread at entry point
/// 5. Returns the process ID
pub fn spawn_hello_world() -> Result<(), &'static str> {
    log::info!("========================================");
    log::info!("SPAWNING USERSPACE HELLO WORLD");
    log::info!("========================================");
    log::info!("Binary size: {} bytes", HELLO_BINARY.len());
    log::info!("");

    // Parse and load the ELF binary
    match elf::spawn_elf_process(HELLO_BINARY, "hello_world", &[]) {
        Ok((process_id, thread_id)) => {
            log::info!("✓ Userspace process spawned successfully!");
            log::info!("  Process ID: {:?}", process_id);
            log::info!("  Thread ID: {:?}", thread_id);
            log::info!("  Binary: userspace/hello/hello");
            log::info!("");
            log::info!("Expected output:");
            log::info!("  Hello from userspace!");
            log::info!("  Syscalls are working!");
            log::info!("  Exiting gracefully...");
            log::info!("");
            log::info!("========================================");
            Ok(())
        }
        Err(e) => {
            log::error!("✗ Failed to spawn userspace process: {:?}", e);
            Err("Failed to spawn userspace process")
        }
    }
}

/// Test entry point called from comprehensive test suite
///
/// Returns true if the userspace process spawned successfully, false otherwise.
pub fn test_userspace_hello() -> bool {
    let result = match spawn_hello_world() {
        Ok(()) => {
            log::info!("Userspace hello test initiated");
            true
        }
        Err(e) => {
            log::error!("Userspace hello test failed: {}", e);
            false
        }
    };

    // Give the userspace process time to run and output its messages
    for _ in 0..100 {
        scheduler::yield_now();
    }

    log::info!("Userspace hello test complete (check output above)");
    result
}
