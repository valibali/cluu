//! IRQ-Safe Kernel Logger
//!
//! Provides logging without allocation, formatting, or locks.
//! Safe to use from interrupt handlers and any kernel context.
//!
//! # Design Principles
//!
//! 1. **IRQ-Safe**: No mutexes, no locks, no waiting
//! 2. **No Allocation**: No heap usage, no formatting
//! 3. **Zero-overhead in Release**: Logging is compiled out in release builds
//! 4. **Simple**: Just write strings to UART
//!
//! # Debug Levels
//!
//! - ERROR: Critical errors only
//! - WARN: Warnings and errors
//! - INFO: Informational messages (default)
//! - DEBUG: Verbose debugging
//! - TRACE: Very verbose (via feature)

use crate::uart::COM2;

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

/// Get the current log level based on build configuration
#[inline]
pub const fn current_log_level() -> LogLevel {
    #[cfg(not(debug_assertions))]
    {
        // Release diagnostics mode: keep DEBUG logs enabled.
        #[cfg(feature = "log-trace")]
        return LogLevel::Trace;

        #[cfg(not(feature = "log-trace"))]
        return LogLevel::Debug;
    }

    #[cfg(debug_assertions)]
    {
        #[cfg(feature = "log-trace")]
        return LogLevel::Trace;

        #[cfg(all(feature = "log-debug", not(feature = "log-trace")))]
        return LogLevel::Debug;

        // Default to DEBUG for debug builds (was INFO)
        #[cfg(all(not(feature = "log-debug"), not(feature = "log-trace")))]
        return LogLevel::Info;
    }
}

/// Check if a log level should be logged
#[inline]
pub const fn should_log(level: LogLevel) -> bool {
    #[cfg(not(debug_assertions))]
    {
        let _ = level;
        return false;
    }

    #[cfg(debug_assertions)]
    {
        level as u8 <= current_log_level() as u8
    }
}

/// Log a message at the specified level
///
/// This is IRQ-safe and does not allocate or use locks.
#[inline]
pub fn log(level: LogLevel, msg: &str) {
    if !should_log(level) {
        return;
    }

    // Write level prefix
    let prefix = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };

    COM2.write_str(prefix);
    COM2.write_str(msg);
    COM2.write_str("\n");
}

/// Log an error message
#[inline]
pub fn error(msg: &str) {
    log(LogLevel::Error, msg);
}

/// Log a warning message
#[inline]
pub fn warn(msg: &str) {
    log(LogLevel::Warn, msg);
}

/// Log an info message
#[inline]
pub fn info(msg: &str) {
    log(LogLevel::Info, msg);
}

/// Log a debug message
#[inline]
pub fn debug(msg: &str) {
    log(LogLevel::Debug, msg);
}

/// Log a trace message
#[inline]
pub fn trace(msg: &str) {
    log(LogLevel::Trace, msg);
}

/// Log a hexadecimal value
///
/// This is IRQ-safe and does not allocate.
pub fn log_hex(level: LogLevel, prefix: &str, value: u64) {
    if !should_log(level) {
        return;
    }

    // Write level prefix
    let level_str = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };

    COM2.write_str(level_str);
    COM2.write_str(prefix);
    write_hex(value);
    COM2.write_str("\n");
}

/// Write a hexadecimal value without allocation
fn write_hex(mut value: u64) {
    COM2.write_str("0x");

    // Handle zero case
    if value == 0 {
        COM2.write_str("0");
        return;
    }

    // Convert to hex (most significant digit first)
    let mut buf = [0u8; 16];
    let mut i = 0;

    while value > 0 && i < 16 {
        let digit = (value & 0xF) as u8;
        buf[15 - i] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + (digit - 10)
        };
        value >>= 4;
        i += 1;
    }

    // Write digits (skip leading zeros)
    let start = 16 - i;
    for &byte in &buf[start..] {
        COM2.write_byte(byte);
    }
}

/// Log a decimal value
///
/// This is IRQ-safe and does not allocate.
pub fn log_dec(level: LogLevel, prefix: &str, value: u64) {
    if !should_log(level) {
        return;
    }

    // Write level prefix
    let level_str = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };

    COM2.write_str(level_str);
    COM2.write_str(prefix);
    write_dec(value);
    COM2.write_str("\n");
}

/// Write a decimal value without allocation
fn write_dec(mut value: u64) {
    // Handle zero case
    if value == 0 {
        COM2.write_str("0");
        return;
    }

    // Convert to decimal (most significant digit first)
    let mut buf = [0u8; 20]; // u64 max is 20 digits
    let mut i = 0;

    while value > 0 && i < 20 {
        buf[19 - i] = b'0' + (value % 10) as u8;
        value /= 10;
        i += 1;
    }

    // Write digits
    let start = 20 - i;
    for &byte in &buf[start..] {
        COM2.write_byte(byte);
    }
}

/// Initialize the logger
///
/// Must be called after UART initialization.
pub fn init() {
    #[cfg(debug_assertions)]
    {
        info("Logger initialized (IRQ-safe, no allocation)");

        #[cfg(feature = "log-trace")]
        info("Log level: TRACE");

        #[cfg(all(feature = "log-debug", not(feature = "log-trace")))]
        info("Log level: DEBUG");

        #[cfg(all(not(feature = "log-debug"), not(feature = "log-trace")))]
        info("Log level: INFO");
    }

    #[cfg(not(debug_assertions))]
    {
        // Intentionally silent in release builds.
    }
}
