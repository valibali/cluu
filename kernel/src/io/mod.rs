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

pub mod pio;
pub mod device;
pub mod fd;
pub mod tty_device;

pub use pio::{Pio, Io, ReadOnly};

// Re-export device abstraction types
pub use device::{Errno, S_IFCHR, S_IFMT};
pub use fd::FileDescriptorTable;
pub use tty_device::TtyDevice;
