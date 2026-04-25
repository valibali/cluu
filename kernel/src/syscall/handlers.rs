//! System Call Handlers - Token-Based Model
//!
//! This module implements handler functions for the minimal syscall set (7 syscalls).
//!
//! # Syscalls
//!
//! 1. **sys_send** - Send IPC message to endpoint
//! 2. **sys_recv** - Receive IPC message from endpoint
//! 3. **sys_call** - Call (send + receive) for synchronous RPC
//! 4. **sys_reply** - Reply to IPC sender
//! 5. **sys_yield** - Yield CPU to scheduler
//! 6. **sys_invoke** - Generic operation invocation on a token
//! 7. **sys_debug_print** - Debug output (debug builds only)
//!
//! # Security
//!
//! All handlers must:
//! 1. Validate token handles (expiration, signature, rights)
//! 2. Check user pointers are in userspace range
//! 3. Return errors instead of panicking

use crate::error::Error;
use crate::syscall::{SyscallArgs, SyscallResult};
use crate::token::{lookup_token, InvokeOp, TokenHandle};
use core::sync::atomic::{AtomicU64, Ordering};

const IPC_REG_INLINE_FLAG: usize = 1usize << (usize::BITS - 1);
const IPC_REG_INLINE_MAX_PAYLOAD: usize = 32;
/// Flag bit on `invoke_thread_create`'s `args.arg6`: create the thread
/// SUSPENDED. Userspace must call `thread_resume` to make it runnable.
/// Mirrors `THREAD_CREATE_START_SUSPENDED` in `userspace/libcluu/src/syscall.rs`.
const THREAD_CREATE_START_SUSPENDED: u64 = 0x1;
/// sys_call uses arg4/arg5 for reply_buf/reply_len, leaving only arg2+arg6 (2 registers = 16B)
/// for inline send data. This is an ABI constraint, not a tunable — sys_send has 4 free registers.
const IPC_REG_INLINE_MAX_CALL_PAYLOAD: usize = 16;
const IPC_CALL_TRACE_LIMIT: u64 = 512;
static IPC_CALL_TRACE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Helper: check if IPC call tracing is active for this call index.
#[inline]
fn ipc_trace_active(idx: u64) -> bool {
    idx < IPC_CALL_TRACE_LIMIT && klibcluu::should_log(klibcluu::LogLevel::Trace)
}

// ═══════════════════════════════════════════════════════════════════════════
// IPC Syscalls
// ═══════════════════════════════════════════════════════════════════════════

/// sys_send - Send IPC message to endpoint
///
/// # Arguments
///
/// - arg1: endpoint_token (TokenHandle)
/// - arg2: msg_ptr (*const u8)
/// - arg3: msg_len (usize)
/// - arg4-arg6: unused
///
/// # Returns
///
/// - Ok(0): Success
/// - Err(Error): Token invalid, insufficient rights, or IPC error
pub fn sys_send(args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    let token_handle = TokenHandle::from_raw(args.arg1);
    let msg_ptr = args.arg2;
    let msg_len = args.arg3;

    let (token, obj_ref) = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_SEND) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref = crate::token::check_object_type(obj_ref, ObjectType::Endpoint)
        .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let inline_flag_set = (msg_len & IPC_REG_INLINE_FLAG) != 0;
    if inline_flag_set {
        if !crate::ipc::endpoint::register_fast_enabled() {
            return Err(Error::InvalidParameter);
        }
        let inline_len = msg_len & !IPC_REG_INLINE_FLAG;
        if inline_len > IPC_REG_INLINE_MAX_PAYLOAD {
            return Err(Error::InvalidParameter);
        }
        let mut buffer = [0u8; IPC_REG_INLINE_MAX_PAYLOAD];
        let chunks = [args.arg2, args.arg4, args.arg5, args.arg6];
        let mut copied = 0usize;
        for chunk in chunks {
            if copied >= inline_len {
                break;
            }
            let bytes = chunk.to_ne_bytes();
            let chunk_len = core::cmp::min(bytes.len(), inline_len - copied);
            buffer[copied..copied + chunk_len].copy_from_slice(&bytes[..chunk_len]);
            copied += chunk_len;
        }
        crate::ipc::endpoint::send_from_kernel(endpoint_id, &buffer[..inline_len])?;
    } else {
        let page_table_root =
            crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
        crate::ipc::endpoint::send_from_user(endpoint_id, msg_ptr, msg_len, page_table_root)?;
    }
    Ok(0)
}

/// sys_recv - Receive IPC message from one or more endpoints (recv_any)
///
/// # Arguments
///
/// - arg1: tokens_ptr (*const usize) - pointer to array of endpoint tokens
/// - arg2: tokens_count (usize) - number of tokens in array
/// - arg3: buf_ptr (*mut u8)
/// - arg4: buf_len (usize) - high bit is NONBLOCK_FLAG
/// - arg5: timeout_ms (0 = block forever, >0 = timeout in milliseconds)
/// - arg6: sender_out_ptr (*mut usize, optional) - authenticated sender thread id
///
/// # Returns
///
/// - Ok((index << 32) | bytes_received): index of endpoint that had message, and length
/// - Err(Error::Timeout): Timeout expired before message arrived
/// - Err(Error): Token invalid, insufficient rights, or IPC error
pub fn sys_recv(args: SyscallArgs) -> SyscallResult {
    use crate::token::{EndpointId, ObjectRef, ObjectType, Rights};

    let tokens_ptr = args.arg1;
    let tokens_count = args.arg2;
    let buf_ptr = args.arg3;
    let buf_len = args.arg4;
    let timeout_ms = args.arg5 as u64;
    let sender_out_ptr = args.arg6;

    // Timeout semantics:
    // - 0: non-blocking (return WouldBlock immediately if no message)
    // - u64::MAX: block forever
    // - 1..MAX-1: block with timeout in milliseconds
    let nonblocking = timeout_ms == 0;

    // Validate tokens_count
    const MAX_RECV_ENDPOINTS: usize = 16;
    if tokens_count == 0 || tokens_count > MAX_RECV_ENDPOINTS {
        return Err(Error::InvalidArgument);
    }

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;

    // Read token handles from userspace and resolve to endpoint IDs
    let mut endpoint_ids: [Option<EndpointId>; MAX_RECV_ENDPOINTS] = [None; MAX_RECV_ENDPOINTS];
    for (i, endpoint_slot) in endpoint_ids.iter_mut().enumerate().take(tokens_count) {
        let token_addr = tokens_ptr + i * core::mem::size_of::<usize>();
        crate::syscall::userptr::validate_user_buffer(token_addr, core::mem::size_of::<usize>())?;

        let mut token_raw: usize = 0;
        // Safety: token_raw points to a kernel buffer sized for usize.
        unsafe {
            crate::syscall::userptr::copy_from_user(
                &mut token_raw as *mut usize as *mut u8,
                token_addr,
                core::mem::size_of::<usize>(),
                page_table_root,
            )?;
        }

        let token_handle = TokenHandle::from_raw(token_raw);
        let (token, obj_ref) = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
        if !token.has_right(Rights::IPC_RECV) {
            return Err(Error::PermissionDenied);
        }

        let endpoint_ref = crate::token::check_object_type(obj_ref, ObjectType::Endpoint)
            .map_err(|_| Error::InvalidArgument)?;
        if let ObjectRef::Endpoint(id) = endpoint_ref {
            *endpoint_slot = Some(id);
        } else {
            return Err(Error::InvalidArgument);
        }
    }

    // Rotate scan start per thread to prevent head-of-list starvation in recv_any.
    let scan_start = crate::sched::ThreadManager::with_thread_mut(current, |thread| {
        thread.next_recv_scan_start(tokens_count)
    })
    .unwrap_or(0);

    let write_sender_tid = |sender: Option<crate::sched::ThreadId>| -> Result<(), Error> {
        if sender_out_ptr == 0 {
            return Ok(());
        }
        crate::syscall::userptr::validate_user_buffer(
            sender_out_ptr,
            core::mem::size_of::<usize>(),
        )?;
        let sender_raw = sender.map_or(0usize, |tid: crate::sched::ThreadId| tid.as_u64() as usize);
        // Safety: sender_out_ptr is a validated userspace buffer for usize.
        unsafe {
            crate::syscall::userptr::copy_to_user(
                sender_out_ptr,
                &sender_raw as *const usize as *const u8,
                core::mem::size_of::<usize>(),
                page_table_root,
            )?;
        }
        Ok(())
    };

    let find_endpoint_index = |endpoint: EndpointId| -> Option<usize> {
        endpoint_ids
            .iter()
            .take(tokens_count)
            .position(|id| *id == Some(endpoint))
    };

    // Fast completion for direct deliveries that woke us from a previous blocked recv syscall.
    // The current userspace wrapper retries recv_any after WouldBlock, so consume the pending
    // delivery before re-arming wait state.
    if let Some((delivered_endpoint, delivered_len, delivered_sender)) =
        crate::sched::ThreadManager::take_current_recv_wait_delivery()
    {
        if let Some(index) = find_endpoint_index(delivered_endpoint) {
            write_sender_tid(delivered_sender)?;
            return Ok((index << 32) | delivered_len);
        }
    }

    // Try to receive from each endpoint in round-robin order
    let try_recv_any = || -> Result<(usize, usize), Error> {
        for offset in 0..tokens_count {
            let i = (scan_start + offset) % tokens_count;
            if let Some(endpoint_id) = endpoint_ids[i] {
                match crate::ipc::endpoint::recv_to_user_nonblocking(
                    endpoint_id,
                    buf_ptr,
                    buf_len,
                    page_table_root,
                ) {
                    Ok((len, sender)) => {
                        crate::telemetry::record_ipc_recv_scan_steps(offset + 1);
                        write_sender_tid(sender)?;
                        return Ok((i, len));
                    }
                    Err(Error::WouldBlock) | Err(Error::NotFound) => continue,
                    Err(err) => return Err(err),
                }
            }
        }
        Err(Error::WouldBlock)
    };

    // First attempt
    match try_recv_any() {
        Ok((index, len)) => return Ok((index << 32) | len),
        Err(Error::WouldBlock) if nonblocking => {
            crate::telemetry::record_ipc_recv_would_block();
            return Err(Error::WouldBlock);
        }
        Err(Error::WouldBlock) => { /* fall through to blocking */ }
        Err(err) => return Err(err),
    }

    // Arm recv wait before enqueueing on endpoints to close registration/block race.
    crate::sched::ThreadManager::arm_current_recv_wait_with_buffer(buf_ptr, buf_len);
    let recv_wait_ticket =
        crate::sched::ThreadManager::current_recv_wait_ticket().ok_or(Error::InvalidState)?;

    // Register as waiter on all endpoints
    for endpoint_id in endpoint_ids.iter().take(tokens_count).filter_map(|id| *id) {
        if let Err(err) = crate::ipc::endpoint::register_wait(
            endpoint_id,
            current,
            recv_wait_ticket,
            buf_ptr,
            buf_len,
            page_table_root,
        ) {
            crate::sched::ThreadManager::disarm_current_recv_wait();
            return Err(err);
        }
    }

    // Re-check once after wait registration and before blocking. This closes the
    // "registered but not yet blocked" window and prevents missed wakeups.
    match try_recv_any() {
        Ok((index, len)) => {
            crate::sched::ThreadManager::disarm_current_recv_wait();
            return Ok((index << 32) | len);
        }
        Err(Error::WouldBlock) => {}
        Err(err) => {
            crate::sched::ThreadManager::disarm_current_recv_wait();
            return Err(err);
        }
    }

    if let Some((delivered_endpoint, delivered_len, delivered_sender)) =
        crate::sched::ThreadManager::take_current_recv_wait_delivery()
    {
        if let Some(index) = find_endpoint_index(delivered_endpoint) {
            write_sender_tid(delivered_sender)?;
            crate::sched::ThreadManager::disarm_current_recv_wait();
            return Ok((index << 32) | delivered_len);
        }
    }

    // Block with or without timeout
    let wait_start_tick = crate::sched::ThreadManager::current_tick();
    let blocked = if timeout_ms == u64::MAX {
        // Block forever
        crate::sched::ThreadManager::block_current_recv_wait(recv_wait_ticket)
    } else {
        // Block with timeout
        let deadline = crate::sched::ThreadManager::ms_to_deadline(timeout_ms);
        crate::sched::ThreadManager::block_current_recv_wait_with_timeout(
            recv_wait_ticket,
            deadline,
        )
    };

    if !blocked {
        // A sender wake can race just before we park. In that case, do not
        // self-block; consume any pending delivery/queue state immediately.
        crate::sched::ThreadManager::disarm_current_recv_wait();
        if let Some((delivered_endpoint, delivered_len, delivered_sender)) =
            crate::sched::ThreadManager::take_current_recv_wait_delivery()
        {
            if let Some(index) = find_endpoint_index(delivered_endpoint) {
                write_sender_tid(delivered_sender)?;
                return Ok((index << 32) | delivered_len);
            }
        }
        return match try_recv_any() {
            Ok((index, len)) => Ok((index << 32) | len),
            Err(Error::WouldBlock) => {
                crate::telemetry::record_ipc_recv_would_block();
                Err(Error::WouldBlock)
            }
            Err(err) => Err(err),
        };
    }

    crate::architecture::x86_64::syscall::request_resched();
    let wait_ticks = crate::sched::ThreadManager::current_tick().saturating_sub(wait_start_tick);
    crate::telemetry::record_ipc_recv_wait_ticks(wait_ticks);

    // After waking, check if it was due to timeout
    if crate::sched::ThreadManager::check_and_clear_timeout_wake() {
        crate::sched::ThreadManager::disarm_current_recv_wait();
        crate::telemetry::record_ipc_recv_timeout();
        Err(Error::Timeout)
    } else {
        crate::sched::ThreadManager::disarm_current_recv_wait();
        if let Some((delivered_endpoint, delivered_len, delivered_sender)) =
            crate::sched::ThreadManager::take_current_recv_wait_delivery()
        {
            let Some(index) = find_endpoint_index(delivered_endpoint) else {
                // A direct delivery may target a stale wait registration from an older recv_any
                // call. Treat this as a spurious wake and continue with normal queue probing.
                return match try_recv_any() {
                    Ok((index, len)) => Ok((index << 32) | len),
                    Err(Error::WouldBlock) => {
                        crate::telemetry::record_ipc_recv_would_block();
                        Err(Error::WouldBlock)
                    }
                    Err(err) => Err(err),
                };
            };
            write_sender_tid(delivered_sender)?;
            return Ok((index << 32) | delivered_len);
        }

        match try_recv_any() {
            Ok((index, len)) => Ok((index << 32) | len),
            Err(Error::WouldBlock) => {
                crate::telemetry::record_ipc_recv_would_block();
                Err(Error::WouldBlock) // Message arrived on one endpoint, retry will succeed
            }
            Err(err) => Err(err),
        }
    }
}

