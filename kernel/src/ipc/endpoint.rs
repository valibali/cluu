//! IPC endpoint repository and message queue.
//!
//! This provides a minimal queue-based endpoint implementation so userspace
//! can send exit notifications to a parent process manager.

use crate::error::Error;
use crate::sched::ThreadId;
use crate::token::EndpointId;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

pub const IPC_MESSAGE_MAX: usize = 4096; // One page - reduces syscall overhead for large transfers

#[derive(Clone)]
pub struct EndpointMessage {
    len: usize,
    bytes: [u8; IPC_MESSAGE_MAX],
}

impl EndpointMessage {
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        if data.len() > IPC_MESSAGE_MAX {
            return Err(Error::InvalidParameter);
        }
        let mut bytes = [0u8; IPC_MESSAGE_MAX];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            len: data.len(),
            bytes,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn raw_bytes(&self) -> &[u8; IPC_MESSAGE_MAX] {
        &self.bytes
    }
}

/// A call message includes the caller's thread ID for reply routing
#[derive(Clone)]
pub struct CallMessage {
    pub caller: ThreadId,
    pub message: EndpointMessage,
    pub cookie: u64,
}

pub trait ByteEndpoint: Send {
    fn send(&mut self, data: &[u8]) -> Result<Option<ThreadId>, Error>;
    fn recv(&mut self, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error>;
    fn recv_nonblocking(&mut self) -> Result<Option<EndpointMessage>, Error>;
    /// Send a call message (with caller ID for reply routing)
    fn send_call(
        &mut self,
        caller: ThreadId,
        data: &[u8],
        cookie: u64,
    ) -> Result<Option<ThreadId>, Error>;
    /// Receive, preferring call messages. Returns (message, caller_id if call)
    fn recv_call(
        &mut self,
        receiver: ThreadId,
    ) -> Result<Option<(EndpointMessage, Option<ThreadId>)>, Error>;
}

pub struct QueueEndpoint {
    /// Regular message queue
    queue: VecDeque<EndpointMessage>,
    /// Call message queue (messages from call() that expect reply)
    call_queue: VecDeque<CallMessage>,
    /// Threads waiting to receive
    waiting_receivers: VecDeque<ThreadId>,
    /// Threads waiting to send (backpressure - queue was full)
    waiting_senders: VecDeque<ThreadId>,
    /// The caller currently being served (for reply routing)
    current_caller: Option<ThreadId>,
    /// Active callers keyed by call cookie.
    callers_by_cookie: BTreeMap<u64, ThreadId>,
}

const MAX_QUEUE_LEN: usize = 1024;
const MAX_CALL_QUEUE_LEN: usize = 256;
const BUSY_LOG_EVERY: u64 = 64;

#[derive(Copy, Clone)]
struct QueueStats {
    queue_len: usize,
    call_queue_len: usize,
    waiting_len: usize,
    callers_len: usize,
}

impl QueueEndpoint {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            call_queue: VecDeque::new(),
            waiting_receivers: VecDeque::new(),
            waiting_senders: VecDeque::new(),
            current_caller: None,
            callers_by_cookie: BTreeMap::new(),
        }
    }

    /// Get and clear the current caller (for reply)
    pub fn take_current_caller(&mut self) -> Option<ThreadId> {
        self.current_caller.take()
    }

    pub fn take_caller_by_cookie(&mut self, cookie: u64) -> Option<ThreadId> {
        self.callers_by_cookie.remove(&cookie)
    }

    pub fn take_any_caller(&mut self) -> Option<ThreadId> {
        if self.callers_by_cookie.len() != 1 {
            return None;
        }
        let cookie = *self.callers_by_cookie.keys().next()?;
        self.callers_by_cookie.remove(&cookie)
    }

    fn stats(&self) -> QueueStats {
        QueueStats {
            queue_len: self.queue.len(),
            call_queue_len: self.call_queue.len(),
            waiting_len: self.waiting_receivers.len(),
            callers_len: self.callers_by_cookie.len(),
        }
    }

    fn enqueue_receiver_if_absent(&mut self, receiver: ThreadId) {
        if !self.waiting_receivers.contains(&receiver) {
            self.waiting_receivers.push_back(receiver);
        }
    }

    fn wake_one_waiting_sender(&mut self) {
        if let Some(sender_id) = self.waiting_senders.pop_front() {
            crate::sched::ThreadManager::wake_thread(sender_id);
        }
    }
}

