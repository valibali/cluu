//! IPC helpers

use crate::types::*;
use crate::error::Result;
use crate::syscall::{syscall4, SyscallNumber};

/// Send a message (one-way)
pub fn send(cap: u8, msg: &Message, flags: IpcFlags) -> Result<()> {
    unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Send as usize,
            cap as usize,
            msg as *const Message as usize,
            flags.bits() as usize,
        )?
    };
    Ok(())
}

/// Receive a message
pub fn recv(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Recv as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )?
    };
    Ok(())
}

/// Call (send + wait for reply)
pub fn call(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Call as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )?
    };
    Ok(())
}

/// Reply to a received message
pub fn reply(cap: u8, msg: &Message, flags: IpcFlags) -> Result<()> {
    unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::Reply as usize,
            cap as usize,
            msg as *const Message as usize,
            flags.bits() as usize,
        )?
    };
    Ok(())
}

/// Reply and receive next message (server loop optimization)
pub fn reply_recv(cap: u8, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    unsafe {
        syscall4(
            SyscallNumber::Ipc,
            IpcOp::ReplyRecv as usize,
            cap as usize,
            msg as *mut Message as usize,
            flags.bits() as usize,
        )?
    };
    Ok(())
}
