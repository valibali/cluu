//! VFS client helpers for IPC-based file operations.
//!
//! The client uses IPC call semantics with an inline payload for path strings.
//! Replies encode status in words[0] and return values in subsequent words.

use crate::error::{Error, Result};
use crate::fs::protocol::{VFS_CLOSE, VFS_OPEN, VFS_READ_GRANT};
use crate::ipc;
use crate::types::Message;

/// Handle to an open file in the VFS service.
#[derive(Debug, Clone, Copy)]
pub struct VfsFile {
    pub fd: usize,
    pub size: usize,
}

/// Result of a zero-copy grant read.
#[derive(Debug, Clone, Copy)]
pub struct VfsGrant {
    /// Base address mapped in the caller address space.
    pub base: usize,
    /// Offset into `base` where the data begins.
    pub offset: usize,
    /// Length of valid data.
    pub len: usize,
}

/// Simple VFS client wrapper.
pub struct VfsClient {
    endpoint: usize,
}

impl VfsClient {
    /// Create a new client for the given VFS endpoint token.
    pub const fn new(endpoint: usize) -> Self {
        Self { endpoint }
    }

    /// Open a path in the VFS service.
    pub fn open(&self, path: &str) -> Result<VfsFile> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_OPEN, payload.len(), &[]);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, payload, &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(VfsFile {
            fd: reply.words[1],
            size: reply.words[2],
        })
    }

    /// Close a file descriptor in the VFS service.
    pub fn close(&self, file: VfsFile) -> Result<()> {
        let mut msg = Message::new(VFS_CLOSE, [0; 6], 2);
        msg.words[0] = 0;
        msg.words[1] = file.fd;
        ipc::call(self.endpoint, &mut msg, crate::IpcFlags::empty())?;
        // call() already wrote reply into msg; use msg as reply for consistency.
        parse_status(msg.words[0])?;
        Ok(())
    }

    /// Read data using a zero-copy grant into the caller address space.
    ///
    /// The caller provides:
    /// - `target_space_token`: token for its own address space with SPACE_MAP
    /// - `target_base`: page-aligned target virtual address
    pub fn read_grant(
        &self,
        file: VfsFile,
        offset: usize,
        len: usize,
        target_space_token: usize,
        target_base: usize,
    ) -> Result<VfsGrant> {
        let mut msg = Message::new(VFS_READ_GRANT, [0; 6], 6);
        msg.words[0] = 0;
        msg.words[1] = file.fd;
        msg.words[2] = offset;
        msg.words[3] = len;
        msg.words[4] = target_space_token;
        msg.words[5] = target_base;
        ipc::call(self.endpoint, &mut msg, crate::IpcFlags::empty())?;
        parse_status(msg.words[0])?;
        Ok(VfsGrant {
            base: target_base,
            offset: msg.words[2],
            len: msg.words[1],
        })
    }
}

fn make_payload_message(label: u32, payload_len: usize, words: &[usize]) -> Message {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload_len;
    let mut count = 1;
    for (idx, word) in words.iter().enumerate() {
        if idx + 1 >= msg.words.len() {
            break;
        }
        msg.words[idx + 1] = *word;
        count += 1;
    }
    msg.tag.words = count as u8;
    msg
}

fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        let err = match signed as i32 {
            -1 => Error::InvalidArgument,
            -2 => Error::OutOfMemory,
            -3 => Error::NotFound,
            -4 => Error::PermissionDenied,
            -5 => Error::AlreadyExists,
            -6 => Error::Timeout,
            _ => Error::InvalidOperation,
        };
        return Err(err);
    }
    Ok(())
}
