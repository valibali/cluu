//! IPC helpers
//!
//! Higher-level IPC wrappers using the Message type.

use crate::boot::process_info;
use crate::error::Result;
use crate::syscall;
use crate::types::*;
use crate::Error;
use alloc::vec::Vec;
use core::cmp;
use core::mem::{align_of, size_of};
use core::sync::atomic::{fence, Ordering};

pub const PROC_EXIT_LABEL: u32 = 1;
pub const CONSOLE_WRITE_LABEL: u32 = 1;
pub const CONSOLE_CLEAR_LABEL: u32 = 2;
pub const CONSOLE_CURSOR_LABEL: u32 = 3;
pub const CONSOLE_BLINK_LABEL: u32 = 4;
pub const CONSOLE_WRITE_SYNC_LABEL: u32 = 5;
pub const IPC_CHUNK_BYTES_DEFAULT: usize = 256;
pub const IPC_SEND_RETRIES_DEFAULT: u32 = 256;
pub const IPC_BACKOFF_MAX_DEFAULT: u32 = 64;
/// Inline stack buffer size for small IPC payloads (avoid heap use).
const IPC_INLINE_STACK_MAX: usize = 512;
pub const KBD_EVENT_LABEL: u32 = 1;
pub const TTY_READ_LABEL: u32 = 1;
pub const TTY_WRITE_LABEL: u32 = 2;
pub const TTY_CTL_LABEL: u32 = 3;
pub const TTY_REGISTER_LABEL: u32 = 4;
pub const TTY_WRITE_SYNC_LABEL: u32 = 5;
pub const TTY_READ_REQUEST_LABEL: u32 = 6;
pub const CONSOLE_FB_INFO_LABEL: u32 = 6;
pub const TTY_POLL_QUERY_LABEL: u32 = 7;
pub const CONSOLE_ACTIVATE_LABEL: u32 = 8;
pub const CONSOLE_DEACTIVATE_LABEL: u32 = 9;
pub const PROCMGR_QUERY_CTTY_LABEL: u32 = 11;
/// tty → procmgr: request shell spawn for this VT.
/// words[0] = tty endpoint token (stdout for the shell).
/// Payload = path string (e.g., "/dev/initrd/bin/shell\0").
pub const TTY_SPAWN_SHELL_LABEL: u32 = 13;
/// kbd → vtmgr: request VT switch.
/// words[0] = target VT index (0-3).
pub const VTMGR_SWITCH_VT_LABEL: u32 = 15;
/// vtmgr → console: create a new VT buffer.
/// words[0] = VT index (0-3).
pub const CONSOLE_CREATE_VT_LABEL: u32 = 17;
/// per-VT write: tty → console with VT index routing.
/// words[0] = payload length, words[1] = VT index.
pub const CONSOLE_WRITE_VT_LABEL: u32 = 18;
/// per-VT synchronous write (call): tty → console with VT index routing.
/// words[0] = payload length, words[1] = VT index.
pub const CONSOLE_WRITE_VT_SYNC_LABEL: u32 = 19;
/// Generic service spawn request (any service → procmgr).
/// words[0] = payload length, words[1] = priority,
/// words[2] = TOKEN_EXTRA_0 mode (0=none, 1=listen, 2=grantable),
/// words[3] = param override count.
/// Payload = path\0 + param overrides (each: u16 index LE + u64 value LE = 10 bytes).
pub const PROCMGR_SPAWN_SERVICE_LABEL: u32 = 20;

/// Procmgr → VFS: register a per-client filesystem view.
/// words[1] = client_id (sender_tid of the target process),
/// words[2] = mount count.
/// Payload: for each mount: u16 src_len LE + u16 dst_len LE + u8 flags (bit 0 = writable) + src_bytes + dst_bytes.
pub const VFS_SET_VIEW_LABEL: u32 = 21;

