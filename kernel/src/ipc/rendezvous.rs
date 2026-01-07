//! Rendezvous IPC Mechanism
//!
//! This module implements synchronous rendezvous-style IPC where sender
//! and receiver must both be ready before message transfer occurs.
//!
//! # Algorithm
//!
//! 1. **Sender arrives first**:
//!    - Sender blocks and waits in queue
//!    - When receiver arrives, match occurs
//!    - Message transferred, both threads unblock
//!
//! 2. **Receiver arrives first**:
//!    - Receiver blocks and waits in queue
//!    - When sender arrives, match occurs
//!    - Message transferred, both threads unblock
//!
//! # Data Structures
//!
//! - `waiting_senders`: Queue of blocked senders
//! - `waiting_receivers`: Queue of blocked receivers
//! - On operation, check if partner waiting; if so, complete immediately
//!
//! # Thread Safety
//!
//! The rendezvous point must be accessed under lock in multi-CPU systems.
//! For now, single-CPU operation is assumed.

use crate::error::Error;
use crate::ipc::message::Message;
use crate::ipc::traits::IpcEndpoint;
use crate::sched::thread::ThreadId;
use alloc::collections::VecDeque;

/// Waiting Sender
///
/// Represents a thread blocked waiting to send a message.
#[derive(Debug, Clone)]
struct WaitingSender {
    /// Thread ID of sender
    thread_id: ThreadId,

    /// Message to send
    message: Message,
}

/// Rendezvous Point
///
/// Implements synchronous IPC rendezvous between threads.
///
/// # Operation
///
/// When a thread performs an IPC operation:
/// 1. Check if partner is waiting
/// 2. If yes: complete rendezvous, unblock both threads
/// 3. If no: add to waiting queue, block calling thread
///
/// # Example
///
/// ```rust,no_run
/// let mut rendezvous = RendezvousPoint::new();
///
/// // Thread A tries to send (no receiver yet)
/// match rendezvous.send(thread_a, &msg) {
///     Err(Error::WouldBlock) => {
///         // Block thread A, switch to another thread
///     }
///     Ok(()) => {
///         // Message delivered (receiver was already waiting)
///     }
///     Err(e) => {
///         // Handle error
///     }
/// }
///
/// // Thread B tries to receive
/// match rendezvous.recv(thread_b) {
///     Ok((sender_id, msg)) => {
///         // Got message from thread A
///         // Both threads now unblocked
///     }
///     Err(Error::WouldBlock) => {
///         // Block thread B, switch to another thread
///     }
///     Err(e) => {
///         // Handle error
///     }
/// }
/// ```
pub struct RendezvousPoint {
    /// Queue of threads waiting to send messages
    waiting_senders: VecDeque<WaitingSender>,

    /// Queue of threads waiting to receive messages
    waiting_receivers: VecDeque<ThreadId>,
}

impl RendezvousPoint {
    /// Create a new rendezvous point
    pub fn new() -> Self {
        Self {
            waiting_senders: VecDeque::new(),
            waiting_receivers: VecDeque::new(),
        }
    }

    /// Get number of waiting senders
    pub fn sender_count(&self) -> usize {
        self.waiting_senders.len()
    }

    /// Get number of waiting receivers
    pub fn receiver_count(&self) -> usize {
        self.waiting_receivers.len()
    }

    /// Check if endpoint is idle (no waiting threads)
    pub fn is_idle(&self) -> bool {
        self.waiting_senders.is_empty() && self.waiting_receivers.is_empty()
    }

    /// Cancel a pending send operation
    ///
    /// Removes the sender from the waiting queue if present.
    /// Returns true if sender was found and removed.
    pub fn cancel_send(&mut self, thread_id: ThreadId) -> bool {
        if let Some(pos) = self
            .waiting_senders
            .iter()
            .position(|s| s.thread_id == thread_id)
        {
            self.waiting_senders.remove(pos);
            true
        } else {
            false
        }
    }

    /// Cancel a pending receive operation
    ///
    /// Removes the receiver from the waiting queue if present.
    /// Returns true if receiver was found and removed.
    pub fn cancel_recv(&mut self, thread_id: ThreadId) -> bool {
        if let Some(pos) = self.waiting_receivers.iter().position(|&r| r == thread_id) {
            self.waiting_receivers.remove(pos);
            true
        } else {
            false
        }
    }
}

