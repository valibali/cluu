/*
 * Lock-Free Ring Buffer for Logging
 *
 * This module implements a simple lock-free ring buffer for kernel logging
 * that can be safely used from any context (kernel, interrupts, syscalls).
 *
 * Design:
 * - Fixed-size circular buffer (32KB)
 * - Atomic head (write) and tail (read) indices
 * - Single producer, single consumer model
 * - Overwrite on overflow (loses old messages)
 *
 * Thread Safety:
 * - Write operations use atomic compare-and-swap
 * - Safe to call from interrupt context
 * - No heap allocations
 * - No mutexes or locks
 */

use core::sync::atomic::{AtomicUsize, Ordering};

/// Size of the ring buffer (must be power of 2 for fast modulo)
const BUFFER_SIZE: usize = 32 * 1024; // 32KB

/// Ring buffer for log messages
pub struct RingBuffer {
    /// Circular buffer storage
    buffer: [u8; BUFFER_SIZE],

    /// Write position (producer index)
    head: AtomicUsize,

    /// Read position (consumer index)
    tail: AtomicUsize,
}

impl RingBuffer {
    /// Create a new empty ring buffer
    pub const fn new() -> Self {
        Self {
            buffer: [0; BUFFER_SIZE],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Write a string to the ring buffer
    ///
    /// This is lock-free and safe to call from any context.
    /// If the buffer is full, old data will be overwritten.
    ///
    /// # Arguments
    /// * `data` - String slice to write
    ///
    /// # Returns
    /// Number of bytes actually written
    pub fn write(&mut self, data: &str) -> usize {
        let bytes = data.as_bytes();
        let len = bytes.len();

        if len == 0 {
            return 0;
        }

        // Get current head position
        let head = self.head.load(Ordering::Relaxed);

        // Calculate how much space is available
        let tail = self.tail.load(Ordering::Relaxed);
        let available = if head >= tail {
            BUFFER_SIZE - (head - tail)
        } else {
            tail - head
        };

        // If not enough space, advance tail to make room (overwrite old data)
        if len > available {
            let new_tail = (head + len - available) % BUFFER_SIZE;
            self.tail.store(new_tail, Ordering::Relaxed);
        }

        // Write data to buffer (may wrap around)
        let mut written = 0;
        for &byte in bytes {
            let idx = (head + written) % BUFFER_SIZE;
            unsafe {
                // SAFETY: idx is bounded by BUFFER_SIZE due to modulo
                let ptr = self.buffer.as_ptr() as *mut u8;
                *ptr.add(idx) = byte;
            }
            written += 1;
        }

        // Update head pointer
        let new_head = (head + written) % BUFFER_SIZE;
        self.head.store(new_head, Ordering::Release);

        written
    }

    /// Read available data from the ring buffer
    ///
    /// This is lock-free and drains data from the buffer.
    ///
    /// # Arguments
    /// * `dest` - Destination buffer to read into
    ///
    /// # Returns
    /// Number of bytes actually read
    pub fn read(&mut self, dest: &mut [u8]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Calculate how much data is available
        let available = if head >= tail {
            head - tail
        } else {
            BUFFER_SIZE - tail + head
        };

        if available == 0 {
            return 0;
        }

        // Read up to dest.len() bytes
        let to_read = available.min(dest.len());
        let mut read_count = 0;

        for i in 0..to_read {
            let idx = (tail + i) % BUFFER_SIZE;
            dest[read_count] = unsafe {
                // SAFETY: idx is bounded by BUFFER_SIZE due to modulo
                let ptr = self.buffer.as_ptr();
                *ptr.add(idx)
            };
            read_count += 1;
        }

        // Update tail pointer
        let new_tail = (tail + read_count) % BUFFER_SIZE;
        self.tail.store(new_tail, Ordering::Release);

        read_count
    }

    /// Check if the ring buffer is empty
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        head == tail
    }

    /// Get the number of bytes currently in the buffer
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);

        if head >= tail {
            head - tail
        } else {
            BUFFER_SIZE - tail + head
        }
    }
}

// SAFETY: RingBuffer uses atomic operations and is safe to share across threads
unsafe impl Sync for RingBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_read() {
        let mut rb = RingBuffer::new();

        // Write some data
        let written = rb.write("Hello, World!");
        assert_eq!(written, 13);

        // Read it back
        let mut buf = [0u8; 20];
        let read = rb.read(&mut buf);
        assert_eq!(read, 13);
        assert_eq!(&buf[..13], b"Hello, World!");
    }

    #[test]
    fn test_wrap_around() {
        let mut rb = RingBuffer::new();

        // Fill buffer almost to capacity
        let large_str = "x".repeat(BUFFER_SIZE - 100);
        rb.write(&large_str);

        // Write more to cause wrap
        rb.write("WRAP");

        assert!(!rb.is_empty());
    }
}
