//! CLUU Microkernel
//!
//! Main kernel entry point and initialization sequence.
//!
//! Boot sequence:
//! 1. UART initialization (for logging output)
//! 2. GDT initialization (kernel/user segments, TSS)
//! 3. IDT initialization (exception handlers, interrupt handlers)
//! 4. Syscall mechanism setup (MSRs for SYSCALL/SYSRET)
//! 5. Logger initialization
//! 6. Enter idle loop (TODO: start scheduler and init process)

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate klibcluu;

// Import kernel modules from lib
mod arch;
mod error;
mod syscall;

// Use kernel lib modules
use cluu_kernel::{bootboot, mm};

use core::panic::PanicInfo;

/// 64KB aligned stack for BSP
#[repr(C, align(16))]
pub struct AlignedBspStack([u8; 64 * 1024]);

#[no_mangle]
pub static mut BSP_STACK: AlignedBspStack = AlignedBspStack([0; 64 * 1024]);

/// Entry point called by BOOTBOOT Loader
///
/// This naked assembly function:
/// 1. Sets up a proper 64KB kernel stack
/// 2. Jumps to the Rust kernel_main() function
///
/// For now, we don't handle multi-core (AP parking) - that's Phase 8.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> () {
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
    // Phase 1: Initialize hardware and core CPU structures

    // 1.1: UART - Serial port for logging (COM2 at 0x2F8)
    // Must be first so we can log everything else
    unsafe {
        klibcluu::uart::init();
    }

    // 1.2: Logger - IRQ-safe kernel logger
    klibcluu::logger::init();

    klibcluu::info("=== Kernel Boot Started ===");

    // 1.3: GDT - Global Descriptor Table
    // Sets up memory segmentation and privilege levels (Ring 0/3)
    arch::x86_64::gdt::init();

    // 1.4: PIC - Programmable Interrupt Controller
    // Disable/remap the legacy 8259 PIC to prevent spurious interrupts
    unsafe {
        arch::x86_64::pic::init(); // Remap to vectors 32-47 and mask all
    }

    // 1.5: IDT - Interrupt Descriptor Table
    // Handles CPU exceptions and hardware interrupts
    arch::x86_64::idt::init();

    // 1.6: Syscall mechanism
    // Configures SYSCALL/SYSRET instructions for fast system calls
    unsafe {
        arch::x86_64::syscall::init();
    }

    // Phase 2: Memory Management Setup
    // Create bootloader adapter (abstraction layer)
    let bootboot_ptr = &raw const bootboot::bootboot as *const bootboot::BOOTBOOT;
    let boot_info = unsafe { mm::boot::BootbootAdapter::new(bootboot_ptr) };

    // Initialize memory management (bootloader-agnostic)
    unsafe {
        mm::init(&boot_info);
    }

    // Initialize kernel heap (for Vec, BTreeMap, etc.)
    unsafe {
        mm::heap::init().expect("Failed to initialize heap");
    }

    // Phase 3: Log initialization status

    klibcluu::logger::info("CLUU Microkernel v0.1.0");
    klibcluu::logger::info("Phase 7b: IRQ-Safe Logging with SOLID Architecture");
    klibcluu::logger::info("========================================");
    klibcluu::logger::info("Kernel initialization complete:");
    klibcluu::logger::info("  [✓] UART (COM2 at 0x2F8)");
    klibcluu::logger::info("  [✓] IRQ-safe logger (zero-cost in release)");
    klibcluu::logger::info("  [✓] GDT (kernel/user segments)");
    klibcluu::logger::info("  [✓] IDT (exception/interrupt handlers)");
    klibcluu::logger::info("  [✓] Syscall mechanism (SYSCALL/SYSRET)");
    klibcluu::logger::info("  [✓] Memory Management (PMM, VMM, physmap)");
    klibcluu::logger::info("  [✓] Kernel heap (2 MiB, huge page)");

    // Phase 3: Report syscall status
    klibcluu::logger::info("========================================");
    klibcluu::logger::info("Syscall interface ready:");
    klibcluu::logger::info("  - 14 syscalls defined");
    klibcluu::logger::info("  - sys_yield: Implemented");
    klibcluu::logger::info("  - sys_debug_print: Implemented");
    klibcluu::logger::info("  - sys_token_create/delete: Validated");
    klibcluu::logger::info("  - 10 stubs: Return NotImplemented");
    klibcluu::logger::info("");
    klibcluu::logger::info("TODO Phase 8:");
    klibcluu::logger::info("  - Integrate scheduler");
    klibcluu::logger::info("  - ELF loader for userspace");
    klibcluu::logger::info("  - Test with userspace programs");

    klibcluu::logger::info("========================================");
    klibcluu::logger::info("Entering idle loop (scheduler not yet started)");
    klibcluu::logger::info("");

    // TODO Phase 8: Start scheduler and launch init process
    // For now, just idle
    idle_loop()
}

/// Idle loop - halt CPU waiting for interrupts
fn idle_loop() -> ! {
    loop {
        // Use HLT instruction to save power
        // CPU will wake on next interrupt
        x86_64::instructions::hlt();
    }
}

/// Panic handler
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Disable interrupts to prevent further issues
    x86_64::instructions::interrupts::disable();

    // Try to log the panic (using IRQ-safe logger)
    klibcluu::logger::error("========================================");
    klibcluu::logger::error("KERNEL PANIC!");
    klibcluu::logger::error("========================================");

    if let Some(location) = info.location() {
        klibcluu::logger::error("Location:");
        klibcluu::COM2.write_str("  File: ");
        klibcluu::COM2.write_str(location.file());
        klibcluu::logger::log_dec(klibcluu::LogLevel::Error, ":", location.line() as u64);
    }

    if let Some(msg) = info.message().as_str() {
        klibcluu::COM2.write_str("Message: ");
        klibcluu::COM2.write_str(msg);
        klibcluu::COM2.write_str("\n");
    }

    klibcluu::logger::error("========================================");
    klibcluu::logger::error("System halted.");

    // Halt forever
    loop {
        unsafe {
            core::arch::asm!("cli; hlt");
        }
    }
}
