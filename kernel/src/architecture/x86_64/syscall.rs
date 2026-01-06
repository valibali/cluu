//! System Call Support for x86_64
//!
//! This module provides the glue between the NASM syscall entry point
//! and the Rust syscall dispatcher.
//!
//! # Architecture
//!
//! The syscall flow is:
//! 1. Userspace executes SYSCALL instruction
//! 2. CPU jumps to syscall_entry (NASM assembly)
//! 3. Assembly saves context, switches to kernel stack
//! 4. Assembly calls syscall_dispatch() (Rust)
//! 5. Rust dispatcher validates and handles syscall
//! 6. Assembly restores context and returns with SYSRET
//!
//! # MSR Setup
//!
//! The SYSCALL/SYSRET mechanism requires three MSRs to be configured:
//! - **IA32_STAR**: Segment selectors for kernel/user mode
//! - **IA32_LSTAR**: Address of syscall entry point (syscall_entry)
//! - **IA32_FMASK**: RFLAGS mask (clear interrupt flag on syscall)

use crate::error::Error;
use crate::syscall::{dispatch_syscall, SyscallArgs, SyscallNumber};
use x86_64::registers::model_specific::{LStar, SFMask};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

// ═══════════════════════════════════════════════════════════════════════════
// External Assembly Symbol
// ═══════════════════════════════════════════════════════════════════════════

