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
use crate::syscall::{SyscallArgs, SyscallNumber, dispatch_syscall};
use x86_64::registers::model_specific::{LStar, Star, SFMask};
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
    // Configure IA32_LSTAR - Syscall entry point address
    // ─────────────────────────────────────────────────────────────────────

    let entry_addr = VirtAddr::new(syscall_entry as *const () as u64);
    LStar::write(entry_addr);

    klibcluu::debug("  LSTAR (entry point): 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "", entry_addr.as_u64());

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
    // Star::write() configures segments for SYSCALL/SYSRET transitions:
    //   - SYSCALL (user→kernel): loads kernel CS/SS
    //   - SYSRET (kernel→user): loads user CS/SS

    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::PrivilegeLevel;

    Star::write(
        SegmentSelector::new(1, PrivilegeLevel::Ring0), // kernel CS = 0x08
        SegmentSelector::new(2, PrivilegeLevel::Ring0), // kernel SS = 0x10
        SegmentSelector::new(3, PrivilegeLevel::Ring3), // user CS = 0x1B
        SegmentSelector::new(4, PrivilegeLevel::Ring3), // user SS = 0x23
    ).expect("Failed to write IA32_STAR");

    klibcluu::debug("  STAR: kernel_cs=0x08, kernel_ss=0x10, user_cs=0x1B, user_ss=0x23");

    // ─────────────────────────────────────────────────────────────────────
    // Configure IA32_FMASK - RFLAGS mask
    // ─────────────────────────────────────────────────────────────────────

    // Bits set in FMASK are CLEARED from RFLAGS during syscall.
    // We want to clear the interrupt flag (IF) to disable interrupts
    // during syscall handling.

    let flags_mask = RFlags::INTERRUPT_FLAG;
    SFMask::write(flags_mask);

    klibcluu::debug("  FMASK (clear IF): 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "", flags_mask.bits());

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
