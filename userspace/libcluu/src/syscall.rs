//! Raw syscall interface
//!
//! This module provides both low-level and high-level syscall interfaces.

use crate::error::{Error, Result};

/// Syscall numbers matching kernel SyscallNumber enum
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNumber {
    Ipc = 0,
    Yield = 1,
    ThreadCreate = 2,
    ThreadDestroy = 3,
    SpaceCreate = 4,
    SpaceDestroy = 5,
    Grant = 6,
    Map = 7,
    Unmap = 8,
    TokenCreate = 9,
    TokenDelete = 10,
    IrqAttach = 11,
    IrqAck = 12,
    DebugPrint = 255,
}

/// Raw syscall invocation using x86_64 SYSCALL instruction
///
/// # Safety
///
/// This function is unsafe because it directly invokes kernel syscalls
/// with arbitrary arguments. The caller must ensure arguments are valid.
///
/// # Arguments
///
/// - `number`: Syscall number (RAX)
/// - `arg1-arg6`: Arguments (RDI, RSI, RDX, R10, R8, R9)
///
/// # Returns
///
/// - `Ok(value)`: Success with return value
/// - `Err(error)`: Error with errno
#[inline]
pub unsafe fn syscall_raw(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> Result<usize> {
    let ret: isize;

    core::arch::asm!(
        "syscall",
        inlateout("rax") number => ret,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rcx") _, // Clobbered by SYSCALL (saves RIP)
        lateout("r11") _, // Clobbered by SYSCALL (saves RFLAGS)
        options(nostack),
    );

    if ret < 0 {
        Err(Error::from_errno(ret))
    } else {
        Ok(ret as usize)
    }
}

/// Helper for syscalls with 0 arguments
#[inline]
unsafe fn syscall0(n: SyscallNumber) -> Result<usize> {
    syscall_raw(n as usize, 0, 0, 0, 0, 0, 0)
}

/// Helper for syscalls with 1 argument
#[inline]
unsafe fn syscall1(n: SyscallNumber, arg1: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, 0, 0, 0, 0, 0)
}

/// Helper for syscalls with 2 arguments
#[inline]
unsafe fn syscall2(n: SyscallNumber, arg1: usize, arg2: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, 0, 0, 0, 0)
}

/// Helper for syscalls with 3 arguments
#[inline]
#[allow(dead_code)]
unsafe fn syscall3(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, 0, 0, 0)
}

/// Helper for syscalls with 4 arguments
#[inline]
pub unsafe fn syscall4(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, 0, 0)
}

/// Helper for syscalls with 5 arguments
#[inline]
#[allow(dead_code)]
unsafe fn syscall5(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, arg5, 0)
}

/// Helper for syscalls with 6 arguments
#[inline]
#[allow(dead_code)]
unsafe fn syscall6(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize, arg6: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, arg5, arg6)
}

//
// Type-Safe Syscall Wrappers
//

/// Voluntarily yield CPU to other threads
///
/// This syscall always succeeds and causes the scheduler to
/// consider running another thread. If no other thread is ready,
/// the current thread continues executing immediately.
///
/// # Examples
///
/// ```no_run
/// use libcluu::yield_cpu;
///
/// // In a busy-wait loop
/// loop {
///     if check_condition() {
///         break;
///     }
///     yield_cpu().expect("yield failed");
/// }
/// ```
#[inline]
pub fn yield_cpu() -> Result<()> {
    unsafe { syscall0(SyscallNumber::Yield)? };
    Ok(())
}

/// Print debug message to kernel log
///
/// Prints a UTF-8 string to the kernel log. The message is prefixed
/// with "[USERSPACE]" in the kernel log output.
///
/// # Arguments
///
/// - `message`: UTF-8 string to print (max 4KB)
///
/// # Errors
///
/// - `InvalidAddress`: Null pointer or kernel address
/// - `InvalidParameter`: Message too long (>4KB) or not UTF-8
///
/// # Examples
///
/// ```no_run
/// use libcluu::debug_print;
///
/// debug_print("Hello from userspace!").expect("debug_print failed");
/// ```
#[inline]
pub fn debug_print(message: &str) -> Result<()> {
    let ptr = message.as_ptr() as usize;
    let len = message.len();
    unsafe { syscall2(SyscallNumber::DebugPrint, ptr, len)? };
    Ok(())
}

