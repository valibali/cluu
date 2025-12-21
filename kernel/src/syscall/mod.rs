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

/// Per-CPU scratch area for syscall handling
///
/// This structure is accessed via the GS segment base during syscall entry.
/// It contains the kernel stack pointer for the current thread and space
/// to save the user stack pointer.
///
/// Layout (offsets for assembly):
/// - [0]: user_rsp (saved user stack pointer)
/// - [8]: kernel_rsp (current thread's kernel stack)
#[repr(C)]
struct SyscallScratch {
    user_rsp: u64,
    kernel_rsp: u64,
}

impl SyscallScratch {
    const fn new() -> Self {
        Self {
            user_rsp: 0,
            kernel_rsp: 0,
        }
    }
}

/// Global syscall scratch area (single CPU for now)
///
/// This is accessed directly by assembly code, so it must be:
/// - #[no_mangle] for stable symbol name
/// - static mut for direct memory access
/// - Properly synchronized (only modified during context switches)
///
/// Safety: This is safe for single-CPU because:
/// - Only modified by scheduler during context switches (interrupts disabled)
/// - Read by syscall entry point (atomic operation)
/// - No concurrent access possible on single CPU
///
/// TODO: Make this per-CPU when we add SMP support
#[unsafe(no_mangle)]
static mut SYSCALL_SCRATCH: SyscallScratch = SyscallScratch {
    user_rsp: 0,
    kernel_rsp: 0,
};

/// Set the kernel stack pointer for syscall handling
///
/// This must be called whenever switching to a new thread to update
/// the syscall entry point's kernel stack pointer.
///
/// # Arguments
/// * `kernel_stack_ptr` - Pointer to the top of the thread's kernel stack
///
/// # Safety
/// The kernel_stack_ptr must point to a valid, aligned kernel stack with
/// sufficient space for the syscall frame (~128 bytes minimum).
///
/// # Synchronization Requirements
/// - **MUST** be called with interrupts disabled (prevents races)
/// - Called by scheduler during context switches
/// - Single-CPU only (multi-CPU requires per-CPU data via IA32_KERNEL_GS_BASE)
///
/// # Panics
/// Panics if kernel_stack_ptr is 0 (uninitialized stack)
pub fn set_kernel_stack(kernel_stack_ptr: u64) {
    // Verify interrupts are disabled for safety
    if x86_64::instructions::interrupts::are_enabled() {
        panic!("set_kernel_stack() called with interrupts enabled - UNSAFE!");
    }

    // Verify stack pointer is valid
    if kernel_stack_ptr == 0 {
        panic!("set_kernel_stack() called with NULL pointer - stack not initialized!");
    }

    // Verify stack pointer is in kernel address space (high half)
    if kernel_stack_ptr < 0xffff_8000_0000_0000 {
        panic!("set_kernel_stack() called with userspace pointer 0x{:x} - must be kernel address!", kernel_stack_ptr);
    }

    unsafe {
        SYSCALL_SCRATCH.kernel_rsp = kernel_stack_ptr;
    }
}

