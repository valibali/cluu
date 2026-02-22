//! Raw syscall interface for CLUU microkernel
//!
//! This module provides the minimal syscall interface (7 syscalls total):
//! - IPC: Send, Recv, Call, Reply
//! - Scheduling: Yield
//! - Operations: Invoke (token-based)
//! - Debug: DebugPrint

use crate::error::{Error, Result};
use core::sync::atomic::{AtomicU8, Ordering};

const IPC_REG_INLINE_FLAG: usize = 1usize << (usize::BITS - 1);
const IPC_REG_INLINE_MAX_PAYLOAD: usize = 32;
const IPC_REG_FAST_UNKNOWN: u8 = 0;
const IPC_REG_FAST_ENABLED: u8 = 1;
const IPC_REG_FAST_DISABLED: u8 = 2;
static IPC_REG_FAST_STATE: AtomicU8 = AtomicU8::new(IPC_REG_FAST_UNKNOWN);

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
    ThreadSetFSBase = 6,
    ThreadGetId = 7,

    // Space operations
    SpaceCreate = 10,
    SpaceDestroy = 11,
    SpaceMap = 12,
    SpaceUnmap = 13,
    SpaceGrant = 14,
    SpaceMapRange = 15, // Batch mapping for multiple pages
    SpaceProtect = 16,  // Batch permission update for mapped pages
    FutexWait = 17,
    FutexWake = 18,

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

    // I/O port operations
    PortIn8 = 52,
    PortIn16 = 53,
    PortIn32 = 54,
    PortOut8 = 55,
    PortOut16 = 56,
    PortOut32 = 57,

    // Memory translation
    VirtToPhys = 58,
    PmmAllocLarge = 59,

    // Clock/time
    ClockNow = 60,
    ClockFrequency = 61,

    // Frame operations
    FrameAllocate = 70,
    FrameFree = 71,
    FrameGetPhys = 72,
}

/// Page mapping flags for space_map.
pub const MAP_DEVICE: usize = 0x100;

/// Request 2MB large page mapping when possible (for space_map_range).
/// Requires: zero-fill only (no data), 2MB-aligned virtual address, >= 512 pages.
pub const MAP_LARGE_PAGES: usize = 0x200;

/// Map using a frame token instead of kernel-allocated frame.
/// When set, arg4 (data_len) is reinterpreted as the frame token handle.
pub const MAP_FRAME_TOKEN: usize = 0x400;

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
        // Caller-saved argument registers are clobbered by the kernel path.
        // Model them as inlateout (not plain inputs) to avoid UB under optimization.
        inlateout("rdi") arg1 => _,
        inlateout("rsi") arg2 => _,
        inlateout("rdx") arg3 => _,
        inlateout("r10") arg4 => _,
        inlateout("r8") arg5 => _,
        inlateout("r9") arg6 => _,
        // The kernel syscall entry does not currently preserve all SysV callee-saved
        // registers. Declare them clobbered so optimized callers don't keep live
        // state (e.g. token handles) across the boundary in these registers.
        lateout("r12") _,
        lateout("r13") _,
        lateout("r14") _,
        lateout("r15") _,
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

/// Returns the first observed RBX change across syscall_raw (if any) and clears the flag.
/// Diagnostic removed - kernel preserves RBX correctly.
pub fn take_rbx_change() -> Option<(u64, u64)> {
    None
}