/// sys_call - Call (send + receive) for synchronous RPC
///
/// # Arguments
///
/// - arg1: endpoint_token (TokenHandle)
/// - arg2: msg_ptr (*const u8)
/// - arg3: msg_len (usize)
/// - arg4: reply_buf (*mut u8)
/// - arg5: reply_len (usize)
/// - arg6: unused
///
/// # Returns
///
/// - Ok(bytes_received): Number of bytes in reply
/// - Err(Error): Token invalid, insufficient rights, or IPC error
pub fn sys_call(args: SyscallArgs) -> SyscallResult {
    use crate::sched::CallReplyInfo;
    use crate::token::{ObjectRef, ObjectType, Rights};

    let token_handle = TokenHandle::from_raw(args.arg1);
    let msg_ptr = args.arg2;
    let msg_len = args.arg3;
    let reply_buf = args.arg4;
    let reply_len = args.arg5;
    // arg6 is used for inline data in the fast path (IPC_REG_INLINE_FLAG).
    // In the slow path, arg6 is interpreted as timeout_ms (0 = block forever).
    let trace_idx = IPC_CALL_TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    if ipc_trace_active(trace_idx) {
        klibcluu::trace("sys_call entry");
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  endpoint_token=",
            token_handle.as_raw() as u64,
        );
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  msg_len=", msg_len as u64);
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  reply_len=", reply_len as u64);
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  caller=",
            crate::sched::ThreadManager::current()
                .map(|tid| tid.as_u64())
                .unwrap_or(0),
        );
    }

    // 1. Validate token and check IPC_CALL right
    let (token, obj_ref) = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_CALL) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref = crate::token::check_object_type(obj_ref, ObjectType::Endpoint)
        .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };
    if ipc_trace_active(trace_idx) {
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  endpoint_id=",
            endpoint_id.as_u64(),
        );
    }

    let inline_flag_set = (msg_len & IPC_REG_INLINE_FLAG) != 0;
    if inline_flag_set {
        if !crate::ipc::endpoint::register_fast_enabled() {
            return Err(Error::InvalidParameter);
        }
        let inline_len = msg_len & !IPC_REG_INLINE_FLAG;
        if inline_len > IPC_REG_INLINE_MAX_CALL_PAYLOAD {
            return Err(Error::InvalidParameter);
        }

        let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;
        let page_table_root =
            crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
        let reply_id = crate::sched::ThreadManager::alloc_reply_id();
        if !crate::sched::ThreadManager::set_call_reply_info(
            reply_id,
            CallReplyInfo {
                caller: current,
                reply_buf_ptr: reply_buf,
                reply_buf_len: reply_len,
                page_table_root,
                server_thread_id: None,
            },
        ) {
            return Err(Error::Overflow);
        }

        // Build 56-byte UserMessage buffer (required by inject_reply_id)
        let mut buffer = [0u8; crate::ipc::endpoint::USER_MESSAGE_SIZE];
        let a2_bytes = args.arg2.to_ne_bytes();
        let a6_bytes = args.arg6.to_ne_bytes();
        let copy_a2 = core::cmp::min(8, inline_len);
        buffer[..copy_a2].copy_from_slice(&a2_bytes[..copy_a2]);
        if inline_len > 8 {
            let copy_a6 = core::cmp::min(8, inline_len - 8);
            buffer[8..8 + copy_a6].copy_from_slice(&a6_bytes[..copy_a6]);
        }

        // Pass buffer.len() (56) as payload_len — NOT inline_len
        // inject_reply_id writes at offset 48 (words[5]), so the full 56 bytes
        // must be delivered to the receiver
        let payload_len = buffer.len();
        if let Err(e) = crate::ipc::endpoint::call_from_kernel_with_reply_id(
            endpoint_id,
            &mut buffer,
            payload_len,
            reply_id,
        ) {
            let _ = crate::sched::ThreadManager::take_call_reply_info(reply_id);
            return Err(e);
        }

        crate::sched::ThreadManager::block_current();
        crate::architecture::x86_64::syscall::request_resched();
        return Ok(0);
    }
    // --- existing slow path follows unchanged below ---

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;

    // 2. Allocate reply ID and store reply buffer info
    let reply_id = crate::sched::ThreadManager::alloc_reply_id();
    if !crate::sched::ThreadManager::set_call_reply_info(
        reply_id,
        CallReplyInfo {
            caller: current,
            reply_buf_ptr: reply_buf,
            reply_buf_len: reply_len,
            page_table_root,
            server_thread_id: None,
        },
    ) {
        klibcluu::warn("sys_call: set_call_reply_info failed");
        return Err(Error::Overflow);
    }

    // 3. Send call message with reply_id injected
    if let Err(e) = crate::ipc::endpoint::call_from_user_with_reply_id(
        endpoint_id,
        msg_ptr,
        msg_len,
        page_table_root,
        reply_id,
    ) {
        // Rollback: remove orphaned CALL_REPLY_MAP entry on any send error
        // (prevents leak when caller retries after WouldBlock backpressure)
        let _ = crate::sched::ThreadManager::take_call_reply_info(reply_id);
        return Err(e);
    }

    // 5. Block waiting for reply, with optional timeout.
    //    arg6 is only meaningful in the slow path (fast path returned above).
    //    0 or u64::MAX = block forever; 1..MAX-1 = timeout in ms.
    let timeout_ms = args.arg6 as u64;
    let has_timeout = timeout_ms != 0 && timeout_ms != u64::MAX;
    if has_timeout {
        let deadline = crate::sched::ThreadManager::ms_to_deadline(timeout_ms);
        crate::sched::ThreadManager::block_current_with_timeout(deadline);
    } else {
        crate::sched::ThreadManager::block_current();
    }
    crate::architecture::x86_64::syscall::request_resched();

    // After wake: distinguish reply-delivered vs timeout.
    // Race: deliver_reply may consume CALL_REPLY_MAP *and* the timeout may
    // fire simultaneously. Check reply delivery first — if the entry is gone,
    // the reply succeeded regardless of the timeout flag.
    if has_timeout {
        if !crate::sched::ThreadManager::has_call_reply_info(reply_id) {
            // Reply was delivered — clear stale timeout flag, succeed.
            let _ = crate::sched::ThreadManager::check_and_clear_timeout_wake();
        } else if crate::sched::ThreadManager::check_and_clear_timeout_wake() {
            // Timed out — clean up orphaned CALL_REPLY_MAP entry.
            let _ = crate::sched::ThreadManager::take_call_reply_info(reply_id);
            return Err(Error::Timeout);
        }
    }

    // Return value will be set by deliver_reply in thread.context.rax
    Ok(0)
}

/// sys_reply - Reply to IPC sender using one-time reply token
///
/// # Arguments
///
/// - arg1: reply_id (u64) - implicit reply ID from received message header
/// - arg2: msg_ptr (*const u8) - reply message
/// - arg3: msg_len (usize) - reply length
/// - arg4-arg6: unused (or inline payload when IPC_REG_INLINE_FLAG set)
///
/// # Returns
///
/// - Ok(bytes_sent): Number of bytes in reply
/// - Err(Error): Invalid reply ID or IPC error
pub fn sys_reply(args: SyscallArgs) -> SyscallResult {
    let reply_id = crate::token::ReplyId::new(args.arg1 as u64);
    let msg_ptr = args.arg2;
    let msg_len = args.arg3;

    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;

    // Copy reply payload from registers (fast path) or userspace memory (legacy path)
    let mut buffer = [0u8; crate::ipc::endpoint::IPC_MESSAGE_MAX];
    let inline_flag_set = (msg_len & IPC_REG_INLINE_FLAG) != 0;
    let reply_len = if inline_flag_set {
        if !crate::ipc::endpoint::register_fast_enabled() {
            return Err(Error::InvalidParameter);
        }
        let inline_len = msg_len & !IPC_REG_INLINE_FLAG;
        if inline_len > IPC_REG_INLINE_MAX_PAYLOAD {
            return Err(Error::InvalidParameter);
        }
        let chunks = [args.arg2, args.arg4, args.arg5, args.arg6];
        let mut copied = 0usize;
        for chunk in chunks {
            if copied >= inline_len {
                break;
            }
            let bytes = chunk.to_ne_bytes();
            let chunk_len = core::cmp::min(bytes.len(), inline_len - copied);
            buffer[copied..copied + chunk_len].copy_from_slice(&bytes[..chunk_len]);
            copied += chunk_len;
        }
        inline_len
    } else {
        let page_table_root =
            crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
        crate::syscall::userptr::validate_user_buffer(msg_ptr, msg_len)?;
        if msg_len > buffer.len() {
            return Err(Error::InvalidParameter);
        }
        unsafe {
            crate::syscall::userptr::copy_from_user(
                buffer.as_mut_ptr(),
                msg_ptr,
                msg_len,
                page_table_root,
            )?;
        }
        msg_len
    };

    // Try call reply first, then fault reply
    match crate::sched::ThreadManager::take_call_reply_info_verified(reply_id, current) {
        Some(call_info) => crate::ipc::endpoint::deliver_reply(call_info, &buffer[..reply_len]),
        None => {
            match crate::sched::ThreadManager::take_fault_reply_info_verified(reply_id, current) {
                Some(fault_info) => {
                    handle_fault_reply(fault_info, &buffer[..reply_len]);
                    Ok(0)
                }
                None => Err(Error::PermissionDenied),
            }
        }
    }
}

