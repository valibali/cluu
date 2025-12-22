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
mod fs;
mod initrd;
mod io;
mod loaders;
mod memory;
mod scheduler;
mod syscall;
mod tests;
mod utils;
mod vfs;

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

    // Step 3.5: Initialize SYSCALL/SYSRET mechanism
    syscall::init();

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

    // Step 4.5: Initialize initrd (initial ramdisk)
    initrd::init();

    // Step 5: Initialize IDT (Interrupt Descriptor Table)
    arch::x86_64::idt::init();

    // Step 6: Initialize system drivers
    drivers::system::init();

    // Step 7: Initialize display driver
    drivers::display::init();

    // Step 8: Initialize input drivers
    drivers::input::init();

    // Initialize keyboard decoder
    drivers::input::keyboard::init_keyboard();

    // Step 9: Initialize console
    utils::io::console::init();

    // Step 10: Initialize scheduler
    scheduler::init();

    // Step 10.5: Initialize IPC system
    scheduler::ipc::init();

    // Step 10.55: Initialize shared memory subsystem
    scheduler::shmem::init();
    log::info!("Shared memory subsystem initialized");

    // Step 10.6: Initialize VFS subsystem
    vfs::init();
    log::info!("VFS subsystem initialized (waiting for VFS server)");

    // Step 10.7: Initialize SYSCALL/SYSRET infrastructure
    syscall::init();
    log::info!("SYSCALL/SYSRET infrastructure initialized");

    // Step 11: Enable interrupts
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // Step 12: Initialize TTY system
    components::tty::init_tty0();
    log::info!("TTY system initialized");

    // Step 13: Spawn VFS server (PID 2) - BEFORE enabling scheduler
    log::info!("Spawning VFS server...");

    // Get initrd info for mapping
    let (initrd_phys, initrd_size) = initrd::get_info();
    log::info!(
        "Initrd: phys=0x{:x}, size={} bytes ({} MB)",
        initrd_phys.as_u64(),
        initrd_size,
        initrd_size / 1024 / 1024
    );

    // Format arguments for VFS server
    use alloc::format;
    let initrd_addr_str = format!("0x{:x}", initrd_phys.as_u64());
    let initrd_size_str = format!("{}", initrd_size);

    log::info!(
        "VFS server args: addr={}, size={}",
        initrd_addr_str,
        initrd_size_str
    );

    match loaders::elf::spawn_elf_process(
        &vfs::vfs_read_file("sys/vfs_server").expect("VFS server binary not found"),
        "vfs_server",
        &[initrd_addr_str.as_str(), initrd_size_str.as_str()],
    ) {
        Ok((pid, tid)) => {
            log::info!("VFS server spawned: PID={:?}, TID={:?}", pid, tid);

            // Map initrd into VFS server address space
            log::info!("Mapping initrd into VFS server...");
            match scheduler::shmem::shmem_create_from_phys(
                initrd_phys,
                initrd_size,
                scheduler::ProcessId(0), // Kernel owns it
                scheduler::shmem::ShmemPermissions {
                    read: true,
                    write: false, // Read-only
                },
            ) {
                Ok(shmem_id) => {
                    log::info!("Created shmem region {} for initrd", shmem_id.0);

                    // Map into VFS server at fixed address 0x500000000
                    match scheduler::shmem::shmem_map(
                        shmem_id,
                        pid,         // VFS server PID
                        0x500000000, // Fixed address (must match VFS server's expectation)
                        scheduler::shmem::ShmemPermissions {
                            read: true,
                            write: false,
                        },
                    ) {
                        Ok(virt_addr) => {
                            log::info!("Initrd mapped into VFS server at 0x{:x}", virt_addr);
                        }
                        Err(e) => {
                            log::error!("Failed to map initrd into VFS server: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to create shmem for initrd: {:?}", e);
                }
            }

            log::info!("VFS server created successfully");
        }
        Err(e) => {
            log::warn!(
                "Failed to spawn VFS server: {:?} (continuing without VFS)",
                e
            );
        }
    }

    // Step 14: Create shell thread
    log::info!("Creating shell thread...");
    scheduler::spawn_thread(shell_thread_main, "kshell");

    // Step 15: Enable scheduler (spawns built-in idle thread)
    scheduler::enable();
    log::info!("Kernel initialization complete!");

    log::info!("Entering idle loop - scheduler is now in control");

    // Main kernel trap loop
    // The scheduler has taken over - threads will be switched by timer interrupts
    // This loop just halts the CPU to save power between interrupts
    loop {
        x86_64::instructions::hlt();
    }
}

/// Shell thread main function
fn shell_thread_main() {
    log::info!("Shell thread starting...");

    // Initialize shell
    utils::ui::kshell::KShell::init();
    log::info!("Shell initialized - ready for user input");

    // Main shell loop - handle keyboard input using blocking I/O
    // The thread blocks (0% CPU) until keyboard interrupt arrives
    loop {
        // Blocking read: thread sleeps until keystroke arrives
        let ch = drivers::input::keyboard::read_char_blocking();
        utils::ui::kshell::KShell::handle_char(ch);
    }
}

// Re-export test functions
pub use tests::*;

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
