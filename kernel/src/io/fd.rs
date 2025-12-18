/*
 * File Descriptor Table
 *
 * Per-thread file descriptor table for managing open devices/files.
 * Each entry maps an integer FD to a Device trait object.
 *
 * Standard FDs:
 * - 0: stdin  (read)
 * - 1: stdout (write)
 * - 2: stderr (write)
 *
 * FDs 3+ are allocated dynamically for opened files.
 */

use super::device::{Device, Errno};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

/// Per-thread file descriptor table
///
/// Manages the mapping from file descriptor integers to Device trait objects.
/// Uses Arc for reference counting, allowing multiple FDs to point to the
/// same device (e.g., stdin/stdout/stderr all pointing to TTY0).
pub struct FileDescriptorTable {
    fds: BTreeMap<i32, Arc<dyn Device>>,
    next_fd: i32,
}

impl FileDescriptorTable {
    /// Create a new empty file descriptor table
    ///
    /// FDs 0, 1, 2 are reserved for stdin/stdout/stderr and must be
    /// explicitly inserted by the caller.
    pub fn new() -> Self {
        Self {
            fds: BTreeMap::new(),
            next_fd: 3, // 0, 1, 2 reserved for stdin/stdout/stderr
        }
    }

    /// Get device by file descriptor
    ///
    /// Returns a cloned Arc to the device, or EBADF if FD is invalid.
    pub fn get(&self, fd: i32) -> Result<Arc<dyn Device>, Errno> {
        self.fds.get(&fd).cloned().ok_or(Errno::EBADF)
    }

    /// Insert device at specific file descriptor
    ///
    /// Used for initializing stdin/stdout/stderr (FDs 0, 1, 2).
    /// Overwrites existing FD if present.
    pub fn insert(&mut self, fd: i32, device: Arc<dyn Device>) {
        self.fds.insert(fd, device);
    }

    /// Allocate new file descriptor (auto-assign)
    ///
    /// Assigns the next available FD (>= 3) and inserts the device.
    /// Returns the allocated FD.
    pub fn alloc(&mut self, device: Arc<dyn Device>) -> i32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.fds.insert(fd, device);
        fd
    }

    /// Close a file descriptor
    ///
    /// Removes the FD from the table. Returns EBADF if FD doesn't exist.
    /// The device is automatically cleaned up when the last Arc is dropped.
    pub fn close(&mut self, fd: i32) -> Result<(), Errno> {
        self.fds.remove(&fd).ok_or(Errno::EBADF)?;
        Ok(())
    }

    /// Duplicate file descriptor (dup2 semantics)
    ///
    /// Makes newfd refer to the same device as oldfd.
    /// If newfd was open, it is closed first.
    /// Returns newfd on success, or an error if oldfd is invalid.
    pub fn dup(&mut self, oldfd: i32, newfd: i32) -> Result<i32, Errno> {
        let device = self.get(oldfd)?;
        self.insert(newfd, device);
        Ok(newfd)
    }

    /// Get number of open file descriptors
    pub fn count(&self) -> usize {
        self.fds.len()
    }

    /// Check if a file descriptor is valid
    pub fn is_valid(&self, fd: i32) -> bool {
        self.fds.contains_key(&fd)
    }
}

impl Default for FileDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Tests would require mock Device implementation
    // Can be added when testing infrastructure is set up
}
