//! Error types for CLUU syscalls
//!
//! This module defines error codes that match the kernel's Error enum.
//! All syscalls return Result<T, Error>.

use core::fmt;

/// Error codes returned by syscalls
///
/// These match the kernel's Error enum and errno values.
#[repr(isize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Invalid argument (-1)
    InvalidArgument = -1,
    /// Invalid address (-2)
    InvalidAddress = -2,
    /// Out of memory (-3)
    OutOfMemory = -3,
    /// Not found (-4)
    NotFound = -4,
    /// Permission denied (-5)
    PermissionDenied = -5,
    /// Already exists (-6)
    AlreadyExists = -6,
    /// Timeout (-7)
    Timeout = -7,
    /// Invalid operation (-8)
    InvalidOperation = -8,
    /// Invalid state (-9)
    InvalidState = -9,
    /// Buffer too small (-10)
    BufferTooSmall = -10,
    /// Overflow (-11)
    Overflow = -11,
    /// Would block (-12)
    WouldBlock = -12,
    /// Not implemented (-13)
    NotImplemented = -13,
    /// Busy (-14)
    Busy = -14,
    /// Invalid parameter (-15)
    InvalidParameter = -15,
    /// Unknown error (not in kernel enum)
    Unknown = -999,
}

impl Error {
    /// Convert errno value to Error
    pub fn from_errno(errno: isize) -> Self {
        match errno {
            -1 => Error::InvalidArgument,
            -2 => Error::InvalidAddress,
            -3 => Error::OutOfMemory,
            -4 => Error::NotFound,
            -5 => Error::PermissionDenied,
            -6 => Error::AlreadyExists,
            -7 => Error::Timeout,
            -8 => Error::InvalidOperation,
            -9 => Error::InvalidState,
            -10 => Error::BufferTooSmall,
            -11 => Error::Overflow,
            -12 => Error::WouldBlock,
            -13 => Error::NotImplemented,
            -14 => Error::Busy,
            -15 => Error::InvalidParameter,
            _ => Error::Unknown,
        }
    }

    /// Convert Error to errno value
    pub const fn to_errno(self) -> isize {
        self as isize
    }

    /// Get error message
    pub const fn message(&self) -> &'static str {
        match self {
            Error::InvalidArgument => "Invalid argument",
            Error::InvalidAddress => "Invalid address",
            Error::OutOfMemory => "Out of memory",
            Error::NotFound => "Not found",
            Error::PermissionDenied => "Permission denied",
            Error::AlreadyExists => "Already exists",
            Error::Timeout => "Timeout",
            Error::InvalidOperation => "Invalid operation",
            Error::InvalidState => "Invalid state",
            Error::BufferTooSmall => "Buffer too small",
            Error::Overflow => "Overflow",
            Error::WouldBlock => "Would block",
            Error::NotImplemented => "Not implemented",
            Error::Busy => "Busy",
            Error::InvalidParameter => "Invalid parameter",
            Error::Unknown => "Unknown error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message(), self.to_errno())
    }
}

impl From<klibcluu::boot_elf::BootElfError> for Error {
    fn from(err: klibcluu::boot_elf::BootElfError) -> Self {
        match err {
            klibcluu::boot_elf::BootElfError::InvalidFormat => Error::InvalidArgument,
            klibcluu::boot_elf::BootElfError::NoLoadableSegments => Error::InvalidArgument,
        }
    }
}

/// Result type for syscalls
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_errno() {
        assert_eq!(Error::from_errno(-1), Error::InvalidArgument);
        assert_eq!(Error::from_errno(-15), Error::InvalidParameter);
        assert_eq!(Error::from_errno(-999), Error::Unknown);
    }

    #[test]
    fn test_to_errno() {
        assert_eq!(Error::InvalidArgument.to_errno(), -1);
        assert_eq!(Error::InvalidParameter.to_errno(), -15);
    }

    #[test]
    fn test_message() {
        assert_eq!(Error::InvalidArgument.message(), "Invalid argument");
        assert_eq!(Error::NotImplemented.message(), "Not implemented");
    }
}
