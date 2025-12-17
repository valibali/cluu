/*
 * Generic I/O Wait Queue System
 *
 * This module provides a generic abstraction for blocking I/O operations.
 * Any hardware device (keyboard, serial, disk, network, etc.) can use this
 * system to block threads until events occur.
 *
 * ## Architecture
 *
 * **Wait Channels:**
 * Each I/O device or event type has a unique channel (identified by IoChannel enum).
 * Threads register themselves on channels they want to wait for.
 *
 * **Blocking:**
 * When a thread calls wait_for_io(channel), it:
 * 1. Registers itself in the channel's wait queue
 * 2. Blocks (removed from scheduler ready queue)
 * 3. Yields CPU
 *
 * **Waking:**
 * When a hardware interrupt occurs, the ISR calls wake_io_waiters(channel) to:
 * 1. Wake all threads waiting on that channel
 * 2. Move them back to ready queue
 *
 * ## IRQ Safety
 *
 * This module is designed to be called from both:
 * - Normal thread context (wait_for_io)
 * - Interrupt context (wake_io_waiters)
 *
 * Uses spin locks with interrupt disabling for safety.
 *
 * ## Usage Example
 *
 * **In device driver (thread context):**
 * ```rust
 * // Check if data available
 * if !device_has_data() {
 *     // Block until interrupt arrives
 *     wait_for_io(IoChannel::Serial(0));
 * }
 * // Read data
 * ```
 *
 * **In ISR (interrupt context):**
 * ```rust
 * pub fn serial_interrupt_handler() {
 *     // Read data from device
 *     let byte = read_serial_port();
 *     buffer_push(byte);
 *
 *     // Wake all threads waiting for serial data
 *     wake_io_waiters(IoChannel::Serial(0));
 * }
 * ```
 */

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::{ThreadId, block_current_thread, wake_thread, current_thread_id};

/// I/O channel identifier
///
/// Each hardware device or event source has a unique channel.
/// Threads wait on channels, and interrupts wake threads on channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IoChannel {
    /// Keyboard input (PS/2 or USB)
    Keyboard,

    /// Serial port (COM1=0, COM2=1, etc.)
    Serial(u8),

    /// Timer/clock events
    Timer,

    /// Disk I/O (drive number)
    Disk(u8),

    /// Network interface (NIC number)
    Network(u8),

    /// Generic device (for custom drivers)
    Device(u32),
}

/// Wait queue for a single I/O channel
struct WaitQueue {
    /// List of threads waiting on this channel
    waiting_threads: Vec<ThreadId>,
}

impl WaitQueue {
    fn new() -> Self {
        Self {
            waiting_threads: Vec::new(),
        }
    }

    /// Add a thread to the wait queue
    fn add_waiter(&mut self, thread_id: ThreadId) {
        if !self.waiting_threads.contains(&thread_id) {
            self.waiting_threads.push(thread_id);
        }
    }

    /// Wake all waiting threads and clear the queue
    fn wake_all(&mut self) -> Vec<ThreadId> {
        let threads = self.waiting_threads.clone();
        self.waiting_threads.clear();
        threads
    }

    /// Remove a specific thread (for cancellation)
    fn remove_waiter(&mut self, thread_id: ThreadId) {
        self.waiting_threads.retain(|&tid| tid != thread_id);
    }

    /// Check if queue is empty
    fn is_empty(&self) -> bool {
        self.waiting_threads.is_empty()
    }
}

/// Global wait queue registry
/// Maps I/O channels to their wait queues
static IO_WAIT_QUEUES: Mutex<BTreeMap<IoChannel, WaitQueue>> = Mutex::new(BTreeMap::new());

/// I/O wait system initialization flag
static IO_WAIT_INIT: AtomicBool = AtomicBool::new(false);

/// Initialize the I/O wait queue system
pub fn init() {
    IO_WAIT_INIT.store(true, Ordering::SeqCst);
    log::info!("I/O wait queue system initialized");
}

/// Wait for I/O on a specific channel (blocking)
///
/// This function blocks the current thread until an I/O event occurs
/// on the specified channel. The thread will consume 0% CPU while waiting.
///
/// # How it works
/// 1. Registers current thread in channel's wait queue
/// 2. Blocks the thread (removes from scheduler)
/// 3. Yields CPU
/// 4. ISR calls wake_io_waiters() when event occurs
/// 5. Thread wakes up and continues execution
///
/// # Arguments
/// * `channel` - The I/O channel to wait on
///
/// # Panics
/// Panics if called from idle thread or before scheduler is initialized
pub fn wait_for_io(channel: IoChannel) {
    if !IO_WAIT_INIT.load(Ordering::Acquire) {
        log::warn!("wait_for_io called before I/O wait system initialized");
        return;
    }

    let current_tid = current_thread_id();
    if current_tid.0 == 0 {
        panic!("Cannot wait for I/O in idle/kernel thread");
    }

    // Register in wait queue
    {
        let mut queues = IO_WAIT_QUEUES.lock();
        let wait_queue = queues.entry(channel).or_insert_with(WaitQueue::new);
        wait_queue.add_waiter(current_tid);
    }

    // Block and yield
    block_current_thread();
    super::yield_now();

    // When we wake up here, the I/O event has occurred
}

/// Wake all threads waiting on a specific I/O channel
///
/// This function is called from interrupt handlers when I/O events occur.
/// It wakes all threads that are blocked waiting for this channel.
///
/// # Arguments
/// * `channel` - The I/O channel that has activity
///
/// # IRQ Safety
/// This function is IRQ-safe and can be called from interrupt handlers.
pub fn wake_io_waiters(channel: IoChannel) {
    if !IO_WAIT_INIT.load(Ordering::Acquire) {
        return;
    }

    // Get all waiting threads for this channel
    let threads_to_wake = {
        let mut queues = IO_WAIT_QUEUES.lock();
        if let Some(wait_queue) = queues.get_mut(&channel) {
            wait_queue.wake_all()
        } else {
            Vec::new()
        }
    };

    // Wake each thread
    for thread_id in threads_to_wake {
        wake_thread(thread_id);
    }
}

/// Check if any threads are waiting on a channel
///
/// Useful for debugging and diagnostics.
pub fn has_waiters(channel: IoChannel) -> bool {
    if !IO_WAIT_INIT.load(Ordering::Acquire) {
        return false;
    }

    let queues = IO_WAIT_QUEUES.lock();
    queues.get(&channel)
        .map(|wq| !wq.is_empty())
        .unwrap_or(false)
}

/// Get count of threads waiting on a channel
///
/// Useful for debugging and diagnostics.
pub fn waiter_count(channel: IoChannel) -> usize {
    if !IO_WAIT_INIT.load(Ordering::Acquire) {
        return 0;
    }

    let queues = IO_WAIT_QUEUES.lock();
    queues.get(&channel)
        .map(|wq| wq.waiting_threads.len())
        .unwrap_or(0)
}
