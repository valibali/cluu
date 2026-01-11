//! Raw syscall interface for CLUU microkernel
//!
//! This module provides the minimal syscall interface (7 syscalls total):
//! - IPC: Send, Recv, Call, Reply
//! - Scheduling: Yield
//! - Operations: Invoke (token-based)
//! - Debug: DebugPrint

use crate::error::{Error, Result};

/// Syscall numbers matching kernel SyscallNumber enum
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallNumber {
    /// Send IPC message to endpoint
    Send = 0,

    /// Receive IPC message from endpoint
    Recv = 1,

    /// Call (send + receive) - synchronous RPC
    Call = 2,

    /// Reply to IPC sender
    Reply = 3,

    /// Yield CPU to scheduler
    Yield = 4,

    /// Invoke operation on a token
    Invoke = 5,

    /// Debug print (only in debug builds)
    DebugPrint = 255,
}

/// Invoke operations for sys_invoke()
///
/// These match the kernel's InvokeOp enum and determine what
/// operation is performed on a token.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeOp {
    // Thread operations
    ThreadCreate = 0,
    ThreadDestroy = 1,
    ThreadSuspend = 2,
    ThreadResume = 3,
    ThreadSetPriority = 4,

    // Space operations
    SpaceCreate = 10,
    SpaceDestroy = 11,
    SpaceMap = 12,
    SpaceUnmap = 13,
    SpaceGrant = 14,

    // Token operations
    TokenDerive = 20,
    TokenRevoke = 21,

    // IRQ operations
    IrqAttach = 30,
    IrqAck = 31,

    // IPC operations
    EndpointCreate = 40,
}

/// Page mapping flags for space_map.
pub const MAP_DEVICE: usize = 0x100;

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
unsafe fn syscall3(n: SyscallNumber, arg1: usize, arg2: usize, arg3: usize) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, 0, 0, 0)
}

/// Helper for syscalls with 4 arguments
#[inline]
unsafe fn syscall4(
    n: SyscallNumber,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, 0, 0)
}

/// Helper for syscalls with 5 arguments
#[inline]
unsafe fn syscall5(
    n: SyscallNumber,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, arg5, 0)
}