/// Handle a fault reply from a fault handler process.
///
/// Convention: label=0 → RESUME (restore saved context, re-execute faulting instruction)
///             label≠0 → KILL (terminate the faulted thread)
fn handle_fault_reply(fault_info: crate::sched::FaultReplyInfo, reply_data: &[u8]) {
    // Parse label from reply message (first 4 bytes = UserMessageTag.label)
    let label = if reply_data.len() >= 4 {
        u32::from_ne_bytes([reply_data[0], reply_data[1], reply_data[2], reply_data[3]])
    } else {
        1 // Default: kill (no valid message)
    };

    let thread_id = fault_info.faulted_thread;

    if label == 0 {
        // RESUME: restore saved context and wake thread
        crate::sched::ThreadManager::with_thread_mut(thread_id, |t| {
            if let Some(fault_state) = t.fault_state.take() {
                t.context = fault_state.saved_context;
            }
        });
        crate::sched::ThreadManager::wake_thread(thread_id);
        klibcluu::trace("Fault reply: RESUME thread ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
    } else {
        // KILL: mark thread dead
        crate::sched::ThreadManager::with_thread_mut(thread_id, |t| {
            t.fault_state = None;
        });
        let _ = crate::sched::ThreadManager::mark_thread_dead(thread_id);
        klibcluu::trace("Fault reply: KILL thread ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Scheduling Syscall
// ═══════════════════════════════════════════════════════════════════════════

/// sys_yield - Yield CPU to scheduler
///
/// Voluntarily gives up the CPU and allows another thread to run.
/// In INITMODE (cooperative), this is the only way to switch threads.
/// In NORMALMODE (preemptive), threads can also be preempted by timer.
///
/// # Arguments
///
/// - all unused
///
/// # Returns
///
/// - Ok(0): Always succeeds
pub fn sys_yield(_args: SyscallArgs) -> SyscallResult {
    // Note: Context switch happens in syscall_entry.asm after this returns
    // The syscall_entry calls schedule_and_switch() which will:
    // 1. Get current thread ID (from CURRENT_THREAD)
    // 2. Save current thread's context
    // 3. Add current thread back to scheduler
    // 4. Pick next thread and return its context

    // In INITMODE, the first yield from each critical thread signals readiness.
    // When all critical processes have signaled, we switch to NORMALMODE.
    // Note: We do NOT mark the last signaling thread as dead - all critical
    // processes should continue running in NORMALMODE.
    crate::architecture::x86_64::syscall::request_resched();

    if crate::sched::ThreadManager::is_init_mode()
        && crate::sched::ThreadManager::critical_processes_remaining() > 0
    {
        crate::sched::ThreadManager::signal_critical_process_ready();
    }

    Ok(0)
}

// ═══════════════════════════════════════════════════════════════════════════
// Generic Operation Syscall
// ═══════════════════════════════════════════════════════════════════════════

/// sys_invoke - Invoke operation on a token
///
/// This is the workhorse syscall that handles all object operations:
/// - Thread: create, destroy, suspend, resume, set priority
/// - Space: create, destroy, map, unmap, grant
/// - Token: derive, revoke
/// - IRQ: attach, ack
///
/// # Arguments
///
/// - arg1: token_handle (TokenHandle)
/// - arg2: operation (InvokeOp)
/// - arg3-arg6: operation-specific arguments
///
/// # Returns
///
/// - Ok(value): Operation-specific return value (often new token handle)
/// - Err(Error): Token invalid, insufficient rights, or operation error
pub fn sys_invoke(args: SyscallArgs) -> SyscallResult {
    let token_handle = TokenHandle::from_raw(args.arg1);
    let operation = InvokeOp::from_usize(args.arg2).ok_or(Error::InvalidArgument)?;

    // Validate and lookup token
    let (token, obj_ref) = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;

    //klibcluu::trace("sys_invoke: operation = ");
    //klibcluu::log_dec(klibcluu::LogLevel::Trace, "", args.arg2 as u64);

    // Dispatch based on operation
    match operation {
        // Thread operations
        InvokeOp::ThreadCreate => invoke_thread_create(&token, obj_ref, args),
        InvokeOp::ThreadDestroy => invoke_thread_destroy(&token, obj_ref, args),
        InvokeOp::ThreadSuspend => invoke_thread_suspend(&token, obj_ref, args),
        InvokeOp::ThreadResume => invoke_thread_resume(&token, obj_ref, args),
        InvokeOp::ThreadSetPriority => invoke_thread_set_priority(&token, obj_ref, args),
        InvokeOp::ThreadSetFaultEndpoint => invoke_thread_set_fault_endpoint(&token, obj_ref, args),
        InvokeOp::ThreadSetFSBase => invoke_thread_set_fs_base(&token, obj_ref, args),
        InvokeOp::ThreadGetId => invoke_thread_get_id(&token, obj_ref, args),
        InvokeOp::ThreadGetStats => invoke_thread_get_stats(&token, obj_ref, args),

        // Space operations
        InvokeOp::SpaceCreate => invoke_space_create(&token, obj_ref, args),
        InvokeOp::SpaceDestroy => invoke_space_destroy(&token, obj_ref, args),
        InvokeOp::SpaceMap => invoke_space_map(&token, obj_ref, args),
        InvokeOp::SpaceUnmap => invoke_space_unmap(&token, obj_ref, args),
        InvokeOp::SpaceGrant => invoke_space_grant(&token, obj_ref, args),
        InvokeOp::SpaceMapRange => invoke_space_map_range(&token, obj_ref, args),
        InvokeOp::SpaceProtect => invoke_space_protect(&token, obj_ref, args),
        InvokeOp::SpaceGetStats => invoke_space_get_stats(&token, obj_ref, args),
        InvokeOp::FutexWait => invoke_futex_wait(&token, obj_ref, args),
        InvokeOp::FutexWake => invoke_futex_wake(&token, obj_ref, args),

        // Token operations
        InvokeOp::TokenDerive => invoke_token_derive(token_handle, &token, obj_ref, args),
        InvokeOp::TokenRevoke => invoke_token_revoke(token_handle, &token, obj_ref, args),

        // IRQ operations
        InvokeOp::IrqAttach => invoke_irq_attach(&token, obj_ref, args),
        InvokeOp::IrqAck => invoke_irq_ack(&token, obj_ref, args),

        // IPC operations
        InvokeOp::EndpointCreate => invoke_endpoint_create(&token, obj_ref, args),

        // PCI operations
        InvokeOp::PciConfigRead => invoke_pci_config_read(&token, obj_ref, args),
        InvokeOp::PciConfigWrite => invoke_pci_config_write(&token, obj_ref, args),

        // I/O port operations
        InvokeOp::PortIn8 => invoke_port_in8(&token, obj_ref, args),
        InvokeOp::PortIn16 => invoke_port_in16(&token, obj_ref, args),
        InvokeOp::PortIn32 => invoke_port_in32(&token, obj_ref, args),
        InvokeOp::PortOut8 => invoke_port_out8(&token, obj_ref, args),
        InvokeOp::PortOut16 => invoke_port_out16(&token, obj_ref, args),
        InvokeOp::PortOut32 => invoke_port_out32(&token, obj_ref, args),
        InvokeOp::VirtToPhys => invoke_virt_to_phys(&token, obj_ref, args),
        InvokeOp::PmmAllocLarge => invoke_pmm_alloc_large(&token, obj_ref, args),
        InvokeOp::ClockNow => invoke_clock_now(&token, obj_ref, args),
        InvokeOp::ClockFrequency => invoke_clock_frequency(&token, obj_ref, args),

        // Frame operations
        InvokeOp::FrameAllocate => invoke_frame_allocate(&token, obj_ref, args),
        InvokeOp::FrameFree => invoke_frame_free(token_handle, &token, obj_ref, args),
        InvokeOp::FrameGetPhys => invoke_frame_get_phys(&token, obj_ref, args),

        // Notification operations
        InvokeOp::NotificationCreate => invoke_notification_create(&token, obj_ref, args),
        InvokeOp::NotificationSignal => invoke_notification_signal(&token, obj_ref, args),
        InvokeOp::NotificationWait => invoke_notification_wait(&token, obj_ref, args),
        InvokeOp::NotificationPoll => invoke_notification_poll(&token, obj_ref, args),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Invoke Operation Handlers
// ═══════════════════════════════════════════════════════════════════════════

use crate::token::scope::ObjectRef;
use crate::token::Token;

// Thread operations

fn invoke_thread_create(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::sched::{Priority, Thread, ThreadFlags, ThreadManager};
    use crate::token::{Issuer, ObjectRef, ObjectType, OpaqueScope, Rights, Timestamp};
    use x86_64::VirtAddr;

    klibcluu::trace("invoke_thread_create");

    if !token.has_right(Rights::THREAD_CONTROL) {
        klibcluu::warn("invoke_thread_create: missing THREAD_CONTROL right");
        return Err(Error::PermissionDenied);
    }

    let entry = args.arg3 as u64;
    let stack = args.arg4 as u64;
    let priority = if args.arg5 > 255 {
        255
    } else {
        args.arg5 as u8
    };
    let create_flags = args.arg6 as u64;

    if entry == 0 || stack == 0 {
        return Err(Error::InvalidArgument);
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let page_table_root =
        crate::mm::space_repository::with_space(space_id, |space| space.page_table_root)
            .ok_or(Error::NotFound)?;

    let thread_id = ThreadManager::try_alloc_thread_id().map_err(|_| Error::OutOfMemory)?;
    let mut flags = if ThreadManager::is_init_mode() {
        ThreadFlags::COOPERATIVE
    } else {
        ThreadFlags::empty()
    };
    // Flag bit 0: create the thread SUSPENDED. Caller must invoke
    // thread_resume to make it runnable. Used by procmgr to install per-thread
    // VFS views before the thread runs (closes a SET_VIEW vs first-call race).
    if create_flags & THREAD_CREATE_START_SUSPENDED != 0 {
        flags = flags.with(ThreadFlags::SUSPENDED);
    }

    let thread = Thread::new(
        thread_id,
        page_table_root,
        VirtAddr::new(entry),
        VirtAddr::new(stack),
        Priority::new(priority),
        flags,
    );

    let thread_id = ThreadManager::add_thread(thread);

    if ThreadManager::is_init_mode() {
        ThreadManager::register_critical_thread(thread_id);
    }

    let scope = OpaqueScope::random();
    let thread_token = crate::token::create_token(
        scope,
        Rights::thread_full(),
        Issuer::Kernel,
        Timestamp::far_future(),
        ObjectRef::Thread(thread_id),
    );

    Ok(thread_token.as_usize())
}

fn invoke_thread_destroy(_token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !_token.has_right(Rights::DESTROY) {
        klibcluu::warn("invoke_thread_destroy: missing DESTROY right");
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    if !crate::sched::ThreadManager::mark_thread_dead(thread_id) {
        return Err(Error::NotFound);
    }

    crate::telemetry::log_resource_delta("thread_destroy");

    Ok(0)
}

fn invoke_thread_suspend(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::THREAD_SUSPEND) {
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    if !crate::sched::ThreadManager::suspend_thread(thread_id) {
        crate::telemetry::record_thread_suspend(false);
        return Err(Error::NotFound);
    }

    crate::telemetry::record_thread_suspend(true);
    Ok(0)
}

fn invoke_thread_resume(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::THREAD_SUSPEND) {
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    match crate::sched::ThreadManager::resume_thread(thread_id) {
        Some(_) => {
            crate::telemetry::record_thread_resume(true);
            Ok(0)
        }
        None => {
            crate::telemetry::record_thread_resume(false);
            Err(Error::InvalidState)
        }
    }
}

fn invoke_thread_set_priority(
    _token: &Token,
    _obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    klibcluu::warn("invoke_thread_set_priority not yet implemented");
    Err(Error::NotImplemented)
}

/// Set the fault handler endpoint for a thread
///
/// # Arguments
/// - token: Thread token (requires THREAD_CONTROL right)
/// - arg3: Endpoint token handle (or 0 to clear)
fn invoke_thread_set_fault_endpoint(
    token: &Token,
    obj_ref: ObjectRef,
    args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::THREAD_CONTROL) {
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let endpoint_id = if args.arg3 == 0 {
        None
    } else {
        let ep_handle = TokenHandle::from_raw(args.arg3);
        let (ep_token, ep_obj_ref) = lookup_token(ep_handle).map_err(|_| Error::InvalidArgument)?;
        if !ep_token.has_right(Rights::IPC_SEND) {
            return Err(Error::PermissionDenied);
        }
        let ep_ref = crate::token::check_object_type(ep_obj_ref, ObjectType::Endpoint)
            .map_err(|_| Error::InvalidArgument)?;
        if let ObjectRef::Endpoint(id) = ep_ref {
            Some(id)
        } else {
            return Err(Error::InvalidArgument);
        }
    };

    crate::sched::ThreadManager::with_thread_mut(thread_id, |thread| {
        thread.fault_endpoint = endpoint_id;
    })
    .ok_or(Error::NotFound)?;

    Ok(0)
}

fn invoke_thread_set_fs_base(
    token: &Token,
    obj_ref: ObjectRef,
    args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::THREAD_CONTROL) {
        return Err(Error::PermissionDenied);
    }

    // Resolve the target thread: accept a Thread token directly, or a Space
    // token as authorization to set FS base for the *current* thread running
    // in that space (avoids chicken-and-egg when TOKEN_SELF isn't a Thread).
    let thread_id = match crate::token::check_object_type(obj_ref, ObjectType::Thread) {
        Ok(ObjectRef::Thread(id)) => id,
        _ => {
            // Fall back: Space token → set FS base for current thread
            let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
                .map_err(|_| Error::InvalidArgument)?;
            let space_id = if let ObjectRef::Space(id) = space_ref {
                id
            } else {
                return Err(Error::InvalidArgument);
            };

            let current_id =
                crate::sched::ThreadManager::current().ok_or(Error::InvalidArgument)?;

            // Verify the calling thread actually belongs to this space
            let space_root =
                crate::mm::space_repository::with_space(space_id, |s| s.page_table_root)
                    .ok_or(Error::NotFound)?;
            let thread_root =
                crate::sched::ThreadManager::with_thread(current_id, |t| t.page_table_root)
                    .ok_or(Error::NotFound)?;

            if space_root != thread_root {
                return Err(Error::PermissionDenied);
            }

            current_id
        }
    };

    let fs_base = args.arg3 as u64;

    // Security: FS base must be in low userspace range to prevent
    // FS-relative reads from reaching kernel memory via large offsets.
    if fs_base >= 0x0000_7000_0000_0000 {
        return Err(Error::InvalidArgument);
    }

    let is_current = crate::sched::ThreadManager::current() == Some(thread_id);

    crate::sched::ThreadManager::with_thread_mut(thread_id, |thread| {
        thread.context.fs_base = fs_base;
    })
    .ok_or(Error::NotFound)?;

    // If setting for the currently running thread, write MSR immediately
    // so the new FS base takes effect without waiting for a context switch.
    if is_current {
        unsafe {
            x86_64::registers::model_specific::Msr::new(0xC000_0100).write(fs_base);
        }
    }

    Ok(0)
}

fn invoke_thread_get_id(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    Ok(thread_id.as_u64() as usize)
}

fn invoke_thread_get_stats(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::check_object_type(obj_ref, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let cpu_ticks = crate::sched::ThreadManager::with_thread(thread_id, |t| t.cpu_ticks_consumed)
        .unwrap_or(0);

    Ok(cpu_ticks as usize)
}

fn invoke_endpoint_create(token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{Issuer, ObjectRef, Rights, Timestamp};

    if !token.has_right(Rights::CREATE) {
        klibcluu::warn("invoke_endpoint_create: missing CREATE right");
        return Err(Error::PermissionDenied);
    }

    let endpoint_id = crate::ipc::endpoint::try_create_endpoint()?;
    let scope = crate::token::OpaqueScope::random();
    let endpoint_token = crate::token::create_token(
        scope,
        Rights::ipc_full() | Rights::GRANT,
        Issuer::Kernel,
        Timestamp::far_future(),
        ObjectRef::Endpoint(endpoint_id),
    );

    Ok(endpoint_token.as_usize())
}

// Space operations

fn invoke_space_create(token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::mm::{space_repository, AddressSpace};
    use crate::token::{Issuer, ObjectRef, OpaqueScope, Rights, Timestamp};

    klibcluu::trace("invoke_space_create");

    if !token.has_right(Rights::CREATE) {
        klibcluu::warn("invoke_space_create: insufficient rights");
        return Err(Error::PermissionDenied);
    }

    let new_space = AddressSpace::new_user().map_err(|e| {
        klibcluu::error("invoke_space_create: failed to create address space: ");
        klibcluu::error(e);
        Error::OutOfMemory
    })?;

    let space_id = space_repository::insert(new_space);

    klibcluu::trace("Created address space: id=");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", space_id.as_u64());

    let scope = OpaqueScope::random();
    let space_token = crate::token::create_token(
        scope,
        Rights::space_full()
            | Rights::CREATE
            | Rights::GRANT
            | Rights::THREAD_CONTROL
            | Rights::THREAD_SUSPEND
            | Rights::DESTROY,
        Issuer::Kernel,
        Timestamp::far_future(),
        ObjectRef::Space(space_id),
    );
    klibcluu::trace("invoke_space_create: minted space token=");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", space_token.as_usize() as u64);
    klibcluu::trace("invoke_space_create: minted role_bits=");
    klibcluu::log_hex(
        klibcluu::LogLevel::Trace,
        "",
        (Rights::space_full()
            | Rights::CREATE
            | Rights::GRANT
            | Rights::THREAD_CONTROL
            | Rights::THREAD_SUSPEND
            | Rights::DESTROY)
            .bits() as u64,
    );

    Ok(space_token.as_usize())
}

fn invoke_space_destroy(_token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !_token.has_right(Rights::DESTROY) {
        klibcluu::warn("invoke_space_destroy: missing DESTROY right");
        return Err(Error::PermissionDenied);
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // Get page_table_root while space is still in repo (for CR3 safety check)
    let pml4_phys = crate::mm::space_repository::with_space(space_id, |s| s.page_table_root)
        .ok_or(Error::NotFound)?;

    // Must not destroy the currently active address space
    let current_cr3 = x86_64::registers::control::Cr3::read().0.start_address();
    if pml4_phys == current_cr3 {
        return Err(Error::InvalidState);
    }

    // Remove from repository (if another thread raced us, we get None)
    let _space = crate::mm::space_repository::remove(space_id).ok_or(Error::NotFound)?;

    // Tear down all user page tables and frames
    unsafe {
        crate::mm::vmm::teardown_user_pages(pml4_phys);
    }

    crate::telemetry::log_resource_delta("space_destroy");
    Ok(0)
}

fn rollback_mapped_4kb(
    space_id: crate::token::scope::AddressSpaceId,
    virt_start: u64,
    mapped_pages: usize,
) {
    use crate::mm::{frame_registry, pmm, space_repository};
    use x86_64::VirtAddr;

    if mapped_pages == 0 {
        return;
    }

    let _ = space_repository::with_space_mut(space_id, |space| {
        let physmap_base = x86_64::VirtAddr::new(crate::mm::physmap::PHYS_MAP_BASE);
        let mut alloc = crate::mm::PmmPageAllocator;
        let mut vmm = unsafe {
            crate::mm::vmm::PageTableManager::for_page_table(
                physmap_base,
                space.page_table_root,
                &mut alloc,
            )
        };

        for i in 0..mapped_pages {
            let addr = VirtAddr::new(virt_start + (i as u64) * 0x1000);
            if let Ok(phys) = vmm.unmap(addr) {
                let phys_addr = phys.as_u64();
                if let Some(frame_id) = frame_registry::lookup_by_phys(phys_addr) {
                    frame_registry::dec_and_maybe_free(frame_id);
                } else {
                    pmm::free_frame(phys_addr);
                }
            }
        }
    });
}

fn invoke_space_map(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::elf;
    use crate::mm::{frame_registry, physmap, pmm, space_repository};
    use crate::syscall::userptr;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    const MAP_DEVICE: u32 = 0x100;
    const MAP_FRAME_TOKEN: u32 = 0x400;

    klibcluu::trace("invoke_space_map");

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_map: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    let virt_addr = args.arg3 as u64;
    let data_ptr = args.arg4;
    let perms = args.arg5 as u32;
    let copy_len = args.arg6;

    if virt_addr & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }

    let writable = (perms & 0x02) != 0;
    let executable = (perms & 0x04) != 0;
    let map_device = (perms & MAP_DEVICE) != 0;
    let map_frame_token = (perms & MAP_FRAME_TOKEN) != 0;

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // Track frame_id for frame-token path so error cleanup can dec_map_count directly
    // without re-looking up the token (which could fail if token was revoked concurrently).
    let mut mapped_frame_id: Option<crate::token::FrameId> = None;

    let frame_phys = if map_frame_token {
        // Frame token path: arg6 = frame token handle
        let frame_token_handle = crate::token::TokenHandle::from_raw(copy_len);
        let (frame_token, frame_obj_ref) =
            crate::token::lookup_token(frame_token_handle).map_err(|_| Error::InvalidArgument)?;
        if !frame_token.has_right(Rights::MAP) {
            return Err(Error::PermissionDenied);
        }
        let frame_ref = crate::token::check_object_type(frame_obj_ref, ObjectType::Frame)
            .map_err(|_| Error::InvalidArgument)?;
        let frame_id = if let ObjectRef::Frame(id) = frame_ref {
            id
        } else {
            return Err(Error::InvalidArgument);
        };
        let phys = frame_registry::get_phys(frame_id).ok_or(Error::NotFound)?;
        frame_registry::inc_map_count(frame_id);
        mapped_frame_id = Some(frame_id);
        phys
    } else if map_device {
        if copy_len != 0 {
            return Err(Error::InvalidArgument);
        }
        if data_ptr == 0 || (data_ptr & 0xFFF) != 0 {
            return Err(Error::InvalidArgument);
        }
        data_ptr as u64
    } else {
        if copy_len > PAGE_SIZE {
            return Err(Error::InvalidArgument);
        }
        pmm::alloc_frame().ok_or(Error::OutOfMemory)?
    };

    let frame_virt = if map_device || map_frame_token {
        core::ptr::null_mut()
    } else {
        unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 }
    };

    if !map_device && !map_frame_token {
        let caller_page_table_root =
            crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
        if copy_len > 0 {
            userptr::validate_user_buffer(data_ptr, copy_len)?;
            let copy_res = unsafe {
                userptr::copy_from_user(frame_virt, data_ptr, copy_len, caller_page_table_root)
            };
            if copy_res.is_err() {
                pmm::free_frame(frame_phys);
                return Err(Error::InvalidAddress);
            }
        }

        if copy_len < PAGE_SIZE {
            unsafe {
                write_bytes(frame_virt.add(copy_len), 0, PAGE_SIZE - copy_len);
            }
        }
    }

    let result = space_repository::with_space_mut(space_id, |space| unsafe {
        elf::map_user_page(
            virt_addr,
            frame_phys,
            writable,
            executable,
            space.page_table_root,
        )
    });

    match result {
        Some(Ok(())) => Ok(0),
        Some(Err(_)) => {
            if let Some(fid) = mapped_frame_id {
                frame_registry::dec_map_count(fid);
            } else if !map_device {
                pmm::free_frame(frame_phys);
            }
            Err(Error::OutOfMemory)
        }
        None => {
            if let Some(fid) = mapped_frame_id {
                frame_registry::dec_map_count(fid);
            } else if !map_device {
                pmm::free_frame(frame_phys);
            }
            Err(Error::NotFound)
        }
    }
}

fn invoke_space_unmap(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::mm::{frame_registry, pmm, space_repository};
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::SPACE_MAP) {
        return Err(Error::PermissionDenied);
    }

    let virt_addr = args.arg3 as u64;
    let num_pages = if args.arg4 == 0 { 1 } else { args.arg4 };

    if virt_addr & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let result = space_repository::with_space_mut(space_id, |space| {
        let physmap_base = x86_64::VirtAddr::new(crate::mm::physmap::PHYS_MAP_BASE);
        let mut alloc = crate::mm::PmmPageAllocator;
        let mut vmm = unsafe {
            crate::mm::vmm::PageTableManager::for_page_table(
                physmap_base,
                space.page_table_root,
                &mut alloc,
            )
        };

        let mut unmapped = 0u64;
        for i in 0..num_pages {
            let addr = x86_64::VirtAddr::new(virt_addr + (i as u64) * 0x1000);
            if let Ok(phys) = vmm.unmap(addr) {
                let phys_addr = phys.as_u64();
                // Check if this is a tracked frame (allocated via frame
                // capabilities or shared through a grant).
                if let Some(frame_id) = frame_registry::lookup_by_phys(phys_addr) {
                    frame_registry::dec_and_maybe_free(frame_id);
                } else {
                    pmm::free_frame(phys_addr);
                }
                unmapped += 1;
            }
        }
        unmapped
    });

    match result {
        Some(unmapped) => Ok(unmapped as usize),
        None => Err(Error::NotFound),
    }
}

fn invoke_space_protect(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::mm::{space_repository, PageFlags};
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::SPACE_MAP) {
        return Err(Error::PermissionDenied);
    }

    let virt_addr = args.arg3 as u64;
    let num_pages = if args.arg4 == 0 { 1 } else { args.arg4 };
    let perms = args.arg5 as u32;

    if virt_addr & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }
    if (perms & !(0x01 | 0x02 | 0x04)) != 0 {
        return Err(Error::InvalidArgument);
    }

    let writable = (perms & 0x02) != 0;
    let executable = (perms & 0x04) != 0;

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let result = space_repository::with_space_mut(space_id, |space| {
        let physmap_base = x86_64::VirtAddr::new(crate::mm::physmap::PHYS_MAP_BASE);
        let mut alloc = crate::mm::PmmPageAllocator;
        let mut vmm = unsafe {
            crate::mm::vmm::PageTableManager::for_page_table(
                physmap_base,
                space.page_table_root,
                &mut alloc,
            )
        };

        // Validate mapping presence first to avoid partial retagging.
        for i in 0..num_pages {
            let addr = x86_64::VirtAddr::new(virt_addr + (i as u64) * 0x1000);
            if vmm.translate(addr).is_none() {
                return Err(Error::NotFound);
            }
        }

        let flags = PageFlags {
            present: true,
            writable,
            user: true,
            write_through: false,
            cache_disabled: false,
            accessed: false,
            dirty: false,
            huge: false,
            global: false,
            no_execute: !executable,
        };

        for i in 0..num_pages {
            let addr = x86_64::VirtAddr::new(virt_addr + (i as u64) * 0x1000);
            if vmm.protect(addr, flags).is_err() {
                return Err(Error::InvalidAddress);
            }
        }
        Ok(num_pages)
    });

    match result {
        Some(Ok(changed)) => Ok(changed),
        Some(Err(err)) => Err(err),
        None => Err(Error::NotFound),
    }
}

/// Return mapped page counts for a user address space, classified into code,
/// heap, and stack regions.
///
/// Packed return value (usize = u64 on x86_64):
///   bits  0-15: code/data pages
///   bits 16-31: heap pages
///   bits 32-47: stack pages
fn invoke_space_get_stats(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::mm::space_repository;
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let result = space_repository::with_space(space_id, |space| {
        let heap_start = space.heap.start().as_u64();
        let stack_start = space.stack.start.as_u64();
        let stack_end = stack_start + space.stack.size as u64;
        unsafe {
            crate::mm::vmm::count_user_pages(
                space.page_table_root,
                heap_start,
                stack_start,
                stack_end,
            )
        }
    });

    match result {
        Some((code, heap, stack)) => {
            let packed = (code as usize) | ((heap as usize) << 16) | ((stack as usize) << 32);
            Ok(packed)
        }
        None => Err(Error::NotFound),
    }
}

fn resolve_space_for_futex(
    token: &Token,
    obj_ref: ObjectRef,
) -> Result<crate::token::scope::AddressSpaceId, Error> {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::SPACE_MAP) {
        return Err(Error::PermissionDenied);
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };
    Ok(space_id)
}

fn validate_futex_address(addr: usize) -> Result<(), Error> {
    if (addr & 0x3) != 0 {
        return Err(Error::InvalidArgument);
    }
    crate::syscall::userptr::validate_user_buffer(addr, core::mem::size_of::<u32>())
}

fn current_thread_matches_space(
    space_id: crate::token::scope::AddressSpaceId,
) -> Result<(), Error> {
    let current_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    let space_root =
        crate::mm::space_repository::with_space(space_id, |space| space.page_table_root)
            .ok_or(Error::NotFound)?;
    if current_root != space_root {
        return Err(Error::PermissionDenied);
    }
    Ok(())
}

fn read_user_u32(page_table_root: x86_64::PhysAddr, user_addr: usize) -> Result<u32, Error> {
    let mut value = 0u32;
    // Safety: destination is a kernel stack variable with exact size.
    unsafe {
        crate::syscall::userptr::copy_from_user(
            &mut value as *mut u32 as *mut u8,
            user_addr,
            core::mem::size_of::<u32>(),
            page_table_root,
        )?;
    }
    Ok(value)
}

fn invoke_futex_wait(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    let space_id = resolve_space_for_futex(token, obj_ref)?;
    let user_addr = args.arg3;
    let expected = args.arg4 as u32;
    let timeout_ms = (args.arg5 as u64) | ((args.arg6 as u64) << 32);

    validate_futex_address(user_addr)?;
    current_thread_matches_space(space_id)?;

    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;
    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;

    let current_value = read_user_u32(page_table_root, user_addr)?;
    if current_value != expected {
        return Err(Error::WouldBlock);
    }

    crate::sync::futex::enqueue_waiter(space_id, user_addr, current);

    if timeout_ms == 0 {
        crate::sched::ThreadManager::block_current();
    } else {
        let deadline = crate::sched::ThreadManager::ms_to_deadline(timeout_ms);
        crate::sched::ThreadManager::block_current_with_timeout(deadline);
    }
    crate::architecture::x86_64::syscall::request_resched();

    // NOTE: request_resched() only sets a flag — the actual context switch happens
    // on the syscall return path. Any code here runs BEFORE the thread yields,
    // so we must NOT remove ourselves from the futex queue. The wake() function
    // handles removal via pop_wake_candidates() when it wakes us.
    // For timeout wakeups, the stale queue entry is harmless: wake() checks
    // is_blocked() and requeues non-blocked threads.
    Ok(0)
}

fn invoke_futex_wake(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    let space_id = resolve_space_for_futex(token, obj_ref)?;
    let user_addr = args.arg3;
    let max_count = if args.arg4 == 0 { 1 } else { args.arg4 };

    validate_futex_address(user_addr)?;
    current_thread_matches_space(space_id)?;

    Ok(crate::sync::futex::wake(space_id, user_addr, max_count))
}

/// Grant a page from one address space to another (zero-copy sharing)
///
/// # Arguments
///
/// - token: Source address space token (must have SPACE_GRANT right)
/// - arg3: Target space token handle (must have SPACE_MAP right)
/// - arg4: Source virtual address (page-aligned, page to share)
/// - arg5: Target virtual address (page-aligned, where to map)
/// - arg6: Flags (0x02 = writable, 0x04 = executable)
///
/// # Returns
///
/// - Ok(0): Success
/// - Err(Error): Permission denied, invalid address, or mapping failure
fn invoke_space_grant(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::elf;
    use crate::mm::space::layout;
    use crate::mm::{frame_registry, space_repository, vmm::pte_flags};
    use crate::token::{lookup_token, ObjectRef, ObjectType, Rights, TokenHandle};
    use x86_64::VirtAddr;

    // Check source token has SPACE_GRANT right
    if !token.has_right(Rights::SPACE_GRANT) {
        klibcluu::warn("invoke_space_grant: missing SPACE_GRANT right on source token");
        return Err(Error::PermissionDenied);
    }

    let target_token_handle = TokenHandle::from_raw(args.arg3);
    let source_virt = args.arg4 as u64;
    let target_virt = args.arg5 as u64;
    let flags = args.arg6 as u32;
    const ALLOWED_FLAGS: u32 = 0x02 | 0x04;
    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    // Validate alignment
    if source_virt & 0xFFF != 0 || target_virt & 0xFFF != 0 {
        klibcluu::warn("invoke_space_grant: addresses not page-aligned");
        return Err(Error::InvalidArgument);
    }
    if (flags & !ALLOWED_FLAGS) != 0 {
        klibcluu::warn("invoke_space_grant: unsupported flags");
        return Err(Error::InvalidArgument);
    }
    if !(layout::USER_NULL_REGION_END..layout::USER_STACK_TOP).contains(&source_virt) {
        klibcluu::warn("invoke_space_grant: source not in user range");
        return Err(Error::InvalidArgument);
    }
    if !(layout::USER_NULL_REGION_END..layout::USER_STACK_TOP).contains(&target_virt) {
        klibcluu::warn("invoke_space_grant: target not in user range");
        return Err(Error::InvalidArgument);
    }

    // Resolve source address space
    let source_space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let source_space_id = if let ObjectRef::Space(id) = source_space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // Lookup and validate target token
    let (target_token, target_obj_ref) =
        lookup_token(target_token_handle).map_err(|_| Error::InvalidArgument)?;
    if !target_token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_grant: missing SPACE_MAP right on target token");
        return Err(Error::PermissionDenied);
    }

    // Resolve target address space
    let target_space_ref = crate::token::check_object_type(target_obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let target_space_id = if let ObjectRef::Space(id) = target_space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // Translate source virtual address to physical using source page tables.
    let source_mapping = space_repository::with_space(source_space_id, |source_space| {
        elf::translate_vaddr_with_flags(source_space.page_table_root, VirtAddr::new(source_virt))
    });

    let (phys_addr, pte_flags) = match source_mapping {
        Some(Some((phys, pte_flags))) => (phys.as_u64() & PHYS_MASK, pte_flags),
        Some(None) => {
            klibcluu::warn("invoke_space_grant: source page not mapped");
            return Err(Error::InvalidArgument);
        }
        None => {
            klibcluu::warn("invoke_space_grant: source address space not found");
            return Err(Error::NotFound);
        }
    };
    if (pte_flags & pte_flags::PRESENT) == 0 || (pte_flags & pte_flags::USER) == 0 {
        klibcluu::warn("invoke_space_grant: source page not user-accessible");
        return Err(Error::PermissionDenied);
    }

    let writable = (flags & 0x02) != 0;
    let executable = (flags & 0x04) != 0;
    let source_writable = (pte_flags & pte_flags::WRITABLE) != 0;
    let source_executable = (pte_flags & pte_flags::NO_EXECUTE) == 0;
    if writable && !source_writable {
        klibcluu::warn("invoke_space_grant: source page not writable");
        return Err(Error::PermissionDenied);
    }
    if executable && !source_executable {
        klibcluu::warn("invoke_space_grant: source page not executable");
        return Err(Error::PermissionDenied);
    }

    // If the target already has something mapped at `target_virt`, drop
    // its refcount first so we don't leak the prior frame. For grant-
    // tracked frames, the frame is freed to PMM when its last mapping
    // goes away. For untracked frames (e.g. an anonymous page from
    // `space_map_range`), we leak — they stay attributed to the owning
    // space and will be reclaimed at teardown.
    let prior_phys = space_repository::with_space(target_space_id, |target_space| {
        elf::translate_vaddr_with_flags(target_space.page_table_root, VirtAddr::new(target_virt))
    })
    .flatten()
    .map(|(phys, _)| phys.as_u64() & PHYS_MASK);
    if let Some(old_phys) = prior_phys {
        if old_phys != phys_addr {
            if let Some(old_id) = frame_registry::lookup_by_phys(old_phys) {
                frame_registry::dec_and_maybe_free(old_id);
            }
        }
    }

    // Map the physical page into target address space
    let result = space_repository::with_space_mut(target_space_id, |target_space| unsafe {
        elf::map_user_page(
            target_virt,
            phys_addr,
            writable,
            executable,
            target_space.page_table_root,
        )
    });

    match result {
        Some(Ok(())) => {
            // Re-mapping the *same* physical frame to the same target
            // address is a no-op refcount-wise (we already decremented a
            // stale mapping above, which in this case was this same
            // frame — compensate by not incrementing).
            if prior_phys != Some(phys_addr) {
                frame_registry::register_grant_mapping(phys_addr, target_space_id);
            }
            Ok(0)
        }
        Some(Err(_)) => {
            klibcluu::warn("invoke_space_grant: failed to map page in target");
            Err(Error::OutOfMemory)
        }
        None => {
            klibcluu::warn("invoke_space_grant: target address space not found");
            Err(Error::NotFound)
        }
    }
}

/// Batch map multiple pages into an address space
///
/// # Arguments
///
/// - arg3: virt_start (u64) - starting virtual address (must be page-aligned)
/// - arg4: data_ptr (usize) - pointer to source data buffer, or 0 for zero-fill
/// - arg5: flags (usize) - permission flags (same as SpaceMap)
/// - arg6: combined (usize) - upper 32 bits: num_pages, lower 32 bits: data_len
///
/// # Behavior
///
/// Maps `num_pages` consecutive 4KB pages starting at `virt_start`.
/// - If data_ptr is 0, all pages are zero-filled
/// - If data_ptr is non-zero, copies `data_len` bytes from data_ptr into the mapped pages
/// - Any bytes beyond data_len are zero-filled (for .bss sections)
///
/// Flag to request large page (2MB) mapping when possible
const MAP_LARGE_PAGES: u32 = 0x200;

/// Number of 4KB pages in a 2MB large page
const PAGES_PER_LARGE_PAGE: usize = 512;

/// Size of a large page (2MB)
const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

struct MapRange4kbRequest {
    space_id: crate::token::scope::AddressSpaceId,
    virt_start: u64,
    data_ptr: usize,
    data_len: usize,
    num_pages: usize,
    writable: bool,
    executable: bool,
    caller_page_table_root: x86_64::PhysAddr,
    fail_after_pages: Option<usize>,
    fail_on_map_stage: bool,
}

fn invoke_space_map_range(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use crate::syscall::userptr;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    const MAP_DEVICE: u32 = 0x100;
    const MAP_TEST_FAILPOINT: u32 = 0x8000_0000;
    const MAP_TEST_FAIL_ON_MAP_STAGE: u32 = 0x4000_0000;
    const MAP_TEST_FAIL_AFTER_SHIFT: u32 = 16;
    const MAP_TEST_FAIL_AFTER_MASK: u32 = 0x00FF_0000;

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_map_range: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    let virt_start = args.arg3 as u64;
    let data_ptr = args.arg4;
    let flags = args.arg5 as u32;
    let combined = args.arg6;
    let num_pages = combined >> 32;
    let data_len = combined & 0xFFFFFFFF;

    // Validate arguments
    if virt_start & 0xFFF != 0 {
        klibcluu::warn("invoke_space_map_range: virt_start not page-aligned");
        return Err(Error::InvalidArgument);
    }
    if num_pages == 0 {
        return Ok(0); // Nothing to do
    }
    if num_pages > 32768 {
        // Limit batch size to prevent excessive resource consumption (max 128MB)
        klibcluu::warn("invoke_space_map_range: num_pages too large");
        return Err(Error::InvalidArgument);
    }
    let total_size = num_pages * PAGE_SIZE;
    if data_len > total_size {
        klibcluu::warn("invoke_space_map_range: data_len exceeds total size");
        return Err(Error::InvalidArgument);
    }

    let writable = (flags & 0x02) != 0;
    let executable = (flags & 0x04) != 0;
    let use_large_pages = (flags & MAP_LARGE_PAGES) != 0;
    let map_device = (flags & MAP_DEVICE) != 0;
    let fail_after_pages = if (flags & MAP_TEST_FAILPOINT) != 0 {
        let raw = ((flags & MAP_TEST_FAIL_AFTER_MASK) >> MAP_TEST_FAIL_AFTER_SHIFT) as usize;
        Some(raw)
    } else {
        None
    };
    let fail_on_map_stage = (flags & MAP_TEST_FAIL_ON_MAP_STAGE) != 0;

    // For device mapping, data_ptr is a physical address base
    // For regular mapping, validate data buffer if provided
    if !map_device && data_ptr != 0 && data_len > 0 {
        userptr::validate_user_buffer(data_ptr, data_len)?;
    }
    let caller_page_table_root = if map_device {
        None
    } else {
        Some(crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?)
    };

    // Device mapping requires page-aligned physical address and no data copy
    if map_device {
        if data_ptr == 0 || (data_ptr & 0xFFF) != 0 {
            klibcluu::warn("invoke_space_map_range: device mapping requires aligned phys addr");
            return Err(Error::InvalidArgument);
        }
        if data_len != 0 {
            klibcluu::warn("invoke_space_map_range: device mapping cannot copy data");
            return Err(Error::InvalidArgument);
        }
    }

    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // Device mapping: map physical address range directly
    if map_device {
        return map_device_range(space_id, virt_start, data_ptr as u64, num_pages, writable);
    }

    // Check if we can use large pages:
    // - Flag is set
    // - Zero-fill only (no data to copy)
    // - Virtual start is 2MB aligned
    // - At least 512 pages (2MB) requested
    let can_use_large_pages = use_large_pages
        && data_ptr == 0
        && (virt_start & 0x1FFFFF) == 0
        && num_pages >= PAGES_PER_LARGE_PAGE;

    if can_use_large_pages {
        // Use large pages for the bulk of the mapping
        let num_large_pages = num_pages / PAGES_PER_LARGE_PAGE;
        let remaining_pages = num_pages % PAGES_PER_LARGE_PAGE;

        klibcluu::trace("Using large pages: ");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", num_large_pages as u64);

        // Map large pages
        for lp_idx in 0..num_large_pages {
            let virt_addr = virt_start + (lp_idx * LARGE_PAGE_SIZE) as u64;

            // Allocate 2MB-aligned physical memory
            let frame_phys = match pmm::alloc_large_frame() {
                Some(p) => p,
                None => {
                    // Fall back to regular pages if large frame allocation fails
                    klibcluu::warn("Large frame allocation failed, falling back to 4KB pages");
                    return map_range_4kb(MapRange4kbRequest {
                        space_id,
                        virt_start,
                        data_ptr,
                        data_len,
                        num_pages,
                        writable,
                        executable,
                        caller_page_table_root: caller_page_table_root
                            .ok_or(Error::InvalidState)?,
                        fail_after_pages,
                        fail_on_map_stage,
                    });
                }
            };

            // Zero the large frame via physmap
            let frame_virt = unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 };
            unsafe {
                write_bytes(frame_virt, 0, LARGE_PAGE_SIZE);
            }

            // Map the large page
            let result = space_repository::with_space_mut(space_id, |space| unsafe {
                elf::map_user_large_page(
                    virt_addr,
                    frame_phys,
                    writable,
                    executable,
                    space.page_table_root,
                )
            });

            match result {
                Some(Ok(())) => {}
                Some(Err(_)) => {
                    klibcluu::warn("invoke_space_map_range: map_user_large_page failed");
                    pmm::free_large_frame(frame_phys);
                    return Err(Error::OutOfMemory);
                }
                None => {
                    klibcluu::warn("invoke_space_map_range: space not found");
                    return Err(Error::NotFound);
                }
            }
        }

        // Map remaining pages with regular 4KB pages
        if remaining_pages > 0 {
            let remaining_start = virt_start + (num_large_pages * LARGE_PAGE_SIZE) as u64;
            map_remaining_4kb(
                space_id,
                remaining_start,
                remaining_pages,
                writable,
                executable,
            )?;
        }

        klibcluu::trace("invoke_space_map_range: mapped with large pages");
        Ok(num_pages)
    } else {
        // Use regular 4KB pages
        map_range_4kb(MapRange4kbRequest {
            space_id,
            virt_start,
            data_ptr,
            data_len,
            num_pages,
            writable,
            executable,
            caller_page_table_root: caller_page_table_root.ok_or(Error::InvalidState)?,
            fail_after_pages,
            fail_on_map_stage,
        })
    }
}

/// Map a range using 4KB pages (internal helper)
fn map_range_4kb(req: MapRange4kbRequest) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    let MapRange4kbRequest {
        space_id,
        virt_start,
        data_ptr,
        data_len,
        num_pages,
        writable,
        executable,
        caller_page_table_root,
        fail_after_pages,
        fail_on_map_stage,
    } = req;

    let mut bytes_copied = 0usize;
    let mut mapped_pages = 0usize;
    for page_idx in 0..num_pages {
        if !fail_on_map_stage && fail_after_pages.is_some_and(|limit| mapped_pages >= limit) {
            rollback_mapped_4kb(space_id, virt_start, mapped_pages);
            klibcluu::warn("map_range_4kb: injected failpoint triggered");
            return Err(Error::OutOfMemory);
        }

        let virt_addr = virt_start + (page_idx * PAGE_SIZE) as u64;

        // Allocate physical frame
        let frame_phys = match pmm::alloc_frame() {
            Some(frame) => frame,
            None => {
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::OutOfMemory);
            }
        };
        let frame_virt = unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 };

        // Copy data if available, zero-fill the rest
        if data_ptr != 0 && bytes_copied < data_len {
            let remaining_data = data_len - bytes_copied;
            let copy_len = remaining_data.min(PAGE_SIZE);

            let copy_res = unsafe {
                crate::syscall::userptr::copy_from_user(
                    frame_virt,
                    data_ptr + bytes_copied,
                    copy_len,
                    caller_page_table_root,
                )
            };
            if copy_res.is_err() {
                pmm::free_frame(frame_phys);
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::InvalidAddress);
            }
            bytes_copied += copy_len;

            // Zero-fill the rest of the page
            if copy_len < PAGE_SIZE {
                unsafe {
                    write_bytes(frame_virt.add(copy_len), 0, PAGE_SIZE - copy_len);
                }
            }
        } else {
            // Zero-fill entire page
            unsafe {
                write_bytes(frame_virt, 0, PAGE_SIZE);
            }
        }

        if fail_on_map_stage && fail_after_pages.is_some_and(|limit| mapped_pages >= limit) {
            pmm::free_frame(frame_phys);
            rollback_mapped_4kb(space_id, virt_start, mapped_pages);
            klibcluu::warn("map_range_4kb: injected map-stage failpoint triggered");
            return Err(Error::OutOfMemory);
        }

        // Map the page into the address space
        let result = space_repository::with_space_mut(space_id, |space| unsafe {
            elf::map_user_page(
                virt_addr,
                frame_phys,
                writable,
                executable,
                space.page_table_root,
            )
        });

        match result {
            Some(Ok(())) => {
                mapped_pages += 1;
            }
            Some(Err(_)) => {
                klibcluu::warn("map_range_4kb: map_user_page failed");
                pmm::free_frame(frame_phys);
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::OutOfMemory);
            }
            None => {
                klibcluu::warn("map_range_4kb: space not found");
                pmm::free_frame(frame_phys);
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::NotFound);
            }
        }
    }

    klibcluu::trace("map_range_4kb: mapped ");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", num_pages as u64);
    klibcluu::trace(" pages");

    Ok(num_pages)
}

