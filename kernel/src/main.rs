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
mod ipc;
mod loaders;
mod memory;
mod scheduler;
mod shmem;
mod syscall;
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

    // Step 3.5: Initialize IDT (Interrupt Descriptor Table)
    // CRITICAL: Must be initialized BEFORE memory management (CR3 switch)
    // If any exception/NMI occurs during CR3 switch and IDT isn't set up,
    // the CPU will triple fault
    arch::x86_64::idt::init();

    // Step 3.6: Initialize SYSCALL/SYSRET mechanism
    syscall::init();

    // Step 4: Initialize memory management
    log::info!("Initializing memory management...");
    unsafe {
        memory::init(core::ptr::addr_of!(bootboot::bootboot));
    }

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
    scheduler::SchedulerManager::init();

    // Step 10.5: Initialize IPC system
    ipc::port::init();

    // Step 10.55: Initialize shared memory subsystem
    shmem::init();
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

    // Step 13: Spawn VFS server (PID 1) - CRITICAL process
    let vfs_pid = match vfs::spawn_server() {
        Ok((pid, tid)) => {
            log::info!("VFS server spawned: PID={:?}, TID={:?}", pid, tid);
            pid
        }
        Err(e) => {
            log::error!("Failed to spawn VFS server: {}", e);
            log::error!("Cannot continue without VFS server!");
            loop {
                x86_64::instructions::hlt();
            }
        }
    };

    // Register VFS as critical process - scheduler won't enter normal mode until it signals ready
    scheduler::SchedulerManager::register_critical(vfs_pid);

    log::info!("Kernel initialization complete!");

    // Step 15: Spawn shell launcher thread before enabling scheduler
    // This thread will wait for Normal mode and then spawn the shell
    scheduler::ThreadManager::spawn(shell_launcher_thread, "shell_launcher");

    // Enable preemptive scheduler
    // VFS will start running in Boot mode and register
    // Once VFS signals ready, scheduler will transition to Normal mode
    // Then shell_launcher thread will spawn the shell
    scheduler::SchedulerManager::enable();
    log::info!("Preemptive scheduler enabled - transferring control to userspace");

    // Main kernel idle loop
    // Timer interrupts will preempt us and switch to ready threads
    // We become the "emergency idle" - only run if no other threads are ready
    loop {
        x86_64::instructions::hlt();
    }
}

/// Shell launcher thread - waits for Normal mode then spawns shell
///
/// This thread is created before the scheduler is enabled. It will be
/// scheduled along with other threads and will wait for the VFS server
/// to signal ready (transition to Normal mode) before spawning the shell.
fn shell_launcher_thread() {
    // Wait for scheduler to transition to Normal mode
    while !scheduler::SchedulerManager::is_normal_mode() {
        scheduler::SchedulerManager::yield_now();
    }

    // VFS is ready, spawn shell now
    spawn_shell();

    // Thread exits after spawning shell
}

/// Spawn the userspace shell
///
/// This is called by the shell_launcher thread once VFS is ready,
/// ensuring the VFS server is ready before the shell starts.
pub fn spawn_shell() {
    log::info!("Spawning userspace shell...");

    // Try multiple possible paths for the shell binary
    let paths = ["/dev/initrd/bin/shell", "bin/shell", "/bin/shell"];
    let mut shell_binary = alloc::vec::Vec::new();

    for path in &paths {
        log::info!("Trying to load shell from: {}", path);
        match vfs::vfs_read_file(path) {
            Ok(data) => {
                log::info!("Successfully loaded shell from {} ({} bytes)", path, data.len());
                shell_binary = data;
                break;
            }
            Err(e) => {
                log::warn!("Failed to load shell from {}: {}", path, e);
            }
        }
    }

    // If VFS read failed, try direct initrd access as fallback
    if shell_binary.is_empty() {
        log::warn!("VFS read failed, trying direct initrd access...");
        shell_binary = initrd::read_file("bin/shell")
            .map(|data| data.to_vec())
            .unwrap_or_else(|e| {
                log::error!("Failed to read shell from initrd: {}", e);
                alloc::vec::Vec::new()
            });
    }

    if shell_binary.is_empty() {
        log::error!("Could not find shell binary in any location!");
        return;
    }

    if !shell_binary.is_empty() {
        match loaders::elf::spawn_elf_process(
            &shell_binary,
            "shell",
            &[],
            scheduler::ProcessType::User,
        ) {
            Ok((pid, tid)) => {
                log::info!("Shell spawned: PID={:?}, TID={:?}", pid, tid);
            }
            Err(e) => {
                log::warn!("Failed to spawn shell: {:?}", e);
            }
        }
    } else {
        log::warn!("Shell binary is empty, not spawning");
    }
}

/// Shell thread main function (DEPRECATED - will be userspace process)
#[allow(dead_code)]
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
