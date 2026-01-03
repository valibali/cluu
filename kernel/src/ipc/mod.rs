//! IPC (Inter-Process Communication) Module
//!
//! This module implements synchronous message passing between threads using
//! a rendezvous mechanism. IPC is the core communication primitive in the
//! CLUU microkernel.
//!
//! # Overview
//!
//! IPC in CLUU follows the L4 microkernel tradition:
//! - **Synchronous**: Sender blocks until receiver ready, and vice versa
//! - **Rendezvous**: Direct thread-to-thread communication
//! - **Capability-based**: IPC requires capability to target thread
//! - **Buffer transfer**: Optional zero-copy data transfer via page operations
//!
//! # IPC Operations
//!
//! - **Send**: Send message, don't wait for reply
//! - **Recv**: Wait for incoming message
//! - **Call**: Send message + wait for reply (most common)
//! - **Reply**: Reply to a received Call
//! - **ReplyRecv**: Reply + wait for next (server loop optimization)
//!
//! # Message Structure
//!
//! Messages consist of:
//! - **Tag**: Message type and metadata
//! - **Words**: 6 register-passed words (fast path)
//! - **Buffer**: Optional memory buffer descriptor
//! - **Token**: Optional capability token
//! - **Timeout**: Optional timeout value
//!
//! # Buffer Transfer Modes
//!
//! - **Copy**: Copy data between address spaces (safe but slow)
//! - **Grant**: Transfer page ownership (zero-copy)
//! - **Map**: Create shared mapping (zero-copy, shared)
//!
//! # Integration with Scheduler
//!
//! IPC operations integrate tightly with the scheduler:
//! - Threads block when waiting for IPC partner
//! - Threads unblock when IPC completes
//! - Optional time slice donation to receiver

pub mod message;
pub mod traits;
pub mod rendezvous;
pub mod transfer;

// Re-exports
pub use message::{
    BufferDesc, IpcFlags, IpcOp, Message, MessageTag,
};
pub use traits::{IpcEndpoint, MessageTransfer};
pub use rendezvous::RendezvousPoint;
pub use transfer::BufferTransfer;
