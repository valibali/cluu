//! IPC helpers
//!
//! Higher-level IPC wrappers using the Message type.

use crate::error::Result;
use crate::syscall;
use crate::types::*;
use crate::boot;

pub const PROC_EXIT_LABEL: u32 = 1;

/// Send a message (one-way)
pub fn send(endpoint_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Convert Message to bytes and call syscall::ipc_send
    let msg_bytes = msg.as_bytes();
    syscall::ipc_send(endpoint_token, msg_bytes)
}

/// Receive a message
pub fn recv(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    // Get message buffer and call syscall::ipc_recv
    let msg_bytes = msg.as_bytes_mut();
    let _bytes_received = syscall::ipc_recv(endpoint_token, msg_bytes)?;
    Ok(())
}

/// Call (send + wait for reply)
pub fn call(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    // We need to send msg and receive reply into the same buffer
    // Make a temporary copy to send, then receive into the original
    let msg_copy = msg.clone();
    let send_bytes = msg_copy.as_bytes();
    let reply_bytes = msg.as_bytes_mut();
    let _bytes_received = syscall::ipc_call(endpoint_token, send_bytes, reply_bytes)?;
    Ok(())
}

/// Reply to a received message
pub fn reply(msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Send reply using syscall::ipc_reply
    let msg_bytes = msg.as_bytes();
    syscall::ipc_reply(msg_bytes)
}

/// Reply and receive next message (server loop optimization)
///
/// Note: This is currently implemented as reply() + recv() separately.
/// In the future, this could be a single optimized syscall.
pub fn reply_recv(endpoint_token: usize, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    // Send reply first
    reply(msg, flags)?;
    // Then receive next message
    recv(endpoint_token, msg, flags)
}

/// Notify the parent process manager that this process is exiting.
pub fn notify_exit(exit_code: i32) -> Result<()> {
    let info = boot::parent_info();
    if info.exit_endpoint == 0 {
        return Ok(());
    }

    let msg = Message::new(
        PROC_EXIT_LABEL,
        [info.exit_cookie, exit_code as usize, 0, 0, 0, 0],
        2,
    );
    let _ = crate::syscall::debug_print("TRACE: notify_exit");
    send(info.exit_endpoint, &msg, IpcFlags::empty())
}
