//! VFS client helpers for IPC-based file operations.
//!
//! The client uses IPC call semantics with an inline payload for path strings.
//! Replies encode status in words[0] and return values in subsequent words.

use alloc::string::String;
use alloc::vec::Vec;
use crate::error::{Error, Result};
use crate::fs::protocol::{VFS_CLOSE, VFS_MAP_ELF, VFS_OPEN, VFS_READ_GRANT, VFS_READDIR};
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

/// Directory entry returned by readdir.
#[derive(Debug, Clone)]
pub struct VfsDirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Simple VFS client wrapper.
pub struct VfsClient {
    endpoint: usize,
    client_id: usize,
}

impl VfsClient {
    /// Create a new client for the given VFS endpoint token.
    pub const fn new(endpoint: usize, client_id: usize) -> Self {
        Self { endpoint, client_id }
    }

    /// Create a client using the current process registry control endpoint.
    pub fn new_from_registry(endpoint: usize) -> Result<Self> {
        let client_id = crate::registry::control_endpoint();
        if client_id == 0 {
            return Err(Error::InvalidState);
        }
        Ok(Self { endpoint, client_id })
    }

    /// Open a path in the VFS service.
    pub fn open(&self, path: &str) -> Result<VfsFile> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_OPEN, payload.len(), &[self.client_id]);
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
        let mut msg = Message::new(VFS_CLOSE, [0; 6], 3);
        msg.words[0] = 0;
        msg.words[1] = self.client_id;
        msg.words[2] = file.fd;
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
        let mut payload = [0u8; core::mem::size_of::<usize>() * 2];
        payload[..core::mem::size_of::<usize>()].copy_from_slice(&target_base.to_ne_bytes());
        payload[core::mem::size_of::<usize>()..].copy_from_slice(&target_space_token.to_ne_bytes());
        let msg = make_payload_message(
            VFS_READ_GRANT,
            payload.len(),
            &[self.client_id, file.fd, offset, len],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &payload, &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(VfsGrant {
            base: target_base,
            offset: reply.words[2],
            len: reply.words[1],
        })
    }

    /// Map ELF segments into a target address space and return entry point.
    pub fn map_elf(&self, file: VfsFile, target_space_token: usize) -> Result<usize> {
        let msg = make_payload_message(
            VFS_MAP_ELF,
            0,
            &[self.client_id, file.fd, target_space_token],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &[], &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(reply.words[1])
    }

    /// Read directory entries for a path.
    ///
    /// Returns a list of directory entries with name and type info.
    pub fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_READDIR, payload.len(), &[self.client_id]);

        // Use larger buffer for reply with entry data
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) = ipc::call_with_reply_buf(self.endpoint, &msg, payload, &mut reply_buf)?;
        parse_status(reply.words[0])?;

        let entry_count = reply.words[1];
        let data_start = core::mem::size_of::<Message>();
        let data = &reply_buf[data_start..data_start + payload_len];

        // Parse entries: each entry is [name_len: u8, is_dir: u8, name: [u8; name_len]]
        let mut entries = Vec::new();
        let mut offset = 0;
        for _ in 0..entry_count {
            if offset + 2 > data.len() {
                break;
            }
            let name_len = data[offset] as usize;
            let is_dir = data[offset + 1] != 0;
            offset += 2;
            if offset + name_len > data.len() {
                break;
            }
            if let Ok(name) = core::str::from_utf8(&data[offset..offset + name_len]) {
                entries.push(VfsDirEntry {
                    name: String::from(name),
                    is_dir,
                });
            }
            offset += name_len;
        }

        Ok(entries)
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
        return Err(Error::from_errno(signed));
    }
    Ok(())
}