impl ByteEndpoint for QueueEndpoint {
    fn send(&mut self, data: &[u8]) -> Result<Option<ThreadId>, Error> {
        // Check if queue is full - implement backpressure by blocking sender
        if self.queue.len() >= MAX_QUEUE_LEN {
            // Get current thread ID to block it
            if let Some(sender_id) = crate::sched::ThreadManager::current() {
                self.waiting_senders.push_back(sender_id);
                return Err(Error::WouldBlock); // This will block the sender thread
            }
            // Fallback if no current thread (shouldn't happen in normal operation)
            return Err(Error::Busy);
        }
        let msg = EndpointMessage::new(data)?;
        self.queue.push_back(msg);

        let receiver_to_wake = self.waiting_receivers.pop_front();

        // Also wake a waiting sender if queue now has space.
        if !self.waiting_senders.is_empty() && self.queue.len() < MAX_QUEUE_LEN {
            self.wake_one_waiting_sender();
        }

        Ok(receiver_to_wake)
    }

    fn recv(&mut self, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error> {
        // First check call queue (call messages take priority)
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(call_msg.message));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(msg));
        }
        self.enqueue_receiver_if_absent(receiver);
        Err(Error::WouldBlock)
    }

    fn recv_nonblocking(&mut self) -> Result<Option<EndpointMessage>, Error> {
        // First check call queue
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(call_msg.message));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(msg));
        }
        Err(Error::WouldBlock)
    }

    fn send_call(
        &mut self,
        caller: ThreadId,
        data: &[u8],
        cookie: u64,
    ) -> Result<Option<ThreadId>, Error> {
        // Check if call queue is full - implement backpressure
        if self.call_queue.len() >= MAX_CALL_QUEUE_LEN {
            // Block the caller thread until space is available
            self.waiting_senders.push_back(caller);
            return Err(Error::WouldBlock); // This will block the caller thread
        }
        let msg = EndpointMessage::new(data)?;
        self.callers_by_cookie.insert(cookie, caller);
        self.call_queue.push_back(CallMessage {
            caller,
            message: msg,
            cookie,
        });
        Ok(self.waiting_receivers.pop_front())
    }

    fn recv_call(
        &mut self,
        receiver: ThreadId,
    ) -> Result<Option<(EndpointMessage, Option<ThreadId>)>, Error> {
        // First check call queue
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            return Ok(Some((call_msg.message, Some(call_msg.caller))));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            return Ok(Some((msg, None)));
        }
        self.enqueue_receiver_if_absent(receiver);
        Err(Error::WouldBlock)
    }
}

pub trait EndpointStore: Send {
    fn create(&mut self) -> EndpointId;
    fn with_endpoint<F, R>(&mut self, id: EndpointId, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut dyn ByteEndpoint) -> Result<R, Error>;
}

/// Sharded endpoint repository to reduce lock contention and avoid Arc heap allocations
/// Each shard has its own mutex, endpoints distributed by ID hash
const NUM_ENDPOINT_SHARDS: usize = 16;

struct EndpointShard {
    endpoints: BTreeMap<EndpointId, Mutex<QueueEndpoint>>,
}

impl EndpointShard {
    const fn new() -> Self {
        Self {
            endpoints: BTreeMap::new(),
        }
    }
}

/// Hash function for endpoint IDs to determine shard
#[inline(always)]
fn hash_endpoint_id(id: EndpointId) -> usize {
    // Use endpoint ID value modulo number of shards
    // EndpointId is a newtype wrapper around u64
    (id.0 as usize) % NUM_ENDPOINT_SHARDS
}

/// Static sharded endpoint table - each shard has its own mutex
/// This allows concurrent access to different endpoints without contention
/// and avoids Arc heap allocations
static ENDPOINT_SHARDS: [Mutex<EndpointShard>; NUM_ENDPOINT_SHARDS] =
    [const { Mutex::new(EndpointShard::new()) }; NUM_ENDPOINT_SHARDS];

/// Get the shard for a given endpoint ID
#[inline(always)]
fn get_endpoint_shard(id: EndpointId) -> &'static Mutex<EndpointShard> {
    &ENDPOINT_SHARDS[hash_endpoint_id(id)]
}

pub struct EndpointRepository {
    // Empty - kept for compatibility with EndpointStore trait
    // Actual storage is in static ENDPOINT_SHARDS
}

impl EndpointRepository {
    #[allow(dead_code)]
    fn new() -> Self {
        Self {}
    }
}

