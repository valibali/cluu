//! IPC Message Types
//!
//! This module defines the message structures used for IPC communication.
//!
//! # Message Layout
//!
//! Messages are designed for efficiency:
//! - Small messages (≤6 words) fit entirely in registers
//! - Large messages use buffer descriptor for data transfer
//! - Message tag encodes type and length information
//!
//! # Example
//!
//! ```rust,no_run
//! // Small message (register-only)
//! let mut msg = Message::new(MessageTag::new(42, 2, 0));
//! msg.words[0] = 100;
//! msg.words[1] = 200;
//!
//! // Large message with buffer
//! let mut msg = Message::with_buffer(
//!     MessageTag::new(43, 0, MessageTag::HAS_BUFFER),
//!     BufferDesc::new(0x400000, 4096),
//! );
//! ```

use core::fmt;

/// IPC Operation Type
///
/// Defines the type of IPC operation being performed.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcOp {
    /// Send message, don't wait for reply
    ///
    /// Sender blocks until receiver calls Recv.
    Send = 0,

    /// Wait for incoming message
    ///
    /// Receiver blocks until sender calls Send or Call.
    Recv = 1,

    /// Send message + wait for reply
    ///
    /// Most common operation. Sender blocks until receiver calls Reply.
    /// Equivalent to Send followed by Recv.
    Call = 2,

    /// Reply to a received Call
    ///
    /// Unblocks the original caller and sends reply message.
    Reply = 3,

    /// Reply + wait for next message
    ///
    /// Optimization for server loops: reply to current message
    /// and immediately wait for next message in one operation.
    ReplyRecv = 4,
}

/// IPC Flags
///
/// Control how IPC operations behave, especially regarding buffer transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcFlags(u32);

impl IpcFlags {
    /// No flags
    pub const NONE: Self = Self(0);

    /// Transfer page ownership with message (zero-copy)
    ///
    /// Pages are removed from sender and added to receiver.
    /// Sender loses access after the transfer.
    pub const GRANT: Self = Self(1 << 0);

    /// Create shared mapping with message (zero-copy)
    ///
    /// Both sender and receiver can access the same physical pages.
    pub const MAP: Self = Self(1 << 1);

    /// Use timeout from message.timeout field
    ///
    /// If IPC doesn't complete within timeout, return error.
    pub const TIMEOUT: Self = Self(1 << 2);

    /// Donate remaining timeslice to receiver
    ///
    /// Optimization: receiver runs immediately without scheduler overhead.
    pub const DONATE: Self = Self(1 << 3);

    /// Create empty flags
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Check if a flag is set
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) != 0
    }

    /// Set a flag
    pub const fn with(self, flag: Self) -> Self {
        Self(self.0 | flag.0)
    }

    /// Clear a flag
    pub const fn without(self, flag: Self) -> Self {
        Self(self.0 & !flag.0)
    }

    /// Get raw value
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Create from raw value
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
}

/// Message Tag
///
/// Encodes message type, length, and metadata in a compact form.
///
/// # Fields
///
/// - **label**: Application-defined message type (32 bits)
/// - **words**: Number of valid words in message (0-6)
/// - **extra**: Extra flags (HAS_BUFFER, etc.)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageTag {
    /// Application-defined message type
    ///
    /// Common conventions:
    /// - 0: Generic message
    /// - 1-1000: System service messages
    /// - 1001+: Application-specific messages
    pub label: u32,

    /// Number of valid words (0-6)
    ///
    /// Only words[0..words] contain valid data.
    pub words: u8,

    /// Extra flags
    ///
    /// Bit 0: HAS_BUFFER - message includes buffer descriptor
    /// Bits 1-7: Reserved
    pub extra: u8,
}

impl MessageTag {
    /// Flag indicating message has buffer descriptor
    pub const HAS_BUFFER: u8 = 1 << 0;

