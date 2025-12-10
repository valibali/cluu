/*
 * System Reboot Module
 *
 * This module provides system reboot functionality using various methods
 * available on x86_64 systems. It tries multiple approaches to ensure
 * a reliable system restart.
 */

use x86_64::instructions::port::Port;

/// Attempts to reboot the system using multiple methods
pub fn reboot() -> ! {
    log::info!("Initiating system reboot...");
    
    // Method 1: Try ACPI reset (most modern method)
    log::info!("Attempting ACPI reset...");
    acpi_reset();
    
    // Method 2: Try keyboard controller reset (classic method)
    log::info!("Attempting keyboard controller reset...");
    keyboard_reset();
    
    // Method 3: Try triple fault (last resort)
    log::info!("Attempting triple fault reset...");
    triple_fault();
}

/// ACPI reset method - writes to ACPI reset register
fn acpi_reset() {
    // ACPI reset register (0xCF9) - standard reset method
    let mut reset_port = Port::new(0xCF9);
    
    // Write reset command
    unsafe {
        reset_port.write(0x02u8); // Request system reset
        reset_port.write(0x06u8); // Actually perform the reset
    }
    
    // Wait a bit for reset to take effect
    for _ in 0..1000000 {
        unsafe { core::arch::asm!("nop") };
    }
}

/// Keyboard controller reset method - uses PS/2 controller
fn keyboard_reset() {
    // PS/2 controller command port
    let mut cmd_port = Port::new(0x64);
    
    // Send reset command to keyboard controller
    unsafe {
        cmd_port.write(0xFEu8);
    }
    
    // Wait for reset
    for _ in 0..1000000 {
        unsafe { core::arch::asm!("nop") };
    }
}

/// Triple fault method - causes CPU to reset by triggering a triple fault
fn triple_fault() -> ! {
    log::warn!("Triggering triple fault for system reset...");
    
    // Disable interrupts
    x86_64::instructions::interrupts::disable();
    
    // Load invalid IDT to cause triple fault
    unsafe {
        core::arch::asm!(
            "lidt [{}]",
            in(reg) &[0u16; 3] as *const u16,
            options(nostack, nomem)
        );
        
        // Trigger interrupt with invalid IDT
        core::arch::asm!("int 3", options(nostack, nomem));
    }
    
    // If we somehow get here, infinite loop
    loop {
        x86_64::instructions::hlt();
    }
}

/// Emergency halt - stops the system without reboot
pub fn halt() -> ! {
    log::info!("System halt requested");
    x86_64::instructions::interrupts::disable();
    
    loop {
        x86_64::instructions::hlt();
    }
}
