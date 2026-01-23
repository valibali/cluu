//! Per-client file descriptor table for VFS.
//!
//! Each client is identified by a caller-provided ID (typically its registry
//! control endpoint token). This keeps FD namespaces isolated across processes.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A file entry backed by memory (initrd).
#[derive(Clone, Copy)]
pub struct FileEntry {
    pub base: usize,
    pub offset: usize,
    pub size: usize,
}

/// A file entry backed by a remote service (ext2 via virtio-blk).
#[derive(Clone)]
pub struct Ext2Entry {
    /// Remote filesystem endpoint that owns this inode.
    pub endpoint: usize,
    pub inode: u32,
    pub size: usize,
    /// Original path used to open this file (for caching).
    pub path: String,
}

/// A file entry backed by a virtual filesystem (procfs, etc.)
#[derive(Clone)]
pub struct VirtualEntry {
    pub data: Vec<u8>,
    pub path: String,
}

/// Open file handle.
#[derive(Clone)]
pub enum OpenFile {
    /// Memory-backed file (initrd)
    Memory(FileEntry),
    /// Remote-backed file (ext2 via virtio-blk IPC)
    Ext2(Ext2Entry),
    /// Virtual file with generated content
    Virtual(crate::mount::VirtualFile),
}

impl OpenFile {
    pub fn size(&self) -> usize {
        match self {
            OpenFile::Memory(e) => e.size,
            OpenFile::Ext2(e) => e.size,
            OpenFile::Virtual(v) => v.data.len(),
        }
    }

    /// Get the mount prefix this file belongs to (for read routing).
    pub fn mount_prefix(&self) -> Option<&str> {
        match self {
            OpenFile::Ext2(_) => Some("/mnt/disk"),
            OpenFile::Virtual(v) => Some(v.path.as_str()),
            OpenFile::Memory(_) => None,
        }
    }
}

struct ClientTable {
    next_fd: usize,
    entries: BTreeMap<usize, OpenFile>,
}

impl ClientTable {
    fn new() -> Self {
        Self {
            next_fd: 4,
            entries: BTreeMap::new(),
        }
    }
}

pub struct FdTable {
    clients: BTreeMap<usize, ClientTable>,
}

impl FdTable {
    pub fn new() -> Self {
        Self {
            clients: BTreeMap::new(),
        }
    }

    pub fn open(&mut self, client_id: usize, file: OpenFile) -> usize {
        let table = self.clients.entry(client_id).or_insert_with(ClientTable::new);
        let fd = table.next_fd;
        table.next_fd += 1;
        table.entries.insert(fd, file);
        fd
    }

    pub fn get(&self, client_id: usize, fd: usize) -> Option<&OpenFile> {
        self.clients.get(&client_id)?.entries.get(&fd)
    }

    pub fn get_mut(&mut self, client_id: usize, fd: usize) -> Option<&mut OpenFile> {
        self.clients.get_mut(&client_id)?.entries.get_mut(&fd)
    }

    pub fn close(&mut self, client_id: usize, fd: usize) {
        if let Some(table) = self.clients.get_mut(&client_id) {
            table.entries.remove(&fd);
        }
    }
}
