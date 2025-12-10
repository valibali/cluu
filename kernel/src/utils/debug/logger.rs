/*
 * Kernel Logging System
 *
 * This module implements a custom logging system for the CLUU kernel.
 * It provides structured logging capabilities with different log levels
 * and outputs to the serial console for debugging purposes.
 *
 * Why this is important:
 * - Enables systematic debugging and monitoring of kernel operations
 * - Provides different log levels for filtering messages
 * - Integrates with Rust's standard logging framework
 * - Essential for kernel development and troubleshooting
 * - Allows tracking of kernel initialization and runtime behavior
 *
 * The logger outputs to COM2 serial port, allowing external monitoring
 * of kernel messages during development and debugging.
 */

use core::fmt::Write;

use log::{Level, LevelFilter, Metadata, Record};

use crate::serial_println;

/// Custom logger implementation for CluuLogger.
struct CluuLogger;

impl log::Log for CluuLogger {
    /// Checks if the given log level is enabled.
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    /// Logs the record by printing it to the console.
    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            serial_println!("[{}] {}", record.level(), record.args());
        }
    }

    /// Flushes the logger (no-op in this case).
    fn flush(&self) {}
}

/// The CluuLogger instance used for logging.
static LOGGER: CluuLogger = CluuLogger;

/// Initializes the logger and optionally clears the screen.
///
/// # Arguments
///
/// * `clearscr` - A boolean indicating whether to clear the screen before initializing the logger.
///
/// # Panics
///
/// If there is an error initializing the logger, a panic will occur with the corresponding error message.
///
/// # Note
///
/// This function assumes that the debug infrastructure (COM2 port) has already been initialized.
pub fn init(clearscr: bool) {
    if clearscr {
        _ = crate::utils::writer::Writer::new().write_str("\u{001B}[2J\u{001B}[H"); // Clear screen
    }

    let logger_init_result =
        log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info));

    match logger_init_result {
        Ok(_) => serial_println!("Logger initialized correctly"),
        Err(err) => panic!("Error with initializing logger: {}", err),
    }
}
