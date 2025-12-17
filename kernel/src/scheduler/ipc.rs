/*
 * Inter-Process Communication (IPC) System
 *
 * This module implements a Mach-style microkernel IPC system with:
 * - Async send (non-blocking, posts to queue)
 * - Blocking receive (waits for messages)
 * - Fixed 256-byte messages
 * - Port-based communication (global port ID namespace)
 *
 * Design:
 * - Port creation returns a PortId
 * - Only port owner can receive messages
 * - Anyone can send to a port (if they know the ID)
 * - Messages are queued (32 message capacity per port)
 * - Receivers block when no messages available
 * - Senders return error when queue full
 */

use crate::scheduler::{current_thread_id, block_current_thread, wake_thread, yield_now};
use crate::scheduler::thread::ThreadId;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// Default message queue capacity per port
const DEFAULT_PORT_CAPACITY: usize = 32;

/// Port identifier (follows ThreadId pattern)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortId(pub usize);

impl core::fmt::Display for PortId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({})", self.0)
    }
}

/// Fixed 256-byte message
///
/// Messages are cache-line aligned for performance and have a fixed size
/// for simplicity and predictable memory usage.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct Message {
    data: [u8; 256],
}

impl Message {
    /// Create a new empty message (all zeros)
    pub fn new() -> Self {
        Self { data: [0u8; 256] }
    }