/// Map remaining 4KB pages after large page mapping (zero-fill only)
fn map_remaining_4kb(
    space_id: crate::token::scope::AddressSpaceId,
    virt_start: u64,
    num_pages: usize,
    writable: bool,
    executable: bool,
) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    let mut mapped_pages = 0usize;
    for page_idx in 0..num_pages {
        let virt_addr = virt_start + (page_idx * PAGE_SIZE) as u64;

        let frame_phys = match pmm::alloc_frame() {
            Some(frame) => frame,
            None => {
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::OutOfMemory);
            }
        };
        let frame_virt = unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 };

        unsafe {
            write_bytes(frame_virt, 0, PAGE_SIZE);
        }

        let result = space_repository::with_space_mut(space_id, |space| unsafe {
            elf::map_user_page(
                virt_addr,
                frame_phys,
                writable,
                executable,
                space.page_table_root,
            )
        });

        match result {
            Some(Ok(())) => {
                mapped_pages += 1;
            }
            Some(Err(_)) => {
                pmm::free_frame(frame_phys);
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::OutOfMemory);
            }
            None => {
                pmm::free_frame(frame_phys);
                rollback_mapped_4kb(space_id, virt_start, mapped_pages);
                return Err(Error::NotFound);
            }
        }
    }

    Ok(num_pages)
}

