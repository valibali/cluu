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

use core::sync::atomic::{AtomicU64, Ordering};

use crate::uart::COM2;

/// TSC frequency set once during boot via `set_tsc_hz()`.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

/// Tell the logger the TSC frequency so it can print timestamps.
/// Call once after TSC calibration.
pub fn set_tsc_hz(hz: u64) {
    TSC_HZ.store(hz, Ordering::Release);
}

/// Read the TSC (Time Stamp Counter) — no allocation, IRQ-safe.
#[inline]
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a boot-relative timestamp prefix like `[    1.234] `.
/// If TSC_HZ is not yet set, writes nothing.
fn write_timestamp() {
    let hz = TSC_HZ.load(Ordering::Relaxed);
    if hz == 0 {
        return;
    }
    let tsc = rdtsc();
    // Compute seconds and milliseconds
    let secs = tsc / hz;
    let remainder = tsc % hz;
    let millis = remainder * 1000 / hz;

    COM2.write_byte(b'[');
    // Right-align seconds in a 5-char field (space-padded)
    write_dec_padded(secs, 5, b' ');
    COM2.write_byte(b'.');
    // Milliseconds: always 3 digits (zero-padded)
    write_dec_padded(millis, 3, b'0');
    COM2.write_str("] ");
}

/// Write a decimal value right-aligned in `width` chars, padded with `pad`.
fn write_dec_padded(value: u64, width: usize, pad: u8) {
    let mut buf = [pad; 20];
    let mut v = value;
    let mut i = 0;
    if v == 0 {
        buf[19] = b'0';
        i = 1;
    } else {
        while v > 0 && i < 20 {
            buf[19 - i] = b'0' + (v % 10) as u8;
            v /= 10;
            i += 1;
        }
    }
    // Write at least `width` chars (space-padded on left)
    let digits_start = 20 - i;
    let pad_start = if i < width { 20 - width } else { digits_start };
    for idx in pad_start..20 {
        COM2.write_byte(buf[idx]);
    }
}

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

    write_timestamp();

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

/// Log two strings concatenated on a single line. IRQ-safe, no allocation.
pub fn log_str_pair(level: LogLevel, prefix: &str, suffix: &str) {
    if !should_log(level) {
        return;
    }
    write_timestamp();
    let level_str = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };
    COM2.write_str(level_str);
    COM2.write_str(prefix);
    COM2.write_str(suffix);
    COM2.write_str("\n");
}

/// Log a hexadecimal value
///
/// This is IRQ-safe and does not allocate.
pub fn log_hex(level: LogLevel, prefix: &str, value: u64) {
    if !should_log(level) {
        return;
    }

    write_timestamp();

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

    write_timestamp();

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

/// Log a key=value pair on a single line (for machine-parseable telemetry).
pub fn log_kv_dec(level: LogLevel, key: &str, value: u64) {
    if !should_log(level) {
        return;
    }
    write_timestamp();
    let prefix = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };
    COM2.write_str(prefix);
    COM2.write_str(key);
    write_dec(value);
    COM2.write_str("\n");
}

/// Log a signed key=value pair on a single line (for machine-parseable telemetry).
pub fn log_kv_i64(level: LogLevel, key: &str, value: i64) {
    if !should_log(level) {
        return;
    }
    write_timestamp();
    let prefix = match level {
        LogLevel::Error => "[ERROR] ",
        LogLevel::Warn => "[WARN]  ",
        LogLevel::Info => "[INFO]  ",
        LogLevel::Debug => "[DEBUG] ",
        LogLevel::Trace => "[TRACE] ",
    };
    COM2.write_str(prefix);
    COM2.write_str(key);
    if value < 0 {
        COM2.write_str("-");
        write_dec(value.unsigned_abs());
    } else {
        write_dec(value as u64);
    }
    COM2.write_str("\n");
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