impl IpcEndpoint for RendezvousPoint {
    fn send(&mut self, from: ThreadId, msg: &Message) -> Result<(), Error> {
        // Check if a receiver is waiting
        if let Some(_receiver_id) = self.waiting_receivers.pop_front() {
            // Rendezvous! Receiver was waiting for a message.
            // In a full implementation, we would:
            // 1. Unblock the receiver thread
            // 2. Copy message to receiver's buffer
            // 3. Mark both threads as Ready
            //
            // For now, we just return Ok to indicate successful rendezvous.
            // The actual thread unblocking will be done by the IPC system call handler.
            //
            // Return Ok(()) to indicate rendezvous completed
            Ok(())
        } else {
            // No receiver waiting - sender must block
            self.waiting_senders.push_back(WaitingSender {
                thread_id: from,
                message: msg.clone(),
            });

            // Return WouldBlock to tell caller to block this thread
            Err(Error::WouldBlock)
        }
    }

    fn recv(&mut self, to: ThreadId) -> Result<(ThreadId, Message), Error> {
        // Check if a sender is waiting
        if let Some(waiting) = self.waiting_senders.pop_front() {
            // Rendezvous! Sender was waiting to send.
            // In a full implementation, we would:
            // 1. Unblock the sender thread
            // 2. Transfer message to receiver
            // 3. Mark both threads as Ready
            //
            // For now, return the sender ID and message to indicate successful rendezvous.
            Ok((waiting.thread_id, waiting.message))
        } else {
            // No sender waiting - receiver must block
            self.waiting_receivers.push_back(to);

            // Return WouldBlock to tell caller to block this thread
            Err(Error::WouldBlock)
        }
    }

    fn has_pending_senders(&self) -> bool {
        !self.waiting_senders.is_empty()
    }
}

impl Default for RendezvousPoint {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::message::MessageTag;

    #[test]
    fn test_rendezvous_new() {
        let rv = RendezvousPoint::new();
        assert_eq!(rv.sender_count(), 0);
        assert_eq!(rv.receiver_count(), 0);
        assert!(rv.is_idle());
        assert!(!rv.has_pending_senders());
    }

    #[test]
    fn test_send_blocks_when_no_receiver() {
        let mut rv = RendezvousPoint::new();
        let sender = ThreadId::new(1);
        let msg = Message::new(MessageTag::new(42, 1, 0));

        let result = rv.send(sender, &msg);
        assert_eq!(result, Err(Error::WouldBlock));
        assert_eq!(rv.sender_count(), 1);
        assert!(rv.has_pending_senders());
    }

    #[test]
    fn test_recv_blocks_when_no_sender() {
        let mut rv = RendezvousPoint::new();
        let receiver = ThreadId::new(2);

        let result = rv.recv(receiver);
        assert_eq!(result, Err(Error::WouldBlock));
        assert_eq!(rv.receiver_count(), 1);
    }

    #[test]
    fn test_rendezvous_sender_first() {
        let mut rv = RendezvousPoint::new();
        let sender = ThreadId::new(1);
        let receiver = ThreadId::new(2);
        let msg = Message::new(MessageTag::new(42, 1, 0));

        // Sender arrives first
        let result = rv.send(sender, &msg);
        assert_eq!(result, Err(Error::WouldBlock));
        assert_eq!(rv.sender_count(), 1);

        // Receiver arrives - rendezvous occurs
        let result = rv.recv(receiver);
        assert!(result.is_ok());
        let (sender_id, received_msg) = result.unwrap();
        assert_eq!(sender_id, sender);
        assert_eq!(received_msg.tag.label, 42);

        // Both queues now empty
        assert_eq!(rv.sender_count(), 0);
        assert_eq!(rv.receiver_count(), 0);
        assert!(rv.is_idle());
    }

    #[test]
    fn test_rendezvous_receiver_first() {
        let mut rv = RendezvousPoint::new();
        let sender = ThreadId::new(1);
        let receiver = ThreadId::new(2);
        let msg = Message::new(MessageTag::new(42, 1, 0));

        // Receiver arrives first
        let result = rv.recv(receiver);
        assert_eq!(result, Err(Error::WouldBlock));
        assert_eq!(rv.receiver_count(), 1);

        // Sender arrives - rendezvous occurs
        let result = rv.send(sender, &msg);
        assert!(result.is_ok());

        // Both queues now empty
        assert_eq!(rv.sender_count(), 0);
        assert_eq!(rv.receiver_count(), 0);
        assert!(rv.is_idle());
    }

