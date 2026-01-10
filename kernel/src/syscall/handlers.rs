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

/// sys_recv - Receive IPC message from endpoint
///
/// # Arguments
///
/// - arg1: endpoint_token (TokenHandle)
/// - arg2: buf_ptr (*mut u8)
/// - arg3: buf_len (usize)
/// - arg4-arg6: unused
///
/// # Returns
///
/// - Ok(bytes_received): Number of bytes received
/// - Err(Error): Token invalid, insufficient rights, or IPC error
pub fn sys_recv(args: SyscallArgs) -> SyscallResult {
    use crate::token::{ObjectRef, ObjectType, Rights};

    let token_handle = TokenHandle::from_raw(args.arg1);
    let buf_ptr = args.arg2 as usize;
    const NONBLOCK_FLAG: usize = 1usize << (usize::BITS - 1);
    let buf_len_arg = args.arg3;
    let nonblocking = buf_len_arg & NONBLOCK_FLAG != 0;
    let buf_len = buf_len_arg & !NONBLOCK_FLAG;

    let token = lookup_token(token_handle).map_err(|_| Error::InvalidArgument)?;
    if !token.has_right(Rights::IPC_RECV) {
        return Err(Error::PermissionDenied);
    }

    let endpoint_ref =
        crate::token::resolve_token_object(&token, ObjectType::Endpoint)
            .map_err(|_| Error::InvalidArgument)?;
    let endpoint_id = if let ObjectRef::Endpoint(id) = endpoint_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let page_table_root =
        crate::sched::ThreadManager::current_page_table_root().ok_or(Error::InvalidState)?;
    let current = crate::sched::ThreadManager::current().ok_or(Error::InvalidState)?;

    let recv_result = if nonblocking {
        crate::ipc::endpoint::recv_to_user_nonblocking(
            endpoint_id,
            buf_ptr,
            buf_len,
            page_table_root,
        )
    } else {
        crate::ipc::endpoint::recv_to_user(endpoint_id, buf_ptr, buf_len, page_table_root, current)
    };

    match recv_result {
        Ok(len) => Ok(len),
        Err(err @ Error::WouldBlock) => {
            if nonblocking {
                Err(err)
            } else {
                crate::sched::ThreadManager::block_current();
                crate::architecture::x86_64::syscall::request_resched();
                Err(err)
            }
        }
        Err(err) => Err(err),
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
    let _token_handle = TokenHandle::from_raw(args.arg1);
    let _msg_ptr = args.arg2 as *const u8;
    let _msg_len = args.arg3;
    let _reply_buf = args.arg4 as *mut u8;
    let _reply_len = args.arg5;

    // TODO: Implement IPC call
    // 1. Validate token and check IPC_CALL right
    // 2. Validate user pointers
    // 3. Send message
    // 4. Block for reply
    // 5. Copy reply to userspace
    // 6. Return bytes received

    klibcluu::warn("sys_call not yet implemented");
    Err(Error::NotImplemented)
}

/// sys_reply - Reply to IPC sender
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
/// - Err(Error): No pending call or IPC error
pub fn sys_reply(args: SyscallArgs) -> SyscallResult {
    let _msg_ptr = args.arg1 as *const u8;
    let _msg_len = args.arg2;

    // TODO: Implement IPC reply
    // 1. Check current thread has pending call
    // 2. Validate user pointer
    // 3. Copy reply message
    // 4. Unblock caller

    klibcluu::warn("sys_reply not yet implemented");
    Err(Error::NotImplemented)
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
    use crate::mm::{physmap, pmm_simple, space_repository};
    use crate::syscall::userptr;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use core::ptr::{copy_nonoverlapping, write_bytes};

    const PAGE_SIZE: usize = 4096;
    const MAP_DEVICE: u32 = 0x100;

    //klibcluu::trace("invoke_space_map");

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
        pmm_simple::alloc_frame().ok_or(Error::OutOfMemory)?
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
