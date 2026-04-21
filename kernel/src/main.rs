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

// Use kernel lib modules
use cluu_kernel::{architecture, bootboot, bootstrap, mm, telemetry, token};
use klibcluu::crypto;

#[cfg(all(target_os = "none", not(test)))]
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
///
/// # Safety
///
/// This is the kernel entry point. It must only be invoked by the bootloader
/// with the expected BOOTBOOT memory layout and a valid initial stack.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() {
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

        // SysV ABI: at function entry, (RSP + 8) % 16 == 0
        // Since we use jmp (not call), we must adjust RSP to simulate
        // the 8-byte return address that call would push.
        "sub rsp, 8",

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
    architecture::x86_64::gdt::init();

    // 1.4: PIC - Programmable Interrupt Controller
    // Disable/remap the legacy 8259 PIC to prevent spurious interrupts
    unsafe {
        architecture::x86_64::pic::init(); // Remap to vectors 32-47 and mask all
    }

    // 1.5: IDT - Interrupt Descriptor Table
    // Handles CPU exceptions and hardware interrupts
    architecture::x86_64::idt::init();

    // 1.5.1: Enable SMAP/SMEP (Supervisor Mode Access/Execution Prevention)
    unsafe {
        let cpuid7 = core::arch::x86_64::__cpuid_count(7, 0);
        let has_smep = (cpuid7.ebx & (1 << 7)) != 0;
        let has_smap = (cpuid7.ebx & (1 << 20)) != 0;

        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);

        if has_smep {
            cr4 |= 1 << 20;
            klibcluu::info("  SMEP enabled (CR4 bit 20)");
        }
        if has_smap {
            cr4 |= 1 << 21;
            klibcluu::info("  SMAP enabled (CR4 bit 21)");
        }

        core::arch::asm!("mov cr4, {}", in(reg) cr4);

        if has_smap {
            core::arch::asm!("clac", options(nomem, nostack));
        }

        // 1.5.2: Spectre V2 mitigations (IBPB/STIBP)
        architecture::x86_64::spectre::init(cpuid7.edx);
    }

    // 1.5.3: SysV ABI preservation sanity check.
    // The T1.5 syscall fast path trusts that extern "C" callees preserve
    // RBX/RBP/R12-R14. Verify that contract before syscalls go live.
    architecture::x86_64::abi_check::verify_sysv_abi_preservation();

    // 1.6: Syscall mechanism
    // Configures SYSCALL/SYSRET instructions for fast system calls
    unsafe {
        architecture::x86_64::syscall::init();

        // Set up per-CPU data for syscall handling
        // For now, use BSP stack as kernel stack
        // Made pub so thread_manager can access via set_current_thread_kernel_stack()
        pub static mut PER_CPU_DATA: architecture::x86_64::syscall::PerCpuData =
            architecture::x86_64::syscall::PerCpuData::new();

        let per_cpu_ptr = core::ptr::addr_of_mut!(PER_CPU_DATA);
        (*per_cpu_ptr).set_kernel_stack((&raw const BSP_STACK as *const u8 as u64) + (64 * 1024));
        architecture::x86_64::syscall::set_per_cpu_area(&*per_cpu_ptr);
    }

    // Phase 2: Extract initrd info before MM switches page tables
    let bootboot_ptr = &raw const bootboot::bootboot;
    let (initrd_phys, initrd_size) = unsafe {
        let bb = &*bootboot_ptr;
        (bb.initrd_ptr, bb.initrd_size)
    };
    cluu_kernel::ipc::endpoint::set_rendezvous_direct_enabled(true);
    klibcluu::info("ipc direct rendezvous=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", 1);
    cluu_kernel::ipc::endpoint::set_register_fast_enabled(true);
    klibcluu::info("ipc register fast path=");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "", 1);

    // Phase 3: Memory Management Setup
    // Create bootloader adapter (abstraction layer)
    let boot_info = unsafe { mm::boot::BootbootAdapter::new(bootboot_ptr) };

    // Initialize memory management (bootloader-agnostic)
    // This switches to our own page tables and discards bootloader mappings
    unsafe {
        mm::init(&boot_info);
    }

    // Initialize kernel heap (for Vec, BTreeMap, etc.)
    unsafe {
        mm::heap::init().expect("Failed to initialize heap");
    }

    // Initialize crypto/token systems (needed before creating tokens)
    crypto::init();
    token::init();

    // Calibrate TSC before bringing up timer infrastructure.
    let tsc_hz = architecture::x86_64::tsc::calibrate();
    klibcluu::set_tsc_hz(tsc_hz);
    klibcluu::info("TSC calibrated");
    klibcluu::log_dec(klibcluu::LogLevel::Info, "tsc_hz=", tsc_hz);

    // Phase 4: Enable APIC timer (250Hz) before launching userspace
    if let Err(err) = architecture::x86_64::apic::init_timer(250) {
        klibcluu::warn("APIC timer init failed: ");
        klibcluu::warn(err);
    }

    // Phase 5: Bootstrap init thread
    // Pass initrd info directly (BOOTBOOT structure is no longer accessible)
    let _init_thread_id = unsafe {
        match bootstrap::init(initrd_phys, initrd_size) {
            Ok(thread_id) => thread_id,
            Err(e) => {
                klibcluu::error("Bootstrap failed: ");
                klibcluu::log_dec(klibcluu::LogLevel::Error, "", e as u64);
                panic!("Failed to bootstrap init thread");
            }
        }
    };
    telemetry::log_bootstrap_snapshot("post-bootstrap");

    // Phase 6: Log initialization status

    klibcluu::logger::info("CLUU Microkernel v0.1.0");

    // Phase 3: Report syscall status
    klibcluu::logger::info("========================================");

    klibcluu::logger::info("========================================");
    klibcluu::logger::info("Starting scheduler and launching init thread");
    klibcluu::logger::info("");

    // Phase 8: Start scheduler and jump to init thread
    unsafe {
        cluu_kernel::sched::ThreadManager::start();
    }

    #[allow(unreachable_code)] //temporary until pre-emption is working
    idle_loop();
}