    /// Create a new message tag
    ///
    /// # Arguments
    ///
    /// * `label` - Application-defined message type
    /// * `words` - Number of valid words (0-6)
    /// * `extra` - Extra flags (HAS_BUFFER, etc.)
    ///
    /// # Panics
    ///
    /// Panics if `words` > 6.
    pub const fn new(label: u32, words: u8, extra: u8) -> Self {
        assert!(words <= 6, "words must be 0-6");
        Self {
            label,
            words,
            extra,
        }
    }

    /// Check if message has buffer
    pub const fn has_buffer(self) -> bool {
        (self.extra & Self::HAS_BUFFER) != 0
    }

    /// Set buffer flag
    pub const fn with_buffer(self) -> Self {
        Self {
            label: self.label,
            words: self.words,
            extra: self.extra | Self::HAS_BUFFER,
        }
    }

    /// Create empty tag
    pub const fn empty() -> Self {
        Self {
            label: 0,
            words: 0,
            extra: 0,
        }
    }
}

/// Buffer Descriptor
///
/// Describes a memory buffer to be transferred with a message.
///
/// The buffer can be transferred in three modes:
/// - **Copy**: Data copied between address spaces
/// - **Grant**: Page ownership transferred (zero-copy)
/// - **Map**: Shared mapping created (zero-copy)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferDesc {
    /// Virtual address in sender's address space
    pub addr: usize,

    /// Length in bytes
    ///
    /// Must be page-aligned for GRANT and MAP modes.
    pub len: usize,
}

impl BufferDesc {
    /// Create a new buffer descriptor
    pub const fn new(addr: usize, len: usize) -> Self {
        Self { addr, len }
    }

    /// Create empty buffer descriptor
    pub const fn empty() -> Self {
        Self { addr: 0, len: 0 }
    }

    /// Check if buffer is empty
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Check if address and length are page-aligned
    pub const fn is_page_aligned(self) -> bool {
        (self.addr & 0xFFF) == 0 && (self.len & 0xFFF) == 0
    }
}

/// IPC Message
///
/// A message transferred between threads via IPC.
///
/// # Layout
///
/// ```text
/// ┌─────────────────────────────────────┐
/// │ MessageTag (label, words, extra)    │  8 bytes
/// ├─────────────────────────────────────┤
/// │ words[0..6]                         │  48 bytes (6 x 8)
/// ├─────────────────────────────────────┤
/// │ BufferDesc (addr, len)              │  16 bytes
/// ├─────────────────────────────────────┤
/// │ timeout                             │  8 bytes
/// └─────────────────────────────────────┘
/// Total: 80 bytes
/// ```
///
/// # Register Passing
///
/// On x86_64, small messages (≤6 words) can be passed entirely in registers:
/// - Tag: RDI, RSI
/// - words[0]: RDX
/// - words[1]: RCX
/// - words[2]: R8
/// - words[3]: R9
/// - words[4-5]: Stack or memory
///
/// This avoids memory copies for small messages.
#[repr(C)]
#[derive(Clone, PartialEq)]
pub struct Message {
    /// Message type and metadata
    pub tag: MessageTag,

    /// Register-passed payload (6 x 64-bit words)
    ///
    /// Common uses:
    /// - System call parameters
    /// - Error codes
    /// - File descriptors
    /// - Small data values
    pub words: [usize; 6],

    /// Optional buffer descriptor for large data
    pub buffer: BufferDesc,

    /// Timeout in microseconds (if TIMEOUT flag set)
    ///
    /// If 0 and TIMEOUT flag set, IPC returns immediately if partner not ready.
    pub timeout: u64,
}

impl Message {
    /// Create a new message with tag
    pub const fn new(tag: MessageTag) -> Self {
        Self {
            tag,
            words: [0; 6],
            buffer: BufferDesc::empty(),
            timeout: 0,
        }
    }

    /// Create message with buffer
    pub const fn with_buffer(tag: MessageTag, buffer: BufferDesc) -> Self {
        Self {
            tag: tag.with_buffer(),
            words: [0; 6],
            buffer,
            timeout: 0,
        }
    }