/// Map a range of physical device memory (no frame allocation)
fn map_device_range(
    space_id: crate::token::scope::AddressSpaceId,
    virt_start: u64,
    phys_start: u64,
    num_pages: usize,
    writable: bool,
) -> SyscallResult {
    use crate::elf;
    use crate::mm::space_repository;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    for page_idx in 0..num_pages {
        let virt_addr = virt_start + (page_idx * PAGE_SIZE) as u64;
        let phys_addr = phys_start + (page_idx * PAGE_SIZE) as u64;

        let result = space_repository::with_space_mut(space_id, |space| unsafe {
            elf::map_device_page(virt_addr, phys_addr, writable, space.page_table_root)
        });

        match result {
            Some(Ok(())) => {}
            Some(Err(_)) => {
                klibcluu::warn("map_device_range: map_device_page failed");
                return Err(Error::OutOfMemory);
            }
            None => {
                klibcluu::warn("map_device_range: space not found");
                return Err(Error::NotFound);
            }
        }
    }

    Ok(num_pages)
}

// Token operations

fn invoke_token_derive(
    handle: TokenHandle,
    token: &Token,
    obj_ref: ObjectRef,
    args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{AuthorityId, Issuer, Rights, Timestamp};

    klibcluu::trace("invoke_token_derive");

    if !token.has_right(Rights::GRANT) {
        klibcluu::warn("invoke_token_derive: missing GRANT right");
        return Err(Error::PermissionDenied);
    }

    let new_rights = Rights::from_bits((args.arg3 & 0xffffffff) as u32);
    let expire = Timestamp::new(args.arg4 as u64);

    let issuer = Issuer::Authority(AuthorityId::new(handle.as_raw() as u64));

    let derived = crate::token::try_derive_token(token, new_rights, expire, issuer, obj_ref)
        .map_err(|_| Error::OutOfMemory)?;

    Ok(derived.as_usize())
}

