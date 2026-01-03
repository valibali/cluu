//! IPC helpers

use crate::types::*;
use crate::syscall::{syscall4, SyscallNumber};

/// Send a message (one-way)
pub fn send(cap: u8, msg: &Message, flags: IpcFlags) -> Result<()> {
    let ret = unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Send as usize,
            cap as usize,
            msg as *const Message as usize,
            flags.bits() as usize,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}

/// Receive a message
pub fn recv(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    let ret = unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Recv as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}

/// Call (send + wait for reply)
pub fn call(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    let ret = unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Call as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}

/// Reply to a received message
pub fn reply(cap: u8, msg: &Message, flags: IpcFlags) -> Result<()> {
    let ret = unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Reply as usize,
            cap as usize,
            msg as *const Message as usize,
            flags.bits() as usize,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}

/// Reply and receive next message (server loop optimization)
pub fn reply_recv(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    let ret = unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::ReplyRecv as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )
    };
    from_syscall_ret(ret).map(|_| ())
}
