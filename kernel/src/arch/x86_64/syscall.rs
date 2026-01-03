//! System Call Support for x86_64
//!
//! This module provides the glue between the assembly syscall entry point
//! and the Rust syscall dispatcher.
//!
//! # Architecture
//!
//! The syscall flow is:
//! 1. Userspace executes SYSCALL instruction
//! 2. CPU jumps to syscall_entry (assembly)
//! 3. Assembly saves context, switches to kernel stack
//! 4. Assembly calls syscall_handler_rust() (this module)
//! 5. We call dispatch_syscall() from syscall module
//! 6. Assembly restores context and returns with SYSRET
//!
//! # MSR Setup
//!
//! The SYSCALL/SYSRET mechanism requires three MSRs to be configured:
//! - IA32_STAR: Segment selectors for kernel/user mode
//! - IA32_LSTAR: Address of syscall entry point (syscall_entry)
//! - IA32_FMASK: RFLAGS mask (clear interrupt flag on syscall)

use crate::syscall::{SyscallArgs, SyscallNumber, SyscallResult};
use crate::error::Error;
use x86_64::registers::model_specific::{LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

/// External assembly symbols
extern "C" {
    /// Syscall entry point (defined in syscall.asm)
    fn syscall_entry();

    /// Syscall stub (for error cases)
    fn syscall_stub();
}

/// Per-CPU data structure for syscall handling
///
/// This structure is accessed via GS.base register.
/// The layout must match what syscall.asm expects:
/// - Offset 0: Saved user RSP
/// - Offset 8: Kernel RSP (from TSS.RSP0)
#[repr(C)]
pub struct PerCpuData {
    /// Temporary storage for user RSP during syscall
    pub user_rsp: u64,

    /// Kernel stack pointer (loaded from TSS.RSP0)
    pub kernel_rsp: u64,
}

/// Initialize syscall support
///
/// This function must be called after GDT initialization.
/// It sets up the MSRs required for SYSCALL/SYSRET.
///
/// # Safety
///
/// - Must be called only once during kernel initialization
/// - GDT must be initialized first (for segment selectors)
/// - Per-CPU data must be set up for each CPU
pub unsafe fn init() {
    klibcluu::info("Initializing syscall support...");

    // Configure IA32_STAR MSR
    // This MSR contains segment selectors for syscall/sysret:
    // - Bits 32-47: Kernel CS/SS (syscall loads CS from here)
    // - Bits 48-63: User CS/SS (sysret loads CS from here + 16, SS from here + 8)
    //
    // For syscall (user -> kernel):
    // - CS = STAR[47:32]
    // - SS = STAR[47:32] + 8
    //
    // For sysret (kernel -> user):
    // - CS = STAR[63:48] + 16
    // - SS = STAR[63:48] + 8
    //
    // Our GDT layout (from gdt.rs):
    // 0x00: Null descriptor
    // 0x08: Kernel code
    // 0x10: Kernel data
    // 0x18: TSS (lower)
    // 0x20: TSS (upper)
    // 0x28: User data (0x28 | 3 = 0x2B with RPL=3)
    // 0x30: User code (0x30 | 3 = 0x33 with RPL=3)
    //
    // We want:
    // - Kernel CS = 0x08, Kernel SS = 0x10
    // - User CS = 0x33, User SS = 0x2B
    //
    // So STAR[47:32] = 0x08, STAR[63:48] = 0x28
    // But wait, sysret adds 16 to get CS (0x28 + 16 = 0x38, not 0x33)
    // and adds 8 to get SS (0x28 + 8 = 0x30, not 0x2B).
    //
    // Actually, we need to use the base selector without RPL:
    // - STAR[63:48] = 0x28 (user data base)
    // - sysret will load CS from STAR[63:48] + 16 = 0x38, but we want 0x33
    //
    // Wait, let me recalculate based on the standard layout:
    // Standard GDT layout for syscall:
    // 0x00: Null
    // 0x08: Kernel code
    // 0x10: Kernel data
    // 0x18: User code (minus RPL) -> with RPL=3: 0x1B
    // 0x20: User data (minus RPL) -> with RPL=3: 0x23
    //
    // But our GDT has TSS in the middle... Let me use what we have:
    // From gdt.rs, the order is:
    // 1. kernel_code
    // 2. kernel_data
    // 3. tss
    // 4. user_data
    // 5. user_code
    //
    // So if null is 0x00:
    // 0x00: null
    // 0x08: kernel_code
    // 0x10: kernel_data
    // 0x18: tss_low
    // 0x20: tss_high
    // 0x28: user_data -> with RPL=3: 0x2B
    // 0x30: user_code -> with RPL=3: 0x33
    //
    // For sysret to work correctly:
    // - sysret loads CS = STAR[63:48] + 16
    // - sysret loads SS = STAR[63:48] + 8
    //
    // We want CS=0x33 and SS=0x2B, so:
    // - STAR[63:48] + 16 = 0x33 => STAR[63:48] = 0x23
    // - STAR[63:48] + 8 = 0x2B => STAR[63:48] = 0x23
    //
    // But our user_data is at 0x28, not 0x23!
    //
    // There's a mismatch here. The standard approach is to have:
    // kernel_code, kernel_data, user_code, user_data
    // But we have: kernel_code, kernel_data, tss, user_data, user_code
    //
    // Let me check the correct approach: Actually, for SYSCALL/SYSRET,
    // the user segments MUST be consecutive with user_code before user_data
    // (or at least +16 and +8 from base).
    //
    // Given our GDT layout is fixed (from gdt.rs), let me work with it:
    // If STAR[63:48] = 0x20 (pointing between tss_high and user_data):
    // - CS = 0x20 + 16 = 0x30 (user_code base)
    // - SS = 0x20 + 8 = 0x28 (user_data base)
    // Then with RPL=3: CS = 0x33, SS = 0x2B ✓
    //
    // Wait, that's not right either. Let me think differently:
    // The selector value includes the RPL bits, so:
    // - user_code selector = 0x30 (base) | 3 (RPL) = 0x33
    // - user_data selector = 0x28 (base) | 3 (RPL) = 0x2B
    //
    // SYSRET automatically sets RPL=3, so we just need the base:
    // - STAR[63:48] = 0x20 (user_data_base - 8)
    // - CS = 0x20 + 16 = 0x30 -> with RPL=3: 0x33 ✓
    // - SS = 0x20 + 8 = 0x28 -> with RPL=3: 0x2B ✓

    // Get actual segment selectors from GDT
    let kernel_cs = crate::arch::x86_64::gdt::kernel_code_selector();
    let user_data_sel = crate::arch::x86_64::gdt::user_data_selector();

    // For SYSRET, we need the user data selector value minus 8
    // SYSRET adds +16 for CS and +8 for SS to the base value
    let user_data_base = user_data_sel.0 & !3;  // Remove RPL (bits 0-1)
    let user_base_value = user_data_base - 8;

    // Log the values for debugging
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "  kernel_cs=", kernel_cs.0 as u64);
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "  user_data_sel=", user_data_sel.0 as u64);
    klibcluu::log_hex(klibcluu::LogLevel::Debug, "  user_base=", user_base_value as u64);

    // Write STAR MSR directly (bypasses x86_64 crate validation)
    // Format: [63:48] = user_base, [47:32] = kernel_cs
    let star_value = ((user_base_value as u64) << 48) | ((kernel_cs.0 as u64) << 32);
    unsafe {
        x86_64::registers::model_specific::Msr::new(0xC0000081).write(star_value);
    }

    // Configure IA32_LSTAR MSR (Long mode SYSCALL Target Address)
    // This is where the CPU jumps when SYSCALL is executed
    LStar::write(VirtAddr::new(syscall_entry as u64));

    // Configure IA32_FMASK MSR (SYSCALL Flag Mask)
    // These flags will be CLEARED from RFLAGS on syscall entry
    // We want to clear the interrupt flag (IF) for atomicity
    SFMask::write(RFlags::INTERRUPT_FLAG);

    klibcluu::info("Syscall support initialized");
    klibcluu::debug("  STAR: kernel_cs=0x08, user_base=0x20");
    klibcluu::debug("  LSTAR configured");
    klibcluu::debug("  FMASK: INTERRUPT_FLAG cleared on syscall entry");
}

