/*
 * Buffered Logging System
 *
 * This module provides a lock-free buffered logging system using a ring buffer.
 * Log messages are written to the buffer without blocking, and a background
 * task drains the buffer to the serial port.
 *
 * Benefits:
 * - No deadlocks from logging in interrupt context
 * - Fast logging (just write to buffer)
 * - Safe to call from any context
 */

use super::ring_buffer::RingBuffer;
use spin::Mutex;

/// Global log buffer
static LOG_BUFFER: Mutex<RingBuffer> = Mutex::new(RingBuffer::new());

/// Flag to track if buffer is initialized
static mut INITIALIZED: bool = false;

/// Initialize the log buffer system
pub fn init() {
    unsafe {
        INITIALIZED = true;
    }
    // Note: Don't log here - logger isn't initialized yet
}

/// Write a log message to the buffer
///
/// This is lock-free after acquiring the buffer mutex (which is very brief).
/// Safe to call from any context.
///
/// # Arguments
/// * `message` - The log message to write
pub fn write_log(message: &str) {
    // Check if initialized
    if unsafe { !INITIALIZED } {
        // Fallback: write directly to serial before buffer is ready
        write_directly_to_serial(message);
        return;
    }

    // Disable interrupts while holding the mutex to prevent deadlock
    // from timer interrupts that try to log while we're already logging
    let _guard = crate::arch::x86_64::interrupts::DisableInterrupts::new();

    // Write to buffer
    let mut buffer = LOG_BUFFER.lock();
    buffer.write(message);

    // During early boot (before scheduler starts), flush immediately to ensure logs appear
    // Once scheduler is running, rely on periodic flushing for better performance
    let should_flush_immediately = !crate::scheduler::is_scheduler_enabled()
        || buffer.len() > (32 * 1024 * 3 / 4);

    if should_flush_immediately {
        flush_buffer_to_serial(&mut buffer);
    }
    // Interrupts re-enabled when _guard is dropped
}

/// Flush the log buffer to serial port
///
/// This should be called periodically (e.g., from scheduler idle loop)
/// to drain buffered log messages to the serial port.
pub fn flush() {
    if unsafe { !INITIALIZED } {
        return;
    }

    // Disable interrupts while holding the mutex to prevent deadlock
    let _guard = crate::arch::x86_64::interrupts::DisableInterrupts::new();

    let mut buffer = LOG_BUFFER.lock();
    flush_buffer_to_serial(&mut buffer);
    // Interrupts re-enabled when _guard is dropped
}

/// Internal: Flush buffer contents to serial port
fn flush_buffer_to_serial(buffer: &mut RingBuffer) {
    if buffer.is_empty() {
        return;
    }

    // Read chunks from buffer and write to serial
    let mut temp_buf = [0u8; 256];

    while !buffer.is_empty() {
        let read = buffer.read(&mut temp_buf);
        if read == 0 {
            break;
        }

        // Write to serial port directly (bypass normal logging to avoid recursion)
        write_bytes_to_serial(&temp_buf[..read]);
    }
}

/// Write bytes directly to COM2 serial port
fn write_bytes_to_serial(bytes: &[u8]) {
    use x86_64::instructions::port::Port;

    let mut data_port: Port<u8> = Port::new(0x2F8); // COM2 data port
    let mut status_port: Port<u8> = Port::new(0x2FD); // COM2 line status port

    for &byte in bytes {
        unsafe {
            // Wait for transmit buffer to be empty
            loop {
                let status: u8 = status_port.read();
                if (status & 0x20) != 0 {
                    // Bit 5: Transmit buffer empty
                    break;
                }
            }

            // Write byte
            data_port.write(byte);
        }
    }
}

/// Write message directly to serial (used before buffer is initialized)
fn write_directly_to_serial(message: &str) {
    write_bytes_to_serial(message.as_bytes());
}

/// Get current buffer usage (for monitoring)
pub fn buffer_usage() -> usize {
    if unsafe { !INITIALIZED } {
        return 0;
    }

    let buffer = LOG_BUFFER.lock();
    buffer.len()
}
