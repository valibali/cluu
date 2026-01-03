//! Raw syscall interface

/// Syscall numbers
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNumber {
    Ipc = 0,
    Grant = 1,
    Map = 2,
    Unmap = 3,
    ThreadCreate = 4,
    ThreadExit = 5,
    Yield = 6,
    SpaceCreate = 7,
    SpaceDestroy = 8,
    TokenCreate = 9,
    TokenDelete = 10,
    IrqAttach = 11,
    IrqAck = 12,
}

/// Perform a raw syscall
///
/// # Safety
/// This is a low-level interface. Arguments must match the syscall's expected types.
#[inline]
pub unsafe fn syscall0(n: SyscallNumber) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall1(n: SyscallNumber, arg1: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall2(n: SyscallNumber, arg1: usize, arg2: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall3(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall4(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall5(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline]
pub unsafe fn syscall6(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize, arg6: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "int 0x80",
        in("rax") n as usize,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rax") ret,
        options(nostack, preserves_flags)
    );
    ret
}

/// Yield the CPU
pub fn sys_yield() {
    unsafe {
        syscall0(SyscallNumber::Yield);
    }
}

/// Exit the current thread
pub fn sys_thread_exit(code: i32) -> ! {
    unsafe {
        syscall1(SyscallNumber::ThreadExit, code as usize);
    }
    unreachable!()
}
