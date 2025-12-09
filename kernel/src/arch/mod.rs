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
    init_debug_infrastructure();

    // Initialize logger after debug infrastructure is ready
    crate::utils::logger::init(true);

    log::info!("CLUU Kernel Starting...");

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

    // Check if framebuffer is available and print test message
    if let Some(ref mut fb) = *peripheral::FB.lock() {
        fb.puts("Kernel initialized successfully!");
        fb.draw_screen_test();
    }

    log::info!("Kernel initialization complete - entering main loop");

    // NOTE: Interrupts remain disabled until we're ready to handle them
    // This ensures no asynchronous interrupts occur during initialization
    loop {
        hlt();
    }
}

/// Initialize debug infrastructure (COM2 port for logging)
fn init_debug_infrastructure() {
    // Initialize COM2 port for debug/logging output
    peripheral::init_debug_port();
}
