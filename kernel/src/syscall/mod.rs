/*
 * System Call Infrastructure
 *
 * This module implements the SYSCALL/SYSRET fast system call mechanism
 * for x86_64, providing efficient transitions between Ring 3 (userspace)
 * and Ring 0 (kernel) for system call handling.
 *
 * SYSCALL/SYSRET Mechanism:
 * =========================
 *
 * Hardware support:
 * - SYSCALL instruction (Ring 3 → Ring 0): ~100 cycles
 * - SYSRET instruction (Ring 0 → Ring 3): ~100 cycles
 * - Much faster than INT 0x80 (~1000 cycles)
 *
 * CPU automatically saves/restores:
 * - RCX ← RIP (return address)
 * - R11 ← RFLAGS (CPU flags)
 * - CS, SS switched based on MSR configuration
 *
 * MSR Configuration:
 * ==================
 *
 * IA32_EFER (0xC0000080):
 * - Bit 0 (SCE): Enable SYSCALL/SYSRET
 *
 * IA32_STAR (0xC0000081):
 * - [63:48]: User CS:SS base selector (0x20 → 0x23 with RPL=3 for data, 0x2B for code)
 * - [47:32]: Kernel CS:SS base selector (0x08, 0x10)
 *
 * IA32_LSTAR (0xC0000082):
 * - Contains address of syscall entry point
 *
 * IA32_FMASK (0xC0000084):
 * - Flags to clear on SYSCALL (IF=0x200, TF=0x100, DF=0x400)
 *
 * System V ABI Calling Convention:
 * =================================
 *
 * Arguments:
 * - RAX: Syscall number
 * - RDI: Arg 1
 * - RSI: Arg 2
 * - RDX: Arg 3
 * - R10: Arg 4 (RCX used by SYSCALL, so use R10 instead)
 * - R8:  Arg 5
 * - R9:  Arg 6
 *
 * Return:
 * - RAX: Return value (negative = error code)
 *
 * Safety Considerations:
 * ======================
 *
 * - User stack pointer (RSP) saved immediately
 * - Kernel stack used during syscall handling
 * - All registers preserved across syscall
 * - Pointer validation before dereferencing user pointers
 * - Atomic operations for thread-safe data structures
 */

pub mod handlers;
pub mod numbers;

use core::arch::asm;
use handlers::*;
use numbers::*;

/// MSR register numbers
const IA32_EFER: u32 = 0xC0000080;
const IA32_STAR: u32 = 0xC0000081;
const IA32_LSTAR: u32 = 0xC0000082;
const IA32_FMASK: u32 = 0xC0000084;

/// SCE (System Call Extensions) bit in EFER
const EFER_SCE: u64 = 1 << 0;

/// Flags to clear on SYSCALL (IF | DF | TF)
const SYSCALL_FMASK: u64 = 0x700;

/// Initialize SYSCALL/SYSRET mechanism
///
/// This sets up the MSRs required for fast system calls:
/// 1. Enable SYSCALL in EFER
/// 2. Configure segment selectors in STAR
/// 3. Set syscall entry point in LSTAR
/// 4. Configure flags mask in FMASK
///
/// Must be called during kernel initialization, after GDT is set up.
pub fn init() {
    log::info!("Initializing SYSCALL/SYSRET mechanism...");

    unsafe {
        // Enable SYSCALL/SYSRET (set SCE bit in EFER)
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);

        // Configure STAR MSR for segment selectors
        // [63:48] = User CS base (0x20 >> 3 = 0x04, but we use 0x20/0x28 directly)
        // [47:32] = Kernel CS base (0x08)
        //
        // SYSCALL loads:
        // - CS = STAR[47:32] = 0x08 (kernel code)
        // - SS = STAR[47:32] + 8 = 0x10 (kernel data)
        //
        // SYSRET loads:
        // - CS = STAR[63:48] + 16 = 0x20 + 16 = 0x30, but with RPL=3 → 0x2B (user code)
        // - SS = STAR[63:48] + 8 = 0x20 + 8 = 0x28, but with RPL=3 → 0x23 (user data)
        //
        // Note: The +8/+16 offsets are added by CPU, we just provide base
        let star = ((0x20u64) << 48) | ((0x08u64) << 32);
        wrmsr(IA32_STAR, star);

        // Set syscall entry point
        let entry_point = syscall_entry as *const () as u64;
        wrmsr(IA32_LSTAR, entry_point);

        // Configure flags to clear on SYSCALL
        // Clear IF (interrupts), DF (direction flag), TF (trap flag)
        wrmsr(IA32_FMASK, SYSCALL_FMASK);
    }

    log::info!("SYSCALL/SYSRET initialized (entry point: {:p})", syscall_entry as *const ());
}

/// Read from MSR (Model-Specific Register)
///
/// # Safety
/// Must only be called with valid MSR numbers.
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

/// Write to MSR (Model-Specific Register)
///
/// # Safety
/// Must only be called with valid MSR numbers and appropriate values.
#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Syscall entry point (naked function)
///
/// This is the first kernel code executed when userspace calls SYSCALL.
/// It must:
/// 1. Save user state (all registers)
/// 2. Switch to kernel stack
/// 3. Call Rust syscall handler
/// 4. Restore user state
/// 5. Return via SYSRET
///
/// Register state on entry (set by SYSCALL instruction):
/// - RCX = user RIP (return address)
/// - R11 = user RFLAGS
/// - CS = kernel code segment
/// - SS = kernel data segment
/// - RSP = still user stack! (must switch immediately)
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() -> ! {
    core::arch::naked_asm!(
        // TODO: Implement proper kernel stack switching via TSS or per-thread kernel stacks
        // For now, this is a placeholder that will be completed when we implement
        // user threads with separate kernel stacks.
        //
        // Proper implementation needs:
        // 1. Get current thread's kernel stack from thread-local storage
        // 2. Save user RSP somewhere accessible
        // 3. Switch to kernel stack
        // 4. Call syscall_handler_rust
        // 5. Restore user RSP
        // 6. Restore registers
        // 7. SYSRETQ

        // For Phase 4, just halt - full implementation in Phase 7
        "cli",
        "hlt",
        "2:",
        "jmp 2b",
    );
}

/// Rust syscall dispatcher
///
/// Called from assembly entry point with all arguments in registers.
/// Dispatches to appropriate handler based on syscall number.
///
/// # Arguments (System V ABI):
/// - syscall_num: RAX (syscall number)
/// - arg1-arg6: RDI, RSI, RDX, R10, R8, R9
///
/// # Returns
/// - RAX: Return value (negative = error)
#[unsafe(no_mangle)]
extern "C" fn syscall_handler_rust(
    syscall_num: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    _arg4: usize,
    _arg5: usize,
    _arg6: usize,
) -> isize {
    match syscall_num {
        SYS_READ => sys_read(arg1 as i32, arg2 as *mut u8, arg3),
        SYS_WRITE => sys_write(arg1 as i32, arg2 as *const u8, arg3),
        SYS_CLOSE => sys_close(arg1 as i32),
        SYS_FSTAT => sys_fstat(arg1 as i32, arg2 as *mut u8),
        SYS_LSEEK => sys_lseek(arg1 as i32, arg2 as i64, arg3 as i32),
        SYS_BRK => sys_brk(arg1 as *mut u8),
        SYS_ISATTY => sys_isatty(arg1 as i32),
        SYS_EXIT => sys_exit(arg1 as i32),
        SYS_YIELD => sys_yield(),
        _ => -ENOSYS,
    }
}
