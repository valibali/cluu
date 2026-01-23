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
pub const CONSOLE_WRITE_SYNC_LABEL: u32 = 5;
pub const IPC_CHUNK_BYTES_DEFAULT: usize = 256;
pub const IPC_SEND_RETRIES_DEFAULT: u32 = 256;
pub const IPC_BACKOFF_MAX_DEFAULT: u32 = 64;
pub const KBD_EVENT_LABEL: u32 = 1;
pub const TTY_READ_LABEL: u32 = 1;
pub const TTY_WRITE_LABEL: u32 = 2;
pub const TTY_CTL_LABEL: u32 = 3;
pub const TTY_REGISTER_LABEL: u32 = 4;
pub const TTY_WRITE_SYNC_LABEL: u32 = 5;
pub const TTY_CTL_SYNC: u32 = 1;
pub const CALL_COOKIE_TAG: u8 = 1;
pub const CALL_COOKIE_WORD: usize = 5;

/// Tag indicating the message contains a reply token (new system)
pub const REPLY_TOKEN_TAG: u8 = 2;
/// Word index where reply token handle is stored
pub const REPLY_TOKEN_WORD: usize = 5;

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

/// Send a message with an inline payload, retrying on busy endpoints.
pub fn send_with_retry(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    send_with_retry_timeout(endpoint_token, label, payload, 0)
}

/// Send a message with an inline payload, retrying on busy endpoints with backoff.
///
/// When `max_retries` is 0, retries indefinitely.
///
/// Note: Error::WouldBlock means the kernel blocked the thread and will wake it
/// when space is available. We just retry - the kernel handles the blocking.
pub fn send_with_retry_timeout(
    endpoint_token: usize,
    label: u32,
    payload: &[u8],
    max_retries: u32,
) -> Result<()> {
    let max_backoff = IPC_BACKOFF_MAX_DEFAULT;
    let mut backoff = 1u32;
    let mut retries = 0u32;
    loop {
        match send_with_payload(endpoint_token, label, payload) {
            Ok(()) => return Ok(()),
            Err(Error::WouldBlock) => {
                // Kernel blocked us and will wake when space is available
                // Just retry - no need for backoff since kernel handles blocking
                continue;
            }
            Err(Error::Busy) => {
                retries = retries.saturating_add(1);
                if max_retries != 0 && retries > max_retries {
                    return Err(Error::Busy);
                }
                for _ in 0..backoff {
                    let _ = syscall::yield_cpu();
                }
                backoff = (backoff.saturating_mul(2)).min(max_backoff);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Synchronous write to TTY - waits for acknowledgement before returning.
/// Use this before exiting to ensure output is flushed.
pub fn tty_write_sync(endpoint_token: usize, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 1);
    msg.words[0] = payload.len();
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(endpoint_token, &msg, payload, &mut reply)
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

/// Extract reply token from a received call message
///
/// Returns the reply token handle if the message was from a call, None otherwise.
pub fn extract_reply_token(msg: &Message) -> Option<usize> {
    if msg.tag.extra == REPLY_TOKEN_TAG {
        Some(msg.words[REPLY_TOKEN_WORD])
    } else {
        None
    }
}

/// Reply to a received call message using the reply token
///
/// # Arguments
///
/// - `reply_token`: The reply token extracted from the received call message
/// - `msg`: Reply message to send
/// - `_flags`: IPC flags (currently unused)
pub fn reply(reply_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes();
    syscall::ipc_reply(reply_token, msg_bytes)?;
    Ok(())
}

/// Reply with an additional payload appended after the message header.
pub fn reply_with_payload(reply_token: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_reply(reply_token, &buffer)?;
    Ok(())
}

/// Copy an IPC call cookie from a request into a reply.
///
/// Servers should call this when replying to ipc_call() so the kernel
/// can route the reply to the correct caller even if calls overlap.
pub fn copy_call_cookie(reply: &mut Message, request: &Message) {
    if request.tag.extra != CALL_COOKIE_TAG {
        return;
    }
    reply.tag.extra = CALL_COOKIE_TAG;
    reply.words[CALL_COOKIE_WORD] = request.words[CALL_COOKIE_WORD];
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

/// Call with payload, receiving both message and reply payload.
///
/// Returns (reply_message, bytes_in_reply_payload).
pub fn call_with_reply_buf(
    endpoint_token: usize,
    msg: &Message,
    send_payload: &[u8],
    reply_buf: &mut [u8],
) -> Result<(Message, usize)> {
    use core::mem::size_of;

    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + send_payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(send_payload);

    let bytes_received = syscall::ipc_call(endpoint_token, &buffer, reply_buf)?;

    if bytes_received < size_of::<Message>() {
        return Err(Error::InvalidState);
    }

    // Parse the reply message
    let reply_msg = unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
    let payload_len = bytes_received - size_of::<Message>();

    Ok((reply_msg, payload_len))
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
