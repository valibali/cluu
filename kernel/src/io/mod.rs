/*
 * Input/Output System
 *
 * This module provides low-level I/O operations and interfaces
 * for hardware communication, replacing the syscall system
 * with direct hardware access patterns.
 *
 * Also provides device abstraction layer for TTY and future
 * file descriptor support.
 */

pub mod device;
pub mod fd;
pub mod pio;
pub mod tty_device;
pub mod vfs_file;

pub use pio::{Io, Pio, ReadOnly};

// Re-export device abstraction types
pub use fd::FileDescriptorTable;
pub use tty_device::TtyDevice;
pub use vfs_file::VfsFile;