fn invoke_token_revoke(
    handle: TokenHandle,
    _token: &Token,
    _obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    klibcluu::trace("invoke_token_revoke");

    crate::token::revoke_token(handle).map_err(|_| {
        klibcluu::warn("invoke_token_revoke: token not found");
        Error::InvalidArgument
    })?;

    Ok(0)
}

// IRQ operations

fn invoke_irq_attach(_token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights, TokenHandle};

    if !_token.has_right(Rights::IRQ_HANDLE) {
        klibcluu::warn("invoke_irq_attach: missing IRQ_HANDLE right");
        return Err(Error::PermissionDenied);
    }

    let endpoint_handle = TokenHandle::from_raw(_args.arg3);
    let irq_number = _args.arg4 as u8;

    let (_endpoint_token, ep_obj_ref) = lookup_token(endpoint_handle).map_err(|_| {
        klibcluu::warn("invoke_irq_attach: invalid endpoint token");
        Error::InvalidArgument
    })?;
    let endpoint_ref =
        crate::token::check_object_type(ep_obj_ref, ObjectType::Endpoint).map_err(|_| {
            klibcluu::warn("invoke_irq_attach: endpoint resolve failed");
            Error::InvalidArgument
        })?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    klibcluu::trace("invoke_irq_attach: irq=");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", irq_number as u64);
    klibcluu::trace("invoke_irq_attach: endpoint_id=");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint_id.as_u64());

    crate::devices::irq::attach(irq_number, endpoint_id)?;
    unsafe {
        crate::architecture::x86_64::pic::unmask(irq_number);
    }
    Ok(0)
}

