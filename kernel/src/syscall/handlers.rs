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
    let msg_ptr = args.arg2 as usize;
    let msg_len = args.arg3;

    let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_SEND) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref = crate::token::resolve_token_object(&token, ObjectType::Endpoint)
        .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    crate::ipc::endpoint::send_from_user(endpoint_id, msg_ptr, msg_len, page_table_root)?;
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
///
/// # Returns
///
/// - Ok((index << 32) | bytes_received): index of endpoint that had message, and length
/// - Err(Error::Timeout): Timeout expired before message arrived
/// - Err(Error): Token invalid, insufficient rights, or IPC error
pub fn sys_recv(args: SyscallArgs) -> SyscallResult {
    use crate::token::{EndpointId, ObjectRef, ObjectType, Rights};

    let tokens_ptr = args.arg1 as usize;
    let tokens_count = args.arg2;
    let buf_ptr = args.arg3 as usize;
    let buf_len = args.arg4;
    let timeout_ms = args.arg5 as u64;

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
    for i in 0..tokens_count {
        let token_addr = tokens_ptr + i * core::mem::size_of::<usize>();
        crate::syscall::userptr::validate_user_buffer(token_addr, core::mem::size_of::<usize>())?;

        let mut token_raw: usize = 0;
        crate::syscall::userptr::copy_from_user(
            &mut token_raw as *mut usize as *mut u8,
            token_addr,
            core::mem::size_of::<usize>(),
            page_table_root,
        )?;

        let token_handle = TokenHandle::from_raw(token_raw);
        let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
        if !token.has_right(Rights::IPC_RECV) {
            return Err(Error::PermissionDenied);
        }

        let endpoint_ref = crate::token::resolve_token_object(&token, ObjectType::Endpoint)
            .map_err(|_| Error::InvalidArgument)?;
        if let ObjectRef::Endpoint(id) = endpoint_ref {
            endpoint_ids[i] = Some(id);
        } else {
            return Err(Error::InvalidArgument);
        }
    }

    // Try to receive from each endpoint in order
    let try_recv_any = || -> Result<(usize, usize), Error> {
        for i in 0..tokens_count {
            if let Some(endpoint_id) = endpoint_ids[i] {
                match crate::ipc::endpoint::recv_to_user_nonblocking(
                    endpoint_id,
                    buf_ptr,
                    buf_len,
                    page_table_root,
                ) {
                    Ok(len) => return Ok((i, len)),
                    Err(Error::WouldBlock) => continue,
                    Err(err) => return Err(err),
                }
            }
        }
        Err(Error::WouldBlock)
    };

    // First attempt
    match try_recv_any() {
        Ok((index, len)) => return Ok((index << 32) | len),
        Err(Error::WouldBlock) if nonblocking => return Err(Error::WouldBlock),
        Err(Error::WouldBlock) => { /* fall through to blocking */ }
        Err(err) => return Err(err),
    }

    // Register as waiter on all endpoints
    for i in 0..tokens_count {
        if let Some(endpoint_id) = endpoint_ids[i] {
            // Try recv which will register us as a waiter if no message
            let _ = crate::ipc::endpoint::recv_to_user(
                endpoint_id, buf_ptr, buf_len, page_table_root, current
            );
        }
    }

    // Block with or without timeout
    if timeout_ms == u64::MAX {
        // Block forever
        crate::sched::ThreadManager::block_current();
    } else {
        // Block with timeout
        let deadline = crate::sched::ThreadManager::ms_to_deadline(timeout_ms);
        crate::sched::ThreadManager::block_current_with_timeout(deadline);
    }
    crate::architecture::x86_64::syscall::request_resched();

    // After waking, check if it was due to timeout
    if crate::sched::ThreadManager::check_and_clear_timeout_wake() {
        Err(Error::Timeout)
    } else {
        Err(Error::WouldBlock) // Message arrived on one endpoint, retry will succeed
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
    let msg_ptr = args.arg2 as usize;
    let msg_len = args.arg3;
    let reply_buf = args.arg4 as usize;
    let reply_len = args.arg5;

    // 1. Validate token and check IPC_CALL right
    let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_CALL) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref = crate::token::resolve_token_object(&token, ObjectType::Endpoint)
        .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;

    // 2. Store reply buffer info in current thread before sending
    crate::sched::ThreadManager::with_thread_mut(current, |thread| {
        thread.call_reply_info = Some(CallReplyInfo {
            reply_buf_ptr: reply_buf,
            reply_buf_len: reply_len,
            page_table_root,
        });
    });

    // 3. Send call message (includes our thread ID for reply routing)
    crate::ipc::endpoint::call_from_user(endpoint_id, msg_ptr, msg_len, page_table_root, current)?;

    // 4. Block waiting for reply
    crate::sched::ThreadManager::block_current();
    crate::architecture::x86_64::syscall::request_resched();

    // 5. When we wake up, the reply has been copied to our buffer by reply handler
    // Check if reply was actually delivered (reply_info was consumed)
    let reply_delivered = crate::sched::ThreadManager::with_thread(current, |thread| {
        thread.call_reply_info.is_none()
    })
    .unwrap_or(false);

    if !reply_delivered {
        // Woken without a reply (e.g., timeout or error) - clean up
        crate::sched::ThreadManager::with_thread_mut(current, |thread| {
            thread.call_reply_info = None;
        });
        return Err(Error::InvalidState);
    }

    // 6. Return success - the actual byte count was set by reply handler
    // For now, return reply_len as we don't track actual bytes written
    // TODO: Store actual reply length in thread
    Ok(reply_len)
}

