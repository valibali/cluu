//! IPC Traits
//!
//! This module defines the core traits for IPC operations.
//! These traits follow SOLID principles and allow for different
//! implementations and testing strategies.

use crate::error::Error;
use crate::ipc::message::Message;
use crate::mm::traits::PageFlags;
use crate::sched::thread::ThreadId;
use x86_64::VirtAddr;

/// IPC Endpoint Operations
///
/// Defines the interface for IPC endpoints that can send and receive messages.
///
/// # Synchronous Semantics
///
/// - `send()`: Blocks sender until receiver calls `recv()`
/// - `recv()`: Blocks receiver until sender calls `send()`
/// - Operations complete when both parties are ready (rendezvous)
///
/// # Thread Safety
///
/// Implementations must be `Send` to allow transfer between threads.
///
/// # Example Implementation
///
/// ```rust,no_run
/// struct RendezvousPoint {
///     waiting_sender: Option<(ThreadId, Message)>,
///     waiting_receiver: Option<ThreadId>,
/// }
///
/// impl IpcEndpoint for RendezvousPoint {
///     fn send(&mut self, from: ThreadId, msg: &Message) -> Result<(), Error> {
///         if let Some(receiver) = self.waiting_receiver.take() {
///             // Receiver waiting - complete rendezvous immediately
///             // ... unblock receiver, transfer message ...
///             Ok(())
///         } else {
///             // No receiver - block sender
///             self.waiting_sender = Some((from, msg.clone()));
///             Err(Error::WouldBlock)
///         }
///     }
///
///     fn recv(&mut self, to: ThreadId) -> Result<(ThreadId, Message), Error> {
///         if let Some((sender, msg)) = self.waiting_sender.take() {
///             // Sender waiting - complete rendezvous immediately
///             // ... unblock sender ...
///             Ok((sender, msg))
///         } else {
///             // No sender - block receiver
///             self.waiting_receiver = Some(to);
///             Err(Error::WouldBlock)
///         }
///     }
///
///     fn has_pending_senders(&self) -> bool {
///         self.waiting_sender.is_some()
///     }
/// }
/// ```
pub trait IpcEndpoint: Send {
    /// Send a message to this endpoint
    ///
    /// # Arguments
    ///
    /// * `from` - Sending thread ID
    /// * `msg` - Message to send
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Message delivered (receiver was waiting)
    /// * `Err(Error::WouldBlock)` - No receiver ready, sender should block
    /// * `Err(Error)` - Other error
    ///
    /// # Blocking Behavior
    ///
    /// If this returns `WouldBlock`, the caller should:
    /// 1. Add sender to endpoint's waiting queue
    /// 2. Mark sender thread as Blocked
    /// 3. Call scheduler to switch to another thread
    /// 4. Sender will be unblocked when receiver arrives
    fn send(&mut self, from: ThreadId, msg: &Message) -> Result<(), Error>;

    /// Receive a message from this endpoint
    ///
    /// # Arguments
    ///
    /// * `to` - Receiving thread ID
    ///
    /// # Returns
    ///
    /// * `Ok((sender, msg))` - Message received (sender was waiting)
    /// * `Err(Error::WouldBlock)` - No sender ready, receiver should block
    /// * `Err(Error)` - Other error
    ///
    /// # Blocking Behavior
    ///
    /// If this returns `WouldBlock`, the caller should:
    /// 1. Add receiver to endpoint's waiting queue
    /// 2. Mark receiver thread as Blocked
    /// 3. Call scheduler to switch to another thread
    /// 4. Receiver will be unblocked when sender arrives
    fn recv(&mut self, to: ThreadId) -> Result<(ThreadId, Message), Error>;

    /// Check if there are pending senders
    ///
    /// This allows optimistic recv() - if senders are pending,
    /// recv() will not block.
    fn has_pending_senders(&self) -> bool;
}

