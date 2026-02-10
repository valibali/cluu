#![allow(unused)]
//! Unified mount table for the VFS service.
//!
//! All mount points are declared in one place with a clean plugin architecture.
//! Supported backends:
//! - Initrd: Direct memory access to tar archive
//! - Remote: IPC forwarding to external service (e.g., virtio-blk)
//! - Virtual: Dynamic content generation (e.g., procfs)

use crate::fd_table::{Ext2Entry, FileEntry, OpenFile};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::ipc::{call_with_payload, call_with_reply_buf};
use libcluu::tar::{find_member, list_entries};
use libcluu::types::Message;
use libcluu::{Error, Result};

/// IPC labels for remote filesystem operations
const FS_OPEN: u32 = 0x300;
const FS_READ: u32 = 0x302;
const FS_READDIR: u32 = 0x304;
const FS_UNLINK: u32 = 0x307;
const FS_MKDIR: u32 = 0x308;
const FS_RMDIR: u32 = 0x309;
const FS_RENAME: u32 = 0x30A;
const FS_CREATE: u32 = 0x30B;
const IPC_MESSAGE_MAX: usize = 256;

/// Directory entry for readdir results.
#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Mount backend trait - all mount types implement this.
pub trait MountBackend: Send + Sync {
    /// Backend name for debugging.
    fn name(&self) -> &'static str;

    /// Open a file at the given relative path.
    /// full_path is the original absolute path (for caching).
    fn open(&self, rel_path: &str, full_path: &str) -> Result<OpenFile>;

    /// Read directory entries at the given relative path.
    fn readdir(&self, rel_path: &str) -> Result<Vec<DirEntry>>;

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

    fn create_file(&self, rel_path: &str, mode: usize) -> Result<()> {
        let _ = (rel_path, mode);
        Err(Error::NotImplemented)
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

    fn open(&self, rel_path: &str, _full_path: &str) -> Result<OpenFile> {
        let slice = find_member(self.data, rel_path)
            .or_else(|| find_member(self.data, &dot_prefixed(rel_path)))
            .ok_or(Error::NotFound)?;

        let base = self.data.as_ptr() as usize;
        let offset = slice.as_ptr() as usize - base;

        Ok(OpenFile::Memory(FileEntry {
            base,
            offset,
            size: slice.len(),
        }))
    }

    fn readdir(&self, rel_path: &str) -> Result<Vec<DirEntry>> {
        let entries = list_entries(self.data, rel_path);
        Ok(entries
            .into_iter()
            .map(|e| DirEntry {
                name: e.name,
                is_dir: e.is_dir,
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

    fn open(&self, rel_path: &str, full_path: &str) -> Result<OpenFile> {
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
        }))
    }

    fn readdir(&self, rel_path: &str) -> Result<Vec<DirEntry>> {
        let req = Message::new(FS_READDIR, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) =
            call_with_reply_buf(self.endpoint, &req, rel_path.as_bytes(), &mut reply_buf)?;

        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::NotFound);
        }

        let entry_count = reply.words[1];
        let data_start = size_of::<Message>();
        let data = &reply_buf[data_start..data_start + payload_len];

        // Parse entries: [name_len: u8, is_dir: u8, name bytes...]
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
                entries.push(DirEntry {
                    name: String::from(name),
                    is_dir,
                });
            }
            offset += name_len;
        }

        Ok(entries)
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

        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::InvalidState);
        }

        let bytes_read = reply.words[1].min(max_len);
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

    fn create_file(&self, rel_path: &str, mode: usize) -> Result<()> {
        let req = Message::new(FS_CREATE, [rel_path.len(), 0, mode, 0, 0, 0], 3);
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(self.endpoint, &req, rel_path.as_bytes(), &mut reply)?;
        parse_status(reply.words[0])
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

    fn open(&self, rel_path: &str, full_path: &str) -> Result<OpenFile> {
        let entry = self.find_entry(rel_path).ok_or(Error::NotFound)?;

        match entry {
            VirtualEntry::File(generator) => {
                let data = generator()?;
                Ok(OpenFile::Virtual(VirtualFile {
                    data,
                    path: String::from(full_path),
                }))
            }
            VirtualEntry::Dir(_) => Err(Error::InvalidArgument), // Can't open dir as file
        }
    }

    fn readdir(&self, rel_path: &str) -> Result<Vec<DirEntry>> {
        if rel_path.is_empty() || rel_path == "/" {
            // List all entries at root
            return Ok(self
                .entries
                .iter()
                .filter(|(name, _)| !name.contains('/'))
                .map(|(name, entry)| DirEntry {
                    name: String::from(*name),
                    is_dir: matches!(entry, VirtualEntry::Dir(_)),
                })
                .collect());
        }

        let entry = self.find_entry(rel_path).ok_or(Error::NotFound)?;
        match entry {
            VirtualEntry::Dir(generator) => generator(),
            VirtualEntry::File(_) => Err(Error::InvalidArgument),
        }
    }
}

