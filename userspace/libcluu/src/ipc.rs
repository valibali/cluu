//! IPC helpers
//!
//! Higher-level IPC wrappers using the Message type.

use crate::boot::process_info;
use crate::error::Result;
use crate::syscall;
use crate::types::*;
use crate::Error;
use alloc::vec::Vec;

pub const PROC_EXIT_LABEL: u32 = 1;
pub const CONSOLE_WRITE_LABEL: u32 = 1;
pub const CONSOLE_CLEAR_LABEL: u32 = 2;
pub const CONSOLE_CURSOR_LABEL: u32 = 3;
pub const CONSOLE_BLINK_LABEL: u32 = 4;
pub const KBD_EVENT_LABEL: u32 = 1;
pub const TTY_READ_LABEL: u32 = 1;
pub const TTY_WRITE_LABEL: u32 = 2;
pub const TTY_CTL_LABEL: u32 = 3;
pub const TTY_REGISTER_LABEL: u32 = 4;

/// Send a message (one-way)
pub fn send(endpoint_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Convert Message to bytes and call syscall::ipc_send
    let msg_bytes = msg.as_bytes();
    syscall::ipc_send(endpoint_token, msg_bytes)
}

/// Send a message with an inline payload appended after the Message header.
pub fn send_with_payload(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload.len();
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_send(endpoint_token, &buffer)
}

/// Call (send + wait for reply) with an inline payload appended after the Message header.
pub fn call_with_payload(
    endpoint_token: usize,
    msg: &Message,
    payload: &[u8],
    reply: &mut Message,
) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    let reply_bytes = reply.as_bytes_mut();
    let _ = syscall::ipc_call(endpoint_token, &buffer, reply_bytes)?;
    Ok(())
}

/// Receive a message
pub fn recv(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes_mut();
    loop {
        match syscall::ipc_recv(endpoint_token, msg_bytes) {
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => {
                let _ = syscall::yield_cpu();
            }
            Err(err) => return Err(err),
        }
    }
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
///
/// # Arguments
///
/// - `endpoint_token`: The endpoint token we received the call on
/// - `msg`: Reply message to send
/// - `_flags`: IPC flags (currently unused)
pub fn reply(endpoint_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Send reply using syscall::ipc_reply
    let msg_bytes = msg.as_bytes();
    syscall::ipc_reply(endpoint_token, msg_bytes)?;
    Ok(())
}

/// Reply and receive next message (server loop optimization)
///
/// Note: This is currently implemented as reply() + recv() separately.
/// In the future, this could be a single optimized syscall.
pub fn reply_recv(endpoint_token: usize, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    // Send reply first
    reply(endpoint_token, msg, flags)?;
    // Then receive next message
    recv(endpoint_token, msg, flags)
}

/// Notify the parent process manager that this process is exiting.
pub fn notify_exit(exit_code: i32) -> Result<()> {
    let info = process_info();
    if info.exit_token == 0 {
        // No parent to notify (e.g., init process)
        return Ok(());
    }

    let msg = Message::new(
        PROC_EXIT_LABEL,
        [info.exit_cookie, exit_code as usize, 0, 0, 0, 0],
        2,
    );
    send(info.exit_token, &msg, IpcFlags::empty())
}