    /// Create empty message
    pub const fn empty() -> Self {
        Self::new(MessageTag::empty())
    }

    /// Set timeout
    pub const fn with_timeout(mut self, timeout: u64) -> Self {
        self.timeout = timeout;
        self
    }

    /// Check if message has buffer
    pub const fn has_buffer(&self) -> bool {
        self.tag.has_buffer()
    }

    /// Check if message is empty (no words, no buffer)
    pub const fn is_empty(&self) -> bool {
        self.tag.words == 0 && !self.tag.has_buffer()
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("tag", &self.tag)
            .field("words", &&self.words[..self.tag.words as usize])
            .field("buffer", &self.buffer)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_op_values() {
        assert_eq!(IpcOp::Send as u8, 0);
        assert_eq!(IpcOp::Recv as u8, 1);
        assert_eq!(IpcOp::Call as u8, 2);
        assert_eq!(IpcOp::Reply as u8, 3);
        assert_eq!(IpcOp::ReplyRecv as u8, 4);
    }

    #[test]
    fn test_ipc_flags() {
        let flags = IpcFlags::empty();
        assert!(!flags.contains(IpcFlags::GRANT));
        assert!(!flags.contains(IpcFlags::MAP));

        let flags = flags.with(IpcFlags::GRANT);
        assert!(flags.contains(IpcFlags::GRANT));
        assert!(!flags.contains(IpcFlags::MAP));

        let flags = flags.with(IpcFlags::MAP);
        assert!(flags.contains(IpcFlags::GRANT));
        assert!(flags.contains(IpcFlags::MAP));

        let flags = flags.without(IpcFlags::GRANT);
        assert!(!flags.contains(IpcFlags::GRANT));
        assert!(flags.contains(IpcFlags::MAP));
    }

    #[test]
    fn test_message_tag() {
        let tag = MessageTag::new(42, 3, 0);
        assert_eq!(tag.label, 42);
        assert_eq!(tag.words, 3);
        assert!(!tag.has_buffer());

        let tag = tag.with_buffer();
        assert!(tag.has_buffer());
    }

    #[test]
    fn test_buffer_desc() {
        let buf = BufferDesc::new(0x400000, 4096);
        assert_eq!(buf.addr, 0x400000);
        assert_eq!(buf.len, 4096);
        assert!(!buf.is_empty());
        assert!(buf.is_page_aligned());

        let buf = BufferDesc::new(0x400001, 4096);
        assert!(!buf.is_page_aligned());

        let buf = BufferDesc::empty();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_message_creation() {
        let msg = Message::new(MessageTag::new(42, 2, 0));
        assert_eq!(msg.tag.label, 42);
        assert_eq!(msg.tag.words, 2);
        assert!(!msg.has_buffer());

        let msg = Message::with_buffer(MessageTag::new(43, 0, 0), BufferDesc::new(0x400000, 4096));
        assert!(msg.has_buffer());
        assert_eq!(msg.buffer.addr, 0x400000);
        assert_eq!(msg.buffer.len, 4096);
    }

    #[test]
    fn test_message_empty() {
        let msg = Message::empty();
        assert!(msg.is_empty());
        assert_eq!(msg.tag.words, 0);
        assert!(!msg.has_buffer());
    }

    #[test]
    fn test_message_with_timeout() {
        let msg = Message::empty().with_timeout(1000);
        assert_eq!(msg.timeout, 1000);
    }

    #[test]
    fn test_message_size() {
        use core::mem;

        // Message should be reasonably sized
        // Tag: 8 bytes, words: 48 bytes, buffer: 16 bytes, timeout: 8 bytes = 80 bytes
        assert_eq!(mem::size_of::<MessageTag>(), 8);
        assert_eq!(mem::size_of::<BufferDesc>(), 16);
        assert_eq!(mem::size_of::<Message>(), 80);
    }

    #[test]
    #[should_panic(expected = "words must be 0-6")]
    fn test_message_tag_invalid_words() {
        MessageTag::new(0, 7, 0);
    }
}
