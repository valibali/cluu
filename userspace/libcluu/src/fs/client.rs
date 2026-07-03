//! VFS client helpers for IPC-based file operations.
//!
//! The client uses IPC call semantics with an inline payload for path strings.
//! Replies encode status in words[0] and return values in subsequent words.

use crate::error::{Error, Result};
use crate::fs::protocol::{
    VFS_BOUNCE_SETUP, VFS_CLOSE, VFS_FSTAT, VFS_LINK, VFS_MAP_ELF, VFS_MKDIR, VFS_OPEN,
    VFS_READDIR, VFS_READ_GRANT, VFS_READ_RING, VFS_REALPATH, VFS_RENAME, VFS_RING_SETUP,
    VFS_RMDIR, VFS_STAT, VFS_UNLINK, VFS_WRITE,
};
use crate::ipc::{self, make_payload_message};
use crate::types::Message;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

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

/// Shared-ring metadata returned by VFS ring setup.
#[derive(Debug, Clone, Copy)]
pub struct VfsReadRing {
    pub base: usize,
    pub bytes: usize,
    pub capacity: usize,
}

/// Result of one VFS ring read request.
#[derive(Debug, Clone, Copy)]
pub struct VfsReadRingChunk {
    pub len: usize,
    pub notify_seq: u32,
    pub eof: bool,
}

/// File metadata returned by stat/fstat. Wire format v2.
///
/// Layout of the 40-byte serialised payload:
///   [0..8]   size   (u64 LE)
///   [8..12]  mode   (u32 LE)  — S_IFMT | perms
///   [12..20] mtime  (u64 LE)  — unix seconds
///   [20..24] nlink  (u32 LE)
///   [24..28] uid    (u32 LE)
///   [28..32] gid    (u32 LE)
///   [32..40] blocks (u64 LE)  — 512-byte units
#[derive(Debug, Clone, Copy, Default)]
pub struct VfsStat {
    pub size: u64,
    pub mode: u32,    // S_IFMT | perms
    pub mtime: u64,   // unix seconds
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub blocks: u64,  // 512-byte units
}

impl VfsStat {
    /// Serialise into the 40-byte wire representation.
    pub fn to_bytes(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];
        buf[0..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8..12].copy_from_slice(&self.mode.to_le_bytes());
        buf[12..20].copy_from_slice(&self.mtime.to_le_bytes());
        buf[20..24].copy_from_slice(&self.nlink.to_le_bytes());
        buf[24..28].copy_from_slice(&self.uid.to_le_bytes());
        buf[28..32].copy_from_slice(&self.gid.to_le_bytes());
        buf[32..40].copy_from_slice(&self.blocks.to_le_bytes());
        buf
    }

    /// Parse from the 40-byte wire representation.
    pub fn from_bytes(buf: &[u8; 40]) -> Self {
        let mut a = [0u8; 8];
        let mut b = [0u8; 4];

        a.copy_from_slice(&buf[0..8]);
        let size = u64::from_le_bytes(a);

        b.copy_from_slice(&buf[8..12]);
        let mode = u32::from_le_bytes(b);

        a.copy_from_slice(&buf[12..20]);
        let mtime = u64::from_le_bytes(a);

        b.copy_from_slice(&buf[20..24]);
        let nlink = u32::from_le_bytes(b);

        b.copy_from_slice(&buf[24..28]);
        let uid = u32::from_le_bytes(b);

        b.copy_from_slice(&buf[28..32]);
        let gid = u32::from_le_bytes(b);

        a.copy_from_slice(&buf[32..40]);
        let blocks = u64::from_le_bytes(a);

        Self { size, mode, mtime, nlink, uid, gid, blocks }
    }
}

/// Directory entry returned by readdir.
#[derive(Debug, Clone)]
pub struct VfsDirEntry {
    pub name: String,
    pub stat: VfsStat,
    /// Convenience alias — `true` when `stat.mode` has `S_IFDIR` set.
    /// Retained for backward-compatibility with existing callers.
    pub is_dir: bool,
}

