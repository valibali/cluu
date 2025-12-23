/*
 * VFS Protocol Definitions
 *
 * This module defines the message protocol for communication between
 * kernel VFS stub and userspace VFS server via IPC.
 *
 * Message Layout (256 bytes total):
 * - Offset 0-3:   request_type (u32) - Operation type
 * - Offset 4-7:   result (i32) - Return value (0 = success, negative = error)
 * - Offset 8-15:  request_id (u64) - Unique request ID for matching responses
 * - Offset 16-23: reply_port_id (u64) - Port ID for sending response
 * - Offset 24-31: fd (i32) + flags (i32) - File descriptor and open flags
 * - Offset 32-39: offset (u64) - File offset for read/write/seek
 * - Offset 40-47: count (u64) - Byte count for read/write
 * - Offset 48-55: shmem_id (i64) - Shared memory ID for fsitem (or -1 if none)
 * - Offset 56-255: data (200 bytes) - Path string or buffer data
 */

use crate::scheduler::ipc::Message;

/// VFS request types
pub const VFS_OPEN: u32 = 1;
pub const VFS_READ: u32 = 2;
pub const VFS_WRITE: u32 = 3;
pub const VFS_CLOSE: u32 = 4;
pub const VFS_STAT: u32 = 5;
pub const VFS_LSEEK: u32 = 6;

/// VFS error codes (negative values returned in result field)
pub const VFS_SUCCESS: i32 = 0;
pub const VFS_ERR_NOT_FOUND: i32 = -2;      // ENOENT: No such file or directory
pub const VFS_ERR_ACCESS: i32 = -13;        // EACCES: Permission denied
pub const VFS_ERR_INVALID: i32 = -22;       // EINVAL: Invalid argument
pub const VFS_ERR_NO_MEM: i32 = -12;        // ENOMEM: Out of memory
pub const VFS_ERR_BAD_FD: i32 = -9;         // EBADF: Bad file descriptor
pub const VFS_ERR_IO: i32 = -5;             // EIO: I/O error
pub const VFS_ERR_NOT_DIR: i32 = -20;       // ENOTDIR: Not a directory
pub const VFS_ERR_IS_DIR: i32 = -21;        // EISDIR: Is a directory
pub const VFS_ERR_NOSYS: i32 = -38;         // ENOSYS: Function not implemented

/// Open flags (compatible with standard Unix open flags)
pub const O_RDONLY: i32 = 0x0000;
pub const O_WRONLY: i32 = 0x0001;
pub const O_RDWR: i32 = 0x0002;
pub const O_CREAT: i32 = 0x0040;
pub const O_EXCL: i32 = 0x0080;
pub const O_TRUNC: i32 = 0x0200;
pub const O_APPEND: i32 = 0x0400;

/// Seek modes (compatible with standard Unix lseek)
pub const SEEK_SET: i32 = 0;
pub const SEEK_CUR: i32 = 1;
pub const SEEK_END: i32 = 2;

/// Maximum path length in VFS message
pub const MAX_PATH_LEN: usize = 200;

/// VFS request structure
///
/// This is a structured view over the IPC Message format.
/// It provides type-safe access to the various fields in the VFS protocol.
pub struct VfsRequest {
    msg: Message,
}

impl VfsRequest {
    /// Create a new VFS request message
    pub fn new() -> Self {
        Self {
            msg: Message::new(),
        }
    }

    /// Create a VFS request from an existing IPC message
    pub fn from_message(msg: Message) -> Self {
        Self { msg }
    }

    /// Get the underlying IPC message (for sending)
    pub fn into_message(self) -> Message {
        self.msg
    }

    /// Get immutable reference to underlying message
    pub fn as_message(&self) -> &Message {
        &self.msg
    }

    /// Get mutable reference to underlying message
    pub fn as_message_mut(&mut self) -> &mut Message {
        &mut self.msg
    }

    // Request type (u32 at offset 0)
    pub fn request_type(&self) -> u32 {
        self.msg.get_u32(0)
    }

    pub fn set_request_type(&mut self, value: u32) {
        self.msg.set_u32(0, value);
    }

    // Result (i32 at offset 4)
    pub fn result(&self) -> i32 {
        self.msg.get_u32(4) as i32
    }

    pub fn set_result(&mut self, value: i32) {
        self.msg.set_u32(4, value as u32);
    }

    // Request ID (u64 at offset 8)
    pub fn request_id(&self) -> u64 {
        self.msg.get_u64(8)
    }

    pub fn set_request_id(&mut self, value: u64) {
        self.msg.set_u64(8, value);
    }