impl EndpointStore for EndpointRepository {
    fn create(&mut self) -> EndpointId {
        let id = EndpointId::new(NEXT_ENDPOINT_ID.fetch_add(1, Ordering::SeqCst));
        let shard = get_endpoint_shard(id);
        let mut shard_guard = shard.lock();
        shard_guard.endpoints.insert(id, Mutex::new(QueueEndpoint::new()));
        id
    }

    fn with_endpoint<F, R>(&mut self, id: EndpointId, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut dyn ByteEndpoint) -> Result<R, Error>,
    {
        let shard = get_endpoint_shard(id);
        let mut shard_guard = shard.lock();
        let endpoint = shard_guard.endpoints.get_mut(&id).ok_or(Error::NotFound)?;
        let mut guard = endpoint.lock();
        f(&mut *guard)
    }
}

lazy_static! {
    static ref ENDPOINTS: Mutex<EndpointRepository> = Mutex::new(EndpointRepository::new());
}

lazy_static! {
    static ref RECV_LOGGED: Mutex<BTreeSet<EndpointId>> = Mutex::new(BTreeSet::new());
}
lazy_static! {
    static ref SEND_LOGGED: Mutex<BTreeSet<EndpointId>> = Mutex::new(BTreeSet::new());
}
lazy_static! {
    static ref BUSY_COUNTS: Mutex<BTreeMap<(EndpointId, u8), u64>> = Mutex::new(BTreeMap::new());
}

static NEXT_ENDPOINT_ID: AtomicU64 = AtomicU64::new(1);

pub fn create_endpoint() -> EndpointId {
    let id = EndpointId::new(NEXT_ENDPOINT_ID.fetch_add(1, Ordering::SeqCst));
    let shard = get_endpoint_shard(id);
    let mut shard_guard = shard.lock();
    shard_guard.endpoints.insert(id, Mutex::new(QueueEndpoint::new()));
    id
}

pub fn send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard.send(data)
}

pub fn try_send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (non-blocking)
    let mut shard_guard = shard.try_lock().ok_or(Error::WouldBlock)?;
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.try_lock().ok_or(Error::WouldBlock)?;
    guard.send(data)
}

pub fn recv(endpoint: EndpointId, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard.recv(receiver)
}

pub fn recv_nonblocking(endpoint: EndpointId) -> Result<Option<EndpointMessage>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard.recv_nonblocking()
}