/// Capability token (48 bytes: 32-byte payload + 16-byte HMAC)
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct CapabilityToken {
    data: [u8; 48],
}

impl CapabilityToken {
    pub const fn new() -> Self {
        Self { data: [0u8; 48] }
    }

    pub fn as_bytes(&self) -> &[u8; 48] {
        &self.data
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8; 48] {
        &mut self.data
    }
}

impl Default for CapabilityToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Create HMAC-signed capability token
///
/// Converts a capability handle to a cryptographic token that can
/// be transferred to another process.
///
/// # Arguments
///
/// - `cap_handle`: Capability handle to convert
/// - `token`: Output buffer for 48-byte token
///
/// # Errors
///
/// - `NotImplemented`: Not yet implemented
/// - `InvalidAddress`: Invalid output buffer
pub fn token_create(cap_handle: u8, token: &mut CapabilityToken) -> Result<()> {
    unsafe {
        syscall2(
            SyscallNumber::TokenCreate,
            cap_handle as usize,
            token as *mut CapabilityToken as usize,
        )?
    };
    Ok(())
}

/// Validate and consume capability token
///
/// Validates a cryptographic token and converts it back to a
/// capability handle in the current process.
///
/// # Arguments
///
/// - `token`: 48-byte token to validate
///
/// # Returns
///
/// Capability handle on success
///
/// # Errors
///
/// - `NotImplemented`: Not yet implemented
/// - `InvalidAddress`: Invalid token buffer
/// - `PermissionDenied`: Token validation failed
pub fn token_delete(token: &CapabilityToken) -> Result<u8> {
    let handle = unsafe {
        syscall1(
            SyscallNumber::TokenDelete,
            token as *const CapabilityToken as usize,
        )?
    };
    Ok(handle as u8)
}

/// Attach thread to receive interrupt notifications
///
/// # Arguments
///
/// - `irq_cap`: IRQ capability handle
/// - `endpoint_cap`: Notification endpoint capability
///
/// # Errors
///
/// - `NotImplemented`: Not yet implemented
pub fn irq_attach(irq_cap: u8, endpoint_cap: u8) -> Result<()> {
    unsafe {
        syscall2(
            SyscallNumber::IrqAttach,
            irq_cap as usize,
            endpoint_cap as usize,
        )?
    };
    Ok(())
}

/// Acknowledge interrupt and re-enable IRQ line
///
/// # Arguments
///
/// - `irq_cap`: IRQ capability handle
///
/// # Errors
///
/// - `NotImplemented`: Not yet implemented
pub fn irq_ack(irq_cap: u8) -> Result<()> {
    unsafe { syscall1(SyscallNumber::IrqAck, irq_cap as usize)? };
    Ok(())
}

/// Exit the current thread (stub - loops forever)
///
/// Note: The kernel doesn't have a ThreadExit syscall yet,
/// so this just loops forever. In the future, this will call
/// sys_thread_destroy on the current thread handle.
pub fn thread_exit(_code: i32) -> ! {
    // TODO: Call sys_thread_destroy(current_thread_handle) when available
    let _ = debug_print("Thread exiting");
    loop {
        let _ = unsafe { syscall0(SyscallNumber::Yield) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(SyscallNumber::Yield as usize, 1);
        assert_eq!(SyscallNumber::DebugPrint as usize, 255);
        assert_eq!(SyscallNumber::IrqAttach as usize, 11);
        assert_eq!(SyscallNumber::IrqAck as usize, 12);
    }

    #[test]
    fn test_capability_token_size() {
        assert_eq!(core::mem::size_of::<CapabilityToken>(), 48);
        assert_eq!(core::mem::align_of::<CapabilityToken>(), 8);
    }
}
