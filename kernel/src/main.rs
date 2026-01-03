//! CLUU Microkernel
//!
//! Minimal kernel entry point for testing the build system.
//! This will be replaced with proper initialization in Phase 2.

#![no_std]
#![no_main]

extern crate klibcluu;

use core::panic::PanicInfo;

/// Entry point called by BOOTBOOT Loader
#[no_mangle]
fn _start() -> ! {
    // TODO: Initialize kernel subsystems
    // This is a placeholder for Phase 2 implementation

    loop {
        // Halt CPU
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

/// Panic handler
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // TODO: Use klibcluu debug output when initialized
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}