    /// Write a u64 value at the specified byte offset
    pub fn set_u64(&mut self, offset: usize, value: u64) {
        if offset + 8 <= 256 {
            self.data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Read a u64 value from the specified byte offset
    pub fn get_u64(&self, offset: usize) -> u64 {
        if offset + 8 <= 256 {
            u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
        } else {
            0
        }
    }

    /// Write a u32 value at the specified byte offset
    pub fn set_u32(&mut self, offset: usize, value: u32) {
        if offset + 4 <= 256 {
            self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Read a u32 value from the specified byte offset
    pub fn get_u32(&self, offset: usize) -> u32 {
        if offset + 4 <= 256 {
            u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
        } else {
            0
        }
    }

    /// Get immutable reference to the message data
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable reference to the message data
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Copy data from a slice into the message
    pub fn copy_from_slice(&mut self, src: &[u8]) {
        let len = src.len().min(256);
        self.data[..len].copy_from_slice(&src[..len]);
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

/// Message queue entry (stored in port's message queue)
struct QueuedMessage {
    message: Message,
    sender_id: ThreadId,
}

/// Port structure
///
/// Each port has:
/// - A unique ID
/// - An owner (only owner can receive)
/// - A message queue (FIFO, bounded capacity)
/// - A wait queue (threads blocked on receive)
struct Port {
    id: PortId,
    owner: ThreadId,
    message_queue: VecDeque<QueuedMessage>,
    wait_queue: Vec<ThreadId>,
    capacity: usize,
}

impl Port {
    fn new(id: PortId, owner: ThreadId, capacity: usize) -> Self {
        Self {
            id,
            owner,
            message_queue: VecDeque::with_capacity(capacity),
            wait_queue: Vec::new(),
            capacity,
        }
    }
}

/// IPC error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Port ID doesn't exist in registry
    PortNotFound,
    /// Port exists but no owner/receiver registered
    NoReceiver,
    /// Message queue is at capacity
    QueueFull,
    /// Thread doesn't own this port (can't receive)
    NotOwner,
    /// Port ID is 0 or otherwise invalid
    InvalidPortId,
}

impl core::fmt::Display for IpcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IpcError::PortNotFound => write!(f, "Port not found"),
            IpcError::NoReceiver => write!(f, "No receiver registered"),
            IpcError::QueueFull => write!(f, "Message queue full"),
            IpcError::NotOwner => write!(f, "Not port owner"),
            IpcError::InvalidPortId => write!(f, "Invalid port ID"),
        }
    }
}

/// Global port registry
///
/// Maps PortId → Port for all active ports in the system.
/// Protected by Mutex for thread-safe access.
static PORT_REGISTRY: Mutex<Option<BTreeMap<PortId, Port>>> = Mutex::new(None);

/// Next port ID to allocate (starts at 1, port 0 reserved)
static NEXT_PORT_ID: AtomicUsize = AtomicUsize::new(1);

/// IPC initialization flag
static IPC_INIT: AtomicBool = AtomicBool::new(false);

/// Initialize the IPC system
///
/// Must be called during kernel initialization, after the scheduler is set up.
pub fn init() {
    let mut registry = PORT_REGISTRY.lock();
    *registry = Some(BTreeMap::new());
    IPC_INIT.store(true, Ordering::SeqCst);
    log::info!("IPC system initialized");
}

/// Create a new port
///
/// The calling thread becomes the owner of the port and is the only
/// thread that can receive messages from it. Any thread can send to the port.
///
/// Returns the port ID on success.
pub fn port_create() -> Result<PortId, IpcError> {
    if !IPC_INIT.load(Ordering::SeqCst) {
        return Err(IpcError::InvalidPortId);
    }

    let current_tid = current_thread_id();
    if current_tid.0 == 0 {
        // Idle thread can't create ports
        return Err(IpcError::InvalidPortId);
    }

    // Allocate new port ID
    let port_id = PortId(NEXT_PORT_ID.fetch_add(1, Ordering::SeqCst));

    // Create port structure
    let port = Port::new(port_id, current_tid, DEFAULT_PORT_CAPACITY);

    // Insert into registry
    let mut registry = PORT_REGISTRY.lock();
    if let Some(ref mut map) = *registry {
        map.insert(port_id, port);
        log::debug!("Port {} created by thread {}", port_id.0, current_tid.0);
        Ok(port_id)
    } else {
        Err(IpcError::InvalidPortId)
    }
}

/// Destroy a port
///
/// Only the port owner can destroy a port. All threads waiting on the port
/// will be woken and will receive PortNotFound errors on their next receive attempt.
///
/// Returns Ok if the port was destroyed, or an error if:
/// - Port doesn't exist
/// - Calling thread is not the owner
pub fn port_destroy(port_id: PortId) -> Result<(), IpcError> {
    if !IPC_INIT.load(Ordering::SeqCst) {
        return Err(IpcError::PortNotFound);
    }

    let current_tid = current_thread_id();

    let waiters = {
        let mut registry = PORT_REGISTRY.lock();
        if let Some(ref mut map) = *registry {
            // Check if port exists and we own it
            if let Some(port) = map.get(&port_id) {
                if port.owner != current_tid {
                    return Err(IpcError::NotOwner);
                }
                // Get waiters before removing port
                let waiters = port.wait_queue.clone();

                // Remove port from registry
                map.remove(&port_id);
                log::debug!("Port {} destroyed by thread {}", port_id.0, current_tid.0);

                waiters
            } else {
                return Err(IpcError::PortNotFound);
            }
        } else {
            return Err(IpcError::PortNotFound);
        }
    };

    // Wake all waiting threads (they'll get PortNotFound on next iteration)
    for waiter in waiters {
        wake_thread(waiter);
    }

    Ok(())
}

/// Send a message to a port (non-blocking)
///
/// Posts the message to the port's queue and returns immediately.
/// If a receiver is blocked waiting for messages, it will be woken.
///
/// Returns:
/// - Ok(()) if message was queued successfully
/// - Err(IpcError::PortNotFound) if port doesn't exist
/// - Err(IpcError::QueueFull) if port's message queue is at capacity
pub fn port_send(port_id: PortId, message: Message) -> Result<(), IpcError> {
    if !IPC_INIT.load(Ordering::SeqCst) {
        return Err(IpcError::PortNotFound);
    }

    let current_tid = current_thread_id();

    let waiter = {
        let mut registry = PORT_REGISTRY.lock();
        if let Some(ref mut map) = *registry {
            let port = map.get_mut(&port_id).ok_or(IpcError::PortNotFound)?;

            // Check if queue is full
            if port.message_queue.len() >= port.capacity {
                return Err(IpcError::QueueFull);
            }

            // Add message to queue
            let queued = QueuedMessage {
                message,
                sender_id: current_tid,
            };
            port.message_queue.push_back(queued);

            // Wake a waiting receiver if any
            port.wait_queue.pop()
        } else {
            return Err(IpcError::PortNotFound);
        }
    };

    // Wake the receiver outside the lock
    if let Some(waiter) = waiter {
        wake_thread(waiter);
    }

    Ok(())
}

/// Receive a message from a port (blocking)
///
/// If messages are available in the queue, returns the next message immediately.
/// If the queue is empty, blocks the calling thread until a message arrives.
///
/// Only the port owner can receive messages.
///
/// Returns:
/// - Ok(Message) when a message is received
/// - Err(IpcError::PortNotFound) if port doesn't exist
/// - Err(IpcError::NotOwner) if calling thread is not the port owner
pub fn port_recv(port_id: PortId) -> Result<Message, IpcError> {
    if !IPC_INIT.load(Ordering::SeqCst) {
        return Err(IpcError::PortNotFound);
    }

    let current_tid = current_thread_id();

    loop {
        // Try to receive message
        {
            let mut registry = PORT_REGISTRY.lock();
            if let Some(ref mut map) = *registry {
                let port = map.get_mut(&port_id).ok_or(IpcError::PortNotFound)?;

                // Check ownership
                if port.owner != current_tid {
                    return Err(IpcError::NotOwner);
                }

                // If message available, return it
                if let Some(queued) = port.message_queue.pop_front() {
                    return Ok(queued.message);
                }

                // No message - add to wait queue if not already there
                if !port.wait_queue.contains(&current_tid) {
                    port.wait_queue.push(current_tid);
                }
            } else {
                return Err(IpcError::PortNotFound);
            }
        }

        // Block and wait for message
        log::debug!("port_recv: blocking thread {} on port {}", current_tid.0, port_id.0);
        block_current_thread();
        log::debug!("port_recv: yielding thread {}", current_tid.0);
        yield_now();
        log::debug!("port_recv: thread {} woke up, retrying", current_tid.0);

        // When woken, loop back to try receiving
    }
}

/// Try to receive a message from a port (non-blocking)
///
/// If messages are available, returns Some(message).
/// If the queue is empty, returns None immediately without blocking.
///
/// Only the port owner can receive messages.
///
/// Returns:
/// - Ok(Some(Message)) if a message was available
/// - Ok(None) if queue is empty
/// - Err(IpcError::PortNotFound) if port doesn't exist
/// - Err(IpcError::NotOwner) if calling thread is not the port owner
pub fn port_try_recv(port_id: PortId) -> Result<Option<Message>, IpcError> {
    if !IPC_INIT.load(Ordering::SeqCst) {
        return Err(IpcError::PortNotFound);
    }

    let current_tid = current_thread_id();

    let mut registry = PORT_REGISTRY.lock();
    if let Some(ref mut map) = *registry {
        let port = map.get_mut(&port_id).ok_or(IpcError::PortNotFound)?;

        // Check ownership
        if port.owner != current_tid {
            return Err(IpcError::NotOwner);
        }

        // Return message if available, None otherwise
        Ok(port.message_queue.pop_front().map(|q| q.message))
    } else {
        Err(IpcError::PortNotFound)
    }
}

/// Port statistics for debugging
pub struct PortStats {
    pub port_id: PortId,
    pub owner: ThreadId,
    pub messages_queued: usize,
    pub threads_waiting: usize,
    pub capacity: usize,
}

/// Get statistics for a port (for debugging)
pub fn get_port_stats(port_id: PortId) -> Option<PortStats> {
    let registry = PORT_REGISTRY.lock();
    if let Some(ref map) = *registry {
        map.get(&port_id).map(|port| PortStats {
            port_id: port.id,
            owner: port.owner,
            messages_queued: port.message_queue.len(),
            threads_waiting: port.wait_queue.len(),
            capacity: port.capacity,
        })
    } else {
        None
    }
}

/// List all active ports (for debugging)
pub fn list_all_ports() -> Vec<PortId> {
    let registry = PORT_REGISTRY.lock();
    if let Some(ref map) = *registry {
        map.keys().copied().collect()
    } else {
        Vec::new()
    }
}