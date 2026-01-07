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
    let _token_handle = TokenHandle::from_raw(args.arg1);
    let _msg_ptr = args.arg2 as *const u8;
    let _msg_len = args.arg3;

    // TODO: Implement IPC send
    // 1. Validate token and check IPC_SEND right
    // 2. Validate user pointer
    // 3. Copy message from userspace
    // 4. Send to endpoint

    klibcluu::warn("sys_send not yet implemented");
    Err(Error::NotImplemented)
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
    let _token_handle = TokenHandle::from_raw(args.arg1);
    let _buf_ptr = args.arg2 as *mut u8;
    let _buf_len = args.arg3;

    // TODO: Implement IPC receive
    // 1. Validate token and check IPC_RECV right
    // 2. Validate user pointer
    // 3. Block until message available
    // 4. Copy message to userspace
    // 5. Return bytes received

    klibcluu::warn("sys_recv not yet implemented");
    Err(Error::NotImplemented)
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
    klibcluu::trace("sys_yield");

    // Note: Context switch happens in syscall_entry.asm after this returns
    // The syscall_entry calls schedule_and_switch() which will:
    // 1. Get current thread ID (from CURRENT_THREAD)
    // 2. Save current thread's context
    // 3. Add current thread back to scheduler
    // 4. Pick next thread and return its context

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

    klibcluu::trace("sys_invoke: operation = ");
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", args.arg2 as u64);

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
    let thread = Thread::new(
        thread_id,
        page_table_root,
        VirtAddr::new(entry),
        VirtAddr::new(stack),
        Priority::new(priority),
        ThreadFlags::empty(),
    );

    let thread_id = ThreadManager::add_thread(thread);

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
    klibcluu::warn("invoke_thread_destroy not yet implemented");
    Err(Error::NotImplemented)
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
        Rights::space_full() | Rights::CREATE,
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
    use crate::mm::space_repository;
    use crate::token::{ObjectRef, ObjectType, Rights};
    use x86_64::VirtAddr;

    klibcluu::trace("invoke_space_map");

    if !token.has_right(Rights::SPACE_MAP) {
        klibcluu::warn("invoke_space_map: missing SPACE_MAP right");
        return Err(Error::PermissionDenied);
    }

    let virt_addr = args.arg3 as u64;
    let phys_addr = args.arg4 as u64;
    let perms = args.arg5 as u32;

    if virt_addr & 0xFFF != 0 || phys_addr & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }

    let writable = (perms & 0x02) != 0;
    let executable = (perms & 0x04) != 0;

    let space_ref = crate::token::resolve_token_object(token, ObjectType::Space)
        .map_err(|_| Error::InvalidArgument)?;

    let space_id = if let ObjectRef::Space(id) = space_ref {
        id
    } else {
        return Err(Error::InvalidArgument);
    };

    let result = space_repository::with_space_mut(space_id, |space| unsafe {
        elf::map_user_page(
            VirtAddr::new(virt_addr),
            phys_addr,
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
    klibcluu::warn("invoke_irq_attach not yet implemented");
    Err(Error::NotImplemented)
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

    // Validate user buffer
    userptr::validate_user_buffer(msg_ptr, msg_len)?;

    let msg_ptr = msg_ptr as *const u8;

    // Safety: Pointer validated above
    let msg_slice = unsafe { core::slice::from_raw_parts(msg_ptr, msg_len) };

    // Convert to string (best effort)
    if let Ok(msg) = core::str::from_utf8(msg_slice) {
        klibcluu::debug("[USER] ");
        klibcluu::debug(msg);
    } else {
        klibcluu::debug("[USER] <invalid UTF-8>");
    }

    Ok(0)
}

#[cfg(not(debug_assertions))]
pub fn sys_debug_print(_args: SyscallArgs) -> SyscallResult {
    // Debug print disabled in release builds
    Err(Error::NotImplemented)
}