/// Get the current kernel stack pointer
///
/// Returns the kernel stack pointer that will be used for the next syscall.
/// This is mainly for debugging purposes.
pub fn get_kernel_stack() -> u64 {
    unsafe { SYSCALL_SCRATCH.kernel_rsp }
}

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
///
/// Stack frame layout (after switching to kernel stack):
/// - [rsp+0]:   R15
/// - [rsp+8]:   R14
/// - [rsp+16]:  R13
/// - [rsp+24]:  R12
/// - [rsp+32]:  R11 (user RFLAGS)
/// - [rsp+40]:  R10
/// - [rsp+48]:  R9
/// - [rsp+56]:  R8
/// - [rsp+64]:  RBP
/// - [rsp+72]:  RDI
/// - [rsp+80]:  RSI
/// - [rsp+88]:  RDX
/// - [rsp+96]:  RCX (user RIP)
/// - [rsp+104]: RBX
/// - [rsp+112]: RAX
/// - [rsp+120]: user RSP (saved)
#[unsafe(naked)]
pub unsafe extern "C" fn syscall_entry() -> ! {
    core::arch::naked_asm!(
        // ========================================
        // PHASE 1: SAVE USER STATE AND SWITCH STACK
        // ========================================

        // Get address of SYSCALL_SCRATCH
        // Note: We use a simpler approach for single-CPU - just use absolute address
        "lea rbx, [rip + {scratch}]",

        // Save user RSP to scratch[0]
        "mov [rbx + 0], rsp",

        // Load kernel RSP from scratch[8]
        "mov rsp, [rbx + 8]",

        // Now we're on kernel stack - safe to use it

        // ========================================
        // PHASE 2: BUILD STACK FRAME
        // ========================================

        // Push user RSP (from scratch area)
        "mov rbx, [rbx + 0]",
        "push rbx",

        // Push all general purpose registers
        "push rax",    // Syscall number
        "push rbx",
        "push rcx",    // User RIP (saved by SYSCALL)
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",    // User RFLAGS (saved by SYSCALL)
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // ========================================
        // PHASE 3: CALL RUST HANDLER
        // ========================================

        // Set up arguments according to System V ABI:
        // RDI = syscall number (from RAX)
        // RSI = arg1 (from RDI)
        // RDX = arg2 (from RSI)
        // RCX = arg3 (from RDX)
        // R8  = arg4 (from R10, since RCX used by SYSCALL)
        // R9  = arg5 (from R8)
        // Stack = arg6 (from R9)
        //
        // CRITICAL: Move in REVERSE order to avoid overwriting registers before using them!
        // If we did "mov rdi, rax; mov rsi, rdi", we'd copy RAX to RSI instead of original RDI!

        "push r9",         // arg6 = R9 (save first before R9 gets overwritten)
        "mov r9, r8",      // arg5 = R8 (safe, R9 already saved to stack)
        "mov r8, r10",     // arg4 = R10 (safe, R8 already copied to R9)
        "mov rcx, rdx",    // arg3 = RDX (safe, no conflict)
        "mov rdx, rsi",    // arg2 = RSI (safe, RDX already copied)
        "mov rsi, rdi",    // arg1 = RDI (safe, RSI already copied)
        "mov rdi, rax",    // syscall_num = RAX (safe, RDI already copied)

        // Align stack to 16 bytes (required by System V ABI)
        "sub rsp, 8",

        // Call Rust handler
        "call {handler}",

        // Clean up stack alignment
        "add rsp, 16",

        // Return value is in RAX - save it
        "mov rbx, rax",

        // ========================================
        // PHASE 4: RESTORE USER STATE
        // ========================================

        // Restore all registers (except RAX - it has return value)
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",    // User RFLAGS
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",    // User RIP
        "add rsp, 8", // Skip saved RBX
        "add rsp, 8", // Skip saved RAX (we have return value in RBX)

        // Restore user RSP
        "pop rsp",

        // Move return value to RAX
        "mov rax, rbx",

        // ========================================
        // PHASE 5: RETURN TO USERSPACE
        // ========================================

        // SYSRET will:
        // - Set RIP = RCX (user RIP)
        // - Set RFLAGS = R11 (user RFLAGS)
        // - Set CS/SS to user segments (from STAR MSR)
        "sysretq",

        scratch = sym SYSCALL_SCRATCH,
        handler = sym syscall_handler_rust,
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
    log::debug!("Syscall {} called with args: {:#x}, {:#x}, {:#x}",
                syscall_num, arg1, arg2, arg3);

    let ret = match syscall_num {
        SYS_READ => sys_read(arg1 as i32, arg2 as *mut u8, arg3),
        SYS_WRITE => sys_write(arg1 as i32, arg2 as *const u8, arg3),
        SYS_CLOSE => sys_close(arg1 as i32),
        SYS_FSTAT => sys_fstat(arg1 as i32, arg2 as *mut u8),
        SYS_LSEEK => sys_lseek(arg1 as i32, arg2 as i64, arg3 as i32),
        SYS_BRK => sys_brk(arg1 as *mut u8),
        SYS_ISATTY => sys_isatty(arg1 as i32),
        SYS_EXIT => sys_exit(arg1 as i32),
        SYS_YIELD => sys_yield(),
        SYS_GETPID => sys_getpid(),
        SYS_GETPPID => sys_getppid(),
        SYS_SPAWN => sys_spawn(arg1 as *const u8, arg2 as *const *const u8),
        SYS_WAITPID => sys_waitpid(arg1 as i32, arg2 as *mut i32, arg3 as i32),
        SYS_PORT_CREATE => sys_port_create(),
        SYS_PORT_DESTROY => sys_port_destroy(arg1),
        SYS_PORT_SEND => sys_port_send(arg1, arg2 as *const u8, arg3),
        SYS_PORT_RECV => sys_port_recv(arg1, arg2 as *mut u8, arg3),
        SYS_PORT_TRY_RECV => sys_port_try_recv(arg1, arg2 as *mut u8, arg3),
        SYS_REGISTER_PORT_NAME => sys_register_port_name(arg1 as *const u8, arg2),
        SYS_LOOKUP_PORT_NAME => sys_lookup_port_name(arg1 as *const u8),
        _ => {
            log::warn!("Unknown syscall number: {}", syscall_num);
            -ENOSYS
        }
    };

    log::debug!("Syscall {} returning: {}", syscall_num, ret);
    ret
}
