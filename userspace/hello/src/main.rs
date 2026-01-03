//! Hello World - Simple userspace program for CLUU microkernel
//!
//! This program demonstrates basic syscall usage through libcluu.
//! It prints messages to the kernel log and yields the CPU.

#![no_std]
#![no_main]

use libcluu::{debug_print, yield_cpu, Result};

/// Main entry point
///
/// Called by libcluu's _start runtime after kernel loads the program.
#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(_) => {
            let _ = debug_print("[SUCCESS] Program completed successfully");
            0
        }
        Err(_e) => {
            // Can't format errors without alloc, so just return error code
            -1
        }
    }
}

/// Main program logic
fn run() -> Result<()> {
    // Print hello message
    debug_print("=========================================")?;
    debug_print("  Hello from CLUU Userspace!")?;
    debug_print("=========================================")?;
    debug_print("")?;

    // Demonstrate multiple syscalls
    debug_print("Testing syscalls:")?;
    debug_print("  [1/3] debug_print syscall... OK")?;

    // Yield CPU
    yield_cpu()?;
    debug_print("  [2/3] yield_cpu syscall... OK")?;

    // Another yield
    yield_cpu()?;
    debug_print("  [3/3] second yield_cpu... OK")?;

    debug_print("")?;
    debug_print("All syscalls working correctly!")?;
    debug_print("")?;

    // Demonstrate busy-wait pattern with yield
    debug_print("Running busy-wait loop (10 iterations):")?;
    for i in 0..10 {
        // In a real program, you'd check some condition here
        // For demo, we just yield and continue
        yield_cpu()?;

        // Can't format strings without alloc, so use fixed messages
        match i {
            0 => debug_print("  Iteration 0")?,
            1 => debug_print("  Iteration 1")?,
            2 => debug_print("  Iteration 2")?,
            3 => debug_print("  Iteration 3")?,
            4 => debug_print("  Iteration 4")?,
            5 => debug_print("  Iteration 5")?,
            6 => debug_print("  Iteration 6")?,
            7 => debug_print("  Iteration 7")?,
            8 => debug_print("  Iteration 8")?,
            9 => debug_print("  Iteration 9")?,
            _ => {}
        }
    }

    debug_print("")?;
    debug_print("Loop complete!")?;
    debug_print("")?;
    debug_print("=========================================")?;
    debug_print("  Goodbye from userspace!")?;
    debug_print("=========================================")?;

    Ok(())
}