    // Reply port ID (u64 at offset 16)
    pub fn reply_port_id(&self) -> u64 {
        self.msg.get_u64(16)
    }

    pub fn set_reply_port_id(&mut self, value: u64) {
        self.msg.set_u64(16, value);
    }

    // File descriptor (i32 at offset 24)
    pub fn fd(&self) -> i32 {
        self.msg.get_u32(24) as i32
    }

    pub fn set_fd(&mut self, value: i32) {
        self.msg.set_u32(24, value as u32);
    }

    // Flags (i32 at offset 28)
    pub fn flags(&self) -> i32 {
        self.msg.get_u32(28) as i32
    }

    pub fn set_flags(&mut self, value: i32) {
        self.msg.set_u32(28, value as u32);
    }

    // Offset (u64 at offset 32)
    pub fn offset(&self) -> u64 {
        self.msg.get_u64(32)
    }

    pub fn set_offset(&mut self, value: u64) {
        self.msg.set_u64(32, value);
    }

    // Count (u64 at offset 40)
    pub fn count(&self) -> u64 {
        self.msg.get_u64(40)
    }

    pub fn set_count(&mut self, value: u64) {
        self.msg.set_u64(40, value);
    }

    // Shared memory ID (i64 at offset 48) - NEW for fsitem support
    pub fn shmem_id(&self) -> i64 {
        self.msg.get_u64(48) as i64
    }

    pub fn set_shmem_id(&mut self, value: i64) {
        self.msg.set_u64(48, value as u64);
    }

    // Data buffer (bytes 56-255, total 200 bytes)
    pub fn data(&self) -> &[u8] {
        &self.msg.as_bytes()[56..256]
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.msg.as_bytes_mut()[56..256]
    }

    /// Copy path string into data field
    pub fn set_path(&mut self, path: &str) {
        let data = self.data_mut();
        let len = path.len().min(MAX_PATH_LEN - 1); // Leave room for null terminator
        data[..len].copy_from_slice(&path.as_bytes()[..len]);
        data[len] = 0; // Null terminator
    }

    /// Get path string from data field
    pub fn path(&self) -> Option<&str> {
        let data = self.data();
        // Find null terminator
        let len = data.iter().position(|&b| b == 0)?;
        core::str::from_utf8(&data[..len]).ok()
    }

    /// Copy buffer data into data field
    pub fn set_buffer(&mut self, buffer: &[u8]) {
        let data = self.data_mut();
        let len = buffer.len().min(MAX_PATH_LEN);
        data[..len].copy_from_slice(&buffer[..len]);
    }

    /// Get buffer data from data field
    pub fn buffer(&self) -> &[u8] {
        let count = self.count() as usize;
        let len = count.min(MAX_PATH_LEN);
        &self.data()[..len]
    }
}

impl Default for VfsRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper functions to create VFS requests

/// Create a VFS_OPEN request
pub fn create_open_request(request_id: u64, path: &str, flags: i32) -> VfsRequest {
    let mut req = VfsRequest::new();
    req.set_request_type(VFS_OPEN);
    req.set_request_id(request_id);
    req.set_flags(flags);
    req.set_path(path);
    req
}

/// Create a VFS_READ request
pub fn create_read_request(request_id: u64, fd: i32, count: u64) -> VfsRequest {
    let mut req = VfsRequest::new();
    req.set_request_type(VFS_READ);
    req.set_request_id(request_id);
    req.set_fd(fd);
    req.set_count(count);
    req
}

/// Create a VFS_WRITE request
pub fn create_write_request(request_id: u64, fd: i32, buffer: &[u8]) -> VfsRequest {
    let mut req = VfsRequest::new();
    req.set_request_type(VFS_WRITE);
    req.set_request_id(request_id);
    req.set_fd(fd);
    req.set_count(buffer.len() as u64);
    req.set_buffer(buffer);
    req
}

/// Create a VFS_CLOSE request
pub fn create_close_request(request_id: u64, fd: i32) -> VfsRequest {
    let mut req = VfsRequest::new();
    req.set_request_type(VFS_CLOSE);
    req.set_request_id(request_id);
    req.set_fd(fd);
    req
}

/// Create a VFS_LSEEK request
pub fn create_lseek_request(request_id: u64, fd: i32, offset: i64, whence: i32) -> VfsRequest {
    let mut req = VfsRequest::new();
    req.set_request_type(VFS_LSEEK);
    req.set_request_id(request_id);
    req.set_fd(fd);
    req.set_offset(offset as u64);
    req.set_flags(whence); // Use flags field for whence parameter
    req
}
