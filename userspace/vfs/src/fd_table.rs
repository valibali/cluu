//! Per-client file descriptor table for VFS.
//!
//! Each client is identified by authenticated IPC sender identity (thread id
//! from kernel recv metadata). Any caller-provided client hint is ignored.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A file entry backed by memory (initrd).
#[derive(Clone, Copy)]
pub struct FileEntry {
    pub base: usize,
    pub offset: usize,
    pub size: usize,
    /// Effective capability rights for this open file (IPC rights bitmask).
    /// Set to `u64::MAX` on initial open; clamped on FdInherit derive.
    pub rights: u64,
}

/// A file entry backed by a remote service (ext2 via virtio-blk).
#[derive(Clone)]
pub struct Ext2Entry {
    /// Remote filesystem endpoint that owns this inode.
    pub endpoint: usize,
    pub inode: u32,
    pub size: usize,
    /// Original path used to open this file (for caching).
    #[allow(dead_code)]
    pub path: String,
    /// Effective capability rights for this open file (IPC rights bitmask).
    /// Set to `u64::MAX` on initial open; clamped on FdInherit derive.
    pub rights: u64,
}

/// A file entry backed by a virtual filesystem (procfs, etc.)
#[allow(dead_code)]
#[derive(Clone)]
pub struct VirtualEntry {
    pub data: Vec<u8>,
    pub path: String,
}

/// Device type for special device files.
#[derive(Clone, Copy, PartialEq)]
pub enum DeviceType {
    Null,
    Zero,
    Urandom,
    /// /dev/tty1../dev/tty4 — specific VT (1-indexed, vt_index is 0-indexed)
    Tty {
        vt_index: u8,
        endpoint: usize,
    },
    /// /dev/tty0 — alias for the currently active VT
    Tty0 {
        endpoint: usize,
    },
    /// /dev/console — system console (alias for tty:0)
    Console {
        endpoint: usize,
    },
    /// /dev/fb0 — primary framebuffer device
    Fb {
        phys: u64,
        size: u64,
        width: u32,
        height: u32,
        pitch: u32,
        bpp: u32,
    },
}

/// A file entry backed by an in-memory filesystem (per-container tmpfs).
#[derive(Clone)]
pub struct MemFsEntry {
    pub container_id: u64,
    pub inode_id: usize,
    #[allow(dead_code)]
    pub memfs_path: String,
    pub size: usize,
    /// Effective capability rights for this open file (IPC rights bitmask).
    /// Set to `u64::MAX` on initial open; clamped on FdInherit derive.
    pub rights: u64,
}

/// A device file handle.
#[derive(Clone)]
pub struct DeviceFile {
    pub device_type: DeviceType,
    #[allow(dead_code)]
    pub path: String,
    /// Effective capability rights for this open file (IPC rights bitmask).
    /// Set to `u64::MAX` on initial open; clamped on FdInherit derive.
    pub rights: u64,
}

/// A pseudo-terminal slave file handle.
#[derive(Clone)]
pub struct PtsFile {
    /// The pts id (matches the number in `/dev/pts/<id>`).
    pub pts_id: u32,
    /// Full path, e.g. `/dev/pts/5`.
    #[allow(dead_code)]
    pub path: String,
    /// Effective capability rights for this open file (IPC rights bitmask).
    /// Set to `u64::MAX` on initial open; clamped on FdInherit derive.
    pub rights: u64,
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
    /// Special device file (/dev/null, /dev/zero, /dev/urandom)
    Device(DeviceFile),
    /// In-memory filesystem file (per-container tmpfs)
    MemFs(MemFsEntry),
    /// Pseudo-terminal slave (`/dev/pts/<id>`).
    Pts(PtsFile),
}

impl OpenFile {
    pub fn size(&self) -> usize {
        match self {
            OpenFile::Memory(e) => e.size,
            OpenFile::Ext2(e) => e.size,
            OpenFile::Virtual(v) => v.data.len(),
            OpenFile::Device(_) => 0,
            OpenFile::MemFs(e) => e.size,
            OpenFile::Pts(_) => 0,
        }
    }

    /// Get the mount prefix this file belongs to (for read routing).
    #[allow(dead_code)]
    pub fn mount_prefix(&self) -> Option<&str> {
        match self {
            OpenFile::Ext2(_) => Some("/mnt/disk"),
            OpenFile::Virtual(v) => Some(v.path.as_str()),
            OpenFile::Memory(_) => None,
            OpenFile::Device(d) => Some(d.path.as_str()),
            OpenFile::MemFs(_) => None,
            OpenFile::Pts(_) => Some("/dev/pts"),
        }
    }

    /// Effective capability rights for this open file entry.
    ///
    /// Returns `u64::MAX` for variants whose rights are not narrowed (Virtual),
    /// and the stored rights mask for variants that track narrowing
    /// (Memory, Ext2, Device, MemFs, Pts).
    pub fn rights(&self) -> u64 {
        match self {
            OpenFile::Memory(e) => e.rights,
            OpenFile::Ext2(e) => e.rights,
            OpenFile::Virtual(_) => u64::MAX,
            OpenFile::Device(e) => e.rights,
            OpenFile::MemFs(e) => e.rights,
            OpenFile::Pts(e) => e.rights,
        }
    }

    /// Set the effective capability rights on this open file entry.
    /// Used by `handle_derive_child_fd` to record the clamped child rights.
    pub fn set_rights(&mut self, rights: u64) {
        match self {
            OpenFile::Memory(e) => e.rights = rights,
            OpenFile::Ext2(e) => e.rights = rights,
            OpenFile::Virtual(_) => {}
            OpenFile::Device(e) => e.rights = rights,
            OpenFile::MemFs(e) => e.rights = rights,
            OpenFile::Pts(e) => e.rights = rights,
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
        let table = self
            .clients
            .entry(client_id)
            .or_insert_with(ClientTable::new);
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

    /// Close every open fd for `client_id` and return the removed entries so
    /// the caller can fire fd-type-specific teardown (e.g. PTS dec_ref).
    ///
    /// Used by `handle_set_view` cleared-path so PTS ref-counts decrement when
    /// a process exits — otherwise PTS_CLOSED never fires and the cluuterm
    /// window stays open after the shell terminates.
    pub fn close_all_for_client(&mut self, client_id: usize) -> Vec<OpenFile> {
        let Some(table) = self.clients.remove(&client_id) else {
            return Vec::new();
        };
        table.entries.into_values().collect()
    }

    /// Return the effective capability rights for `(client_id, fd)`, or
    /// `None` if the entry does not exist.
    pub fn rights(&self, client_id: usize, fd: usize) -> Option<u64> {
        self.get(client_id, fd).map(|f| f.rights())
    }
}
