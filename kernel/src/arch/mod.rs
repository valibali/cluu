/*
 * Architecture Abstraction Layer
 *
 * This module provides an abstraction layer over different CPU architectures,
 * currently supporting x86_64. It contains the main kernel initialization
 * sequence and architecture-specific setup code.
 *
 * Why this is important:
 * - Provides a clean interface between generic kernel code and arch-specific code
 * - Enables potential future support for other architectures (ARM, RISC-V, etc.)
 * - Contains the critical kernel startup sequence
 * - Manages hardware initialization in the correct order
 * - Provides centralized architecture detection and setup
 *
 * The kstart() function is the main kernel entry point after the initial
 * assembly bootstrap, responsible for initializing all kernel subsystems
 * in the proper order.
 */

#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;

use ::x86_64::instructions::interrupts;
use ::x86_64::instructions::*;

#[cfg(target_arch = "x86_64")]
use self::x86_64::*;

/// Starts the kernel.
///
/// # Returns
///
/// This function does not return.
pub fn kstart() -> ! {
    // Initialize debug/logging infrastructure first (before any logging)
    x86_64::init_debug_infrastructure();

    // Initialize logger after debug infrastructure is ready
    crate::utils::logger::init(true);

    log::info!("CLUU Kernel Starting...");
    log::info!("Architecture: x86_64");

    // Initialize GDT first - critical for proper memory segmentation
    log::info!("Initializing GDT...");
    gdt::init();
    log::info!("GDT initialization complete");

    // Initialize IDT after GDT - handles interrupts and exceptions
    log::info!("Initializing IDT...");
    idt::init();
    log::info!("IDT initialization complete");

    // Initialize peripherals after core CPU structures (excluding debug ports)
    log::info!("Initializing peripherals...");
    peripheral::init_peripherals();
    log::info!("Peripherals initialization complete");

    // Initialize memory management subsystems
    x86_64::init_memory();

    // Check if framebuffer is available and print test message
    if let Some(ref mut fb) = *peripheral::FB.lock() {
        fb.puts("Kernel initialized successfully!");
        fb.draw_screen_test();
    }

    // // ===== TEST: trigger a breakpoint exception to verify IDT/GDT/TSS =====
    // log::info!("Triggering breakpoint exception (int3) test...");
    // interrupts::int3();
    // log::info!("Returned from breakpoint handler successfully.");

    // ===== NOW enable interrupts =====
    log::info!("Enabling interrupts...");
    interrupts::enable();

    log::info!("Kernel initialization complete - starting shell");

    // Initialize console and shell
    log::info!("Initializing console...");
    crate::utils::console::init();
    log::info!("Console initialized");
    
    log::info!("Creating shell...");
    let mut shell = crate::utils::shell::Shell::new();
    log::info!("Shell created, initializing...");
    shell.init();

    log::info!("Shell initialized - ready for user input");

    // Main shell loop
    loop {
        // Check for keyboard input
        if crate::arch::x86_64::peripheral::keyboard::has_char() {
            if let Some(ch) = crate::arch::x86_64::peripheral::keyboard::read_char() {
                shell.handle_char(ch);
            }
        }

        // Yield CPU when no input is available
        hlt();
    }
}
