#![allow(unused)]
//! Unified mount table for the VFS service.
//!
//! All mount points are declared in one place with a clean plugin architecture.
//! Supported backends:
//! - Initrd: Direct memory access to tar archive
//! - Remote: IPC forwarding to external service (e.g., virtio-blk)
//! - Virtual: Dynamic content generation (e.g., procfs)

use crate::fd_table::{Ext2Entry, FileEntry, OpenFile};
use crate::memfs::MemFs;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::mem::size_of;
use core::pin::Pin;
use libcluu::ipc::{call_with_payload, call_with_reply_buf};
use libcluu::tar::{find_member, list_entries};
use libcluu::types::Message;
use libcluu::{Error, Result};

/// IPC labels for remote filesystem operations
const FS_OPEN: u32 = 0x300;
const FS_READ: u32 = 0x302;
const FS_STAT: u32 = 0x303;
const FS_READDIR: u32 = 0x304;
const FS_UNLINK: u32 = 0x307;
const FS_MKDIR: u32 = 0x308;
const FS_RMDIR: u32 = 0x309;
const FS_RENAME: u32 = 0x30A;
const FS_CREATE: u32 = 0x30B;
const FS_LINK: u32 = 0x30C;
const FS_REALPATH: u32 = 0x30D;
const IPC_MESSAGE_MAX: usize = 256;

/// Build a minimal DirEntryStat for backends that can't provide full metadata.
fn default_stat(is_dir: bool, size: u64) -> DirEntryStat {
    let mode = if is_dir { 0o040755u32 } else { 0o100644u32 };
    DirEntryStat {
        size,
        mode,
        mtime: 0,
        nlink: 1,
        uid: 0,
        gid: 0,
        blocks: (size + 511) / 512,
    }
}

/// Directory entry for readdir results (internal VFS representation).
#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// Full file metadata (v2). Populated by all backends.
    pub stat: DirEntryStat,
}

/// Compact stat fields carried inline on every DirEntry.
#[derive(Clone, Copy, Default)]
pub struct DirEntryStat {
    pub size:   u64,
    pub mode:   u32,
    pub mtime:  u64,
    pub nlink:  u32,
    pub uid:    u32,
    pub gid:    u32,
    pub blocks: u64,
}

/// Mount backend trait - all mount types implement this.
pub trait MountBackend: Send + Sync {
    /// Backend name for debugging.
    fn name(&self) -> &'static str;

    /// Open a file at the given relative path.
    /// full_path is the original absolute path (for caching).
    fn open(&self, rel_path: &str, full_path: &str, caller_tid: usize) -> Result<OpenFile>;

    /// Read directory entries at the given relative path.
    fn readdir(&self, rel_path: &str, caller_tid: usize) -> Result<Vec<DirEntry>>;

    /// Stat a path without reading directory entries.
    ///
    /// Returns `DirEntryStat` with mode/size/etc. For remote backends this
    /// is a lightweight open+stat-by-inode IPC pair; for memory-backed
    /// backends it opens and inspects the file type.
    ///
    /// Default implementation: `open` + synthesize from `OpenFile`.
    /// Override in `RemoteBackend` to avoid the expensive `readdir` probe
    /// that reads ALL directory entries (which can exceed IPC_MESSAGE_MAX
    /// for large directories like /bin with 100+ entries).
    fn stat_by_path(&self, rel_path: &str, full_path: &str, caller_tid: usize) -> Result<DirEntryStat> {
        let file = self.open(rel_path, full_path, caller_tid)?;
        let size = file.size() as u64;
        let mode = match &file {
            OpenFile::Device(_) => 0o020666u32,
            OpenFile::Virtual(_) => 0o040755u32,
            _ => 0o100644u32,
        };
        Ok(DirEntryStat {
            size,
            mode,
            mtime: 0,
            nlink: 1,
            uid: 0,
            gid: 0,
            blocks: (size + 511) / 512,
        })
    }

    /// Read file data (for remote backends that need IPC).
    fn read(&self, file: &OpenFile, offset: usize, len: usize) -> Result<Vec<u8>> {
        // Default implementation for memory-backed files
        let _ = (file, offset, len);
        Err(Error::InvalidOperation)
    }

    fn unlink(&self, rel_path: &str) -> Result<()> {
        let _ = rel_path;
        Err(Error::NotImplemented)
    }

    fn mkdir(&self, rel_path: &str, mode: usize) -> Result<()> {
        let _ = (rel_path, mode);
        Err(Error::NotImplemented)
    }

    fn rmdir(&self, rel_path: &str) -> Result<()> {
        let _ = rel_path;
        Err(Error::NotImplemented)
    }

    fn rename(&self, rel_old: &str, rel_new: &str) -> Result<()> {
        let _ = (rel_old, rel_new);
        Err(Error::NotImplemented)
    }

