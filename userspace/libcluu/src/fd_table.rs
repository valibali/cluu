//! Client-side file descriptor table for POSIX compatibility.
//!
//! This module provides a process-local mapping from POSIX file descriptors
//! to CLUU IPC endpoints. Each fd tracks its capabilities (read, write, seek, tty).
//!
//! # Default FDs
//!
//! At process startup, fds 0-3 are pre-populated:
//! - fd 0 (stdin): READ | IS_TTY
//! - fd 1 (stdout): WRITE | IS_TTY
//! - fd 2 (stderr): WRITE | IS_TTY
//! - fd 3 (stdlog): WRITE | IS_TTY

use alloc::collections::BTreeMap;
use bitflags::bitflags;
use spin::Mutex;

bitflags! {
    /// Capabilities that a file descriptor supports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FdCaps: u32 {
        /// Can read from this fd
        const READ = 0b0001;
        /// Can write to this fd
        const WRITE = 0b0010;
        /// Supports lseek (files, not TTY/pipes)
        const SEEK = 0b0100;
        /// Is a terminal (for _isatty)
        const IS_TTY = 0b1000;
        /// Is a pipe endpoint
        const IS_PIPE = 0b0001_0000;
    }
}

/// Entry in the file descriptor table.
#[derive(Debug, Clone)]
pub struct FdEntry {
    /// IPC endpoint token for this fd
    pub endpoint: usize,
    /// Capabilities this fd supports
    pub caps: FdCaps,
    /// Current seek position (for seekable fds)
    pub position: u64,
    /// VFS-side fd if opened via VFS (None for TTY/direct endpoints)
    pub remote_fd: Option<usize>,
    /// Client ID for VFS operations
    pub client_id: usize,
    /// Cached file size (if known)
    pub file_size: Option<usize>,
    /// Cached mode bits (if known)
    pub file_mode: Option<usize>,
    /// Procmgr's pipe-table handle for pipe-end fds. None for non-pipe entries.
    /// Used at close time to send PROCMGR_PIPE_CLOSE_LABEL.
    pub pipe_id: Option<usize>,
}

impl FdEntry {
    /// Create a TTY fd entry (stdin/stdout/stderr/stdlog).
    pub fn tty(endpoint: usize, readable: bool, writable: bool) -> Self {
        let mut caps = FdCaps::IS_TTY;
        if readable {
            caps |= FdCaps::READ;
        }
        if writable {
            caps |= FdCaps::WRITE;
        }
        Self {
            endpoint,
            caps,
            position: 0,
            remote_fd: None,
            client_id: 0,
            file_size: None,
            file_mode: None,
            pipe_id: None,
        }
    }

    /// Create a VFS file fd entry.
    pub fn file(
        endpoint: usize,
        remote_fd: usize,
        client_id: usize,
        readable: bool,
        writable: bool,
    ) -> Self {
        let mut caps = FdCaps::SEEK; // Files are seekable
        if readable {
            caps |= FdCaps::READ;
        }
        if writable {
            caps |= FdCaps::WRITE;
        }
        Self {
            endpoint,
            caps,
            position: 0,
            remote_fd: Some(remote_fd),
            client_id,
            file_size: None,
            file_mode: None,
            pipe_id: None,
        }
    }

    /// Check if this fd is a TTY.
    pub fn is_tty(&self) -> bool {
        self.caps.contains(FdCaps::IS_TTY)
    }

    /// Check if this fd is readable.
    pub fn is_readable(&self) -> bool {
        self.caps.contains(FdCaps::READ)
    }

    /// Check if this fd is writable.
    pub fn is_writable(&self) -> bool {
        self.caps.contains(FdCaps::WRITE)
    }

    /// Check if this fd supports seeking.
    pub fn is_seekable(&self) -> bool {
        self.caps.contains(FdCaps::SEEK)
    }

    /// Check if this fd is a pipe endpoint.
    pub fn is_pipe(&self) -> bool {
        self.caps.contains(FdCaps::IS_PIPE)
    }

    /// Create a pipe read-end fd entry.
    pub fn pipe_read(endpoint: usize, pipe_id: usize) -> Self {
        Self {
            endpoint,
            caps: FdCaps::READ | FdCaps::IS_PIPE,
            position: 0,
            remote_fd: None,
            client_id: 0,
            file_size: None,
            file_mode: None,
            pipe_id: Some(pipe_id),
        }
    }

    /// Create a pipe write-end fd entry.
    pub fn pipe_write(endpoint: usize, pipe_id: usize) -> Self {
        Self {
            endpoint,
            caps: FdCaps::WRITE | FdCaps::IS_PIPE,
            position: 0,
            remote_fd: None,
            client_id: 0,
            file_size: None,
            file_mode: None,
            pipe_id: Some(pipe_id),
        }
    }
}

/// Process-local file descriptor table.
pub struct FdTable {
    entries: BTreeMap<i32, FdEntry>,
    next_fd: i32,
}

