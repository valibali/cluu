//! System Call Interface
//!
//! This module defines the system call interface for the CLUU microkernel.
//! Syscalls are the primary mechanism for userspace to interact with the kernel.
//!
//! # Syscall Convention
//!
//! On x86_64, syscalls use registers for parameters:
//! - RAX: Syscall number
//! - RDI: Argument 1
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
//! 1. Validate capability handles before use
//! 2. Check user pointers are in userspace range
//! 3. Verify rights before operations
//! 4. Return errors instead of panicking

mod handlers;
pub mod userptr;

use crate::error::Error;

/// Syscall Numbers
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

impl SyscallNumber {
    pub fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::Ipc),
            1 => Some(Self::Yield),
            2 => Some(Self::ThreadCreate),
            3 => Some(Self::ThreadDestroy),
            4 => Some(Self::SpaceCreate),
            5 => Some(Self::SpaceDestroy),
            6 => Some(Self::Grant),
            7 => Some(Self::Map),
            8 => Some(Self::Unmap),
            9 => Some(Self::TokenCreate),
            10 => Some(Self::TokenDelete),
            11 => Some(Self::IrqAttach),
            12 => Some(Self::IrqAck),
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
        Self { arg1, arg2, arg3, arg4, arg5, arg6 }
    }

    pub const fn empty() -> Self {
        Self::new(0, 0, 0, 0, 0, 0)
    }
}

pub type SyscallResult = Result<usize, Error>;

pub fn dispatch_syscall(number: SyscallNumber, args: SyscallArgs) -> SyscallResult {
    use handlers::*;

    match number {
        SyscallNumber::Ipc => sys_ipc(args),
        SyscallNumber::Yield => sys_yield(args),
        SyscallNumber::ThreadCreate => sys_thread_create(args),
        SyscallNumber::ThreadDestroy => sys_thread_destroy(args),
        SyscallNumber::SpaceCreate => sys_space_create(args),
        SyscallNumber::SpaceDestroy => sys_space_destroy(args),
        SyscallNumber::Grant => sys_grant(args),
        SyscallNumber::Map => sys_map(args),
        SyscallNumber::Unmap => sys_unmap(args),
        SyscallNumber::TokenCreate => sys_token_create(args),
        SyscallNumber::TokenDelete => sys_token_delete(args),
        SyscallNumber::IrqAttach => sys_irq_attach(args),
        SyscallNumber::IrqAck => sys_irq_ack(args),
        SyscallNumber::DebugPrint => sys_debug_print(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_syscall_number_conversion() {
        assert_eq!(SyscallNumber::from_usize(0), Some(SyscallNumber::Ipc));
        assert_eq!(SyscallNumber::from_usize(1), Some(SyscallNumber::Yield));
        assert_eq!(SyscallNumber::from_usize(255), Some(SyscallNumber::DebugPrint));
        assert_eq!(SyscallNumber::from_usize(256), None);
    }

    #[test]
    fn test_syscall_number_as_usize() {
        assert_eq!(SyscallNumber::Ipc.as_usize(), 0);
        assert_eq!(SyscallNumber::Yield.as_usize(), 1);
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
