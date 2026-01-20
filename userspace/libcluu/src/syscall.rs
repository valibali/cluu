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
    SpaceMapRange = 15, // Batch mapping for multiple pages

    // Token operations
    TokenDerive = 20,
    TokenRevoke = 21,

    // IRQ operations
    IrqAttach = 30,
    IrqAck = 31,

    // IPC operations
    EndpointCreate = 40,

    // PCI operations
    PciConfigRead = 50,
    PciConfigWrite = 51,
}

/// Page mapping flags for space_map.
pub const MAP_DEVICE: usize = 0x100;

/// Request 2MB large page mapping when possible (for space_map_range).
/// Requires: zero-fill only (no data), 2MB-aligned virtual address, >= 512 pages.
pub const MAP_LARGE_PAGES: usize = 0x200;

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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Receive IPC message from any of the given endpoints (recv_any)
///
/// Waits for a message on any of the provided endpoints. Returns which
/// endpoint received the message and the message length.
///
/// # Timeout semantics
///
/// - `0`: Non-blocking (return WouldBlock immediately if no message)
/// - `u64::MAX`: Block forever
/// - `1..MAX-1`: Block with timeout in milliseconds
///
/// # Arguments
///
/// - `tokens`: Slice of endpoint tokens to wait on
/// - `buf`: Buffer to receive message into
/// - `timeout_ms`: Timeout value (see semantics above)
///
/// # Returns
///
/// - `Ok((index, bytes_received))`: Index of endpoint that had message, and length
/// - `Err(Error::WouldBlock)`: No message available (timeout_ms=0 only)
/// - `Err(Error::Timeout)`: Timeout expired before message arrived
/// - `Err(error)`: Other errors (invalid token, buffer too small, etc.)
#[inline]
pub fn ipc_recv_any(tokens: &[usize], buf: &mut [u8], timeout_ms: u64) -> Result<(usize, usize)> {
    let nonblocking = timeout_ms == 0;

    loop {
        let result = unsafe {
            syscall5(
                SyscallNumber::Recv,
                tokens.as_ptr() as usize,
                tokens.len(),
                buf.as_mut_ptr() as usize,
                buf.len(),
                timeout_ms as usize,
            )
        };

        match result {
            Ok(value) => {
                // Result is (index << 32) | msg_len
                let index = value >> 32;
                let msg_len = value & 0xFFFFFFFF;
                return Ok((index, msg_len));
            }
            Err(Error::WouldBlock) if nonblocking => {
                return Err(Error::WouldBlock);
            }
            Err(Error::WouldBlock) => {
                // Kernel woke us up after blocking, retry to actually get the message
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Receive IPC message from endpoint (blocking forever)
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
#[inline]
pub fn ipc_recv(endpoint_token: usize, buf: &mut [u8]) -> Result<usize> {
    let tokens = [endpoint_token];
    let (_index, len) = ipc_recv_any(&tokens, buf, u64::MAX)?;
    Ok(len)
}

/// Receive IPC message from endpoint (non-blocking)
///
/// Returns immediately with WouldBlock if no message is available.
#[inline]
pub fn ipc_recv_nonblocking(endpoint_token: usize, buf: &mut [u8]) -> Result<usize> {
    let tokens = [endpoint_token];
    let (_index, len) = ipc_recv_any(&tokens, buf, 0)?;
    Ok(len)
}

/// Receive IPC message from endpoint with timeout
///
/// Blocks until a message arrives or the timeout expires.
///
/// # Arguments
///
/// - `endpoint_token`: Token handle for the endpoint
/// - `buf`: Buffer to receive message into
/// - `timeout_ms`: Timeout in milliseconds (use u64::MAX for block forever)
///
/// # Returns
///
/// - `Ok(bytes_received)`: Number of bytes received
/// - `Err(Error::Timeout)`: Timeout expired before message arrived
/// - `Err(error)`: Other errors (invalid token, buffer too small, etc.)
#[inline]
pub fn ipc_recv_timeout(endpoint_token: usize, buf: &mut [u8], timeout_ms: u64) -> Result<usize> {
    let tokens = [endpoint_token];
    let (_index, len) = ipc_recv_any(&tokens, buf, timeout_ms)?;
    Ok(len)
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
/// - `endpoint_token`: Token handle for the endpoint we received the call on
/// - `msg`: Reply message to send
///
/// # Returns
///
/// - `Ok(bytes_sent)`: Number of bytes sent in reply
/// - `Err(error)`: No pending call or send failed
#[inline]
pub fn ipc_reply(endpoint_token: usize, msg: &[u8]) -> Result<usize> {
    unsafe {
        syscall3(
            SyscallNumber::Reply,
            endpoint_token,
            msg.as_ptr() as usize,
            msg.len(),
        )
    }
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
///
/// # Safety
/// Caller must ensure the arguments are valid for the selected operation.
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

/// Map multiple consecutive pages into an address space in a single syscall
///
/// This is more efficient than multiple space_map calls when mapping
/// contiguous regions (like ELF segments or stacks).
///
/// # Arguments
///
/// - `space_token`: Token for the address space (requires SPACE_MAP right)
/// - `virt_start`: Starting virtual address (must be page-aligned)
/// - `source_ptr`: Pointer to source data, or 0 for zero-fill
/// - `flags`: Permission flags (same as space_map)
/// - `num_pages`: Number of 4KB pages to map
/// - `data_len`: Length of data to copy from source_ptr (can be less than num_pages * 4096)
///
/// # Returns
///
/// - `Ok(pages_mapped)`: Number of pages successfully mapped
/// - `Err(error)`: Mapping failed
pub fn space_map_range(
    space_token: usize,
    virt_start: usize,
    source_ptr: usize,
    flags: usize,
    num_pages: usize,
    data_len: usize,
) -> Result<usize> {
    // Pack num_pages and data_len into a single usize
    // Upper 32 bits: num_pages, lower 32 bits: data_len
    let combined = (num_pages << 32) | (data_len & 0xFFFFFFFF);

    unsafe {
        invoke(
            space_token,
            InvokeOp::SpaceMapRange,
            virt_start,
            source_ptr,
            flags,
            combined,
        )
    }
}

/// Grant a page from one address space to another (zero-copy sharing)
///
/// This syscall shares a physical page between two address spaces without
/// copying data. The source page is mapped into the target address space.
///
/// # Arguments
///
/// - `source_space_token`: Source address space token (requires SPACE_GRANT right)
/// - `target_space_token`: Target address space token (requires SPACE_MAP right)
/// - `source_virt`: Virtual address in source space (page-aligned)
/// - `target_virt`: Virtual address in target space where to map (page-aligned)
/// - `flags`: Permission flags (0x02 = writable, 0x04 = executable)
///
/// # Returns
///
/// - `Ok(())`: Page granted successfully
/// - `Err(error)`: Grant failed (permission denied, invalid address, etc.)
///
/// # Example
///
/// ```no_run
/// use libcluu::syscall::space_grant;
///
/// // Share a page from producer to consumer
/// space_grant(
///     my_space_token,      // Source space with SPACE_GRANT right
///     target_space_token,  // Target space with SPACE_MAP right
///     0x1000_0000,         // Source page address
///     0x2000_0000,         // Target page address
///     0x02,                // Writable
/// ).expect("grant failed");
/// ```
pub fn space_grant(
    source_space_token: usize,
    target_space_token: usize,
    source_virt: usize,
    target_virt: usize,
    flags: usize,
) -> Result<()> {
    unsafe {
        invoke(
            source_space_token,
            InvokeOp::SpaceGrant,
            target_space_token,
            source_virt,
            target_virt,
            flags,
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

/// Read from PCI configuration space
///
/// # Arguments
///
/// - `pci_token`: Token with PCI_ACCESS right
/// - `bus`: PCI bus number (0-255)
/// - `device`: PCI device number (0-31)
/// - `function`: PCI function number (0-7)
/// - `offset`: Register offset (must be 4-byte aligned)
///
/// # Returns
///
/// - `Ok(value)`: 32-bit value read from PCI config space
/// - `Err(error)`: Read failed
pub fn pci_config_read(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u32> {
    let devfn = ((device & 0x1F) << 3) | (function & 0x07);
    let value = unsafe {
        invoke(
            pci_token,
            InvokeOp::PciConfigRead,
            bus as usize,
            devfn as usize,
            offset as usize,
            0,
        )?
    };
    Ok(value as u32)
}

/// Write to PCI configuration space
///
/// # Arguments
///
/// - `pci_token`: Token with PCI_ACCESS right
/// - `bus`: PCI bus number (0-255)
/// - `device`: PCI device number (0-31)
/// - `function`: PCI function number (0-7)
/// - `offset`: Register offset (must be 4-byte aligned)
/// - `value`: 32-bit value to write
///
/// # Returns
///
/// - `Ok(())`: Write successful
/// - `Err(error)`: Write failed
pub fn pci_config_write(
    pci_token: usize,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u32,
) -> Result<()> {
    let devfn = ((device & 0x1F) << 3) | (function & 0x07);
    unsafe {
        invoke(
            pci_token,
            InvokeOp::PciConfigWrite,
            bus as usize,
            devfn as usize,
            offset as usize,
            value as usize,
        )?
    };
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