fn invoke_irq_ack(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::IRQ_ACK) {
        klibcluu::warn("invoke_irq_ack: missing IRQ_ACK right");
        return Err(Error::PermissionDenied);
    }

    let irq_ref = crate::token::check_object_type(obj_ref, ObjectType::Irq).map_err(|_| {
        klibcluu::warn("invoke_irq_ack: failed to resolve IRQ object");
        Error::InvalidArgument
    })?;

    let irq_number = if let ObjectRef::Irq(n) = irq_ref {
        n
    } else {
        return Err(Error::InvalidArgument);
    };

    if irq_number >= 16 {
        klibcluu::warn("invoke_irq_ack: IRQ number out of range");
        return Err(Error::InvalidArgument);
    }

    // Send EOI: APIC first if enabled, then PIC
    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    }
    unsafe {
        crate::architecture::x86_64::pic::send_eoi(irq_number as u8);
    }

    Ok(0)
}

// PCI operations

/// Read from PCI configuration space
///
/// # Arguments
/// - arg3: bus (8 bits)
/// - arg4: device (5 bits) | function (3 bits) packed as (device << 3) | function
/// - arg5: offset (register offset, must be 4-byte aligned)
///
/// # Returns
/// - Ok(value): 32-bit value read from PCI config space
fn invoke_pci_config_read(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_pci_config_read: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let bus = args.arg3 as u8;
    let devfn = args.arg4 as u8;
    let offset = args.arg5 as u8;

    // Offset must be 4-byte aligned
    if offset & 0x03 != 0 {
        return Err(Error::InvalidArgument);
    }

    let value = pci_config_read_u32(bus, devfn >> 3, devfn & 0x07, offset);
    Ok(value as usize)
}

/// Write to PCI configuration space
///
/// # Arguments
/// - arg3: bus (8 bits)
/// - arg4: device (5 bits) | function (3 bits) packed as (device << 3) | function
/// - arg5: offset (register offset, must be 4-byte aligned)
/// - arg6: value to write (32 bits)
///
/// # Returns
/// - Ok(0): Write successful
fn invoke_pci_config_write(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_pci_config_write: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let bus = args.arg3 as u8;
    let devfn = args.arg4 as u8;
    let offset = args.arg5 as u8;
    let value = args.arg6 as u32;

    // Offset must be 4-byte aligned
    if offset & 0x03 != 0 {
        return Err(Error::InvalidArgument);
    }

    pci_config_write_u32(bus, devfn >> 3, devfn & 0x07, offset, value);
    Ok(0)
}

/// Read 32-bit value from PCI configuration space using I/O ports 0xCF8/0xCFC
fn pci_config_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    use x86_64::instructions::port::Port;

    let address: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    unsafe {
        let mut config_address: Port<u32> = Port::new(0xCF8);
        let mut config_data: Port<u32> = Port::new(0xCFC);

        config_address.write(address);
        config_data.read()
    }
}

/// Write 32-bit value to PCI configuration space using I/O ports 0xCF8/0xCFC
fn pci_config_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    use x86_64::instructions::port::Port;

    let address: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    unsafe {
        let mut config_address: Port<u32> = Port::new(0xCF8);
        let mut config_data: Port<u32> = Port::new(0xCFC);

        config_address.write(address);
        config_data.write(value);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// I/O Port Operations
// ═══════════════════════════════════════════════════════════════════════════

/// Read 8-bit value from I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
///
/// # Returns
/// - Ok(value): 8-bit value read from port
fn invoke_port_in8(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_in8: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u8> =
            x86_64::instructions::port::Port::new(port);
        Ok(p.read() as usize)
    }
}

/// Read 16-bit value from I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
///
/// # Returns
/// - Ok(value): 16-bit value read from port
fn invoke_port_in16(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_in16: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u16> =
            x86_64::instructions::port::Port::new(port);
        Ok(p.read() as usize)
    }
}

