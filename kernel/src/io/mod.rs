/*
 * Input/Output System
 *
 * This module provides low-level I/O operations and interfaces
 * for hardware communication, replacing the syscall system
 * with direct hardware access patterns.
 */

pub mod pio;

pub use pio::{Pio, Io, ReadOnly};