    fn link(&self, rel_old: &str, rel_new: &str) -> Result<()> {
        let _ = (rel_old, rel_new);
        Err(Error::NotImplemented)
    }

    fn create_file(&self, rel_path: &str, mode: usize) -> Result<()> {
        let _ = (rel_path, mode);
        Err(Error::NotImplemented)
    }

    fn realpath(&self, rel_path: &str) -> Result<String> {
        Ok(String::from(rel_path))
    }
}

/// Async mount backend trait — for backends that need to await IPC or
/// other asynchronous operations (single-threaded executor; no `Send` bound).
///
/// Object-safe: methods return `Pin<Box<dyn Future + '_>>` so the trait can
/// be used as `dyn AsyncMountBackend`. The lifetime `'_` ties the future to
/// `&self`, keeping the borrow checker honest without requiring `Send`.
pub trait AsyncMountBackend: Sync {
    /// Backend name for debugging.
    fn name(&self) -> &'static str;

    /// Open a file at the given relative path (async).
    fn open_async(
        &self,
        rel_path: &str,
        full_path: &str,
        caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<OpenFile>> + '_>>;

    /// Read directory entries at the given relative path (async).
    fn readdir_async(
        &self,
        rel_path: &str,
        caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirEntry>>> + '_>>;

    /// Stat a path without reading directory entries (async).
    fn stat_async(
        &self,
        rel_path: &str,
        full_path: &str,
        caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<DirEntryStat>> + '_>> {
        let _ = (rel_path, full_path, caller_tid);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Read file data (async).
    fn read_async(
        &self,
        file: &OpenFile,
        offset: usize,
        len: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + '_>> {
        let _ = (file, offset, len);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Write file data (async).
    fn write_async(
        &self,
        file: &OpenFile,
        offset: usize,
        data: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<usize>> + '_>> {
        let _ = (file, offset, data);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Unlink a file (async).
    fn unlink_async(
        &self,
        rel_path: &str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = rel_path;
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Create a directory (async).
    fn mkdir_async(
        &self,
        rel_path: &str,
        mode: usize,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = (rel_path, mode);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Remove a directory (async).
    fn rmdir_async(
        &self,
        rel_path: &str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = rel_path;
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Rename a file (async).
    fn rename_async(
        &self,
        rel_old: &str,
        rel_new: &str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = (rel_old, rel_new);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Create a hard link (async).
    fn link_async(
        &self,
        rel_old: &str,
        rel_new: &str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = (rel_old, rel_new);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Create a new file (async).
    fn create_file_async(
        &self,
        rel_path: &str,
        mode: usize,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + '_>> {
        let _ = (rel_path, mode);
        Box::pin(async move { Err(Error::NotImplemented) })
    }

    /// Resolve a path to its canonical form (async).
    fn realpath_async(
        &self,
        rel_path: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + '_>> {
        let _ = rel_path;
        Box::pin(async move { Err(Error::NotImplemented) })
    }
}

/// Type-erased mount backend — either sync or async.
///
/// `MountTable` stores `AnyMount` per mount point. The sync path dispatches
/// through `MountBackend` as before; the async path is driven separately via
/// `get_async_backend()` / `is_async()` by callers that own an executor.
pub enum AnyMount {
    /// Synchronous backend — all operations complete immediately.
    Sync(Box<dyn MountBackend>),
    /// Asynchronous backend — operations return futures to be polled.
    Async(Box<dyn AsyncMountBackend>),
}

impl AnyMount {
    /// Return the sync backend, or `Err(InvalidOperation)` if this is async.
    ///
    /// Used by `MountTable` sync methods (`open`, `readdir`, …) to dispatch
    /// to the underlying `MountBackend` or reject async mounts.
    fn as_sync(&self) -> Result<&dyn MountBackend> {
        match self {
            AnyMount::Sync(b) => Ok(b.as_ref()),
            AnyMount::Async(_) => Err(Error::InvalidOperation),
        }
    }
}

impl From<Box<dyn MountBackend>> for AnyMount {
    fn from(b: Box<dyn MountBackend>) -> Self {
        AnyMount::Sync(b)
    }
}

impl From<Box<dyn AsyncMountBackend>> for AnyMount {
    fn from(b: Box<dyn AsyncMountBackend>) -> Self {
        AnyMount::Async(b)
    }
}

/// Initrd backend - serves files from a tar archive in memory.
pub struct InitrdBackend {
    data: &'static [u8],
}

impl InitrdBackend {
    pub fn new(data: &'static [u8]) -> Self {
        Self { data }
    }
}

impl MountBackend for InitrdBackend {
    fn name(&self) -> &'static str {
        "initrd"
    }

    fn open(&self, rel_path: &str, _full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        let slice = find_member(self.data, rel_path)
            .or_else(|| find_member(self.data, &dot_prefixed(rel_path)))
            .ok_or(Error::NotFound)?;

        let base = self.data.as_ptr() as usize;
        let offset = slice.as_ptr() as usize - base;

        Ok(OpenFile::Memory(FileEntry {
            base,
            offset,
            size: slice.len(),
            rights: u64::MAX,
        }))
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        let entries = list_entries(self.data, rel_path);
        Ok(entries
            .into_iter()
            .map(|e| {
                // For initrd tar entries we can probe the actual data slice for size.
                let size = if !e.is_dir {
                    find_member(self.data, &e.name)
                        .or_else(|| find_member(self.data, &dot_prefixed(&e.name)))
                        .map(|s| s.len() as u64)
                        .unwrap_or(0)
                } else {
                    0
                };
                let mode = if e.is_dir { 0o040755u32 } else { 0o100644u32 };
                let blocks = (size + 511) / 512;
                DirEntry {
                    name: e.name,
                    is_dir: e.is_dir,
                    stat: DirEntryStat {
                        size,
                        mode,
                        mtime: 0,
                        nlink: 1,
                        uid: 0,
                        gid: 0,
                        blocks,
                    },
                }
            })
            .collect())
    }
}

/// Remote backend - forwards requests to an external service via IPC.
pub struct RemoteBackend {
    endpoint: usize,
    service_name: &'static str,
}

impl RemoteBackend {
    pub fn new(endpoint: usize, service_name: &'static str) -> Self {
        Self {
            endpoint,
            service_name,
        }
    }
}

impl MountBackend for RemoteBackend {
    fn name(&self) -> &'static str {
        self.service_name
    }

    fn open(&self, rel_path: &str, full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        let req = Message::new(FS_OPEN, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;

        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::NotFound);
        }

        let inode = reply.words[1];
        let size = reply.words[2];

        Ok(OpenFile::Ext2(Ext2Entry {
            endpoint: self.endpoint,
            inode: inode as u32,
            size,
            path: String::from(full_path),
            rights: u64::MAX,
        }))
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut offset = 0usize;
        loop {
            let req = Message::new(FS_READDIR, [rel_path.len(), 0, offset, 0, 0, 0], 1);
            let mut reply_buf = [0u8; 4096];
            let (reply, payload_len) =
                call_with_reply_buf(self.endpoint, &req, rel_path.as_bytes(), &mut reply_buf)?;

            let status = reply.words[1] as isize;
            if status < 0 {
                return Err(Error::NotFound);
            }

            let entry_count = reply.words[2];
            let total = reply.words[3];
            let data_start = size_of::<Message>();
            let data = &reply_buf[data_start..data_start + payload_len];

            // Parse entries (wire format v2). Per entry:
            //   [name_len: u8][is_dir: u8]
            //   [size: u64 LE][mode: u32 LE][mtime: u64 LE]
            //   [nlink: u32 LE][uid: u32 LE][gid: u32 LE]
            //   [name: name_len bytes]
            // = 34 + name_len bytes
            let mut parse_offset = 0;
            for _ in 0..entry_count {
                if parse_offset + 34 > data.len() { break; }
                let name_len = data[parse_offset] as usize;
                let is_dir = data[parse_offset + 1] != 0;
                parse_offset += 2;
                let size = u64::from_le_bytes(data[parse_offset..parse_offset+8].try_into().unwrap_or([0u8;8]));
                parse_offset += 8;
                let mode = u32::from_le_bytes(data[parse_offset..parse_offset+4].try_into().unwrap_or([0u8;4]));
                parse_offset += 4;
                let mtime = u64::from_le_bytes(data[parse_offset..parse_offset+8].try_into().unwrap_or([0u8;8]));
                parse_offset += 8;
                let nlink = u32::from_le_bytes(data[parse_offset..parse_offset+4].try_into().unwrap_or([0u8;4]));
                parse_offset += 4;
                let uid = u32::from_le_bytes(data[parse_offset..parse_offset+4].try_into().unwrap_or([0u8;4]));
                parse_offset += 4;
                let gid = u32::from_le_bytes(data[parse_offset..parse_offset+4].try_into().unwrap_or([0u8;4]));
                parse_offset += 4;
                if parse_offset + name_len > data.len() { break; }
                if let Ok(name) = core::str::from_utf8(&data[parse_offset..parse_offset + name_len]) {
                    let blocks = (size + 511) / 512;
                    let stat = DirEntryStat { size, mode, mtime, nlink, uid, gid, blocks };
                    entries.push(DirEntry { name: String::from(name), is_dir, stat });
                }
                parse_offset += name_len;
            }

            offset += entry_count;
            if offset >= total || entry_count == 0 {
                break;
            }
        }
        Ok(entries)
    }

    fn stat_by_path(&self, rel_path: &str, _full_path: &str, _caller_tid: usize) -> Result<DirEntryStat> {
        let req = Message::new(FS_OPEN, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;

        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::NotFound);
        }

        let inode = reply.words[1] as u64;
        let size = reply.words[2] as u64;

        let stat_req = Message::new(FS_STAT, [0, inode as usize, 0, 0, 0, 0], 2);
        let mut stat_reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &stat_req, &[], &mut stat_reply)?;

        let stat_status = stat_reply.words[0] as isize;
        if stat_status < 0 {
            return Err(Error::from_errno(stat_status));
        }

        let remote_size = stat_reply.words[1] as u64;
        let mode_flags = stat_reply.words[2];
        let mtime = stat_reply.words[3] as u64;
        let nlink = (stat_reply.words[4] & 0xFFFF) as u32;
        let uid = ((stat_reply.words[4] >> 16) & 0xFFFF) as u32;
        let gid = (stat_reply.words[5] & 0xFFFF) as u32;
        let is_dir = (mode_flags & 1) != 0;
        let mode = if is_dir { 0o040755u32 } else { 0o100644u32 };
        let final_size = if remote_size > 0 { remote_size } else { size };
        let blocks = (final_size + 511) / 512;

        Ok(DirEntryStat {
            size: final_size,
            mode,
            mtime,
            nlink: if nlink == 0 { 1 } else { nlink },
            uid,
            gid,
            blocks,
        })
    }

    fn realpath(&self, rel_path: &str) -> Result<String> {
        let req = Message::new(FS_REALPATH, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) =
            call_with_reply_buf(self.endpoint, &req, rel_path.as_bytes(), &mut reply_buf)?;
        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::from_errno(status));
        }
        let data_start = core::mem::size_of::<Message>();
        let bytes = &reply_buf[data_start..data_start + payload_len];
        core::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| Error::InvalidArgument)
    }

    fn read(&self, file: &OpenFile, offset: usize, len: usize) -> Result<Vec<u8>> {
        let inode = match file {
            OpenFile::Ext2(e) => e.inode,
            _ => return Err(Error::InvalidArgument),
        };

        let max_len = len.min(IPC_MESSAGE_MAX - size_of::<Message>());
        let req = Message::new(FS_READ, [0, 0, inode as usize, offset, max_len, 0], 5);
        let mut reply_buf = alloc::vec![0u8; size_of::<Message>() + max_len];
        let (reply, payload_len) = call_with_reply_buf(self.endpoint, &req, &[], &mut reply_buf)?;

        let status = reply.words[1] as isize;
        if status < 0 {
            return Err(Error::InvalidState);
        }

        let bytes_read = reply.words[2].min(max_len);
        let data_start = size_of::<Message>();
        let data_len = payload_len.min(bytes_read);

        Ok(reply_buf[data_start..data_start + data_len].to_vec())
    }

    fn unlink(&self, rel_path: &str) -> Result<()> {
        let req = Message::new(FS_UNLINK, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;
        parse_status(reply.words[0])
    }

    fn mkdir(&self, rel_path: &str, mode: usize) -> Result<()> {
        let req = Message::new(FS_MKDIR, [rel_path.len(), 0, mode, 0, 0, 0], 3);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;
        parse_status(reply.words[0])
    }

    fn rmdir(&self, rel_path: &str) -> Result<()> {
        let req = Message::new(FS_RMDIR, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;
        parse_status(reply.words[0])
    }

    fn rename(&self, rel_old: &str, rel_new: &str) -> Result<()> {
        let old_bytes = rel_old.as_bytes();
        let new_bytes = rel_new.as_bytes();
        let mut payload = Vec::with_capacity(old_bytes.len() + new_bytes.len());
        payload.extend_from_slice(old_bytes);
        payload.extend_from_slice(new_bytes);
        let req = Message::new(FS_RENAME, [payload.len(), 0, old_bytes.len(), 0, 0, 0], 3);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, &payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    fn link(&self, rel_old: &str, rel_new: &str) -> Result<()> {
        let old_bytes = rel_old.as_bytes();
        let new_bytes = rel_new.as_bytes();
        let mut payload = Vec::with_capacity(old_bytes.len() + new_bytes.len());
        payload.extend_from_slice(old_bytes);
        payload.extend_from_slice(new_bytes);
        let req = Message::new(FS_LINK, [payload.len(), 0, old_bytes.len(), 0, 0, 0], 3);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, &payload, &mut reply)?;
        parse_status(reply.words[0])
    }

    fn create_file(&self, rel_path: &str, mode: usize) -> Result<()> {
        let req = Message::new(FS_CREATE, [rel_path.len(), 0, mode, 0, 0, 0], 3);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;
        parse_status(reply.words[0])
    }
}

impl RemoteBackend {
    /// Fetch full stat for a single entry path from the remote FS backend.
    ///
    /// Opens the file by path (FS_OPEN) and then queries FS_STAT by inode.
    /// Falls back to a synthesized stat on any IPC error.
    fn stat_entry(&self, path: &str, is_dir: bool) -> DirEntryStat {
        // FS_OPEN to resolve inode number and base size.
        let req = Message::new(FS_OPEN, [path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply = Message::new(0, [0; 6], 0);
        if call_with_payload(self.endpoint, &req, path.as_bytes(), &mut reply).is_err() {
            return default_stat(is_dir, 0);
        }
        let status = reply.words[0] as isize;
        if status < 0 {
            return default_stat(is_dir, 0);
        }
        let inode = reply.words[1] as u64;
        let size = reply.words[2] as u64;

        // Query FS_STAT by inode for full metadata.
        let stat_req = Message::new(FS_STAT, [0, inode as usize, 0, 0, 0, 0], 2);
        let mut stat_reply = Message::new(0, [0; 6], 0);
        if call_with_payload(self.endpoint, &stat_req, &[], &mut stat_reply).is_err() {
            return default_stat(is_dir, size);
        }
        let stat_status = stat_reply.words[0] as isize;
        if stat_status < 0 {
            return default_stat(is_dir, size);
        }
        // Remote FS_STAT reply (v2): words[1]=size, words[2]=flags,
        // words[3]=mtime, words[4]=(uid<<16)|nlink, words[5]=gid
        let remote_size = stat_reply.words[1] as u64;
        let mode_flags = stat_reply.words[2];
        let mtime = stat_reply.words[3] as u64;
        let nlink = (stat_reply.words[4] & 0xFFFF) as u32;
        let uid = ((stat_reply.words[4] >> 16) & 0xFFFF) as u32;
        let gid = (stat_reply.words[5] & 0xFFFF) as u32;
        let is_dir_from_flags = (mode_flags & 1) != 0;
        let actual_is_dir = is_dir || is_dir_from_flags;
        let mode = if actual_is_dir { 0o040755u32 } else { 0o100644u32 };
        let blocks = (remote_size + 511) / 512;
        DirEntryStat {
            size: remote_size,
            mode,
            mtime,
            nlink: if nlink == 0 { 1 } else { nlink },
            uid,
            gid,
            blocks,
        }
    }
}

/// Framebuffer geometry forwarded from boot params.
#[derive(Clone, Copy)]
pub struct FbInfo {
    pub phys: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
}

/// Device backend - special device files (/dev/null, /dev/zero, /dev/urandom, /dev/tty*, /dev/fb0).
pub struct DeviceBackend {
    /// Endpoints for tty:0..tty:3 (resolved via registry at VFS startup).
    pub tty_endpoints: [usize; 4],
    /// Primary framebuffer geometry; None if no framebuffer is available.
    pub fb: Option<FbInfo>,
}

impl DeviceBackend {
    pub fn new() -> Self {
        Self {
            tty_endpoints: [0; 4],
            fb: None,
        }
    }

    /// Set framebuffer geometry (called at VFS boot from PARAM_VFS_FB_* params).
    pub fn set_fb(&mut self, info: FbInfo) {
        self.fb = Some(info);
    }
}

impl MountBackend for DeviceBackend {
    fn name(&self) -> &'static str {
        "devfs"
    }

    fn open(&self, rel_path: &str, full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        use crate::fd_table::{DeviceFile, DeviceType};

        let rel = rel_path.trim_start_matches('/');
        let device_type = match rel {
            "null" => DeviceType::Null,
            "zero" => DeviceType::Zero,
            "urandom" | "random" => DeviceType::Urandom,
            "tty0" => DeviceType::Tty0 {
                endpoint: self.tty_endpoints[0],
            },
            "tty1" => DeviceType::Tty {
                vt_index: 0,
                endpoint: self.tty_endpoints[0],
            },
            "tty2" => DeviceType::Tty {
                vt_index: 1,
                endpoint: self.tty_endpoints[1],
            },
            "tty3" => DeviceType::Tty {
                vt_index: 2,
                endpoint: self.tty_endpoints[2],
            },
            "tty4" => DeviceType::Tty {
                vt_index: 3,
                endpoint: self.tty_endpoints[3],
            },
            "console" => DeviceType::Console {
                endpoint: self.tty_endpoints[0],
            },
            "fb0" => {
                let Some(info) = self.fb else {
                    return Err(Error::NotFound);
                };
                DeviceType::Fb {
                    phys: info.phys,
                    size: info.size,
                    width: info.width,
                    height: info.height,
                    pitch: info.pitch,
                    bpp: info.bpp,
                }
            }
            _ => return Err(Error::NotFound),
        };

        Ok(OpenFile::Device(DeviceFile {
            device_type,
            path: String::from(full_path),
            rights: u64::MAX,
        }))
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        let rel = rel_path.trim_start_matches('/');
        if !rel.is_empty() {
            return Err(Error::NotFound);
        }

        let dev_stat = DirEntryStat {
            size: 0,
            mode: 0o020666u32, // S_IFCHR | rw-rw-rw-
            mtime: 0,
            nlink: 1,
            uid: 0,
            gid: 0,
            blocks: 0,
        };

        let names = [
            "null", "zero", "urandom", "random",
            "tty0", "tty1", "tty2", "tty3", "tty4", "console", "fb0",
        ];
        Ok(names.iter().map(|&n| DirEntry {
            name: String::from(n),
            is_dir: false,
            stat: dev_stat,
        }).collect())
    }
}

/// Entry for a devmgr-registered device visible to this VFS instance.
#[derive(Clone)]
pub struct DevRegistryEntry {
    pub device_id: u32,
    pub class: u8,
    pub driver_endpoint: usize,
    pub path: String,
}

/// Dynamic /dev registry — devmgr-registered devices (input, disk, etc.).
///
/// Heap-allocated in VfsServer; raw pointer handed to `DevRegistryMount`.
/// Mirrors the PtsRegistry pattern.
pub struct DevRegistry {
    entries: alloc::vec::Vec<DevRegistryEntry>,
}

impl DevRegistry {
    pub fn new() -> Self {
        Self {
            entries: alloc::vec::Vec::new(),
        }
    }

    pub fn register(&mut self, entry: DevRegistryEntry) {
        self.entries.push(entry);
    }

    pub fn find(&self, rel_path: &str) -> Option<&DevRegistryEntry> {
        let rel = rel_path.trim_start_matches('/');
        self.entries.iter().find(|e| {
            let p = e.path.trim_start_matches("/dev/");
            p == rel
        })
    }

    pub fn list(&self) -> &[DevRegistryEntry] {
        &self.entries
    }
}

pub struct DevRegistryMount {
    registry: *const DevRegistry,
}

unsafe impl Send for DevRegistryMount {}
unsafe impl Sync for DevRegistryMount {}

impl DevRegistryMount {
    pub fn new(registry: *const DevRegistry) -> Self {
        Self { registry }
    }

    fn reg(&self) -> &DevRegistry {
        unsafe { &*self.registry }
    }
}

impl MountBackend for DevRegistryMount {
    fn name(&self) -> &'static str {
        "devreg"
    }

    fn open(&self, rel_path: &str, full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        use crate::fd_table::{DeviceFile, DeviceType};

        let entry = self.reg().find(rel_path).ok_or(Error::NotFound)?;
        Ok(OpenFile::Device(DeviceFile {
            device_type: DeviceType::Dynamic {
                device_id: entry.device_id,
                class: entry.class,
                driver_endpoint: entry.driver_endpoint,
            },
            path: String::from(full_path),
            rights: u64::MAX,
        }))
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        let rel = rel_path.trim_start_matches('/');
        if !rel.is_empty() {
            return Err(Error::NotFound);
        }

        let dev_stat = DirEntryStat {
            size: 0,
            mode: 0o020666u32,
            mtime: 0,
            nlink: 1,
            uid: 0,
            gid: 0,
            blocks: 0,
        };

        Ok(self
            .reg()
            .entries
            .iter()
            .map(|e| {
                let name = alloc::string::String::from(
                    e.path.trim_start_matches("/dev/"),
                );
                DirEntry {
                    name,
                    is_dir: false,
                    stat: dev_stat,
                }
            })
            .collect())
    }
}

/// Virtual file content generator.
pub type VirtualFileGenerator = fn() -> Result<Vec<u8>>;

/// Virtual directory entry generator.
pub type VirtualDirGenerator = fn() -> Result<Vec<DirEntry>>;

/// Virtual filesystem entry.
pub enum VirtualEntry {
    File(VirtualFileGenerator),
    Dir(VirtualDirGenerator),
}

/// Virtual backend - dynamic content generation (procfs, sysfs, etc.)
pub struct VirtualBackend {
    name: &'static str,
    entries: &'static [(&'static str, VirtualEntry)],
}

impl VirtualBackend {
    pub const fn new(name: &'static str, entries: &'static [(&'static str, VirtualEntry)]) -> Self {
        Self { name, entries }
    }

    fn find_entry(&self, path: &str) -> Option<&VirtualEntry> {
        let path = path.trim_start_matches('/');
        for (name, entry) in self.entries {
            if *name == path {
                return Some(entry);
            }
        }
        None
    }
}

impl MountBackend for VirtualBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn open(&self, rel_path: &str, full_path: &str, _caller_tid: usize) -> Result<OpenFile> {
        let entry = self.find_entry(rel_path).ok_or(Error::NotFound)?;

        match entry {
            VirtualEntry::File(generator) => {
                let data = generator()?;
                Ok(OpenFile::Virtual(VirtualFile {
                    data,
                    path: String::from(full_path),
                    rights: u64::MAX,
                }))
            }
            VirtualEntry::Dir(_) => Err(Error::InvalidArgument), // Can't open dir as file
        }
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        if rel_path.is_empty() || rel_path == "/" {
            // List all entries at root
            return Ok(self
                .entries
                .iter()
                .filter(|(name, _)| !name.contains('/'))
                .map(|(name, entry)| {
                    let is_dir = matches!(entry, VirtualEntry::Dir(_));
                    let mode = if is_dir { 0o040555u32 } else { 0o100444u32 };
                    DirEntry {
                        name: String::from(*name),
                        is_dir,
                        stat: DirEntryStat { mode, nlink: 1, ..Default::default() },
                    }
                })
                .collect());
        }

        let entry = self.find_entry(rel_path).ok_or(Error::NotFound)?;
        match entry {
            VirtualEntry::Dir(generator) => {
                let raw = generator()?;
                Ok(raw.into_iter().map(|e| {
                    let mode = if e.is_dir { 0o040555u32 } else { 0o100444u32 };
                    DirEntry {
                        is_dir: e.is_dir,
                        stat: DirEntryStat { mode, nlink: 1, ..Default::default() },
                        name: e.name,
                    }
                }).collect())
            }
            VirtualEntry::File(_) => Err(Error::InvalidArgument),
        }
    }
}

/// Virtual file handle with generated content.
#[derive(Clone)]
pub struct VirtualFile {
    pub data: Vec<u8>,
    pub path: String,
    /// Effective capability rights. Always `u64::MAX` — virtual files are
    /// read-only generated content and not subject to FdInherit narrowing.
    pub rights: u64,
}

/// A single mount point configuration.
struct Mount {
    prefix: &'static str,
    backend: AnyMount,
}

/// Unified mount table.
pub struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Add a mount point with the given backend (sync or async).
    pub fn mount(&mut self, prefix: &'static str, backend: impl Into<AnyMount>) {
        self.mounts.push(Mount { prefix, backend: backend.into() });
    }

    /// Convenience: mount a synchronous backend.
    pub fn mount_sync(&mut self, prefix: &'static str, backend: Box<dyn MountBackend>) {
        self.mount(prefix, AnyMount::Sync(backend));
    }

    /// Convenience: mount an asynchronous backend.
    pub fn mount_async(&mut self, prefix: &'static str, backend: Box<dyn AsyncMountBackend>) {
        self.mount(prefix, AnyMount::Async(backend));
    }

    /// Convenience: mount initrd at a path.
    pub fn mount_initrd(&mut self, prefix: &'static str, data: &'static [u8]) {
        self.mount_sync(prefix, Box::new(InitrdBackend::new(data)));
    }

    /// Convenience: mount remote service at a path.
    pub fn mount_remote(
        &mut self,
        prefix: &'static str,
        endpoint: usize,
        service_name: &'static str,
    ) {
        self.mount_sync(prefix, Box::new(RemoteBackend::new(endpoint, service_name)));
    }

    /// Convenience: mount virtual filesystem at a path.
    pub fn mount_virtual(
        &mut self,
        prefix: &'static str,
        name: &'static str,
        entries: &'static [(&'static str, VirtualEntry)],
    ) {
        self.mount_sync(prefix, Box::new(VirtualBackend::new(name, entries)));
    }