#[no_mangle]
pub extern "C" fn cluu_take_rbx_change(before: *mut u64, after: *mut u64) -> i32 {
    if let Some((b, a)) = take_rbx_change() {
        unsafe {
            if !before.is_null() {
                *before = b;
            }
            if !after.is_null() {
                *after = a;
            }
        }
        1
    } else {
        0
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
    if msg.len() <= IPC_REG_INLINE_MAX_PAYLOAD
        && IPC_REG_FAST_STATE.load(Ordering::Relaxed) != IPC_REG_FAST_DISABLED
    {
        let mut chunk0 = [0u8; 8];
        let mut chunk1 = [0u8; 8];
        let mut chunk2 = [0u8; 8];
        let mut chunk3 = [0u8; 8];
        copy_inline_chunk(msg, 0, &mut chunk0);
        copy_inline_chunk(msg, 8, &mut chunk1);
        copy_inline_chunk(msg, 16, &mut chunk2);
        copy_inline_chunk(msg, 24, &mut chunk3);

        let fast = unsafe {
            syscall6(
                SyscallNumber::Send,
                endpoint_token,
                usize::from_ne_bytes(chunk0),
                IPC_REG_INLINE_FLAG | msg.len(),
                usize::from_ne_bytes(chunk1),
                usize::from_ne_bytes(chunk2),
                usize::from_ne_bytes(chunk3),
            )
        };

        match fast {
            Ok(_) => {
                IPC_REG_FAST_STATE.store(IPC_REG_FAST_ENABLED, Ordering::Relaxed);
                return Ok(());
            }
            Err(Error::InvalidParameter) | Err(Error::InvalidArgument) => {
                IPC_REG_FAST_STATE.store(IPC_REG_FAST_DISABLED, Ordering::Relaxed);
            }
            Err(err) => return Err(err),
        }
    }

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
    let (index, len, _sender_tid) = ipc_recv_any_with_sender(tokens, buf, timeout_ms)?;
    Ok((index, len))
}

/// Receive IPC message from any endpoint, returning authenticated sender thread id.
///
/// `sender_tid` is `0` when unavailable (for unauthenticated legacy sends).
#[inline]
pub fn ipc_recv_any_with_sender(
    tokens: &[usize],
    buf: &mut [u8],
    timeout_ms: u64,
) -> Result<(usize, usize, usize)> {
    let nonblocking = timeout_ms == 0;
    let mut sender_tid: usize = 0;

    loop {
        let result = unsafe {
            syscall6(
                SyscallNumber::Recv,
                tokens.as_ptr() as usize,
                tokens.len(),
                buf.as_mut_ptr() as usize,
                buf.len(),
                timeout_ms as usize,
                (&mut sender_tid as *mut usize) as usize,
            )
        };

        match result {
            Ok(value) => {
                // Result is (index << 32) | msg_len
                let index = value >> 32;
                let msg_len = value & 0xFFFFFFFF;
                return Ok((index, msg_len, sender_tid));
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
pub fn ipc_recv(endpoint_token: usize, buf: &mut [u8]) -> Result<usize> {
    let tokens = [endpoint_token];
    // Use a finite timeout and loop to avoid kernel deadline overflow with u64::MAX.
    // 30 seconds per iteration is long enough to avoid busy-waiting but short enough
    // to not overflow any reasonable kernel time representation.
    loop {
        match ipc_recv_any(&tokens, buf, 30_000) {
            Ok((_index, len)) => return Ok(len),
            Err(Error::Timeout) => continue,
            Err(Error::WouldBlock) => continue,
            Err(err) => return Err(err),
        }
    }
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
    if msg.len() <= IPC_REG_INLINE_MAX_PAYLOAD
        && IPC_REG_FAST_STATE.load(Ordering::Relaxed) != IPC_REG_FAST_DISABLED
    {
        let mut chunk0 = [0u8; 8];
        let mut chunk1 = [0u8; 8];
        let mut chunk2 = [0u8; 8];
        let mut chunk3 = [0u8; 8];
        copy_inline_chunk(msg, 0, &mut chunk0);
        copy_inline_chunk(msg, 8, &mut chunk1);
        copy_inline_chunk(msg, 16, &mut chunk2);
        copy_inline_chunk(msg, 24, &mut chunk3);

        let fast = unsafe {
            syscall6(
                SyscallNumber::Reply,
                endpoint_token,
                usize::from_ne_bytes(chunk0),
                IPC_REG_INLINE_FLAG | msg.len(),
                usize::from_ne_bytes(chunk1),
                usize::from_ne_bytes(chunk2),
                usize::from_ne_bytes(chunk3),
            )
        };

        match fast {
            Ok(sent) => {
                IPC_REG_FAST_STATE.store(IPC_REG_FAST_ENABLED, Ordering::Relaxed);
                return Ok(sent);
            }
            Err(Error::InvalidParameter) | Err(Error::InvalidArgument) => {
                IPC_REG_FAST_STATE.store(IPC_REG_FAST_DISABLED, Ordering::Relaxed);
            }
            Err(err) => return Err(err),
        }
    }

    unsafe {
        syscall3(
            SyscallNumber::Reply,
            endpoint_token,
            msg.as_ptr() as usize,
            msg.len(),
        )
    }
}

#[inline]
fn copy_inline_chunk(msg: &[u8], offset: usize, dst: &mut [u8; 8]) {
    if offset >= msg.len() {
        return;
    }
    let copy_len = core::cmp::min(8, msg.len() - offset);
    dst[..copy_len].copy_from_slice(&msg[offset..offset + copy_len]);
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

/// Destroy an address space, freeing all its user page tables and frames.
///
/// # Arguments
///
/// - `space_token`: Address space token with DESTROY right
///
/// # Returns
///
/// - `Ok(())`: Address space destroyed
/// - `Err(error)`: Destruction failed (invalid token, active CR3, etc.)
pub fn space_destroy(space_token: usize) -> Result<()> {
    unsafe {
        invoke(space_token, InvokeOp::SpaceDestroy, 0, 0, 0, 0)?;
    }
    Ok(())
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

/// Unmap pages from the current address space.
///
/// # Arguments
/// - `space_token`: Address space token (requires SPACE_MAP right)
/// - `virt_addr`: Page-aligned virtual address to unmap
/// - `num_pages`: Number of 4K pages to unmap (0 treated as 1)
pub fn space_unmap(space_token: usize, virt_addr: usize, num_pages: usize) -> Result<()> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::SpaceUnmap,
            virt_addr,
            num_pages,
            0,
            0,
        )?;
    }
    Ok(())
}

/// Update page protection flags for mapped pages in an address space.
///
/// # Arguments
///
/// - `space_token`: Token for the target address space (SPACE_MAP right required)
/// - `virt_addr`: Starting virtual address (must be page-aligned)
/// - `num_pages`: Number of pages to retag
/// - `flags`: Permission flags (0x01 read baseline, 0x02 writable, 0x04 executable)
///
/// # Returns
///
/// - `Ok(changed_pages)`: Number of pages updated
/// - `Err(error)`: Permission denied, invalid address, or unmapped page
#[inline]
pub fn space_protect(
    space_token: usize,
    virt_addr: usize,
    num_pages: usize,
    flags: usize,
) -> Result<usize> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::SpaceProtect,
            virt_addr,
            num_pages,
            flags,
            0,
        )
    }
}

/// Futex wait: block until a wake on `(space_token, user_addr)` or timeout.
///
/// The kernel first checks the 32-bit value at `user_addr`. If it does not
/// match `expected`, this returns `Error::WouldBlock` immediately.
///
/// Timeout semantics:
/// - `timeout_ms == 0`: wait indefinitely
/// - `timeout_ms > 0`: bounded wait
#[inline]
pub fn futex_wait(
    space_token: usize,
    user_addr: usize,
    expected: u32,
    timeout_ms: u64,
) -> Result<()> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::FutexWait,
            user_addr,
            expected as usize,
            timeout_ms as usize,
            (timeout_ms >> 32) as usize,
        )?;
    }
    Ok(())
}

