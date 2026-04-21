//! System Call Interface - Token-Based Model
//!
//! This module defines the minimal syscall interface for the CLUU microkernel.
//!
//! # Design Philosophy
//!
//! **7 syscalls total** - Minimal attack surface:
//! - IPC: Send, Recv, Call, Reply (4 syscalls)
//! - Scheduling: Yield (1 syscall)
//! - Operations: Invoke (1 syscall - replaces all object operations)
//! - Debug: DebugPrint (1 syscall)
//!
//! All object operations (thread create, space map, etc.) go through
//! `Invoke` with token-based authorization.
//!
//! # Syscall Convention
//!
//! On x86_64, syscalls use registers for parameters:
//! - RAX: Syscall number
//! - RDI: Argument 1 (often token handle)
//! - RSI: Argument 2
//! - RDX: Argument 3
//! - R10: Argument 4
//! - R8:  Argument 5
//! - R9:  Argument 6
//! - RAX: Return value (or negative errno on error)
//!
//! # Security
//!
//! All syscalls must:
//! 1. Validate token handles and check expiration/signature
//! 2. Verify token rights before operations
//! 3. Check user pointers are in userspace range
//! 4. Return errors instead of panicking
//!
//! # Example
//!
//! ```rust,no_run
//! // Create address space (via Invoke)
//! let space_token = syscall!(Invoke, root_token, OP_SPACE_CREATE, 0, 0, 0, 0)?;
//!
//! // Map page (via Invoke)
//! syscall!(Invoke, space_token, OP_SPACE_MAP, virt, phys, flags, 0)?;
//!
//! // IPC send
//! syscall!(Send, endpoint_token, msg_ptr, msg_len, 0, 0, 0)?;
//! ```

mod handlers;
pub(crate) use handlers::{sys_call, sys_recv, sys_reply, sys_send};
pub mod userptr;

use crate::error::Error;

/// Syscall Numbers - Minimal Set
///
/// Only 7 syscalls for minimal attack surface and clear semantics.
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
    ///
    /// This is the generic operation syscall. What happens depends on:
    /// - The token's scope (which object)
    /// - The token's rights (what operations allowed)
    /// - The operation parameter (which operation to perform)
    Invoke = 5,

    /// Debug print (only in debug builds)
    DebugPrint = 255,
}

impl SyscallNumber {
    pub fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::Send),
            1 => Some(Self::Recv),
            2 => Some(Self::Call),
            3 => Some(Self::Reply),
            4 => Some(Self::Yield),
            5 => Some(Self::Invoke),
            255 => Some(Self::DebugPrint),
            _ => None,
        }
    }

    pub const fn as_usize(self) -> usize {
        self as usize
    }
}

/// Syscall Arguments
#[derive(Debug, Clone, Copy)]
pub struct SyscallArgs {
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
    pub arg4: usize,
    pub arg5: usize,
    pub arg6: usize,
}

impl SyscallArgs {
    pub const fn new(
        arg1: usize,
        arg2: usize,
        arg3: usize,
        arg4: usize,
        arg5: usize,
        arg6: usize,
    ) -> Self {
        Self {
            arg1,
            arg2,
            arg3,
            arg4,
            arg5,
            arg6,
        }
    }

    pub const fn empty() -> Self {
        Self::new(0, 0, 0, 0, 0, 0)
    }
}

pub type SyscallResult = Result<usize, Error>;

/// Dispatch syscall to appropriate handler
///
/// # Arguments
///
/// * `number` - Syscall number (validated by caller)
/// * `args` - Syscall arguments from registers
///
/// # Returns
///
/// * `Ok(usize)` - Success, return value for RAX
/// * `Err(Error)` - Error, will be converted to negative errno
pub fn dispatch_syscall(number: SyscallNumber, args: SyscallArgs) -> SyscallResult {
    use handlers::*;

    match number {
        // IPC syscalls
        SyscallNumber::Send => sys_send(args),
        SyscallNumber::Recv => sys_recv(args),
        SyscallNumber::Call => sys_call(args),
        SyscallNumber::Reply => sys_reply(args),

        // Scheduling
        SyscallNumber::Yield => sys_yield(args),

        // Generic operations
        SyscallNumber::Invoke => sys_invoke(args),

        // Debug
        SyscallNumber::DebugPrint => sys_debug_print(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_number_conversion() {
        assert_eq!(SyscallNumber::from_usize(0), Some(SyscallNumber::Send));
        assert_eq!(SyscallNumber::from_usize(1), Some(SyscallNumber::Recv));
        assert_eq!(SyscallNumber::from_usize(4), Some(SyscallNumber::Yield));
        assert_eq!(SyscallNumber::from_usize(5), Some(SyscallNumber::Invoke));
        assert_eq!(
            SyscallNumber::from_usize(255),
            Some(SyscallNumber::DebugPrint)
        );
        assert_eq!(SyscallNumber::from_usize(256), None);
    }

    #[test]
    fn test_syscall_number_as_usize() {
        assert_eq!(SyscallNumber::Send.as_usize(), 0);
        assert_eq!(SyscallNumber::Recv.as_usize(), 1);
        assert_eq!(SyscallNumber::Yield.as_usize(), 4);
        assert_eq!(SyscallNumber::Invoke.as_usize(), 5);
        assert_eq!(SyscallNumber::DebugPrint.as_usize(), 255);
    }

    #[test]
    fn test_syscall_args() {
        let args = SyscallArgs::new(1, 2, 3, 4, 5, 6);
        assert_eq!(args.arg1, 1);
        assert_eq!(args.arg2, 2);
        assert_eq!(args.arg3, 3);
    }

    #[test]
    fn test_syscall_args_empty() {
        let args = SyscallArgs::empty();
        assert_eq!(args.arg1, 0);
        assert_eq!(args.arg6, 0);
    }
}