/// Read 32-bit value from I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
///
/// # Returns
/// - Ok(value): 32-bit value read from port
fn invoke_port_in32(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_in32: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u32> =
            x86_64::instructions::port::Port::new(port);
        Ok(p.read() as usize)
    }
}

/// Write 8-bit value to I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
/// - arg4: value to write (8 bits)
///
/// # Returns
/// - Ok(0): Write successful
fn invoke_port_out8(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_out8: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;
    let value = args.arg4 as u8;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u8> =
            x86_64::instructions::port::Port::new(port);
        p.write(value);
    }
    Ok(0)
}

/// Write 16-bit value to I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
/// - arg4: value to write (16 bits)
///
/// # Returns
/// - Ok(0): Write successful
fn invoke_port_out16(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_out16: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;
    let value = args.arg4 as u16;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u16> =
            x86_64::instructions::port::Port::new(port);
        p.write(value);
    }
    Ok(0)
}

/// Write 32-bit value to I/O port
///
/// # Arguments
/// - arg3: port address (16 bits)
/// - arg4: value to write (32 bits)
///
/// # Returns
/// - Ok(0): Write successful
fn invoke_port_out32(token: &Token, _obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::PCI_ACCESS) {
        klibcluu::warn("invoke_port_out32: missing PCI_ACCESS right");
        return Err(Error::PermissionDenied);
    }

    let port = args.arg3 as u16;
    let value = args.arg4 as u32;

    unsafe {
        let mut p: x86_64::instructions::port::Port<u32> =
            x86_64::instructions::port::Port::new(port);
        p.write(value);
    }
    Ok(0)
}

/// Translate virtual address to physical address for DMA
///
/// # Arguments
/// - token: Space token (requires SPACE_MAP right)
/// - arg3: Virtual address to translate
///
/// # Returns
/// - Ok(phys_addr): Physical address, or 0 if not mapped
fn invoke_virt_to_phys(token: &Token, obj_ref: ObjectRef, args: SyscallArgs) -> SyscallResult {
    use crate::elf::translate_vaddr;
    use crate::mm::space_repository;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use x86_64::VirtAddr;

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_virt_to_phys: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    // Get the space ID from the token
    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        klibcluu::warn("invoke_virt_to_phys: token not a space");
        return Err(Error::InvalidArgument);
    };

    let virt_addr = VirtAddr::new(args.arg3 as u64);

    // Get the page table root and translate
    let phys_addr = space_repository::with_space(space_id, |space| {
        translate_vaddr(space.page_table_root, virt_addr)
    });

    match phys_addr {
        Some(Some(phys)) => Ok(phys.as_u64() as usize),
        _ => Ok(0), // Not mapped, return 0
    }
}

fn invoke_pmm_alloc_large(token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::mm::pmm;
    use crate::token::Rights;

    // Reuse an existing right that already gates “memory management” operations.
    // SPACE_MAP is appropriate because this is only useful for later mapping.
    if !token.has_right(Rights::SPACE_MAP) {
        return Err(Error::PermissionDenied);
    }

    // 2MB contiguous, 2MB-aligned physical block
    let phys = pmm::alloc_large_frame().ok_or(Error::OutOfMemory)?;
    Ok(phys as usize)
}

fn invoke_clock_now(token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    let tsc = crate::architecture::x86_64::tsc::rdtsc();
    Ok(tsc as usize)
}

fn invoke_clock_frequency(token: &Token, _obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::token::Rights;

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    Ok(crate::architecture::x86_64::tsc::tsc_hz() as usize)
}

// ═══════════════════════════════════════════════════════════════════════════
// Frame Operations
// ═══════════════════════════════════════════════════════════════════════════

/// Allocate a physical frame and return a frame token.
///
/// # Arguments
/// - token: Space token (requires CREATE right, used to identify owner)
/// - arg3: unused
///
/// # Returns
/// - Ok(frame_token_handle): Token handle for the new frame
///   The physical address is returned packed: upper 32 bits in arg4 style
///   Actually we return frame_token in rax and caller can use FrameGetPhys.
fn invoke_frame_allocate(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::mm::frame_registry;
    use crate::token::{
        create_token, scope::OpaqueScope, Issuer, ObjectRef, ObjectType, Rights, Timestamp,
    };

    if !token.has_right(Rights::CREATE) {
        return Err(Error::PermissionDenied);
    }

    // Resolve space to get owner
    let space_ref = crate::token::check_object_type(obj_ref, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;
    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let (frame_id, _phys) = frame_registry::alloc_frame(space_id).ok_or(Error::OutOfMemory)?;

    // Create a frame token with all rights
    let obj_ref = ObjectRef::Frame(frame_id);
    let scope = OpaqueScope::from_object_ref(&obj_ref, frame_id.as_u64());
    let frame_token = create_token(
        scope,
        Rights::all(),
        Issuer::Kernel,
        Timestamp::NEVER,
        obj_ref,
    );

    Ok(frame_token.as_usize())
}

/// Free a tracked frame. Requires DESTROY right and map_count == 0.
///
/// # Arguments
/// - token: Frame token (requires DESTROY right)
fn invoke_frame_free(
    token_handle: TokenHandle,
    token: &Token,
    obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    use crate::mm::frame_registry;
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::DESTROY) {
        return Err(Error::PermissionDenied);
    }

    let frame_ref = crate::token::check_object_type(obj_ref, ObjectType::Frame)
        .map_err(|_| Error::InvalidArgument)?;
    let frame_id = if let ObjectRef::Frame(id) = frame_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    frame_registry::free_frame(frame_id).map_err(|_| Error::InvalidState)?;

    // Revoke the frame token
    let _ = crate::token::revoke_token(token_handle);

    Ok(0)
}

/// Get the physical address of a frame.
///
/// # Arguments
/// - token: Frame token (requires READ right)
///
/// # Returns
/// - Ok(phys_addr)
fn invoke_frame_get_phys(token: &Token, obj_ref: ObjectRef, _args: SyscallArgs) -> SyscallResult {
    use crate::mm::frame_registry;
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        return Err(Error::PermissionDenied);
    }

    let frame_ref = crate::token::check_object_type(obj_ref, ObjectType::Frame)
        .map_err(|_| Error::InvalidArgument)?;
    let frame_id = if let ObjectRef::Frame(id) = frame_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let phys = frame_registry::get_phys(frame_id).ok_or(Error::NotFound)?;
    Ok(phys as usize)
}

// ═══════════════════════════════════════════════════════════════════════════
// Notification operations
// ═══════════════════════════════════════════════════════════════════════════

fn invoke_notification_create(
    token: &Token,
    _obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{Issuer, ObjectRef, Rights, Timestamp};

    if !token.has_right(Rights::CREATE) {
        klibcluu::warn("invoke_notification_create: missing CREATE right");
        return Err(Error::PermissionDenied);
    }

    let notif_id = crate::ipc::notification::try_create_notification()?;
    let scope = crate::token::OpaqueScope::random();
    let notif_token = crate::token::create_token(
        scope,
        Rights::READ | Rights::WRITE | Rights::GRANT | Rights::CREATE,
        Issuer::Kernel,
        Timestamp::far_future(),
        ObjectRef::Notification(notif_id),
    );

    Ok(notif_token.as_usize())
}

fn invoke_notification_signal(
    token: &Token,
    obj_ref: ObjectRef,
    args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::WRITE) {
        klibcluu::warn("invoke_notification_signal: missing WRITE right");
        return Err(Error::PermissionDenied);
    }

    let notif_ref = crate::token::check_object_type(obj_ref, ObjectType::Notification)
        .map_err(|_| Error::InvalidArgument)?;
    let notif_id = if let ObjectRef::Notification(id) = notif_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let bits = args.arg3 as u64;
    crate::ipc::notification::signal(notif_id, bits)?;
    Ok(0)
}

fn invoke_notification_wait(
    token: &Token,
    obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    use crate::sched::ThreadManager;
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        klibcluu::warn("invoke_notification_wait: missing READ right");
        return Err(Error::PermissionDenied);
    }

    let notif_ref = crate::token::check_object_type(obj_ref, ObjectType::Notification)
        .map_err(|_| Error::InvalidArgument)?;
    let notif_id = if let ObjectRef::Notification(id) = notif_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let caller = ThreadManager::current().ok_or(Error::NotFound)?;

    let (bits, was_pending) = crate::ipc::notification::try_wait(notif_id, caller)?;
    if was_pending {
        return Ok(bits as usize);
    }

    // Not pending — caller is now registered as waiter. Set thread state and block.
    ThreadManager::with_thread_mut(caller, |t| {
        t.notification_wait = Some(notif_id);
    });
    ThreadManager::block_current();

    // After waking: read consumed word
    let consumed = crate::ipc::notification::read_consumed(notif_id).unwrap_or(0);
    Ok(consumed as usize)
}

fn invoke_notification_poll(
    token: &Token,
    obj_ref: ObjectRef,
    _args: SyscallArgs,
) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !token.has_right(Rights::READ) {
        klibcluu::warn("invoke_notification_poll: missing READ right");
        return Err(Error::PermissionDenied);
    }

    let notif_ref = crate::token::check_object_type(obj_ref, ObjectType::Notification)
        .map_err(|_| Error::InvalidArgument)?;
    let notif_id = if let ObjectRef::Notification(id) = notif_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let pending = crate::ipc::notification::poll(notif_id)?;
    Ok(pending as usize)
}

// ═══════════════════════════════════════════════════════════════════════════
// Debug Syscall
// ═══════════════════════════════════════════════════════════════════════════

/// sys_debug_print - Print debug message
///
/// # Arguments
///
/// - arg1: msg_ptr (*const u8)
/// - arg2: msg_len (usize)
/// - arg3-arg6: unused
///
/// # Returns
///
/// - Ok(0): Success
/// - Err(Error): Invalid pointer
#[cfg(debug_assertions)]
pub fn sys_debug_print(args: SyscallArgs) -> SyscallResult {
    use crate::syscall::userptr;

    let msg_ptr = args.arg1;
    let msg_len = args.arg2;

    // klibcluu::trace("sys_debug_print: ptr=0x");
    // klibcluu::log_hex(klibcluu::LogLevel::Trace, "", msg_ptr as u64);
    // klibcluu::trace(" len=");
    // klibcluu::log_dec(klibcluu::LogLevel::Trace, "", msg_len as u64);

    if msg_len == 0 {
        klibcluu::info("[USER] <empty debug print>");
        return Ok(0);
    }

    if msg_len > userptr::MAX_DEBUG_PRINT_SIZE {
        klibcluu::warn("sys_debug_print: message too long");
        return Err(Error::InvalidParameter);
    }

    // Validate user buffer
    userptr::validate_user_buffer(msg_ptr, msg_len)?;

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidArgument)?;
    userptr::ensure_pages_mapped(msg_ptr, msg_len, page_table_root)?;

    let mut buf = [0u8; userptr::MAX_DEBUG_PRINT_SIZE];
    unsafe {
        userptr::copy_from_user(buf.as_mut_ptr(), msg_ptr, msg_len, page_table_root)?;
    }
    let msg_slice = &buf[..msg_len];

    // Convert to string (best effort)
    if let Ok(msg) = core::str::from_utf8(msg_slice) {
        use alloc::format;

        klibcluu::info(&format!("[USER] {}", msg));
    } else {
        klibcluu::info("[USER] <invalid UTF-8>");
    }

    Ok(0)
}

#[cfg(not(debug_assertions))]
pub fn sys_debug_print(_args: SyscallArgs) -> SyscallResult {
    // Disabled in release builds.
    Ok(0)
}
