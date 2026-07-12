//! Kernel error types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    InvalidAddress,
    OutOfMemory,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    Timeout,
    InvalidOperation,
    InvalidState,
    BufferTooSmall,
    Overflow,
    WouldBlock,
    NotImplemented,
    Busy,
    InvalidParameter,
    NoSpace,
}

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    /// Convert error to syscall return value (negative errno)
    pub fn to_errno(self) -> isize {
        match self {
            Error::InvalidArgument => -1,
            Error::InvalidAddress => -2,
            Error::OutOfMemory => -3,
            Error::NotFound => -4,
            Error::PermissionDenied => -5,
            Error::AlreadyExists => -6,
            Error::Timeout => -7,
            Error::InvalidOperation => -8,
            Error::InvalidState => -9,
            Error::BufferTooSmall => -10,
            Error::Overflow => -11,
            Error::WouldBlock => -12,
            Error::NotImplemented => -13,
            Error::Busy => -14,
            Error::InvalidParameter => -15,
            Error::NoSpace => -16,
        }
    }
}