/// Helper for syscalls with 6 arguments
#[inline]
unsafe fn syscall6(
    n: SyscallNumber,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> Result<usize> {
    syscall_raw(n as usize, arg1, arg2, arg3, arg4, arg5, arg6)
}

//
// ═══════════════════════════════════════════════════════════════════════════
// IPC Syscalls
// ═══════════════════════════════════════════════════════════════════════════
//

/// Send IPC message to endpoint
///
/// # Arguments
///
/// - `endpoint_token`: Token handle for the endpoint
/// - `msg`: Message buffer to send
///
/// # Returns
///
/// - `Ok(())`: Message sent successfully
/// - `Err(error)`: Send failed (invalid token, endpoint full, etc.)
#[inline]
pub fn ipc_send(endpoint_token: usize, msg: &[u8]) -> Result<()> {
    unsafe {
        syscall3(
            SyscallNumber::Send,
            endpoint_token,
            msg.as_ptr() as usize,
            msg.len(),
        )?
    };
    Ok(())
}

/// Receive IPC message from endpoint
///
/// # Arguments
///
/// - `endpoint_token`: Token handle for the endpoint
/// - `buf`: Buffer to receive message into
///
/// # Returns
///
/// - `Ok(bytes_received)`: Number of bytes received
/// - `Err(error)`: Receive failed (invalid token, buffer too small, etc.)

const IPC_RECV_NONBLOCK_FLAG: usize = 1usize << (usize::BITS - 1);

#[inline]
pub fn ipc_recv(endpoint_token: usize, buf: &mut [u8]) -> Result<usize> {
    unsafe {
        syscall3(
            SyscallNumber::Recv,
            endpoint_token,
            buf.as_mut_ptr() as usize,
            buf.len(),
        )
    }
}

pub fn ipc_recv_nonblocking(endpoint_token: usize, buf: &mut [u8]) -> Result<usize> {
    let len = buf.len();
    let flagged_len = len | IPC_RECV_NONBLOCK_FLAG;
    unsafe {
        syscall3(
            SyscallNumber::Recv,
            endpoint_token,
            buf.as_mut_ptr() as usize,
            flagged_len,
        )
    }
}

/// Receive IPC message from endpoint with timeout
///
/// Blocks until a message arrives or the timeout expires.
///
/// # Arguments
///
/// - `endpoint_token`: Token handle for the endpoint
/// - `buf`: Buffer to receive message into
/// - `timeout_ms`: Timeout in milliseconds (0 = block forever)
///
/// # Returns
///
/// - `Ok(bytes_received)`: Number of bytes received
/// - `Err(Error::Timeout)`: Timeout expired before message arrived
/// - `Err(error)`: Other errors (invalid token, buffer too small, etc.)
#[inline]
pub fn ipc_recv_timeout(endpoint_token: usize, buf: &mut [u8], timeout_ms: usize) -> Result<usize> {
    unsafe {
        syscall4(
            SyscallNumber::Recv,
            endpoint_token,
            buf.as_mut_ptr() as usize,
            buf.len(),
            timeout_ms,
        )
    }
}

/// Call (send + receive) for synchronous RPC
///
/// Sends a message and blocks waiting for a reply.
///
/// # Arguments
///
/// - `endpoint_token`: Token handle for the endpoint
/// - `msg`: Message to send
/// - `reply_buf`: Buffer to receive reply into
///
/// # Returns
///
/// - `Ok(bytes_received)`: Number of bytes in reply
/// - `Err(error)`: Call failed
#[inline]
pub fn ipc_call(endpoint_token: usize, msg: &[u8], reply_buf: &mut [u8]) -> Result<usize> {
    unsafe {
        syscall5(
            SyscallNumber::Call,
            endpoint_token,
            msg.as_ptr() as usize,
            msg.len(),
            reply_buf.as_mut_ptr() as usize,
            reply_buf.len(),
        )
    }
}

/// Reply to IPC sender
///
/// Sends a reply to a thread that called us via ipc_call().
///
/// # Arguments
///
/// - `msg`: Reply message to send
///
/// # Returns
///
/// - `Ok(())`: Reply sent successfully
/// - `Err(error)`: No pending call or send failed
#[inline]
pub fn ipc_reply(msg: &[u8]) -> Result<()> {
    unsafe { syscall2(SyscallNumber::Reply, msg.as_ptr() as usize, msg.len())? };
    Ok(())
}

//
// ═══════════════════════════════════════════════════════════════════════════
// Scheduling Syscall
// ═══════════════════════════════════════════════════════════════════════════
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
/// use libcluu::syscall::yield_cpu;
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

//
// ═══════════════════════════════════════════════════════════════════════════
// Generic Operations (via Invoke)
// ═══════════════════════════════════════════════════════════════════════════
//

/// Invoke generic operation on a token
///
/// This is the workhorse syscall for all object operations. What happens
/// depends on the token's scope and rights, plus the operation requested.
///
/// # Arguments
///
/// - `token_handle`: Token to operate on
/// - `operation`: Operation to perform (see InvokeOp)
/// - `arg1-arg4`: Operation-specific arguments
///
/// # Returns
///
/// - `Ok(value)`: Operation-specific return value (often a new token handle)
/// - `Err(error)`: Invalid token, insufficient rights, or operation failed
#[inline]
pub unsafe fn invoke(
    token_handle: usize,
    operation: InvokeOp,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
) -> Result<usize> {
    syscall6(
        SyscallNumber::Invoke,
        token_handle,
        operation as usize,
        arg1,
        arg2,
        arg3,
        arg4,
    )
}

//
// ═══════════════════════════════════════════════════════════════════════════
// High-Level Wrappers (using Invoke)
// ═══════════════════════════════════════════════════════════════════════════
//

/// Create a new thread
///
/// # Arguments
///
/// - `space_token`: Address space token with THREAD_CONTROL right
/// - `entry`: Thread entry point address
/// - `stack`: Stack top address
/// - `priority`: Thread priority
///
/// # Returns
///
/// - `Ok(thread_token)`: New thread token handle
/// - `Err(error)`: Creation failed
pub fn thread_create(
    space_token: usize,
    entry: usize,
    stack: usize,
    priority: usize,
) -> Result<usize> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::ThreadCreate,
            entry,
            stack,
            priority,
            0,
        )
    }
}

/// Create a new address space
///
/// # Arguments
///
/// - `root_token`: Token with CREATE or SPACE_MAP right
///
/// # Returns
///
/// - `Ok(space_token)`: New address space token handle
/// - `Err(error)`: Creation failed
pub fn space_create(root_token: usize) -> Result<usize> {
    unsafe { invoke(root_token, InvokeOp::SpaceCreate, 0, 0, 0, 0) }
}