pub const TTY_FG_FLAG_FORWARD_CTRL_C: usize = 1 << 0;
pub const TTY_FG_FLAG_NOTIFY_CTRL_C: usize = 1 << 1;
pub const TTY_CTL_SYNC: u32 = 1;
pub const CALL_COOKIE_TAG: u8 = 1;
pub const CALL_COOKIE_WORD: usize = 5;
pub const SHARED_RING_MAGIC: u32 = 0x434c_5555; // "CLUU"
pub const SHARED_RING_VERSION: u16 = 1;
pub const SHARED_RING_DEFAULT_MAP_FLAGS: usize = 0x03; // readable + writable
pub const SHARED_RING_DEFAULT_GRANT_FLAGS: usize = 0x02; // writable
const SHARED_RING_MIN_CAPACITY: usize = 2;

/// Tag indicating the message contains a reply ID
pub const REPLY_ID_TAG: u8 = 2;
/// Word index where reply ID is stored
pub const REPLY_ID_WORD: usize = 5;

/// Shared-ring metadata stored in the first bytes of the shared region.
///
/// This is a single-producer/single-consumer ring with one reserved slot to
/// distinguish full/empty states.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SharedRingHeader {
    pub magic: u32,
    pub version: u16,
    pub reserved: u16,
    pub capacity: u32,
    pub read_idx: u32,
    pub write_idx: u32,
    pub notify_seq: u32,
    pub reserved2: u32,
}

impl SharedRingHeader {
    pub const fn bytes() -> usize {
        size_of::<SharedRingHeader>()
    }
}

/// Virtual region backing a shared ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedRingRegion {
    pub base: usize,
    pub bytes: usize,
    pub pages: usize,
}

/// Single-producer/single-consumer shared ring view over a mapped region.
pub struct SharedRing<'a> {
    header: &'a mut SharedRingHeader,
    data: &'a mut [u8],
    capacity: usize,
}

impl<'a> SharedRing<'a> {
    /// Return total bytes required for a ring with `capacity` payload bytes.
    pub fn bytes_for_capacity(capacity: usize) -> Result<usize> {
        if capacity < SHARED_RING_MIN_CAPACITY {
            return Err(Error::InvalidArgument);
        }
        SharedRingHeader::bytes()
            .checked_add(capacity)
            .ok_or(Error::Overflow)
    }

    /// Initialize a shared ring in `backing`.
    pub fn initialize(backing: &'a mut [u8]) -> Result<Self> {
        let (header, data) = split_shared_ring_backing(backing)?;
        if data.len() < SHARED_RING_MIN_CAPACITY {
            return Err(Error::BufferTooSmall);
        }
        let capacity = data.len().min(u32::MAX as usize);
        *header = SharedRingHeader {
            magic: SHARED_RING_MAGIC,
            version: SHARED_RING_VERSION,
            reserved: 0,
            capacity: capacity as u32,
            read_idx: 0,
            write_idx: 0,
            notify_seq: 0,
            reserved2: 0,
        };
        Ok(Self {
            header,
            data: &mut data[..capacity],
            capacity,
        })
    }

