/*
 * TTY Device Implementation
 *
 * Provides Device trait implementation for TTY (terminal) devices.
 * Integrates with the existing TTY layer (components/tty.rs) and
 * keyboard driver for input/output.
 *
 * Supports:
 * - Canonical mode: Line buffering with editing (default)
 * - Raw mode: Character-by-character input
 * - Echo control: Enable/disable character echo
 */

use super::device::{Device, Errno, Stat, S_IFCHR};
use crate::components::tty;
use crate::drivers::input::keyboard;
use alloc::string::String;

/// TTY device implementation
pub struct TtyDevice {
    tty_id: u8,   // Which TTY (0 = console)
    mode: TtyMode, // Canonical/raw, echo on/off
}

/// TTY input/output mode (minimal termios subset)
#[derive(Clone, Copy)]
pub struct TtyMode {
    pub canonical: bool, // Line buffering (ICANON)
    pub echo: bool,      // Echo characters (ECHO)
}

impl TtyDevice {
    /// Create a new TTY device
    ///
    /// # Arguments
    /// * `tty_id` - TTY identifier (0 = console/TTY0)
    pub fn new(tty_id: u8) -> Self {
        Self {
            tty_id,
            mode: TtyMode {
                canonical: true, // Default: canonical mode (line buffering)
                echo: true,      // Default: echo on
            },
        }
    }

    /// Get current TTY mode
    pub fn mode(&self) -> TtyMode {
        self.mode
    }

    /// Set TTY mode
    pub fn set_mode(&mut self, mode: TtyMode) {
        self.mode = mode;
    }
}

impl Device for TtyDevice {
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.mode.canonical {
            // Canonical mode: Read full line (blocks until Enter)
            // Delegate to existing TTY layer which handles line editing
            let line = self.read_line_blocking()?;
            let bytes = line.as_bytes();
            let copy_len = bytes.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
            Ok(copy_len)
        } else {
            // Raw mode: Read single character (blocks until key pressed)
            let ch = keyboard::read_char_blocking();
            buf[0] = ch as u8;
            Ok(1)
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, Errno> {
        // Validate UTF-8 and write to TTY
        match core::str::from_utf8(buf) {
            Ok(s) => {
                // Use existing TTY write function
                tty::tty0_write_str(s);
                Ok(buf.len())
            }
            Err(_) => Err(Errno::EINVAL), // Invalid UTF-8
        }
    }

    fn ioctl(&self, request: u32, _arg: usize) -> Result<i32, Errno> {
        // Minimal ioctl support for termios
        // Full implementation requires user/kernel memory copy utilities
        match request {
            TCGETS => {
                // Get termios attributes
                // TODO: Copy current mode to user buffer when adding userspace
                Ok(0)
            }
            TCSETS => {
                // Set termios attributes
                // TODO: Read mode from user buffer and update self.mode
                Ok(0)
            }
            _ => Err(Errno::EINVAL),
        }
    }

    fn is_tty(&self) -> bool {
        true
    }

    fn stat(&self) -> Stat {
        Stat {
            st_mode: S_IFCHR | 0o666, // Character device, rw-rw-rw-
            st_size: 0,
            st_blksize: 0,
            st_blocks: 0,
        }
    }

    fn seek(&self, _offset: i64, _whence: i32) -> Result<i64, Errno> {
        // TTYs are not seekable
        Err(Errno::ESPIPE)
    }
}

impl TtyDevice {
    /// Read a full line in canonical mode (blocks until Enter)
    ///
    /// Uses the existing TTY line editor which handles:
    /// - Backspace
    /// - Character echo
    /// - Line history
    fn read_line_blocking(&self) -> Result<String, Errno> {
        // Use existing TTY line editor
        loop {
            let ch = keyboard::read_char_blocking();

            if let Some(line) = tty::tty0_handle_char(ch) {
                // Complete line available (Enter pressed)
                return Ok(line);
            }
            // Continue accumulating characters
        }
    }
}

// ioctl request codes (termios)
const TCGETS: u32 = 0x5401; // Get termios attributes
const TCSETS: u32 = 0x5402; // Set termios attributes