extern "C" {
    /// Syscall entry point (defined in syscall_entry.asm)
    fn syscall_entry();
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-CPU Data
// ═══════════════════════════════════════════════════════════════════════════

/// Per-CPU data structure for syscall handling
///
/// This structure is accessed via GS base register.
/// The layout must match what syscall_entry.asm expects:
///
/// ```text
/// Offset  Size  Description
/// ------  ----  -----------
/// 0x00    8     User RSP (saved during syscall)
/// 0x08    8     Kernel RSP (loaded during syscall)
/// 0x10    8     Current thread pointer (future)
/// 0x18    8     CPU ID (future)
/// ```
#[repr(C)]
#[derive(Debug)]
pub struct PerCpuData {
    /// Temporary storage for user RSP during syscall
    pub user_rsp: u64,

    /// Kernel stack pointer (loaded from TSS.RSP0)
    pub kernel_rsp: u64,

    /// Current thread pointer (for future use)
    pub current_thread: u64,

    /// CPU ID (for future use in SMP)
    pub cpu_id: u64,
}

impl PerCpuData {
    /// Create a new per-CPU data structure
    pub const fn new() -> Self {
        Self {
            user_rsp: 0,
            kernel_rsp: 0,
            current_thread: 0,
            cpu_id: 0,
        }
    }

    /// Set kernel stack pointer
    ///
    /// This should be called when switching threads to update
    /// the kernel stack used for syscalls.
    pub fn set_kernel_stack(&mut self, stack_top: u64) {
        self.kernel_rsp = stack_top;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Syscall Dispatcher (called from assembly)
// ═══════════════════════════════════════════════════════════════════════════

/// Syscall dispatcher called from assembly
///
/// This function is called from syscall_entry.asm with arguments
/// already in the correct registers per x86_64 System V ABI.
///
/// # Arguments
///
/// * `number` - Syscall number (from RAX)
/// * `arg1` - Argument 1 (from RDI, originally RSI in userspace)
/// * `arg2` - Argument 2 (from RSI, originally RDX in userspace)
/// * `arg3` - Argument 3 (from RDX, originally RDX in userspace)
/// * `arg4` - Argument 4 (from RCX, originally R10 in userspace)
/// * `arg5` - Argument 5 (from R8)
/// * `arg6` - Argument 6 (from R9)
///
/// # Returns
///
/// * Positive value: Success
/// * Negative value: Error (-errno)
#[no_mangle]
extern "C" fn syscall_dispatch(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    // Validate syscall number
    let syscall_num = match SyscallNumber::from_usize(number) {
        Some(n) => n,
        None => {
            klibcluu::warn("Invalid syscall number: ");
            klibcluu::log_dec(klibcluu::LogLevel::Warn, "", number as u64);
            return -(Error::InvalidArgument as isize);
        }
    };

    // Package arguments
    let args = SyscallArgs::new(arg1, arg2, arg3, arg4, arg5, arg6);

    // Dispatch to syscall handler
    match dispatch_syscall(syscall_num, args) {
        Ok(ret) => ret as isize,
        Err(e) => -(e as isize),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Initialization
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize syscall support
///
/// This function sets up the MSRs required for SYSCALL/SYSRET instructions.
///
/// # Safety
///
/// - Must be called only once during kernel initialization
/// - GDT must be initialized first (for segment selectors)
/// - Per-CPU data must be set up via set_per_cpu_area()
///
/// # Panics
///
/// Panics if SYSCALL/SYSRET is not supported by the CPU.
pub unsafe fn init() {
    klibcluu::info("Initializing syscall support...");

    // Check if SYSCALL/SYSRET is supported
    let cpuid = core::arch::x86_64::__cpuid(0x80000001);
    if (cpuid.edx & (1 << 11)) == 0 {
        panic!("SYSCALL/SYSRET not supported by CPU");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Enable SYSCALL/SYSRET via EFER.SCE
    // ─────────────────────────────────────────────────────────────────────

    use x86_64::registers::model_specific::Efer;
    use x86_64::registers::model_specific::EferFlags;

    unsafe {
        let mut efer = Efer::read();
        efer.insert(EferFlags::SYSTEM_CALL_EXTENSIONS);
        Efer::write(efer);
    }

    klibcluu::info("  EFER.SCE enabled (SYSCALL/SYSRET instructions active)");

    // ─────────────────────────────────────────────────────────────────────
    // Configure IA32_LSTAR - Syscall entry point address
    // ─────────────────────────────────────────────────────────────────────

    let entry_addr = VirtAddr::new(syscall_entry as *const () as u64);
    LStar::write(entry_addr);

    klibcluu::trace("  LSTAR (entry point): 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", entry_addr.as_u64());

    // ─────────────────────────────────────────────────────────────────────
    // Configure IA32_STAR - Segment selectors
    // ─────────────────────────────────────────────────────────────────────

    // STAR MSR configures segment selectors for SYSCALL/SYSRET instructions.
    //
    // Our GDT layout (see gdt.rs):
    //   0x00: Null
    //   0x08: Kernel code (index=1, RPL=0)
    //   0x10: Kernel data (index=2, RPL=0)
    //   0x18: User code   (index=3, RPL=0, but used as 0x1B with RPL=3)
    //   0x20: User data   (index=4, RPL=0, but used as 0x23 with RPL=3)
    //
    // STAR layout:
    // [63:48] = User CS base (0x20 for data/stack segment base)
    // [47:32] = Kernel CS (0x08 for code)
    //
    // SYSCALL loads:  CS = STAR[47:32], SS = STAR[47:32] + 8
    // SYSRET loads:   CS = STAR[63:48] + 16, SS = STAR[63:48] + 8

    let star = ((0x20u64) << 48) | ((0x08u64) << 32);

    // Write directly to STAR MSR (0xC0000081)
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC0000081u32,  // IA32_STAR
            in("eax") (star & 0xFFFF_FFFF) as u32,
            in("edx") (star >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }

    klibcluu::trace("  STAR: kernel_cs=0x08, kernel_ss=0x10, user_cs=0x2B, user_ss=0x23");

    // ─────────────────────────────────────────────────────────────────────
    // Configure IA32_FMASK - RFLAGS mask
    // ─────────────────────────────────────────────────────────────────────

    // Bits set in FMASK are CLEARED from RFLAGS during syscall.
    // We want to clear the interrupt flag (IF) to disable interrupts
    // during syscall handling.

    let flags_mask = RFlags::INTERRUPT_FLAG;
    SFMask::write(flags_mask);

    klibcluu::trace("  FMASK (clear IF): 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", flags_mask.bits());

    klibcluu::info("Syscall support initialized");
}

/// Set per-CPU area for syscall handling
///
/// This sets the GS base register to point to the per-CPU data structure.
///
/// # Safety
///
/// - Must be called for each CPU before syscalls are enabled
/// - The per_cpu_data must remain valid for the lifetime of the CPU
pub unsafe fn set_per_cpu_area(per_cpu_data: &PerCpuData) {
    use x86_64::registers::model_specific::{GsBase, KernelGsBase};

    let addr = per_cpu_data as *const _ as u64;

    // Set both GS bases to the same value
    // (SWAPGS will swap between GsBase and KernelGsBase)
    GsBase::write(VirtAddr::new(addr));
    KernelGsBase::write(VirtAddr::new(addr));

    klibcluu::trace("Per-CPU area set: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", addr);
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-Thread Kernel Stack Management (SMP-ready)
// ═══════════════════════════════════════════════════════════════════════════

/// Update the kernel stack for the currently running thread
///
/// This must be called before entering userspace (jump_to_thread)
/// and during context switches to ensure SYSCALL uses the correct stack.
///
/// # Architecture
/// - **Single CPU (current)**: Updates global PER_CPU_DATA.kernel_rsp
/// - **Multi CPU (future)**: Each CPU's GS points to its PerCpuData, updated via GS:[8]
///
/// # How It Works
/// - syscall_entry.asm does: `mov rsp, [gs:0x08]` to load kernel stack
/// - This function ensures [gs:0x08] points to current thread's kernel stack
///
/// # Safety
/// Must be called with interrupts disabled during thread switch
pub unsafe fn set_current_thread_kernel_stack(stack_top: u64) {
    // Write directly to PerCpuData.kernel_rsp via GS-relative addressing
    // This works for both single-CPU and SMP scenarios
    // Offset 0x08 is PerCpuData.kernel_rsp
    unsafe {
        core::arch::asm!(
            "mov qword ptr gs:[8], {0}",
            in(reg) stack_top,
            options(nostack, preserves_flags)
        );
    }
}

/// Get the current kernel stack from PerCpuData
///
/// Reads PerCpuData.kernel_rsp via GS-relative addressing.
/// Useful for debugging and verification.
///
/// # Returns
/// The kernel stack pointer that will be used by syscall_entry
///
/// # Safety
/// Must be called in kernel context where GS points to valid PerCpuData
pub unsafe fn get_current_kernel_stack() -> u64 {
    let kernel_rsp: u64;
    core::arch::asm!(
        "mov {0}, qword ptr gs:[8]",  // Read kernel_rsp at offset 0x08
        out(reg) kernel_rsp,
        options(nostack, preserves_flags)
    );
    kernel_rsp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_per_cpu_data_layout() {
        // Verify per-CPU data layout matches assembly expectations
        let data = PerCpuData::new();
        let base = &data as *const _ as usize;

        unsafe {
            let user_rsp_offset = &data.user_rsp as *const _ as usize - base;
            let kernel_rsp_offset = &data.kernel_rsp as *const _ as usize - base;

            assert_eq!(user_rsp_offset, 0x00);
            assert_eq!(kernel_rsp_offset, 0x08);
        }
    }
}