/// Futex wake: wake up to `max_count` waiters blocked on `(space_token, user_addr)`.
///
/// If `max_count` is 0, the kernel treats it as 1.
#[inline]
pub fn futex_wake(space_token: usize, user_addr: usize, max_count: usize) -> Result<usize> {
    unsafe { invoke(space_token, InvokeOp::FutexWake, user_addr, max_count, 0, 0) }
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

/// Revoke a token, invalidating it and all tokens derived from it.
pub fn token_revoke(token_handle: usize) -> Result<usize> {
    unsafe { invoke(token_handle, InvokeOp::TokenRevoke, 0, 0, 0, 0) }
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

/// Suspend a thread without destroying it.
pub fn thread_suspend(thread_token: usize) -> Result<()> {
    unsafe {
        invoke(thread_token, InvokeOp::ThreadSuspend, 0, 0, 0, 0)?;
    }
    Ok(())
}

/// Resume a previously suspended thread.
pub fn thread_resume(thread_token: usize) -> Result<()> {
    unsafe {
        invoke(thread_token, InvokeOp::ThreadResume, 0, 0, 0, 0)?;
    }
    Ok(())
}

/// Set the FS base register for a thread (used for TLS).
///
/// If the target is the currently running thread, the MSR is updated
/// immediately. Otherwise it takes effect on the next context switch
/// to that thread.
pub fn thread_set_fs_base(thread_token: usize, fs_base: usize) -> Result<()> {
    unsafe {
        invoke(thread_token, InvokeOp::ThreadSetFSBase, fs_base, 0, 0, 0)?;
    }
    Ok(())
}

/// Resolve the kernel thread id carried by a thread token.
pub fn thread_get_id(thread_token: usize) -> Result<usize> {
    unsafe { invoke(thread_token, InvokeOp::ThreadGetId, 0, 0, 0, 0) }
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

/// Read 8-bit value from I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_in8(pci_token: usize, port: u16) -> Result<u8> {
    let value = unsafe { invoke(pci_token, InvokeOp::PortIn8, port as usize, 0, 0, 0)? };
    Ok(value as u8)
}

/// Read 16-bit value from I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_in16(pci_token: usize, port: u16) -> Result<u16> {
    let value = unsafe { invoke(pci_token, InvokeOp::PortIn16, port as usize, 0, 0, 0)? };
    Ok(value as u16)
}

/// Read 32-bit value from I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_in32(pci_token: usize, port: u16) -> Result<u32> {
    let value = unsafe { invoke(pci_token, InvokeOp::PortIn32, port as usize, 0, 0, 0)? };
    Ok(value as u32)
}