/// sys_reply - Reply to IPC sender
///
/// # Arguments
///
/// - arg1: endpoint_token (TokenHandle) - endpoint we received the call on
/// - arg2: msg_ptr (*const u8) - reply message
/// - arg3: msg_len (usize) - reply length
/// - arg4-arg6: unused
///
/// # Returns
///
/// - Ok(bytes_sent): Number of bytes in reply
/// - Err(Error): No pending call or IPC error
pub fn sys_reply(args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    let token_handle = TokenHandle::from_raw(args.arg1);
    let msg_ptr = args.arg2 as usize;
    let msg_len = args.arg3;

    // 1. Validate token and check IPC_REPLY right (same as IPC_RECV for now)
    let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_RECV) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref = crate::token::resolve_token_object(&token, ObjectType::Endpoint)
        .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    // 2. Get the current caller for this endpoint
    let caller = crate::ipc::endpoint::take_current_caller(endpoint_id)?;

    // 3. Get page table root for copying from userspace
    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;

    // 4. Deliver reply to caller (copies to caller's buffer and wakes them)
    let bytes_sent =
        crate::ipc::endpoint::reply_from_user(caller, msg_ptr, msg_len, page_table_root)?;

    Ok(bytes_sent)
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
    let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;

    //klibcluu::trace("sys_invoke: operation = ");
    //klibcluu::log_dec(klibcluu::LogLevel::Trace, "", args.arg2 as u64);

    // Dispatch based on operation
    match operation {
        // Thread operations
        InvokeOp::ThreadCreate => invoke_thread_create(&token, args),
        InvokeOp::ThreadDestroy => invoke_thread_destroy(&token, args),
        InvokeOp::ThreadSuspend => invoke_thread_suspend(&token, args),
        InvokeOp::ThreadResume => invoke_thread_resume(&token, args),
        InvokeOp::ThreadSetPriority => invoke_thread_set_priority(&token, args),

        // Space operations
        InvokeOp::SpaceCreate => invoke_space_create(&token, args),
        InvokeOp::SpaceDestroy => invoke_space_destroy(&token, args),
        InvokeOp::SpaceMap => invoke_space_map(&token, args),
        InvokeOp::SpaceUnmap => invoke_space_unmap(&token, args),
        InvokeOp::SpaceGrant => invoke_space_grant(&token, args),
        InvokeOp::SpaceMapRange => invoke_space_map_range(&token, args),

        // Token operations
        InvokeOp::TokenDerive => invoke_token_derive(token_handle, &token, args),
        InvokeOp::TokenRevoke => invoke_token_revoke(token_handle, &token, args),

        // IRQ operations
        InvokeOp::IrqAttach => invoke_irq_attach(&token, args),
        InvokeOp::IrqAck => invoke_irq_ack(&token, args),

        // IPC operations
        InvokeOp::EndpointCreate => invoke_endpoint_create(&token, args),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Invoke Operation Handlers
// ═══════════════════════════════════════════════════════════════════════════

use crate::token::Token;

// Thread operations

fn invoke_thread_create(token: &Token, args: SyscallArgs) -> SyscallResult {
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

    if entry == 0 || stack == 0 {
        return Err(Error::InvalidArgument);
    }

    let space_ref = crate::token::resolve_token_object(token, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let page_table_root =
        crate::mm::space_repository::with_space(space_id, |space| space.page_table_root)
            .ok_or(Error::NotFound)?;

    let thread_id = ThreadManager::alloc_thread_id();
    let flags = if ThreadManager::is_init_mode() {
        ThreadFlags::COOPERATIVE
    } else {
        ThreadFlags::empty()
    };

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

fn invoke_thread_destroy(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    if !_token.has_right(Rights::DESTROY) {
        klibcluu::warn("invoke_thread_destroy: missing DESTROY right");
        return Err(Error::PermissionDenied);
    }

    let thread_ref = crate::token::resolve_token_object(_token, ObjectType::Thread)
        .map_err(|_| Error::InvalidArgument)?;
    let thread_id = if let ObjectRef::Thread(id) = thread_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    crate::sched::ThreadManager::with_thread_mut(thread_id, |thread| {
        thread.make_dead();
    });

    Ok(0)
}

fn invoke_thread_suspend(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_thread_suspend not yet implemented");
    Err(Error::NotImplemented)
}

fn invoke_thread_resume(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_thread_resume not yet implemented");
    Err(Error::NotImplemented)
}

fn invoke_thread_set_priority(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_thread_set_priority not yet implemented");
    Err(Error::NotImplemented)
}

fn invoke_endpoint_create(token: &Token, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{Issuer, ObjectRef, Rights, Timestamp};

    if !token.has_right(Rights::CREATE) {
        klibcluu::warn("invoke_endpoint_create: missing CREATE right");
        return Err(Error::PermissionDenied);
    }

    let endpoint_id = crate::ipc::endpoint::create_endpoint();
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

fn invoke_space_create(token: &Token, _args: SyscallArgs) -> SyscallResult {
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

    Ok(space_token.as_usize())
}

fn invoke_space_destroy(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_space_destroy not yet implemented");
    Err(Error::NotImplemented)
}

fn invoke_space_map(token: &Token, args: SyscallArgs) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use crate::syscall::userptr;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use core::ptr::{copy_nonoverlapping, write_bytes};
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    const MAP_DEVICE: u32 = 0x100;

    klibcluu::trace("invoke_space_map");

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_map: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    let virt_addr = args.arg3 as u64;
    let data_ptr = args.arg4 as usize;
    let perms = args.arg5 as u32;
    let copy_len = args.arg6 as usize;

    if copy_len > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }

    if virt_addr & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }

    let writable = (perms & 0x02) != 0;
    let executable = (perms & 0x04) != 0;
    let map_device = (perms & MAP_DEVICE) != 0;

    let space_ref = crate::token::resolve_token_object(token, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let frame_phys = if map_device {
        if copy_len != 0 {
            return Err(Error::InvalidArgument);
        }
        if data_ptr == 0 || (data_ptr & 0xFFF) != 0 {
            return Err(Error::InvalidArgument);
        }
        data_ptr as u64
    } else {
        pmm::alloc_frame().ok_or(Error::OutOfMemory)?
    };
    let frame_virt = if map_device {
        core::ptr::null_mut()
    } else {
        unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 }
    };

    if !map_device {
        if copy_len > 0 {
            userptr::validate_user_buffer(data_ptr, copy_len)?;
            unsafe {
                copy_nonoverlapping(data_ptr as *const u8, frame_virt, copy_len);
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
        Some(Err(_)) => Err(Error::OutOfMemory),
        None => Err(Error::NotFound),
    }
}

fn invoke_space_unmap(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_space_unmap not yet implemented");
    Err(Error::NotImplemented)
}

fn invoke_space_grant(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_space_grant not yet implemented");
    Err(Error::NotImplemented)
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
/// Flag to request large page (2MB) mapping when possible
const MAP_LARGE_PAGES: u32 = 0x200;

/// Number of 4KB pages in a 2MB large page
const PAGES_PER_LARGE_PAGE: usize = 512;

/// Size of a large page (2MB)
const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

fn invoke_space_map_range(token: &Token, args: SyscallArgs) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use crate::syscall::userptr;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use core::ptr::write_bytes;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    const MAP_DEVICE: u32 = 0x100;

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_map_range: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    let virt_start = args.arg3 as u64;
    let data_ptr = args.arg4 as usize;
    let flags = args.arg5 as u32;
    let combined = args.arg6;
    let num_pages = (combined >> 32) as usize;
    let data_len = (combined & 0xFFFFFFFF) as usize;

    // Validate arguments
    if virt_start & 0xFFF != 0 {
        klibcluu::warn("invoke_space_map_range: virt_start not page-aligned");
        return Err(Error::InvalidArgument);
    }
    if num_pages == 0 {
        return Ok(0); // Nothing to do
    }
    if num_pages > 16384 {
        // Limit batch size to prevent excessive resource consumption (max 64MB)
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

    // For device mapping, data_ptr is a physical address base
    // For regular mapping, validate data buffer if provided
    if !map_device && data_ptr != 0 && data_len > 0 {
        userptr::validate_user_buffer(data_ptr, data_len)?;
    }

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

    let space_ref = crate::token::resolve_token_object(token, ObjectType::Space)
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
                    return map_range_4kb(
                        space_id,
                        virt_start,
                        data_ptr,
                        data_len,
                        num_pages,
                        writable,
                        executable,
                    );
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
        map_range_4kb(
            space_id,
            virt_start,
            data_ptr,
            data_len,
            num_pages,
            writable,
            executable,
        )
    }
}

/// Map a range using 4KB pages (internal helper)
fn map_range_4kb(
    space_id: crate::token::scope::AddressSpaceId,
    virt_start: u64,
    data_ptr: usize,
    data_len: usize,
    num_pages: usize,
    writable: bool,
    executable: bool,
) -> SyscallResult {
    use crate::elf;
    use crate::mm::{physmap, pmm, space_repository};
    use core::ptr::{copy_nonoverlapping, write_bytes};
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    let mut bytes_copied = 0usize;
    for page_idx in 0..num_pages {
        let virt_addr = virt_start + (page_idx * PAGE_SIZE) as u64;

        // Allocate physical frame
        let frame_phys = pmm::alloc_frame().ok_or(Error::OutOfMemory)?;
        let frame_virt = unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 };

        // Copy data if available, zero-fill the rest
        if data_ptr != 0 && bytes_copied < data_len {
            let remaining_data = data_len - bytes_copied;
            let copy_len = remaining_data.min(PAGE_SIZE);

            unsafe {
                copy_nonoverlapping((data_ptr + bytes_copied) as *const u8, frame_virt, copy_len);
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

        // Map the page into the address space
        let result = space_repository::with_space_mut(space_id, |space| unsafe {
            elf::map_user_page(virt_addr, frame_phys, writable, executable, space.page_table_root)
        });

        match result {
            Some(Ok(())) => {}
            Some(Err(_)) => {
                klibcluu::warn("map_range_4kb: map_user_page failed");
                return Err(Error::OutOfMemory);
            }
            None => {
                klibcluu::warn("map_range_4kb: space not found");
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

    for page_idx in 0..num_pages {
        let virt_addr = virt_start + (page_idx * PAGE_SIZE) as u64;

        let frame_phys = pmm::alloc_frame().ok_or(Error::OutOfMemory)?;
        let frame_virt = unsafe { physmap::phys_to_virt_u64(frame_phys) as *mut u8 };

        unsafe {
            write_bytes(frame_virt, 0, PAGE_SIZE);
        }

        let result = space_repository::with_space_mut(space_id, |space| unsafe {
            elf::map_user_page(virt_addr, frame_phys, writable, executable, space.page_table_root)
        });

        match result {
            Some(Ok(())) => {}
            Some(Err(_)) => return Err(Error::OutOfMemory),
            None => return Err(Error::NotFound),
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
            elf::map_user_page(
                virt_addr,
                phys_addr,
                writable,
                false, // device memory not executable
                space.page_table_root,
            )
        });

        match result {
            Some(Ok(())) => {}
            Some(Err(_)) => {
                klibcluu::warn("map_device_range: map_user_page failed");
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

fn invoke_token_derive(handle: TokenHandle, token: &Token, args: SyscallArgs) -> SyscallResult {
    use crate::token::{AuthorityId, Issuer, Rights, Timestamp};

    klibcluu::trace("invoke_token_derive");

    if !token.has_right(Rights::GRANT) {
        klibcluu::warn("invoke_token_derive: missing GRANT right");
        return Err(Error::PermissionDenied);
    }

    let new_rights = Rights::from_bits((args.arg3 & 0xffffffff) as u32);
    let expire = Timestamp::new(args.arg4 as u64);

    let issuer = Issuer::Authority(AuthorityId::new(handle.as_raw() as u64));
    let object_ref = crate::token::resolve_scope(&token.scope).ok_or(Error::InvalidArgument)?;

    let derived = crate::token::derive_token(token, new_rights, expire, issuer, object_ref)
        .ok_or(Error::InvalidArgument)?;

    Ok(derived.as_usize())
}

fn invoke_token_revoke(handle: TokenHandle, _token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::trace("invoke_token_revoke");

    crate::token::revoke_token(handle).map_err(|_| {
        klibcluu::warn("invoke_token_revoke: token not found");
        Error::InvalidArgument
    })?;

    Ok(0)
}

// IRQ operations

fn invoke_irq_attach(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights, TokenHandle};

    if !_token.has_right(Rights::IRQ_HANDLE) {
        klibcluu::warn("invoke_irq_attach: missing IRQ_HANDLE right");
        return Err(Error::PermissionDenied);
    }

    let endpoint_handle = TokenHandle::from_raw(_args.arg3);
    let irq_number = _args.arg4 as u8;

    let endpoint_token = lookup_token(endpoint_handle).map_err(|_| {
        klibcluu::warn("invoke_irq_attach: invalid endpoint token");
        Error::InvalidArgument
    })?;
    let endpoint_ref = crate::token::resolve_token_object(&endpoint_token, ObjectType::Endpoint)
        .map_err(|_| {
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

fn invoke_irq_ack(_token: &Token, _args: SyscallArgs) -> SyscallResult {
    klibcluu::warn("invoke_irq_ack not yet implemented");
    Err(Error::NotImplemented)
}

// ═══════════════════════════════════════════════════════════════════════════
// Debug Syscall
// ═══════════════════════════════════════════════════════════════════════════

/// sys_debug_print - Print debug message
///
/// Only available in debug builds.
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

    let msg_slice = unsafe { core::slice::from_raw_parts(msg_ptr as *const u8, msg_len) };

    // Convert to string (best effort)
    if let Ok(msg) = core::str::from_utf8(msg_slice) {
        klibcluu::info("[USER] ");
        klibcluu::info(msg);
    } else {
        klibcluu::info("[USER] <invalid UTF-8>");
    }

    Ok(0)
}

#[cfg(not(debug_assertions))]
pub fn sys_debug_print(_args: SyscallArgs) -> SyscallResult {
    // Debug print disabled in release builds
    Err(Error::NotImplemented)
}
