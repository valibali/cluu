//! Kernel error types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    OutOfMemory,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    Timeout,
    InvalidOperation,
    InvalidState,
    BufferTooSmall,
    Overflow,
}

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    /// Convert error to syscall return value (negative errno)
    pub fn to_errno(self) -> isize {
        match self {
            Error::InvalidArgument => -1,
            Error::OutOfMemory => -2,
            Error::NotFound => -3,
            Error::PermissionDenied => -4,
            Error::AlreadyExists => -5,
            Error::Timeout => -6,
            Error::InvalidOperation => -7,
            Error::InvalidState => -8,
            Error::BufferTooSmall => -9,
            Error::Overflow => -10,
        }
    }
}