    /// Attach to an already initialized shared ring.
    pub fn attach(backing: &'a mut [u8]) -> Result<Self> {
        let (header, data) = split_shared_ring_backing(backing)?;
        if header.magic != SHARED_RING_MAGIC || header.version != SHARED_RING_VERSION {
            return Err(Error::InvalidState);
        }
        let capacity = header.capacity as usize;
        if capacity < SHARED_RING_MIN_CAPACITY || capacity > data.len() {
            return Err(Error::InvalidState);
        }
        Ok(Self {
            header,
            data: &mut data[..capacity],
            capacity,
        })
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    pub fn available_read(&self) -> usize {
        let read = self.read_idx();
        let write = self.write_idx();
        if write >= read {
            write - read
        } else {
            self.capacity - read + write
        }
    }

    #[inline]
    pub fn available_write(&self) -> usize {
        self.capacity
            .saturating_sub(self.available_read())
            .saturating_sub(1)
    }

    /// Push as many bytes as possible from `src`. Returns bytes written.
    pub fn push(&mut self, src: &[u8]) -> usize {
        if src.is_empty() {
            return 0;
        }
        let to_write = cmp::min(src.len(), self.available_write());
        if to_write == 0 {
            return 0;
        }
        let write = self.write_idx();
        let first = cmp::min(to_write, self.capacity - write);
        self.data[write..write + first].copy_from_slice(&src[..first]);
        if to_write > first {
            let second = to_write - first;
            self.data[..second].copy_from_slice(&src[first..first + second]);
        }
        fence(Ordering::Release);
        self.set_write_idx((write + to_write) % self.capacity);
        to_write
    }

    /// Pop as many bytes as possible into `dst`. Returns bytes read.
    pub fn pop(&mut self, dst: &mut [u8]) -> usize {
        if dst.is_empty() {
            return 0;
        }
        let to_read = cmp::min(dst.len(), self.available_read());
        if to_read == 0 {
            return 0;
        }
        let read = self.read_idx();
        let first = cmp::min(to_read, self.capacity - read);
        dst[..first].copy_from_slice(&self.data[read..read + first]);
        if to_read > first {
            let second = to_read - first;
            dst[first..first + second].copy_from_slice(&self.data[..second]);
        }
        fence(Ordering::Release);
        self.set_read_idx((read + to_read) % self.capacity);
        to_read
    }

    /// Reset read/write cursors.
    pub fn reset(&mut self) {
        self.set_read_idx(0);
        self.set_write_idx(0);
    }

    /// Increment notify sequence to signal peer after data movement.
    pub fn bump_notify_seq(&mut self) -> u32 {
        let next = self.notify_seq().wrapping_add(1);
        unsafe {
            core::ptr::write_volatile(&mut self.header.notify_seq, next);
        }
        next
    }

    #[inline]
    pub fn notify_seq(&self) -> u32 {
        unsafe { core::ptr::read_volatile(&self.header.notify_seq) }
    }

    #[inline]
    fn read_idx(&self) -> usize {
        let raw = unsafe { core::ptr::read_volatile(&self.header.read_idx) } as usize;
        if self.capacity == 0 {
            0
        } else {
            raw % self.capacity
        }
    }

    #[inline]
    fn write_idx(&self) -> usize {
        let raw = unsafe { core::ptr::read_volatile(&self.header.write_idx) } as usize;
        if self.capacity == 0 {
            0
        } else {
            raw % self.capacity
        }
    }

    #[inline]
    fn set_read_idx(&mut self, idx: usize) {
        unsafe {
            core::ptr::write_volatile(&mut self.header.read_idx, idx as u32);
        }
    }

    #[inline]
    fn set_write_idx(&mut self, idx: usize) {
        unsafe {
            core::ptr::write_volatile(&mut self.header.write_idx, idx as u32);
        }
    }
}

fn split_shared_ring_backing(backing: &mut [u8]) -> Result<(&mut SharedRingHeader, &mut [u8])> {
    if backing.len() < SharedRingHeader::bytes() {
        return Err(Error::BufferTooSmall);
    }
    let align = align_of::<SharedRingHeader>();
    if !(backing.as_ptr() as usize).is_multiple_of(align) {
        return Err(Error::InvalidArgument);
    }
    let (header_bytes, data) = backing.split_at_mut(SharedRingHeader::bytes());
    let header = unsafe { &mut *(header_bytes.as_mut_ptr() as *mut SharedRingHeader) };
    Ok((header, data))
}

/// Allocate and map a local shared-ring region in the caller address space.
pub fn alloc_shared_ring_region(
    space_token: usize,
    capacity: usize,
    map_flags: usize,
) -> Result<SharedRingRegion> {
    let min_bytes = SharedRing::bytes_for_capacity(capacity)?;
    let pages = crate::mem::pages_for_size(min_bytes);
    let bytes = pages
        .checked_mul(crate::mem::PAGE_SIZE)
        .ok_or(Error::Overflow)?;
    let base = {
        let mut vspace = crate::vspace::VSPACE.lock();
        vspace.alloc(bytes)?
    };
    match crate::syscall::space_map_range(space_token, base, 0, map_flags, pages, 0) {
        Ok(_) => Ok(SharedRingRegion { base, bytes, pages }),
        Err(err) => {
            let mut vspace = crate::vspace::VSPACE.lock();
            let _ = vspace.free(base, bytes);
            Err(err)
        }
    }
}

/// Unmap and release a previously allocated shared-ring region.
pub fn free_shared_ring_region(space_token: usize, region: SharedRingRegion) -> Result<()> {
    crate::syscall::space_unmap(space_token, region.base, region.pages)?;
    let mut vspace = crate::vspace::VSPACE.lock();
    vspace.free(region.base, region.bytes)?;
    Ok(())
}

/// Grant a mapped ring region to another address space page-by-page.
///
/// Returns the number of pages granted.
pub fn grant_shared_ring_region(
    source_space_token: usize,
    target_space_token: usize,
    source_base: usize,
    target_base: usize,
    bytes: usize,
    grant_flags: usize,
) -> Result<usize> {
    if bytes == 0 {
        return Err(Error::InvalidArgument);
    }
    if !source_base.is_multiple_of(crate::mem::PAGE_SIZE)
        || !target_base.is_multiple_of(crate::mem::PAGE_SIZE)
    {
        return Err(Error::InvalidArgument);
    }
    let pages = crate::mem::pages_for_size(bytes);
    for page_idx in 0..pages {
        let src = source_base + page_idx * crate::mem::PAGE_SIZE;
        let dst = target_base + page_idx * crate::mem::PAGE_SIZE;
        crate::syscall::space_grant(
            source_space_token,
            target_space_token,
            src,
            dst,
            grant_flags,
        )?;
    }
    Ok(pages)
}

/// Send a message (one-way)
pub fn send(endpoint_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    // Convert Message to bytes and call syscall::ipc_send
    let msg_bytes = msg.as_bytes();
    syscall::ipc_send(endpoint_token, msg_bytes)
}

/// Send a message with an inline payload appended after the Message header.
pub fn send_with_payload(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = payload.len();
    send_msg_with_payload(endpoint_token, &msg, payload)
}

/// Send a caller-constructed Message with an inline payload.
///
/// Unlike `send_with_payload`, this preserves the caller's Message header
/// (label, words, extra) exactly as provided. The caller is responsible
/// for setting words[0] to payload.len() if the receiver uses
/// `parse_message` to extract the payload.
pub fn send_msg_with_payload(endpoint_token: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let total_len = header.len() + payload.len();
    if total_len <= IPC_INLINE_STACK_MAX {
        let mut buffer = [0u8; IPC_INLINE_STACK_MAX];
        buffer[..header.len()].copy_from_slice(header);
        buffer[header.len()..total_len].copy_from_slice(payload);
        return syscall::ipc_send(endpoint_token, &buffer[..total_len]);
    }

    let mut buffer = Vec::with_capacity(total_len);
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_send(endpoint_token, &buffer)
}

/// Send a message with an inline payload, retrying on busy endpoints.
pub fn send_with_retry(endpoint_token: usize, label: u32, payload: &[u8]) -> Result<()> {
    send_with_retry_timeout(endpoint_token, label, payload, 0)
}

/// Send a message with an inline payload, retrying on busy endpoints with backoff.
///
/// When `max_retries` is 0, retries indefinitely.
///
/// Note: Error::WouldBlock means the kernel blocked the thread and will wake it
/// when space is available. We just retry - the kernel handles the blocking.
pub fn send_with_retry_timeout(
    endpoint_token: usize,
    label: u32,
    payload: &[u8],
    max_retries: u32,
) -> Result<()> {
    let max_backoff = IPC_BACKOFF_MAX_DEFAULT;
    let mut backoff = 1u32;
    let mut retries = 0u32;
    loop {
        match send_with_payload(endpoint_token, label, payload) {
            Ok(()) => return Ok(()),
            Err(Error::WouldBlock) => {
                // Kernel blocked us and will wake when space is available
                // Just retry - no need for backoff since kernel handles blocking
                continue;
            }
            Err(Error::Busy) => {
                retries = retries.saturating_add(1);
                if max_retries != 0 && retries > max_retries {
                    return Err(Error::Busy);
                }
                for _ in 0..backoff {
                    let _ = syscall::yield_cpu();
                }
                backoff = (backoff.saturating_mul(2)).min(max_backoff);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Synchronous write to TTY - waits for acknowledgement before returning.
/// Use this before exiting to ensure output is flushed.
pub fn tty_write_sync(endpoint_token: usize, payload: &[u8]) -> Result<()> {
    let mut msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 1);
    msg.words[0] = payload.len();
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(endpoint_token, &msg, payload, &mut reply)
}

/// Call (send + wait for reply) with an inline payload appended after the Message header.
pub fn call_with_payload(
    endpoint_token: usize,
    msg: &Message,
    payload: &[u8],
    reply: &mut Message,
) -> Result<()> {
    let header = msg.as_bytes();
    let total_len = header.len() + payload.len();
    let reply_bytes = reply.as_bytes_mut();
    if total_len <= IPC_INLINE_STACK_MAX {
        let mut buffer = [0u8; IPC_INLINE_STACK_MAX];
        buffer[..header.len()].copy_from_slice(header);
        buffer[header.len()..total_len].copy_from_slice(payload);
        let _ = syscall::ipc_call(endpoint_token, &buffer[..total_len], reply_bytes)?;
        return Ok(());
    }

    let mut buffer = Vec::with_capacity(total_len);
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    let _ = syscall::ipc_call(endpoint_token, &buffer, reply_bytes)?;
    Ok(())
}

/// Receive a message
pub fn recv(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes_mut();
    loop {
        match syscall::ipc_recv(endpoint_token, msg_bytes) {
            Ok(_) => return Ok(()),
            Err(Error::WouldBlock) => {
                let _ = syscall::yield_cpu();
            }
            Err(err) => return Err(err),
        }
    }
}

/// Call (send + wait for reply)
pub fn call(endpoint_token: usize, msg: &mut Message, _flags: IpcFlags) -> Result<()> {
    // We need to send msg and receive reply into the same buffer
    // Make a temporary copy to send, then receive into the original
    let msg_copy = msg.clone();
    let send_bytes = msg_copy.as_bytes();
    let reply_bytes = msg.as_bytes_mut();
    let _bytes_received = syscall::ipc_call(endpoint_token, send_bytes, reply_bytes)?;
    Ok(())
}

/// Extract reply ID from a received call message
///
/// Returns the reply ID if the message was from a call, None otherwise.
pub fn extract_reply_id(msg: &Message) -> Option<usize> {
    if msg.tag.extra == REPLY_ID_TAG {
        Some(msg.words[REPLY_ID_WORD])
    } else {
        None
    }
}

/// Reply to a received call message using the reply token
///
/// # Arguments
///
/// - `reply_token`: The reply token extracted from the received call message
/// - `msg`: Reply message to send
/// - `_flags`: IPC flags (currently unused)
pub fn reply(reply_token: usize, msg: &Message, _flags: IpcFlags) -> Result<()> {
    let msg_bytes = msg.as_bytes();
    syscall::ipc_reply(reply_token, msg_bytes)?;
    Ok(())
}

/// Reply with an additional payload appended after the message header.
pub fn reply_with_payload(reply_token: usize, msg: &Message, payload: &[u8]) -> Result<()> {
    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(payload);
    syscall::ipc_reply(reply_token, &buffer)?;
    Ok(())
}

/// Copy an IPC call cookie from a request into a reply.
///
/// Servers should call this when replying to ipc_call() so the kernel
/// can route the reply to the correct caller even if calls overlap.
pub fn copy_call_cookie(reply: &mut Message, request: &Message) {
    if request.tag.extra != CALL_COOKIE_TAG {
        return;
    }
    reply.tag.extra = CALL_COOKIE_TAG;
    reply.words[CALL_COOKIE_WORD] = request.words[CALL_COOKIE_WORD];
}

/// Reply and receive next message (server loop optimization)
///
/// Note: This is currently implemented as reply() + recv() separately.
/// In the future, this could be a single optimized syscall.
pub fn reply_recv(endpoint_token: usize, msg: &mut Message, flags: IpcFlags) -> Result<()> {
    // Send reply first
    reply(endpoint_token, msg, flags)?;
    // Then receive next message
    recv(endpoint_token, msg, flags)
}

/// Call with payload, receiving both message and reply payload.
///
/// Returns (reply_message, bytes_in_reply_payload).
pub fn call_with_reply_buf(
    endpoint_token: usize,
    msg: &Message,
    send_payload: &[u8],
    reply_buf: &mut [u8],
) -> Result<(Message, usize)> {
    use core::mem::size_of;

    let header = msg.as_bytes();
    let mut buffer = Vec::with_capacity(header.len() + send_payload.len());
    buffer.extend_from_slice(header);
    buffer.extend_from_slice(send_payload);

    let bytes_received = syscall::ipc_call(endpoint_token, &buffer, reply_buf)?;

    if bytes_received < size_of::<Message>() {
        return Err(Error::InvalidState);
    }

    // Parse the reply message
    let reply_msg = unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
    let payload_len = bytes_received - size_of::<Message>();

    Ok((reply_msg, payload_len))
}

/// Notify the parent process manager that this process is exiting.
pub fn notify_exit(exit_code: i32) -> Result<()> {
    let info = process_info();
    if info.exit_token == 0 {
        // No parent to notify (e.g., init process)
        return Ok(());
    }

    let msg = Message::new(
        PROC_EXIT_LABEL,
        [info.exit_cookie, exit_code as usize, 0, 0, 0, 0],
        2,
    );
    send(info.exit_token, &msg, IpcFlags::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_ring_roundtrip_wraps() {
        let mut backing = [0u8; 96];
        let mut ring = SharedRing::initialize(&mut backing).expect("init");
        assert_eq!(ring.capacity(), 64); // 32-byte header + 64-byte data

        let first = [1u8; 40];
        assert_eq!(ring.push(&first), 40);
        assert_eq!(ring.available_read(), 40);

        let mut out = [0u8; 24];
        assert_eq!(ring.pop(&mut out), 24);
        assert_eq!(out, [1u8; 24]);

        let second = [2u8; 30];
        assert_eq!(ring.push(&second), 30);

        let mut drain = [0u8; 46];
        assert_eq!(ring.pop(&mut drain), 46);
        assert_eq!(&drain[..16], [1u8; 16].as_slice());
        assert_eq!(&drain[16..], [2u8; 30].as_slice());
    }

    #[test]
    fn shared_ring_attach_rejects_uninitialized() {
        let mut backing = [0u8; 128];
        assert_eq!(
            SharedRing::attach(&mut backing).err(),
            Some(Error::InvalidState)
        );
    }

    #[test]
    fn shared_ring_bytes_overflow_guard() {
        assert_eq!(
            SharedRing::bytes_for_capacity(usize::MAX).err(),
            Some(Error::Overflow)
        );
    }
}