    /// Open a file at the given absolute path.
    pub fn open(&self, path: &str, caller_tid: usize) -> Result<OpenFile> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.open(rel_path, path, caller_tid)
    }

    /// Read directory entries at the given absolute path.
    pub fn readdir(&self, path: &str, caller_tid: usize) -> Result<Vec<DirEntry>> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.readdir(rel_path, caller_tid)
    }

    /// Stat a path without reading directory entries.
    pub fn stat_by_path(&self, path: &str, caller_tid: usize) -> Result<DirEntryStat> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.stat_by_path(rel_path, path, caller_tid)
    }

    /// Iterate over registered mount prefixes (e.g. "/", "/proc", "/dev").
    /// Used by view-aware readdir merging when a view delegates the entire
    /// global tree (supervisor-style) and needs to surface mount points
    /// that don't exist as real directories in the underlying root.
    pub fn mount_prefixes(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.mounts.iter().map(|m| m.prefix)
    }

    pub fn unlink(&self, path: &str) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.unlink(rel_path)
    }

    pub fn mkdir(&self, path: &str, mode: usize) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.mkdir(rel_path, mode)
    }

    pub fn rmdir(&self, path: &str) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.rmdir(rel_path)
    }

    pub fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let (old_mount, rel_old) = self.resolve(old_path)?;
        let (new_mount, rel_new) = self.resolve(new_path)?;
        if old_mount.prefix != new_mount.prefix {
            return Err(Error::InvalidOperation);
        }
        old_mount.backend.as_sync()?.rename(rel_old, rel_new)
    }

    pub fn link(&self, old_path: &str, new_path: &str) -> Result<()> {
        let (old_mount, rel_old) = self.resolve(old_path)?;
        let (new_mount, rel_new) = self.resolve(new_path)?;
        if old_mount.prefix != new_mount.prefix {
            return Err(Error::InvalidOperation);
        }
        old_mount.backend.as_sync()?.link(rel_old, rel_new)
    }

    pub fn create_file(&self, path: &str, mode: usize) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.as_sync()?.create_file(rel_path, mode)
    }

    /// Read file data (for remote/virtual backends).
    pub fn read(
        &self,
        path_prefix: &str,
        file: &OpenFile,
        offset: usize,
        len: usize,
    ) -> Result<Vec<u8>> {
        let (mount, _) = self.resolve(path_prefix)?;
        mount.backend.as_sync()?.read(file, offset, len)
    }

    /// Check if a path matches a mount point.
    pub fn is_mounted(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    /// Get the sync backend for a path (for special handling).
    /// Returns `None` if the path is not mounted or is an async mount.
    pub fn get_backend<'a>(&'a self, path: &'a str) -> Option<&'a dyn MountBackend> {
        self.resolve(path).ok().and_then(|(m, _)| match &m.backend {
            AnyMount::Sync(b) => Some(b.as_ref()),
            AnyMount::Async(_) => None,
        })
    }

    /// Get the async backend for a path.
    /// Returns `None` if the path is not mounted or is a sync mount.
    pub fn get_async_backend<'a>(&'a self, path: &'a str) -> Option<&'a dyn AsyncMountBackend> {
        self.resolve(path).ok().and_then(|(m, _)| match &m.backend {
            AnyMount::Sync(_) => None,
            AnyMount::Async(b) => Some(b.as_ref()),
        })
    }

    /// Returns `true` if the mount at `path` is an async backend.
    pub fn is_async(&self, path: &str) -> bool {
        self.resolve(path).ok().is_some_and(|(m, _)| matches!(m.backend, AnyMount::Async(_)))
    }

    /// Split `path` into (mount-prefix, rel-within-mount). Returns
    /// `("/", path)` when no mount matches. Used by callers that need to
    /// recombine a backend-relative result back into an absolute path.
    /// When no mount matches, the fallback is `("/", path-without-leading-slash)`
    /// — note the rel side has its leading `/` trimmed. Callers that need to
    /// reconstruct an absolute path must take this into account.
    pub fn split_path<'b>(&self, path: &'b str) -> (&'static str, &'b str) {
        let mut best: (&'static str, &'b str) = ("/", path.trim_start_matches('/'));
        for mount in &self.mounts {
            if mount.prefix == "/" {
                continue;
            }
            if path == mount.prefix {
                if mount.prefix.len() > best.0.len() {
                    best = (mount.prefix, "");
                }
            } else if let Some(rest) = path.strip_prefix(mount.prefix) {
                if rest.starts_with('/') && mount.prefix.len() > best.0.len() {
                    best = (mount.prefix, rest.trim_start_matches('/'));
                }
            }
        }
        best
    }

    /// Resolve path to mount and relative path.
    fn resolve<'a>(&'a self, path: &'a str) -> Result<(&'a Mount, &'a str)> {
        let mut best: Option<(&'a Mount, &'a str)> = None;

        for mount in &self.mounts {
            if mount.prefix == "/" && path.starts_with('/') {
                let rel = path.trim_start_matches('/');
                if best.is_none() || mount.prefix.len() > best.unwrap().0.prefix.len() {
                    best = Some((mount, rel));
                }
                continue;
            }
            if path == mount.prefix {
                // Exact match (root of mount)
                let rel: &'a str = "";
                if best.is_none() || mount.prefix.len() > best.unwrap().0.prefix.len() {
                    best = Some((mount, rel));
                }
            } else if let Some(rest) = path.strip_prefix(mount.prefix) {
                // Check for proper path separator
                if rest.starts_with('/') {
                    let rel = rest.trim_start_matches('/');
                    if best.is_none() || mount.prefix.len() > best.unwrap().0.prefix.len() {
                        best = Some((mount, rel));
                    }
                }
            }
        }

        best.ok_or(Error::NotFound)
    }
}

/// In-memory filesystem wrapper for per-container ephemeral storage.
///
/// Wraps `MemFs` in a `RefCell` for interior mutability. Safe because
/// VFS is single-threaded. Not registered in the global MountTable —
/// held per-container and dispatched directly by VfsServer.
pub struct MemFsBackend {
    fs: RefCell<MemFs>,
}

impl MemFsBackend {
    pub fn new(quota_bytes: usize) -> Self {
        Self {
            fs: RefCell::new(MemFs::new(quota_bytes)),
        }
    }

    pub fn borrow(&self) -> core::cell::Ref<'_, MemFs> {
        self.fs.borrow()
    }

    pub fn borrow_mut(&self) -> core::cell::RefMut<'_, MemFs> {
        self.fs.borrow_mut()
    }
}

fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

impl Default for MountTable {
    fn default() -> Self {
        Self::new()
    }
}

fn dot_prefixed(path: &str) -> String {
    let mut prefixed = String::with_capacity(path.len() + 2);
    prefixed.push_str("./");
    prefixed.push_str(path);
    prefixed
}