/// Write 8-bit value to I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_out8(pci_token: usize, port: u16, value: u8) -> Result<()> {
    unsafe {
        invoke(
            pci_token,
            InvokeOp::PortOut8,
            port as usize,
            value as usize,
            0,
            0,
        )?
    };
    Ok(())
}

/// Write 16-bit value to I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_out16(pci_token: usize, port: u16, value: u16) -> Result<()> {
    unsafe {
        invoke(
            pci_token,
            InvokeOp::PortOut16,
            port as usize,
            value as usize,
            0,
            0,
        )?
    };
    Ok(())
}

/// Write 32-bit value to I/O port
///
/// Requires PCI_ACCESS right on the token.
pub fn port_out32(pci_token: usize, port: u16, value: u32) -> Result<()> {
    unsafe {
        invoke(
            pci_token,
            InvokeOp::PortOut32,
            port as usize,
            value as usize,
            0,
            0,
        )?
    };
    Ok(())
}

/// Translate virtual address to physical address for DMA operations.
///
/// Requires a space token with SPACE_MAP right.
/// Returns the physical address, or 0 if the virtual address is not mapped.
pub fn virt_to_phys(space_token: usize, virt_addr: usize) -> Result<u64> {
    let phys = unsafe { invoke(space_token, InvokeOp::VirtToPhys, virt_addr, 0, 0, 0)? };
    Ok(phys as u64)
}

pub fn pmm_alloc_large(space_token: usize) -> Result<u64> {
    // Uses sys_invoke(space_token, InvokeOp::PmmAllocLarge, 0,0,0,0)
    // Return physical address.
    let r = unsafe { invoke(space_token, InvokeOp::PmmAllocLarge, 0, 0, 0, 0) }?;
    Ok(r as u64)
}

/// Query a monotonic clock value (TSC-based).
#[inline]
pub fn clock_now(clock_token: usize) -> Result<u64> {
    let r = unsafe { invoke(clock_token, InvokeOp::ClockNow, 0, 0, 0, 0) }?;
    Ok(r as u64)
}

/// Query calibrated TSC frequency in Hz.
#[inline]
pub fn clock_frequency(clock_token: usize) -> Result<u64> {
    let r = unsafe { invoke(clock_token, InvokeOp::ClockFrequency, 0, 0, 0, 0) }?;
    Ok(r as u64)
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
/// Sends an exit notification to the parent manager and blocks forever.
pub fn thread_exit(code: i32) -> ! {
    let _ = debug_print("Thread exiting");
    let _ = crate::ipc::notify_exit(code);

    // Block forever waiting for procmgr to destroy us
    let info = crate::boot::process_info();
    let ipc_cap = info.tokens[crate::boot::TOKEN_IPC];
    if ipc_cap != 0 {
        if let Ok(ep) = endpoint_create(ipc_cap) {
            let mut buf = [0u8; 64];
            let _ = ipc_recv(ep, &mut buf);
        }
    }
    // Fallback: yield loop
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
        assert_eq!(InvokeOp::ThreadGetId as usize, 7);
        assert_eq!(InvokeOp::SpaceMap as usize, 12);
        assert_eq!(InvokeOp::FutexWait as usize, 17);
        assert_eq!(InvokeOp::FutexWake as usize, 18);
        assert_eq!(InvokeOp::TokenDerive as usize, 20);
        assert_eq!(InvokeOp::IrqAttach as usize, 30);
        assert_eq!(InvokeOp::EndpointCreate as usize, 40);
    }
}