impl FdTable {
    /// Create a new empty fd table.
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_fd: 4, // 0-3 are reserved for stdio
        }
    }

    /// Initialize stdio fds from boot tokens.
    ///
    /// Call this once at process startup. `pipe_mask` indicates which fds
    /// are pipe endpoints (bit 0 = stdin, bit 1 = stdout, etc).
    pub fn init_stdio(
        &mut self,
        stdin: usize,
        stdout: usize,
        stderr: usize,
        stdlog: usize,
        pipe_mask: u8,
    ) {
        let fds: [(i32, usize, bool, bool); 4] = [
            (0, stdin, true, false),  // stdin: readable
            (1, stdout, false, true), // stdout: writable
            (2, stderr, false, true), // stderr: writable
            (3, stdlog, false, true), // stdlog: writable
        ];

        for &(fd_num, token, readable, writable) in &fds {
            if pipe_mask & (1 << fd_num) != 0 {
                // Inherited pipe endpoint: no procmgr pipe_id on this side.
                let caps = if readable {
                    FdCaps::READ | FdCaps::IS_PIPE
                } else {
                    FdCaps::WRITE | FdCaps::IS_PIPE
                };
                let entry = FdEntry {
                    endpoint: token,
                    caps,
                    position: 0,
                    remote_fd: None,
                    client_id: 0,
                    file_size: None,
                    file_mode: None,
                    pipe_id: None,
                };
                self.entries.insert(fd_num, entry);
            } else {
                self.entries
                    .insert(fd_num, FdEntry::tty(token, readable, writable));
            }
        }
        self.next_fd = 4;
    }

    /// Get an fd entry by number.
    pub fn get(&self, fd: i32) -> Option<&FdEntry> {
        self.entries.get(&fd)
    }

    /// Get a mutable fd entry by number.
    pub fn get_mut(&mut self, fd: i32) -> Option<&mut FdEntry> {
        self.entries.get_mut(&fd)
    }

    /// Insert a new fd entry, returning the assigned fd number.
    pub fn insert(&mut self, entry: FdEntry) -> i32 {
        let fd = self.next_fd;
        self.entries.insert(fd, entry);
        self.next_fd += 1;
        fd
    }

    /// Insert an entry at a specific fd number.
    ///
    /// Used for dup2() or restoring specific fds.
    pub fn insert_at(&mut self, fd: i32, entry: FdEntry) {
        self.entries.insert(fd, entry);
        if fd >= self.next_fd {
            self.next_fd = fd + 1;
        }
    }

    /// Remove an fd entry.
    pub fn remove(&mut self, fd: i32) -> Option<FdEntry> {
        self.entries.remove(&fd)
    }

    /// Check if an fd exists.
    pub fn contains(&self, fd: i32) -> bool {
        self.entries.contains_key(&fd)
    }

    /// Get the number of open fds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return true iff any fd in the table has the given pipe_id.
    pub fn any_with_pipe_id(&self, pipe_id: usize) -> bool {
        self.entries.values().any(|e| e.pipe_id == Some(pipe_id))
    }

    /// Iterate over all (fd, entry) pairs.
    pub fn all_fds(&self) -> impl Iterator<Item = (i32, &FdEntry)> {
        self.entries.iter().map(|(&fd, entry)| (fd, entry))
    }

    /// Duplicate an fd (like dup()).
    pub fn dup(&mut self, old_fd: i32) -> Option<i32> {
        let entry = self.entries.get(&old_fd)?.clone();
        Some(self.insert(entry))
    }

    /// Replace an fd entry unconditionally.
    ///
    /// Used by `apply_redirections` at startup to substitute a TTY entry
    /// with a VFS-backed file. Does NOT close the old entry (TTY endpoints
    /// are borrowed, not owned by the fd table).
    pub fn replace(&mut self, fd: i32, entry: FdEntry) {
        self.entries.insert(fd, entry);
        if fd >= self.next_fd {
            self.next_fd = fd + 1;
        }
    }

    /// Duplicate an fd to a specific number (like dup2()).
    pub fn dup2(&mut self, old_fd: i32, new_fd: i32) -> Option<i32> {
        if old_fd == new_fd {
            return if self.contains(old_fd) {
                Some(new_fd)
            } else {
                None
            };
        }
        let entry = self.entries.get(&old_fd)?.clone();
        self.entries.insert(new_fd, entry);
        if new_fd >= self.next_fd {
            self.next_fd = new_fd + 1;
        }
        Some(new_fd)
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Global file descriptor table for the process.
///
/// Protected by a spinlock for thread safety.
pub static FD_TABLE: Mutex<FdTable> = Mutex::new(FdTable::new());

/// Initialize the global fd table with stdio endpoints.
///
/// This should be called once at process startup from `__cluu_init()`.
/// Reads pipe_mask from PARAM_FD_PIPE_MASK to detect pipe-backed fds.
///
/// If the ProcessInfo page contains a VFS fd trailer (PARAM_FD_VFS_OFFSET /
/// PARAM_FD_VFS_LEN), fds with non-zero vfs_client_id are rehydrated as
/// `FdEntry::file(...)` so that reads/writes use the VFS protocol path
/// (`VFS_READ_LABEL` / `VFS_WRITE_LABEL`) rather than the TTY path.
pub fn init_stdio() {
    let info = crate::boot::process_info();
    let pipe_mask = info.params[crate::boot::PARAM_FD_PIPE_MASK] as u8;

    // Read the optional VFS fd trailer.  Layout:
    //   4 × 16 bytes = 64 bytes total
    //   entry i (fd i): [u64 vfs_client_id LE][u64 vfs_remote_fd LE]
    //   vfs_client_id == 0 → fd is not VFS-backed (legacy tty/pipe path).
    let trailer_off = info.params[crate::boot::PARAM_FD_VFS_OFFSET] as usize;
    let trailer_len = info.params[crate::boot::PARAM_FD_VFS_LEN] as usize;

    let vfs_meta: [(usize, usize); 4] = if trailer_len >= 64 && trailer_off != 0 {
        // Safety: PROCESS_INFO_ADDR is a read-only page mapped by procmgr before
        // this process started.  trailer_off is a byte offset within that 4 KB
        // page, written by procmgr and bounded by the page-fit check there.
        let page_base = crate::boot::PROCESS_INFO_ADDR & !(4096 - 1);
        let base = (page_base + trailer_off) as *const u8;
        let mut out = [(0usize, 0usize); 4];
        unsafe {
            for i in 0..4 {
                let p = base.add(i * 16) as *const u64;
                out[i].0 = p.read_unaligned() as usize;          // vfs_client_id
                out[i].1 = p.add(1).read_unaligned() as usize;   // vfs_remote_fd
            }
        }
        out
    } else {
        [(0usize, 0usize); 4]
    };

    let stdin  = info.tokens[crate::boot::TOKEN_STDIN];
    let stdout = info.tokens[crate::boot::TOKEN_STDOUT];
    let stderr = info.tokens[crate::boot::TOKEN_STDERR];
    let stdlog = info.tokens[crate::boot::TOKEN_STDLOG];

    let mut table = FD_TABLE.lock();

    // fd (num, token, readable, writable)
    let fds: [(i32, usize, bool, bool); 4] = [
        (0, stdin,  true,  false),
        (1, stdout, false, true),
        (2, stderr, false, true),
        (3, stdlog, false, true),
    ];

    for &(fd_num, token, readable, writable) in &fds {
        let (vcid, vfd) = vfs_meta[fd_num as usize];
        let entry = if vcid != 0 && vfd != 0 {
            // VFS-backed fd: build a file entry so reads/writes use VFS protocol.
            FdEntry::file(token, vfd, vcid, readable, writable)
        } else if pipe_mask & (1 << fd_num) != 0 {
            // Inherited pipe endpoint.
            let caps = if readable {
                FdCaps::READ | FdCaps::IS_PIPE
            } else {
                FdCaps::WRITE | FdCaps::IS_PIPE
            };
            FdEntry {
                endpoint: token,
                caps,
                position: 0,
                remote_fd: None,
                client_id: 0,
                file_size: None,
                file_mode: None,
                pipe_id: None,
            }
        } else {
            FdEntry::tty(token, readable, writable)
        };
        table.entries.insert(fd_num, entry);
    }
    table.next_fd = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fd_table_basic() {
        let mut table = FdTable::new();
        table.init_stdio(100, 101, 102, 103, 0);

        assert!(table.contains(0));
        assert!(table.contains(1));
        assert!(table.contains(2));
        assert!(table.contains(3));
        assert!(!table.contains(4));

        let stdin = table.get(0).unwrap();
        assert!(stdin.is_tty());
        assert!(stdin.is_readable());
        assert!(!stdin.is_writable());

        let stdout = table.get(1).unwrap();
        assert!(stdout.is_tty());
        assert!(!stdout.is_readable());
        assert!(stdout.is_writable());
    }

    #[test]
    fn test_fd_insert_and_remove() {
        let mut table = FdTable::new();
        let entry = FdEntry::file(200, 10, 1, true, true);
        let fd = table.insert(entry);
        assert_eq!(fd, 4);

        assert!(table.contains(4));
        let file = table.get(4).unwrap();
        assert!(file.is_seekable());
        assert!(!file.is_tty());

        table.remove(4);
        assert!(!table.contains(4));
    }

    #[test]
    fn test_fd_dup() {
        let mut table = FdTable::new();
        table.init_stdio(100, 101, 102, 103, 0);

        let new_fd = table.dup(1).unwrap();
        assert_eq!(new_fd, 4);

        let orig = table.get(1).unwrap();
        let duped = table.get(4).unwrap();
        assert_eq!(orig.endpoint, duped.endpoint);
        assert_eq!(orig.caps, duped.caps);
    }

    #[test]
    fn test_fd_dup2() {
        let mut table = FdTable::new();
        table.init_stdio(100, 101, 102, 103, 0);

        // dup stdout to fd 10
        let result = table.dup2(1, 10).unwrap();
        assert_eq!(result, 10);
        assert!(table.contains(10));

        let duped = table.get(10).unwrap();
        assert_eq!(duped.endpoint, 101);
    }
}