    #[test]
    fn test_multiple_senders() {
        let mut rv = RendezvousPoint::new();
        let sender1 = ThreadId::new(1);
        let sender2 = ThreadId::new(2);
        let sender3 = ThreadId::new(3);
        let receiver = ThreadId::new(10);

        let msg1 = Message::new(MessageTag::new(1, 0, 0));
        let msg2 = Message::new(MessageTag::new(2, 0, 0));
        let msg3 = Message::new(MessageTag::new(3, 0, 0));

        // Three senders arrive
        rv.send(sender1, &msg1).unwrap_err();
        rv.send(sender2, &msg2).unwrap_err();
        rv.send(sender3, &msg3).unwrap_err();
        assert_eq!(rv.sender_count(), 3);

        // Receive messages in FIFO order
        let (id, msg) = rv.recv(receiver).unwrap();
        assert_eq!(id, sender1);
        assert_eq!(msg.tag.label, 1);

        let (id, msg) = rv.recv(receiver).unwrap();
        assert_eq!(id, sender2);
        assert_eq!(msg.tag.label, 2);

        let (id, msg) = rv.recv(receiver).unwrap();
        assert_eq!(id, sender3);
        assert_eq!(msg.tag.label, 3);

        assert!(rv.is_idle());
    }

    #[test]
    fn test_multiple_receivers() {
        let mut rv = RendezvousPoint::new();
        let sender = ThreadId::new(1);
        let receiver1 = ThreadId::new(10);
        let receiver2 = ThreadId::new(11);
        let receiver3 = ThreadId::new(12);

        // Three receivers arrive
        rv.recv(receiver1).unwrap_err();
        rv.recv(receiver2).unwrap_err();
        rv.recv(receiver3).unwrap_err();
        assert_eq!(rv.receiver_count(), 3);

        // Send messages - each rendezvous with waiting receivers in FIFO order
        let msg1 = Message::new(MessageTag::new(1, 0, 0));
        rv.send(sender, &msg1).unwrap();
        assert_eq!(rv.receiver_count(), 2);

        let msg2 = Message::new(MessageTag::new(2, 0, 0));
        rv.send(sender, &msg2).unwrap();
        assert_eq!(rv.receiver_count(), 1);

        let msg3 = Message::new(MessageTag::new(3, 0, 0));
        rv.send(sender, &msg3).unwrap();
        assert_eq!(rv.receiver_count(), 0);

        assert!(rv.is_idle());
    }

    #[test]
    fn test_cancel_send() {
        let mut rv = RendezvousPoint::new();
        let sender = ThreadId::new(1);
        let msg = Message::new(MessageTag::new(42, 0, 0));

        rv.send(sender, &msg).unwrap_err();
        assert_eq!(rv.sender_count(), 1);

        // Cancel the send
        assert!(rv.cancel_send(sender));
        assert_eq!(rv.sender_count(), 0);

        // Cancel again - should return false
        assert!(!rv.cancel_send(sender));
    }

    #[test]
    fn test_cancel_recv() {
        let mut rv = RendezvousPoint::new();
        let receiver = ThreadId::new(2);

        rv.recv(receiver).unwrap_err();
        assert_eq!(rv.receiver_count(), 1);

        // Cancel the recv
        assert!(rv.cancel_recv(receiver));
        assert_eq!(rv.receiver_count(), 0);

        // Cancel again - should return false
        assert!(!rv.cancel_recv(receiver));
    }

    #[test]
    fn test_cancel_middle_of_queue() {
        let mut rv = RendezvousPoint::new();
        let sender1 = ThreadId::new(1);
        let sender2 = ThreadId::new(2);
        let sender3 = ThreadId::new(3);
        let msg = Message::new(MessageTag::new(42, 0, 0));

        // Add three senders
        rv.send(sender1, &msg).unwrap_err();
        rv.send(sender2, &msg).unwrap_err();
        rv.send(sender3, &msg).unwrap_err();
        assert_eq!(rv.sender_count(), 3);

        // Cancel middle sender
        assert!(rv.cancel_send(sender2));
        assert_eq!(rv.sender_count(), 2);

        // Verify remaining senders are sender1 and sender3
        let receiver = ThreadId::new(10);
        let (id, _) = rv.recv(receiver).unwrap();
        assert_eq!(id, sender1);

        let (id, _) = rv.recv(receiver).unwrap();
        assert_eq!(id, sender3);

        assert!(rv.is_idle());
    }
}
