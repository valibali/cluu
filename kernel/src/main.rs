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
#![allow(dead_code)]

use core::panic::PanicInfo;

mod arch;
mod bootboot;
mod syscall;
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
    arch::kstart()
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