/// Simple VFS client wrapper.
pub struct VfsClient {
    endpoint: usize,
    client_id: usize,
    /// Lazy-init bounce buffer for big single-shot replies. `Cell` lets
    /// `&self` methods install/upgrade the buffer on demand without
    /// requiring a `&mut self` API change for callers.
    bounce: Cell<Option<BounceBuffer>>,
}

#[derive(Clone, Copy)]
struct BounceBuffer {
    base: usize,
    bytes: usize,
}

impl VfsClient {
    /// Create a new client for the given VFS endpoint token.
    pub const fn new(endpoint: usize, client_id: usize) -> Self {
        Self {
            endpoint,
            client_id,
            bounce: Cell::new(None),
        }
    }

    pub fn client_id(&self) -> usize {
        self.client_id
    }

    pub fn new_from_registry(endpoint: usize) -> Result<Self> {
        let client_id = crate::registry::control_endpoint();
        if client_id == 0 {
            return Err(Error::InvalidState);
        }
        Ok(Self {
            endpoint,
            client_id,
            bounce: Cell::new(None),
        })
    }

    /// Lazy-allocate and register a bounce buffer for replies that exceed
    /// the inline IPC limit. Idempotent: returns the cached buffer if one
    /// is already established.
    fn ensure_bounce(&self) -> Result<BounceBuffer> {
        if let Some(b) = self.bounce.get() {
            return Ok(b);
        }
        // Map a fresh region in this process's address space.
        let info = crate::boot::process_info();
        let space_token = info.tokens[crate::boot::TOKEN_SPACE];
        let region = crate::ipc::alloc_shared_ring_region(
            space_token,
            64 * 1024,
            crate::ipc::SHARED_RING_DEFAULT_MAP_FLAGS,
        )?;

        // Ask VFS to grant its source slot onto our region.
        let payload = region.base.to_ne_bytes();
        let msg = make_payload_message(
            VFS_BOUNCE_SETUP,
            payload.len(),
            &[self.client_id, space_token],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &payload, &mut reply)?;
        parse_status(reply.words[0])?;
        let bytes = reply.words[1];

        let buf = BounceBuffer { base: region.base, bytes };
        self.bounce.set(Some(buf));
        Ok(buf)
    }

    /// Open a path in the VFS service.
    pub fn open(&self, path: &str) -> Result<VfsFile> {
        self.open_with(path, 0, 0)
    }

