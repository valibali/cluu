//! POSIX pipe implementation using IPC endpoints.
//!
//! A pipe is a unidirectional data channel backed by a single IPC endpoint.
//! The write end sends data messages (label 0x50) and an EOF marker (label 0x51)
//! on close. The read end receives messages and returns data or 0 on EOF.
//!
//! Pipe creation goes through procmgr (PROCMGR_PIPE_CREATE_LABEL) so that each
//! end receives a rights-restricted token: write-only for the write end and
//! read-only for the read end.

use super::c_int;
use crate::fd_table::{FdEntry, FD_TABLE};
use crate::ipc::{PROCMGR_PIPE_CLOSE_LABEL, PROCMGR_PIPE_CREATE_LABEL};
use crate::types::Message;
use crate::IpcFlags;

pub const PIPE_DATA_LABEL: u32 = 0x50;
pub const PIPE_EOF_LABEL: u32 = 0x51;

/// Max data payload per IPC message. The 4-byte label prefix plus this
/// value must not exceed the kernel's IPC_MESSAGE_MAX (4096 bytes).
const PIPE_CHUNK_SIZE: usize = 4092;

/// Send PROCMGR_PIPE_CLOSE_LABEL to procmgr for the given pipe_id.
///
/// Called when the last local fd referencing a pipe_id is closed, so procmgr
/// can free its bookkeeping for the pipe table slot.
pub fn close_pipe_id_in_procmgr(pipe_id: usize) {
    let procmgr_ep = match crate::registry::lookup_service("procmgr:spawn") {
        Some(ep) => ep,
        None => return,
    };
    let mut msg = Message::new(PROCMGR_PIPE_CLOSE_LABEL, [0; 6], 0);
    msg.words[0] = pipe_id;
    let _ = crate::ipc::call(procmgr_ep, &mut msg, IpcFlags::empty());
}

/// Create a pipe (POSIX pipe(2)).
///
/// pipefd[0] is the read end, pipefd[1] is the write end.
///
/// Asks procmgr for a rights-restricted token pair: the write end receives a
/// send-only token and the read end a receive-only token.
#[no_mangle]
pub extern "C" fn pipe(pipefd: *mut c_int) -> c_int {
    if pipefd.is_null() {
        crate::errno::set_errno(crate::errno::EINVAL);
        return -1;
    }

    // Step 1: resolve procmgr's spawn endpoint.
    let procmgr_ep = match crate::registry::lookup_service("procmgr:spawn") {
        Some(ep) => ep,
        None => {
            crate::errno::set_errno(crate::errno::ENOMEM);
            return -1;
        }
    };

    // Step 2: ask procmgr to create the pipe and hand back token pair + pipe_id.
    let mut req = Message::new(PROCMGR_PIPE_CREATE_LABEL, [0; 6], 0);
    if crate::ipc::call(procmgr_ep, &mut req, IpcFlags::empty()).is_err() {
        crate::errno::set_errno(crate::errno::ENOMEM);
        return -1;
    }

    // Step 3: decode the reply that is now in `req`.
    let status = req.words[0];
    if status != 0 {
        crate::errno::set_errno(crate::errno::ENOMEM);
        return -1;
    }
    let write_token = req.words[1];
    let read_token = req.words[2];
    let pipe_id = req.words[3];

    // Step 4: insert fd table entries.
    let mut table = FD_TABLE.lock();
    let read_fd = table.insert(FdEntry::pipe_read(read_token, pipe_id));
    let write_fd = table.insert(FdEntry::pipe_write(write_token, pipe_id));
    drop(table);

    // Step 5: write back fd numbers to caller.
    unsafe {
        *pipefd = read_fd;
        *pipefd.add(1) = write_fd;
    }
    0
}

/// Read data from a pipe endpoint.
///
/// Returns bytes read, 0 on EOF, or -1 on error.
pub fn read_pipe(endpoint: usize, buffer: &mut [u8]) -> isize {
    // 4 bytes for label prefix + up to PIPE_CHUNK_SIZE data bytes
    let mut recv_buf = [0u8; 4 + PIPE_CHUNK_SIZE];
    let len = match crate::syscall::ipc_recv(endpoint, &mut recv_buf) {
        Ok(len) => len,
        Err(_) => return -1,
    };
    if len < 4 {
        return -1;
    }

    let label = u32::from_le_bytes([recv_buf[0], recv_buf[1], recv_buf[2], recv_buf[3]]);
    if label == PIPE_EOF_LABEL {
        return 0; // EOF
    }
    if label != PIPE_DATA_LABEL {
        return -1;
    }

    let data_len = len.saturating_sub(4);
    let to_copy = data_len.min(buffer.len());
    buffer[..to_copy].copy_from_slice(&recv_buf[4..4 + to_copy]);
    to_copy as isize
}

/// Write data to a pipe endpoint, chunking as needed.
///
/// Returns total bytes written, or -1 on error.
/// If the read end is closed (ipc_send fails), sets errno=EPIPE and
/// raises SIGPIPE (unless SIGPIPE is SIG_IGN).
pub fn write_pipe(endpoint: usize, buffer: &[u8]) -> isize {
    let mut sent = 0usize;
    while sent < buffer.len() {
        let chunk = (buffer.len() - sent).min(PIPE_CHUNK_SIZE);
        // Build message: 4-byte label + data
        let mut msg_buf = [0u8; 4 + PIPE_CHUNK_SIZE];
        msg_buf[..4].copy_from_slice(&PIPE_DATA_LABEL.to_le_bytes());
        msg_buf[4..4 + chunk].copy_from_slice(&buffer[sent..sent + chunk]);

        if crate::syscall::ipc_send(endpoint, &msg_buf[..4 + chunk]).is_err() {
            // Broken pipe: raise SIGPIPE then return EPIPE.
            crate::errno::set_errno(crate::errno::EPIPE);
            super::signal::raise(super::signal::SIGPIPE);
            return -1;
        }
        sent += chunk;
    }
    sent as isize
}

/// Send EOF marker on pipe write-end close.
pub fn close_pipe_write(endpoint: usize) {
    let eof_msg = PIPE_EOF_LABEL.to_le_bytes();
    let _ = crate::syscall::ipc_send(endpoint, &eof_msg);
}
