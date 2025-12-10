/*
 * CLUU Microkernel Main Entry Point
 *
 * This is the main entry point for the CLUU microkernel, a bare-metal Rust kernel
 * designed to work with the BOOTBOOT bootloader protocol. It handles the critical
 * early boot process and kernel initialization.
 *
 * Why this is important:
 * - Provides the entry point that BOOTBOOT calls when loading the kernel
 * - Handles multi-core boot process (BSP vs AP core management)
 * - Sets up proper kernel stack before entering Rust code
 * - Implements panic handling for kernel-level errors
 * - Coordinates the overall kernel initialization sequence
 *
 * Key features:
 * - Multi-core aware boot process
 * - Proper stack management for kernel execution
 * - Integration with BOOTBOOT protocol
 * - Safe transition from assembly to Rust code
 * - Comprehensive error handling and logging
 */

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![allow(dead_code)]

extern crate alloc;

use core::panic::PanicInfo;

mod arch;
mod bootboot;
mod components;
mod drivers;
mod io;
mod memory;
mod utils;

#[repr(C, align(16))]
pub struct AlignedBspStack([u8; 64 * 1024]);

#[unsafe(no_mangle)]
pub static mut BSP_STACK: AlignedBspStack = AlignedBspStack([0; 64 * 1024]);

/// ===============================
///  EARLY ENTRY POINT (_start)
/// ===============================
///
/// Called directly by BOOTBOOT loader on ALL CORES.
/// Required to:
///   - Identify BSP using bootboot.bspid
///   - Switch to our own 64 KiB kernel stack
///   - Call into Rust's `kstart`
///   - Park APs
///
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // CPUID leaf 1 → EBX[31:24] = APIC ID
        "mov eax, 1",
        "cpuid",
        "shr ebx, 24",                 // EBX now holds core ID

        // Load &bootboot into RAX
        "lea rax, [rip + bootboot]",

        // Read bspid (u16 @ offset 0x0C)
        "movzx ecx, word ptr [rax + 0x0C]",

        // Compare APIC ID vs bspid
        "cmp ebx, ecx",
        "jne 2f",                      // If not BSP → jump to AP section

        // =======================
        //       BSP PATH
        // =======================

        // Switch to our 64 KiB BSP stack
        "lea rax, [rip + BSP_STACK]",
        "add rax, {stack_size}",
        "mov rsp, rax",

        // Jump into real Rust kernel entry
        "jmp kstart",

        // =======================
        //       AP PATH
        // =======================
        "2:",
        "1:",
        "hlt",
        "jmp 1b",

        stack_size = const 64 * 1024,
    );
}

/// ===============================
///  RUST KERNEL ENTRY POINT
/// ===============================
///
/// Now running on our safe, large BSP stack.
/// APs never run this function.
///
#[unsafe(no_mangle)]
pub extern "C" fn kstart() -> ! {
    // Step 1: Initialize debug infrastructure first
    utils::debug::init_debug_infrastructure();

    // Step 2: Initialize logging system
    utils::debug::logger::init(true);
    log::info!("CLUU Kernel starting...");

    // Step 3: Initialize GDT (Global Descriptor Table)
    arch::x86_64::gdt::init();

    // Step 4: Initialize memory management
    log::info!("Initializing memory management...");
    memory::init(core::ptr::addr_of!(bootboot::bootboot));

    // Test heap allocation
    {
        use alloc::vec::Vec;
        let mut test_vec = Vec::new();
        test_vec.push(42);
        test_vec.push(1337);
        log::info!("Heap test successful: {:?}", test_vec);
    }

    log::info!("Memory management initialized successfully");

    // Step 5: Initialize IDT (Interrupt Descriptor Table)
    arch::x86_64::idt::init();

    // Step 6: Initialize system drivers
    drivers::system::init();

    // Step 7: Initialize display driver
    drivers::display::init();

    // Step 8: Initialize input drivers
    drivers::input::init();

    // Step 9: Initialize console
    utils::io::console::init();

    // Step 10: Enable interrupts
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // Step 11: Initialize TTY system
    components::tty::init_tty0();
    log::info!("TTY system initialized");

    // Step 12: Initialize and start shell
    let mut shell = utils::ui::kshell::KShell::new();
    shell.init();
    log::info!("Shell initialized");

    log::info!("Kernel initialization complete!");

    // Main interactive loop - handle keyboard input for shell
    loop {
        if let Some(ch) = drivers::input::keyboard::read_char() {
            shell.handle_char(ch);
        } else {
            x86_64::instructions::hlt();
        }
    }
}

/// ===============================
///  PANIC HANDLER
/// ===============================
///
/// Logging may fail early, but this is safe once the logger is up.
///
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();

    if let Some(location) = info.location() {
        log::error!(
            "PANIC at {}:{}: {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        log::error!("PANIC: {}", info.message());
    }

    loop {
        x86_64::instructions::hlt();
    }
}