fn idle_loop() -> ! {
    loop {
        // Use HLT instruction to save power
        // CPU will wake on next interrupt
        x86_64::instructions::hlt();
    }
}

/// Panic handler
#[cfg(all(target_os = "none", not(test)))]
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

    dump_stacktrace();

    klibcluu::logger::error("========================================");
    klibcluu::logger::error("System halted.");

    // Halt forever
    loop {
        unsafe {
            core::arch::asm!("cli; hlt");
        }
    }
}

#[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
fn dump_stacktrace() {
    let rsp = read_rsp();
    let mut rbp = read_rbp();
    let max = rsp.saturating_add(256 * 1024);

    klibcluu::logger::error("Stack trace:");
    klibcluu::log_hex(klibcluu::LogLevel::Error, "  rsp=0x", rsp);
    klibcluu::log_hex(klibcluu::LogLevel::Error, "  rbp=0x", rbp);

    for i in 0..32 {
        if rbp < rsp || rbp + 16 > max || (rbp & 0x7) != 0 {
            break;
        }

        let ret = unsafe { *((rbp + 8) as *const u64) };
        if !is_canonical(ret) {
            break;
        }

        klibcluu::COM2.write_str("  #");
        klibcluu::log_dec(klibcluu::LogLevel::Error, "", i as u64);
        klibcluu::log_hex(klibcluu::LogLevel::Error, "  rip=0x", ret);

        let next = unsafe { *(rbp as *const u64) };
        if next <= rbp || !is_canonical(next) {
            break;
        }
        rbp = next;
    }
}

#[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
fn is_canonical(addr: u64) -> bool {
    let top = addr >> 48;
    top == 0 || top == 0xffff
}

#[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
fn read_rbp() -> u64 {
    let rbp: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rbp",
            out(reg) rbp,
            options(nomem, nostack, preserves_flags)
        );
    }
    rbp
}

#[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
fn read_rsp() -> u64 {
    let rsp: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) rsp,
            options(nomem, nostack, preserves_flags)
        );
    }
    rsp
}