/// Syscall handler called from assembly
///
/// This is the bridge between the assembly syscall entry and the Rust
/// syscall dispatcher. The assembly code saves all registers and calls
/// this function.
///
/// # Arguments
///
/// - `number`: Syscall number from RAX
/// - `args_ptr`: Pointer to SyscallArgs structure on the stack
///
/// # Returns
///
/// - Positive value or zero: Success, returned to userspace in RAX
/// - Negative value: Error code (errno), returned to userspace in RAX
///
/// # Safety
///
/// This function is called from assembly with interrupts disabled.
/// The args_ptr points to the kernel stack where arguments are saved.
#[no_mangle]
pub extern "C" fn syscall_handler_rust(number: usize, args_ptr: *const SyscallArgs) -> isize {
    // Safety: args_ptr points to valid SyscallArgs on kernel stack
    let args = unsafe { *args_ptr };

    // Convert syscall number to enum
    let syscall_num = match SyscallNumber::from_usize(number) {
        Some(num) => num,
        None => {
            // Invalid syscall number
            klibcluu::warn("Invalid syscall number:");
            klibcluu::log_dec(klibcluu::LogLevel::Warn, "  number=", number as u64);
            return Error::InvalidOperation.to_errno();
        }
    };

    // Log syscall for debugging (can be disabled in production)
    if klibcluu::logger::should_log(klibcluu::LogLevel::Trace) {
        klibcluu::trace("syscall:");
        klibcluu::log_hex(klibcluu::LogLevel::Trace, "  arg1=", args.arg1 as u64);
        klibcluu::log_hex(klibcluu::LogLevel::Trace, "  arg2=", args.arg2 as u64);
        klibcluu::log_hex(klibcluu::LogLevel::Trace, "  arg3=", args.arg3 as u64);
    }

    // Dispatch to handler
    let result = crate::syscall::dispatch_syscall(syscall_num, args);

    // Convert result to errno
    match result {
        Ok(value) => {
            // Success: return positive value
            value as isize
        }
        Err(error) => {
            // Error: return negative errno
            klibcluu::debug("syscall failed");
            error.to_errno()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_handler_invalid_number() {
        let args = SyscallArgs::empty();
        let result = syscall_handler_rust(9999, &args as *const _);

        // Should return negative errno for invalid syscall
        assert!(result < 0);
        assert_eq!(result, Error::InvalidOperation.to_errno());
    }

    #[test]
    fn test_syscall_handler_not_implemented() {
        let args = SyscallArgs::empty();

        // All syscalls currently return NotImplemented
        let result = syscall_handler_rust(SyscallNumber::Yield.as_usize(), &args as *const _);
        assert_eq!(result, Error::NotImplemented.to_errno());
    }

    #[test]
    fn test_per_cpu_data_layout() {
        use core::mem;

        // Verify layout matches what assembly expects
        assert_eq!(mem::size_of::<PerCpuData>(), 16);

        let data = PerCpuData {
            user_rsp: 0x1234,
            kernel_rsp: 0x5678,
        };

        let ptr = &data as *const _ as *const u64;
        unsafe {
            assert_eq!(*ptr.offset(0), 0x1234); // user_rsp at offset 0
            assert_eq!(*ptr.offset(1), 0x5678); // kernel_rsp at offset 8
        }
    }
}
