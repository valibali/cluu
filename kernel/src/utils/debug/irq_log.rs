/*
 * IRQ-Safe Logging
 *
 * This module provides minimal logging functionality that is safe to use
 * from interrupt handlers. It avoids all heap allocation, formatting,
 * and mutex usage that could cause deadlocks or memory corruption.
 */

use x86_64::instructions::port::Port;

/// Write a simple string directly to serial port without any formatting
/// This is IRQ-safe as it doesn't use mutexes or heap allocation

pub fn irq_log_str(msg: &str) {
    let mut data_port: Port<u8> = Port::new(0x2F8); // COM2 data port
    let mut status_port: Port<u8> = Port::new(0x2FD); // COM2 line status port

    for byte in msg.bytes() {
        unsafe {
            while (status_port.read() & 0x20u8) == 0 {
                core::hint::spin_loop();
            }
            data_port.write(byte);
        }
    }
}

/// Write a newline to serial port
pub fn irq_log_newline() {
    irq_log_str("\r\n");
}

/// Log a simple message with prefix for IRQ context
pub fn irq_log(prefix: &str, msg: &str) {
    irq_log_str("[IRQ] ");
    irq_log_str(prefix);
    irq_log_str(": ");
    irq_log_str(msg);
    irq_log_newline();
}

/// Log just a prefix (for very minimal logging)
pub fn irq_log_simple(prefix: &str) {
    irq_log_str("[IRQ] ");
    irq_log_str(prefix);
    irq_log_newline();
}

/// Convert u64 to hex string (IRQ-safe, no allocation)
pub fn irq_log_hex(prefix: &str, value: u64) {
    irq_log_str(prefix);
    irq_log_str("0x");

    // Convert to hex manually without allocation
    let mut buf = [0u8; 16];
    let hex_digits = b"0123456789abcdef";

    for i in 0..16 {
        let nibble = ((value >> (60 - i * 4)) & 0xF) as usize;
        buf[i] = hex_digits[nibble];
    }

    // Skip leading zeros
    let mut start = 0;
    while start < 15 && buf[start] == b'0' {
        start += 1;
    }

    // Write hex digits
    let mut data_port: Port<u8> = Port::new(0x2F8);
    let mut status_port: Port<u8> = Port::new(0x2FD);

    unsafe {
        for i in start..16 {
            while (status_port.read() & 0x20u8) == 0 {
                core::hint::spin_loop();
            }
            data_port.write(buf[i]);
        }
    }

    irq_log_newline();
}