    /// Open a path in the VFS service with POSIX-like flags/mode.
    pub fn open_with(&self, path: &str, flags: usize, mode: usize) -> Result<VfsFile> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_OPEN, payload.len(), &[self.client_id, flags, mode]);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, payload, &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(VfsFile {
            fd: reply.words[1],
            size: reply.words[2],
        })
    }

    /// Close a file descriptor in the VFS service.
    ///
    /// Uses `call_with_timeout` (5s) so a transiently backlogged VFS recv
    /// queue cannot wedge the caller indefinitely. Close is best-effort —
    /// on timeout the fd may leak in VFS bookkeeping until the calling
    /// process exits (procmgr's PROC_EXIT teardown reclaims).
    pub fn close(&self, file: VfsFile) -> Result<()> {
        let mut msg = Message::new(VFS_CLOSE, [0; 6], 3);
        msg.words[0] = 0;
        msg.words[1] = self.client_id;
        msg.words[2] = file.fd;
        ipc::call_with_timeout(self.endpoint, &mut msg, crate::IpcFlags::empty(), 5000)?;
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

    /// Establish (or refresh) a shared-ring mapping for this client.
    ///
    /// The caller provides a mapped local virtual window and its own space token.
    pub fn setup_read_ring(
        &self,
        target_space_token: usize,
        target_base: usize,
        requested_bytes: usize,
    ) -> Result<VfsReadRing> {
        let payload = target_base.to_ne_bytes();
        let msg = make_payload_message(
            VFS_RING_SETUP,
            payload.len(),
            &[self.client_id, target_space_token, requested_bytes],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &payload, &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(VfsReadRing {
            base: target_base,
            bytes: reply.words[1],
            capacity: reply.words[2],
        })
    }

    /// Request the server to fill bytes into the established shared ring.
    pub fn read_ring(&self, file: VfsFile, offset: usize, len: usize) -> Result<VfsReadRingChunk> {
        let mut msg = Message::new(VFS_READ_RING, [0; 6], 5);
        msg.words[0] = 0;
        msg.words[1] = self.client_id;
        msg.words[2] = file.fd;
        msg.words[3] = offset;
        msg.words[4] = len;
        ipc::call(self.endpoint, &mut msg, crate::IpcFlags::empty())?;
        parse_status(msg.words[0])?;
        Ok(VfsReadRingChunk {
            len: msg.words[1],
            notify_seq: msg.words[2] as u32,
            eof: msg.words[3] != 0,
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
    /// Returns a list of directory entries with full metadata (v2 wire format).
    /// Per-entry layout: [name_len: u32 LE][stat: 40 bytes][name: name_len bytes]
    pub fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        // First attempt: assume small reply fits inline. If VFS says
        // BufferTooSmall, lazy-set-up the bounce buffer and retry once.
        match self.readdir_once(path) {
            Err(Error::BufferTooSmall) => {
                self.ensure_bounce()?;
                self.readdir_once(path)
            }
            other => other,
        }
    }

    /// Single readdir RPC. Decodes inline or bounce-buffer reply based on
    /// the `bounce_flag` returned by the server.
    fn readdir_once(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_READDIR, payload.len(), &[self.client_id]);

        // Heap-allocate reply buffer to avoid 4KB stack frame.
        let mut reply_buf: Vec<u8> = Vec::with_capacity(4096);
        reply_buf.resize(4096, 0u8);
        let (reply, payload_len) =
            ipc::call_with_reply_buf(self.endpoint, &msg, payload, &mut reply_buf)?;
        parse_status(reply.words[1])?;

        let entry_count = reply.words[2];
        let bounce_flag = reply.words[3];
        let blob_len = reply.words[0];

        let entries = if bounce_flag == 0 {
            let data_start = core::mem::size_of::<Message>();
            let data = &reply_buf[data_start..data_start + payload_len];
            parse_readdir_blob(data, entry_count)
        } else {
            // Read blob from our bounce buffer.
            let bounce = self.bounce.get().ok_or(Error::InvalidState)?;
            if blob_len > bounce.bytes {
                return Err(Error::BufferTooSmall);
            }
            let data = unsafe {
                core::slice::from_raw_parts(bounce.base as *const u8, blob_len)
            };
            parse_readdir_blob(data, entry_count)
        };

        Ok(entries)
    }

    /// Write data to a file.
    pub fn write(&self, file: VfsFile, offset: usize, data: &[u8]) -> Result<usize> {
        let msg = make_payload_message(
            VFS_WRITE,
            data.len(),
            &[self.client_id, file.fd, offset, data.len()],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, data, &mut reply)?;
        parse_status(reply.words[0])?;
        Ok(reply.words[1])
    }

    /// Stat a path.
    ///
    /// Reply carries a 40-byte stat payload after the status word (v2 format).
    pub fn stat(&self, path: &str) -> Result<VfsStat> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_STAT, payload.len(), &[self.client_id]);
        let mut reply_buf = [0u8; core::mem::size_of::<Message>() + 40];
        let (reply, payload_len) =
            ipc::call_with_reply_buf(self.endpoint, &msg, payload, &mut reply_buf)?;
        parse_status(reply.words[0])?;
        let data_start = core::mem::size_of::<Message>();
        if payload_len < 40 {
            return Err(crate::error::Error::InvalidArgument);
        }
        let mut stat_bytes = [0u8; 40];
        stat_bytes.copy_from_slice(&reply_buf[data_start..data_start + 40]);
        Ok(VfsStat::from_bytes(&stat_bytes))
    }

    /// Resolve `path` to its canonical absolute form, following symlinks.
    /// Backends without symlinks (memfs, procfs, devfs, initrd) return the
    /// input unchanged.
    pub fn realpath(&self, path: &str) -> Result<String> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_REALPATH, payload.len(), &[self.client_id]);
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) =
            ipc::call_with_reply_buf(self.endpoint, &msg, payload, &mut reply_buf)?;
        parse_status(reply.words[0])?;
        let data_start = core::mem::size_of::<Message>();
        let bytes = &reply_buf[data_start..data_start + payload_len];
        let s = core::str::from_utf8(bytes).map_err(|_| Error::InvalidArgument)?;
        Ok(String::from(s))
    }

    /// Stat an open file descriptor.
    ///
    /// Reply carries a 40-byte stat payload after the status word (v2 format).
    pub fn fstat(&self, file: VfsFile) -> Result<VfsStat> {
        let msg = make_payload_message(VFS_FSTAT, 0, &[self.client_id, file.fd]);
        let mut reply_buf = [0u8; core::mem::size_of::<Message>() + 40];
        let (reply, payload_len) =
            ipc::call_with_reply_buf(self.endpoint, &msg, &[], &mut reply_buf)?;
        parse_status(reply.words[0])?;
        let data_start = core::mem::size_of::<Message>();
        if payload_len < 40 {
            return Err(crate::error::Error::InvalidArgument);
        }
        let mut stat_bytes = [0u8; 40];
        stat_bytes.copy_from_slice(&reply_buf[data_start..data_start + 40]);
        Ok(VfsStat::from_bytes(&stat_bytes))
    }

    /// Create a directory.
    pub fn mkdir(&self, path: &str, mode: usize) -> Result<()> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_MKDIR, payload.len(), &[self.client_id, mode]);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    /// Remove a directory.
    pub fn rmdir(&self, path: &str) -> Result<()> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_RMDIR, payload.len(), &[self.client_id]);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    /// Remove a file.
    pub fn unlink(&self, path: &str) -> Result<()> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_UNLINK, payload.len(), &[self.client_id]);
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    /// Rename/move a path.
    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let old_bytes = old.as_bytes();
        let new_bytes = new.as_bytes();
        let mut payload = Vec::with_capacity(old_bytes.len() + new_bytes.len());
        payload.extend_from_slice(old_bytes);
        payload.extend_from_slice(new_bytes);
        let msg = make_payload_message(
            VFS_RENAME,
            payload.len(),
            &[self.client_id, old_bytes.len()],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    /// Create a hard link.
    pub fn link(&self, old: &str, new: &str) -> Result<()> {
        let old_bytes = old.as_bytes();
        let new_bytes = new.as_bytes();
        let mut payload = Vec::with_capacity(old_bytes.len() + new_bytes.len());
        payload.extend_from_slice(old_bytes);
        payload.extend_from_slice(new_bytes);
        let msg = make_payload_message(
            VFS_LINK,
            payload.len(),
            &[self.client_id, old_bytes.len()],
        );
        let mut reply = Message::new(0, [0; 6], 0);
        ipc::call_with_payload(self.endpoint, &msg, &payload, &mut reply)?;
        parse_status(reply.words[0])
    }
}

fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

/// Parse v2 readdir wire blob: `[name_len: u32 LE][stat: 40][name]` repeating.
fn parse_readdir_blob(data: &[u8], entry_count: usize) -> Vec<VfsDirEntry> {
    let mut entries = Vec::with_capacity(entry_count);
    let mut offset = 0;
    for _ in 0..entry_count {
        if offset + 44 > data.len() {
            break;
        }
        let name_len = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
        ]) as usize;
        offset += 4;

        let mut stat_bytes = [0u8; 40];
        stat_bytes.copy_from_slice(&data[offset..offset + 40]);
        let stat = VfsStat::from_bytes(&stat_bytes);
        offset += 40;

        if offset + name_len > data.len() {
            break;
        }
        if let Ok(name) = core::str::from_utf8(&data[offset..offset + name_len]) {
            let is_dir = (stat.mode & 0o170000) == 0o040000;
            entries.push(VfsDirEntry {
                name: String::from(name),
                stat,
                is_dir,
            });
        }
        offset += name_len;
    }
    entries
}