/// Message Transfer Operations
///
/// Defines operations for transferring message buffers between address spaces.
///
/// # Transfer Modes
///
/// - **Copy**: Copy buffer contents (slow but safe)
/// - **Grant**: Transfer page ownership (fast, zero-copy)
/// - **Map**: Create shared mapping (fast, zero-copy, shared)
///
/// # Safety
///
/// Implementations must ensure:
/// - Grant removes pages from source space
/// - Map creates read-only or read-write mappings as specified
/// - Copy validates address ranges
///
/// # Example
///
/// ```rust,no_run
/// // Copy 4096 bytes from sender to receiver
/// transfer.copy_buffer(
///     sender_space,
///     VirtAddr::new(0x400000),
///     receiver_space,
///     VirtAddr::new(0x500000),
///     4096,
/// )?;
///
/// // Grant page ownership (zero-copy)
/// transfer.grant_buffer(
///     sender_space,
///     VirtAddr::new(0x400000),
///     receiver_space,
///     VirtAddr::new(0x500000),
///     4096,
///     PageFlags::USER_READABLE | PageFlags::USER_WRITABLE,
/// )?;
/// ```
pub trait MessageTransfer: Send {
    /// Copy buffer between address spaces
    ///
    /// # Arguments
    ///
    /// * `from_space` - Source address space ID
    /// * `from_addr` - Source virtual address
    /// * `to_space` - Destination address space ID
    /// * `to_addr` - Destination virtual address
    /// * `len` - Length in bytes
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Buffer copied successfully
    /// * `Err(Error::InvalidAddress)` - Invalid address range
    /// * `Err(Error::NotMapped)` - Page not mapped
    /// * `Err(Error::PermissionDenied)` - Permission violation
    fn copy_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
    ) -> Result<(), Error>;

    /// Grant pages between address spaces (zero-copy)
    ///
    /// Removes pages from source space and adds them to destination space.
    /// Source space loses access after this operation.
    ///
    /// # Arguments
    ///
    /// * `from_space` - Source address space ID
    /// * `from_addr` - Source virtual address (must be page-aligned)
    /// * `to_space` - Destination address space ID
    /// * `to_addr` - Destination virtual address (must be page-aligned)
    /// * `len` - Length in bytes (must be page-aligned)
    /// * `flags` - Page flags for destination mapping
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Pages granted successfully
    /// * `Err(Error::InvalidAddress)` - Address not page-aligned
    /// * `Err(Error::NotMapped)` - Page not mapped in source
    /// * `Err(Error::AlreadyMapped)` - Destination already mapped
    fn grant_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
        flags: PageFlags,
    ) -> Result<(), Error>;

    /// Create shared mapping between address spaces (zero-copy)
    ///
    /// Both spaces can access the same physical pages.
    ///
    /// # Arguments
    ///
    /// * `from_space` - Source address space ID
    /// * `from_addr` - Source virtual address (must be page-aligned)
    /// * `to_space` - Destination address space ID
    /// * `to_addr` - Destination virtual address (must be page-aligned)
    /// * `len` - Length in bytes (must be page-aligned)
    /// * `flags` - Page flags for destination mapping
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Mapping created successfully
    /// * `Err(Error::InvalidAddress)` - Address not page-aligned
    /// * `Err(Error::NotMapped)` - Page not mapped in source
    /// * `Err(Error::AlreadyMapped)` - Destination already mapped
    fn map_buffer(
        &mut self,
        from_space: usize,
        from_addr: VirtAddr,
        to_space: usize,
        to_addr: VirtAddr,
        len: usize,
        flags: PageFlags,
    ) -> Result<(), Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock IpcEndpoint for testing
    struct MockEndpoint {
        send_called: bool,
        recv_called: bool,
    }

    impl MockEndpoint {
        fn new() -> Self {
            Self {
                send_called: false,
                recv_called: false,
            }
        }
    }

    impl IpcEndpoint for MockEndpoint {
        fn send(&mut self, _from: ThreadId, _msg: &Message) -> Result<(), Error> {
            self.send_called = true;
            Ok(())
        }

        fn recv(&mut self, _to: ThreadId) -> Result<(ThreadId, Message), Error> {
            self.recv_called = true;
            Ok((ThreadId::new(1), Message::empty()))
        }

        fn has_pending_senders(&self) -> bool {
            false
        }
    }

    #[test]
    fn test_mock_endpoint() {
        let mut endpoint = MockEndpoint::new();
        assert!(!endpoint.send_called);
        assert!(!endpoint.recv_called);

        let _ = endpoint.send(ThreadId::new(1), &Message::empty());
        assert!(endpoint.send_called);

        let _ = endpoint.recv(ThreadId::new(2));
        assert!(endpoint.recv_called);
    }
}