pub fn send_from_user(
    endpoint: EndpointId,
    msg_ptr: usize,
    msg_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<(), Error> {
    let first_send = {
        let mut logged = SEND_LOGGED.lock();
        logged.insert(endpoint)
    };
    if first_send {
        klibcluu::trace("send_from_user: endpoint_id=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint.as_u64());
        klibcluu::trace("send_from_user: msg_len=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", msg_len as u64);
    }
    crate::syscall::userptr::validate_user_buffer(msg_ptr, msg_len)?;
    let mut buffer = [0u8; IPC_MESSAGE_MAX];
    if msg_len > buffer.len() {
        return Err(Error::InvalidParameter);
    }
    // Safety: buffer is a valid kernel buffer of IPC_MESSAGE_MAX bytes.
    unsafe {
        crate::syscall::userptr::copy_from_user(
            buffer.as_mut_ptr(),
            msg_ptr,
            msg_len,
            page_table_root,
        )?;
    }
    // Try to send - if queue is full, block and return (will be retried from userspace when woken)
    let wake = {
        // Get shard directly (static, no repository lock needed)
        let shard = get_endpoint_shard(endpoint);

        // Lock shard, then endpoint
        let mut shard_guard = shard.lock();
        let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
        let mut guard = endpoint_mutex.lock();
        match guard.send(&buffer[..msg_len]) {
            Ok(wake) => wake, // Success
            Err(Error::WouldBlock) => {
                // Queue is full - sender was added to waiting_senders, need to block
                // Return WouldBlock so syscall returns and context switch can happen
                // When woken, userspace will retry the syscall
                crate::sched::ThreadManager::block_current();
                crate::architecture::x86_64::syscall::request_resched();
                return Err(Error::WouldBlock);
            }
            Err(Error::Busy) => {
                // Fallback for edge cases (shouldn't happen with backpressure)
                log_endpoint_busy(endpoint, guard.stats(), false);
                return Err(Error::Busy);
            }
            Err(err) => return Err(err),
        }
    };

    // Success - wake receiver if any
    if let Some(thread_id) = wake {
        if first_send {
            klibcluu::trace("send_from_user: wake_thread_id=");
            klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
        }
        crate::sched::ThreadManager::wake_thread(thread_id);
    }
    Ok(())
}

pub fn recv_to_user(
    endpoint: EndpointId,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
    receiver: ThreadId,
) -> Result<usize, Error> {
    crate::syscall::userptr::validate_user_buffer(buf_ptr, buf_len)?;
    let msg = match recv(endpoint, receiver) {
        Ok(Some(msg)) => msg,
        Ok(None) => return Ok(0),
        Err(Error::WouldBlock) => return Err(Error::WouldBlock),
        Err(err) => return Err(err),
    };
    if msg.len() > buf_len {
        return Err(Error::BufferTooSmall);
    }
    let mut logged = RECV_LOGGED.lock();
    if logged.insert(endpoint) {
        klibcluu::trace("recv_to_user: endpoint_id=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint.as_u64());
    }
    // Safety: msg.raw_bytes() points to a kernel buffer of msg.len() bytes.
    unsafe {
        crate::syscall::userptr::copy_to_user(
            buf_ptr,
            msg.raw_bytes().as_ptr(),
            msg.len(),
            page_table_root,
        )?;
    }
    Ok(msg.len())
}

pub fn recv_to_user_nonblocking(
    endpoint: EndpointId,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<usize, Error> {
    crate::syscall::userptr::validate_user_buffer(buf_ptr, buf_len)?;
    let msg = match recv_nonblocking(endpoint) {
        Ok(Some(msg)) => msg,
        Ok(None) => return Ok(0),
        Err(Error::WouldBlock) => return Err(Error::WouldBlock),
        Err(err) => return Err(err),
    };
    if msg.len() > buf_len {
        return Err(Error::BufferTooSmall);
    }
    let mut logged = RECV_LOGGED.lock();
    if logged.insert(endpoint) {
        klibcluu::trace("recv_to_user_nonblocking: endpoint_id=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint.as_u64());
        klibcluu::trace("recv_to_user_nonblocking: msg_len=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", msg.len() as u64);
    }
    // Safety: msg.raw_bytes() points to a kernel buffer of msg.len() bytes.
    unsafe {
        crate::syscall::userptr::copy_to_user(
            buf_ptr,
            msg.raw_bytes().as_ptr(),
            msg.len(),
            page_table_root,
        )?;
    }
    Ok(msg.len())
}

/// Send a call message from userspace with reply token injected
pub fn call_from_user_with_reply_token(
    endpoint: EndpointId,
    msg_ptr: usize,
    msg_len: usize,
    page_table_root: x86_64::PhysAddr,
    reply_token: crate::token::TokenHandle,
) -> Result<(), Error> {
    crate::syscall::userptr::validate_user_buffer(msg_ptr, msg_len)?;
    let mut buffer = [0u8; IPC_MESSAGE_MAX];
    if msg_len > buffer.len() {
        return Err(Error::InvalidParameter);
    }
    // Safety: buffer is a valid kernel buffer of IPC_MESSAGE_MAX bytes.
    unsafe {
        crate::syscall::userptr::copy_from_user(
            buffer.as_mut_ptr(),
            msg_ptr,
            msg_len,
            page_table_root,
        )?;
    }

    // Inject reply token handle into message
    inject_reply_token(&mut buffer[..msg_len], reply_token);

    // Try to send - if queue is full, block and return (will be retried from userspace when woken)
    let wake = {
        // Get shard directly (static, no repository lock needed)
        let shard = get_endpoint_shard(endpoint);

        // Lock shard, then endpoint
        let mut shard_guard = shard.lock();
        let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
        let mut guard = endpoint_mutex.lock();
        // Just send as regular message - reply routing via token
        match guard.send(&buffer[..msg_len]) {
            Ok(wake) => wake, // Success
            Err(Error::WouldBlock) => {
                // Queue is full - sender was added to waiting_senders, need to block
                // Return WouldBlock so syscall returns and context switch can happen
                // When woken, userspace will retry the syscall
                crate::sched::ThreadManager::block_current();
                crate::architecture::x86_64::syscall::request_resched();
                return Err(Error::WouldBlock);
            }
            Err(Error::Busy) => {
                // Fallback for edge cases (shouldn't happen with backpressure)
                log_endpoint_busy(endpoint, guard.stats(), true);
                return Err(Error::Busy);
            }
            Err(err) => return Err(err),
        }
    };

    // Success - wake receiver if any
    if let Some(thread_id) = wake {
        crate::sched::ThreadManager::wake_thread(thread_id);
    }
    Ok(())
}

fn log_endpoint_busy(endpoint: EndpointId, stats: QueueStats, is_call: bool) {
    let mut counts = BUSY_COUNTS.lock();
    let key = (endpoint, if is_call { 1 } else { 0 });
    let count = counts.entry(key).or_insert(0);
    *count = count.saturating_add(1);
    if !(*count).is_multiple_of(BUSY_LOG_EVERY) {
        return;
    }
    klibcluu::warn("ipc endpoint busy");
    klibcluu::log_dec(klibcluu::LogLevel::Warn, "  endpoint=", endpoint.as_u64());
    klibcluu::log_dec(
        klibcluu::LogLevel::Warn,
        "  queue_len=",
        stats.queue_len as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Warn,
        "  call_queue_len=",
        stats.call_queue_len as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Warn,
        "  waiting_receivers=",
        stats.waiting_len as u64,
    );
    klibcluu::log_dec(
        klibcluu::LogLevel::Warn,
        "  callers_by_cookie=",
        stats.callers_len as u64,
    );
}

/// Get the current caller for an endpoint (used by reply)
pub fn take_current_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard.take_current_caller().ok_or(Error::InvalidState)
}

/// Get a caller for a specific call cookie.
pub fn take_caller_by_cookie(endpoint: EndpointId, cookie: u64) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard
        .take_caller_by_cookie(cookie)
        .ok_or(Error::InvalidState)
}

pub fn take_any_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let endpoint_mutex = shard_guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    let mut guard = endpoint_mutex.lock();
    guard.take_any_caller().ok_or(Error::InvalidState)
}

/// Tag indicating the message contains a reply token
pub const REPLY_TOKEN_TAG: u8 = 2;
/// Word index where reply token handle is stored
pub const REPLY_TOKEN_WORD: usize = 5;

#[repr(C)]
pub struct UserMessageTag {
    pub label: u32,
    pub words: u8,
    pub extra: u8,
    pub _pad: u16,
}

#[repr(C)]
pub struct UserMessage {
    pub tag: UserMessageTag,
    pub words: [usize; 6],
}

/// Inject reply token handle into message
fn inject_reply_token(buffer: &mut [u8], token_handle: crate::token::TokenHandle) {
    if buffer.len() < core::mem::size_of::<UserMessage>() {
        return;
    }
    let msg = unsafe { &mut *(buffer.as_mut_ptr() as *mut UserMessage) };
    msg.tag.extra = REPLY_TOKEN_TAG;
    msg.words[REPLY_TOKEN_WORD] = token_handle.as_usize();
}

/// Extract reply token handle from message
pub fn extract_reply_token(buffer: &[u8]) -> Option<crate::token::TokenHandle> {
    if buffer.len() < core::mem::size_of::<UserMessage>() {
        return None;
    }
    let msg = unsafe { &*(buffer.as_ptr() as *const UserMessage) };
    if msg.tag.extra != REPLY_TOKEN_TAG {
        return None;
    }
    Some(crate::token::TokenHandle::new(msg.words[REPLY_TOKEN_WORD]))
}

/// Deliver a reply using a ReplyId (one-time use)
///
/// Copies the reply data directly to the caller's reply buffer and wakes them.
pub fn deliver_reply_by_id(
    reply_id: crate::token::ReplyId,
    reply_data: &[u8],
) -> Result<usize, Error> {
    use crate::sched::{CallReplyInfo, ThreadManager};

    // Take the reply info (removes from map - one-time use)
    let reply_info: CallReplyInfo =
        ThreadManager::take_call_reply_info(reply_id).ok_or(Error::InvalidState)?;

    let caller = reply_info.caller;

    // Validate caller thread exists
    ThreadManager::with_thread(caller, |_| ()).ok_or(Error::NotFound)?;

    // Validate and copy reply to caller's buffer
    if reply_data.len() > reply_info.reply_buf_len {
        return Err(Error::BufferTooSmall);
    }

    // Safety: reply_data points to a kernel buffer for reply_data.len() bytes.
    unsafe {
        crate::syscall::userptr::copy_to_user(
            reply_info.reply_buf_ptr,
            reply_data.as_ptr(),
            reply_data.len(),
            reply_info.page_table_root,
        )?;
    }

    // Set the return value in the caller's saved context (RAX).
    let bytes_received = reply_data.len();
    ThreadManager::with_thread_mut(caller, |thread| {
        thread.context.rax = bytes_received as u64;
    });

    // Wake the caller
    ThreadManager::wake_thread(caller);

    Ok(bytes_received)
}