/// Virtual file handle with generated content.
#[derive(Clone)]
pub struct VirtualFile {
    pub data: Vec<u8>,
    pub path: String,
}

/// A single mount point configuration.
struct Mount {
    prefix: &'static str,
    backend: Box<dyn MountBackend>,
}

/// Unified mount table.
pub struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    /// Add a mount point with the given backend.
    pub fn mount(&mut self, prefix: &'static str, backend: Box<dyn MountBackend>) {
        self.mounts.push(Mount { prefix, backend });
    }

    /// Convenience: mount initrd at a path.
    pub fn mount_initrd(&mut self, prefix: &'static str, data: &'static [u8]) {
        self.mount(prefix, Box::new(InitrdBackend::new(data)));
    }

    /// Convenience: mount remote service at a path.
    pub fn mount_remote(
        &mut self,
        prefix: &'static str,
        endpoint: usize,
        service_name: &'static str,
    ) {
        self.mount(prefix, Box::new(RemoteBackend::new(endpoint, service_name)));
    }

    /// Convenience: mount virtual filesystem at a path.
    pub fn mount_virtual(
        &mut self,
        prefix: &'static str,
        name: &'static str,
        entries: &'static [(&'static str, VirtualEntry)],
    ) {
        self.mount(prefix, Box::new(VirtualBackend::new(name, entries)));
    }

    /// Open a file at the given absolute path.
    pub fn open(&self, path: &str) -> Result<OpenFile> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.open(rel_path, path)
    }

    /// Read directory entries at the given absolute path.
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.readdir(rel_path)
    }

    pub fn unlink(&self, path: &str) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.unlink(rel_path)
    }

    pub fn mkdir(&self, path: &str, mode: usize) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.mkdir(rel_path, mode)
    }

    pub fn rmdir(&self, path: &str) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.rmdir(rel_path)
    }

    pub fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let (old_mount, rel_old) = self.resolve(old_path)?;
        let (new_mount, rel_new) = self.resolve(new_path)?;
        if old_mount.prefix != new_mount.prefix {
            return Err(Error::InvalidOperation);
        }
        old_mount.backend.rename(rel_old, rel_new)
    }

    pub fn create_file(&self, path: &str, mode: usize) -> Result<()> {
        let (mount, rel_path) = self.resolve(path)?;
        mount.backend.create_file(rel_path, mode)
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
        mount.backend.read(file, offset, len)
    }

    /// Check if a path matches a mount point.
    pub fn is_mounted(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    /// Get the backend for a path (for special handling).
    pub fn get_backend<'a>(&'a self, path: &'a str) -> Option<&'a dyn MountBackend> {
        self.resolve(path).ok().map(|(m, _)| m.backend.as_ref())
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
            } else if path.starts_with(mount.prefix) {
                // Check for proper path separator
                let rest = &path[mount.prefix.len()..];
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