/// Map page in address space
///
/// # Arguments
///
/// - `space_token`: Address space token with SPACE_MAP right
/// - `virt_addr`: Virtual address to map
/// - `source_ptr`: Pointer to the source bytes (0 to map a zero page)
/// - `flags`: Mapping flags (read/write/execute)
/// - `data_len`: Amount of data to copy (<= 4096 bytes)
///
/// # Returns
///
/// - `Ok(())`: Page mapped successfully
/// - `Err(error)`: Mapping failed
pub fn space_map(
    space_token: usize,
    virt_addr: usize,
    source_ptr: usize,
    flags: usize,
    data_len: usize,
) -> Result<()> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::SpaceMap,
            virt_addr,
            source_ptr,
            flags,
            data_len,
        )?
    };
    Ok(())
}

/// Derive a new token with reduced rights
///
/// # Arguments
///
/// - `token_handle`: Token to derive from (must have GRANT right)
/// - `new_rights`: Rights bitmask for new token (subset of original)
/// - `expire_at`: Expiration timestamp for new token
///
/// # Returns
///
/// - `Ok(new_token)`: Derived token handle
/// - `Err(error)`: Derivation failed
pub fn token_derive(token_handle: usize, new_rights: usize, expire_at: u64) -> Result<usize> {
    unsafe {
        invoke(
            token_handle,
            InvokeOp::TokenDerive,
            new_rights,
            expire_at as usize,
            0,
            0,
        )
    }
}

/// Destroy a thread
///
/// # Arguments
///
/// - `thread_token`: Token handle for the thread
pub fn thread_destroy(thread_token: usize) -> Result<()> {
    unsafe {
        invoke(thread_token, InvokeOp::ThreadDestroy, 0, 0, 0, 0)?;
    }
    Ok(())
}

/// Create a new IPC endpoint.
#[inline]
pub fn endpoint_create(root_token: usize) -> Result<usize> {
    unsafe { invoke(root_token, InvokeOp::EndpointCreate, 0, 0, 0, 0) }
}

/// Attach IRQ handler
///
/// # Arguments
///
/// - `irq_token`: IRQ token
/// - `endpoint_token`: Endpoint token to receive notifications
///
/// # Returns
///
/// - `Ok(())`: IRQ attached successfully
/// - `Err(error)`: Attach failed
pub fn irq_attach(irq_token: usize, endpoint_token: usize, irq_number: usize) -> Result<()> {
    unsafe {
        invoke(
            irq_token,
            InvokeOp::IrqAttach,
            endpoint_token,
            irq_number,
            0,
            0,
        )?
    };
    Ok(())
}

/// Acknowledge IRQ and re-enable
///
/// # Arguments
///
/// - `irq_token`: IRQ token
///
/// # Returns
///
/// - `Ok(())`: IRQ acknowledged
/// - `Err(error)`: Ack failed
pub fn irq_ack(irq_token: usize) -> Result<()> {
    unsafe { invoke(irq_token, InvokeOp::IrqAck, 0, 0, 0, 0)? };
    Ok(())
}

//
// ═══════════════════════════════════════════════════════════════════════════
// Debug Syscall
// ═══════════════════════════════════════════════════════════════════════════
//

/// Print debug message to kernel log
///
/// Prints a UTF-8 string to the kernel log. The message is prefixed
/// with "[USER]" in the kernel log output.
///
/// Only available in debug builds.
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
/// use libcluu::syscall::debug_print;
///
/// debug_print("Hello from userspace!").expect("debug_print failed");
/// ```
#[inline]
pub fn debug_print(message: &str) -> Result<()> {
    if message.is_empty() {
        return Ok(());
    }

    let ptr = message.as_ptr() as usize;
    let len = message.len();
    unsafe { syscall2(SyscallNumber::DebugPrint, ptr, len)? };
    Ok(())
}

/// Exit the current thread
///
/// Sends an exit notification to the parent manager and yields forever.
pub fn thread_exit(code: i32) -> ! {
    let _ = debug_print("Thread exiting");
    let _ = crate::ipc::notify_exit(code);
    loop {
        let _ = yield_cpu();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(SyscallNumber::Send as usize, 0);
        assert_eq!(SyscallNumber::Recv as usize, 1);
        assert_eq!(SyscallNumber::Call as usize, 2);
        assert_eq!(SyscallNumber::Reply as usize, 3);
        assert_eq!(SyscallNumber::Yield as usize, 4);
        assert_eq!(SyscallNumber::Invoke as usize, 5);
        assert_eq!(SyscallNumber::DebugPrint as usize, 255);
    }

    #[test]
    fn test_invoke_ops() {
        assert_eq!(InvokeOp::ThreadCreate as usize, 0);
        assert_eq!(InvokeOp::SpaceMap as usize, 12);
        assert_eq!(InvokeOp::TokenDerive as usize, 20);
        assert_eq!(InvokeOp::IrqAttach as usize, 30);
        assert_eq!(InvokeOp::EndpointCreate as usize, 40);
    }
}
