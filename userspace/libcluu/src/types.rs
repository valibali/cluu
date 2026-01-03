//! Common types for userspace

use bitflags::bitflags;

/// Thread identifier
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(pub u64);

/// Address space identifier
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpaceId(pub u64);

/// Error type
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidArgument = -1,
    OutOfMemory = -2,
    NotFound = -3,
    PermissionDenied = -4,
    AlreadyExists = -5,
    Timeout = -6,
    InvalidOperation = -7,
}

pub type Result<T> = core::result::Result<T, Error>;

/// Convert raw syscall return value to Result
pub fn from_syscall_ret(ret: usize) -> Result<usize> {
    if (ret as isize) < 0 {
        let err = match ret as i32 {
            -1 => Error::InvalidArgument,
            -2 => Error::OutOfMemory,
            -3 => Error::NotFound,
            -4 => Error::PermissionDenied,
            -5 => Error::AlreadyExists,
            -6 => Error::Timeout,
            _ => Error::InvalidOperation,
        };
        Err(err)
    } else {
        Ok(ret)
    }
}

bitflags! {
    /// Memory permissions
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Perms: u8 {
        const READ    = 1 << 0;
        const WRITE   = 1 << 1;
        const EXECUTE = 1 << 2;
        const USER    = 1 << 3;
    }
}

bitflags! {
    /// IPC flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IpcFlags: u32 {
        const GRANT    = 1 << 0;
        const MAP      = 1 << 1;
        const TIMEOUT  = 1 << 2;
        const DONATE   = 1 << 3;
    }
}

/// IPC operation type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcOp {
    Send = 0,
    Recv = 1,
    Call = 2,
    Reply = 3,
    ReplyRecv = 4,
}

/// Message for IPC
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Message {
    pub tag: MessageTag,
    pub words: [usize; 6],
}

/// Message tag
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MessageTag {
    pub label: u32,
    pub words: u8,
    pub extra: u8,
}

impl MessageTag {
    pub const fn new(label: u32, words: u8) -> Self {
        Self { label, words, extra: 0 }
    }
}

impl Message {
    pub const fn new(label: u32, words: [usize; 6], word_count: u8) -> Self {
        Self {
            tag: MessageTag::new(label, word_count),
            words,
        }
    }
}
