//! IPC endpoint repository and message queue.
//!
//! This provides a minimal queue-based endpoint implementation so userspace
//! can send exit notifications to a parent process manager.

use crate::error::Error;
use crate::sched::ThreadId;
use crate::token::EndpointId;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

pub const IPC_MESSAGE_MAX: usize = 4096; // One page - reduces syscall overhead for large transfers

const IPC_INLINE_MAX: usize = 64;

enum UserPayloadBuffer {
    Inline {
        len: usize,
        bytes: [u8; IPC_INLINE_MAX],
    },
    Heap(Vec<u8>),
}

impl UserPayloadBuffer {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Inline { len, bytes } => &bytes[..*len],
            Self::Heap(bytes) => bytes.as_slice(),
        }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        match self {
            Self::Inline { len, bytes } => &mut bytes[..*len],
            Self::Heap(bytes) => bytes.as_mut_slice(),
        }
    }
}

#[derive(Clone)]
pub struct EndpointMessage {
    storage: EndpointMessageStorage,
}

#[derive(Clone)]
enum EndpointMessageStorage {
    Inline {
        len: u16,
        bytes: [u8; IPC_INLINE_MAX],
    },
    Heap(Box<[u8]>),
}

impl EndpointMessage {
    pub fn new(data: &[u8]) -> Result<Self, Error> {
        if data.len() > IPC_MESSAGE_MAX {
            return Err(Error::InvalidParameter);
        }
        if data.len() <= IPC_INLINE_MAX {
            let mut bytes = [0u8; IPC_INLINE_MAX];
            bytes[..data.len()].copy_from_slice(data);
            return Ok(Self {
                storage: EndpointMessageStorage::Inline {
                    len: data.len() as u16,
                    bytes,
                },
            });
        }
        Ok(Self {
            storage: EndpointMessageStorage::Heap(Box::<[u8]>::from(data)),
        })
    }

    pub fn len(&self) -> usize {
        match &self.storage {
            EndpointMessageStorage::Inline { len, .. } => *len as usize,
            EndpointMessageStorage::Heap(bytes) => bytes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> &[u8] {
        match &self.storage {
            EndpointMessageStorage::Inline { len, bytes } => &bytes[..*len as usize],
            EndpointMessageStorage::Heap(bytes) => bytes,
        }
    }
}

/// A call message includes the caller's thread ID for reply routing
#[derive(Clone)]
pub struct CallMessage {
    pub caller: ThreadId,
    pub message: EndpointMessage,
    pub cookie: u64,
}

/// Received IPC message paired with authenticated sender metadata.
#[derive(Clone)]
pub struct ReceivedMessage {
    pub message: EndpointMessage,
    pub sender: Option<ThreadId>,
    /// Kernel-tracked reply_id. Only set by kernel code paths (sys_call, fault forwarding).
    pub reply_id: Option<crate::token::ReplyId>,
}

pub trait ByteEndpoint: Send {
    fn send(&mut self, sender: Option<ThreadId>, data: &[u8]) -> Result<Option<ThreadId>, Error>;
    fn recv(&mut self, receiver: ThreadId) -> Result<Option<ReceivedMessage>, Error>;
    fn recv_nonblocking(&mut self) -> Result<Option<ReceivedMessage>, Error>;
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
    queue: VecDeque<ReceivedMessage>,
    /// Call message queue (messages from call() that expect reply)
    call_queue: VecDeque<CallMessage>,
    /// Threads waiting to receive
    waiting_receivers: VecDeque<RecvWaiter>,
    /// Threads waiting to send (backpressure - queue was full)
    waiting_senders: VecDeque<ThreadId>,
    /// The caller currently being served (for reply routing)
    current_caller: Option<ThreadId>,
    /// Active callers keyed by call cookie.
    callers_by_cookie: BTreeMap<u64, ThreadId>,
}

#[derive(Copy, Clone)]
struct RecvWaiter {
    thread_id: ThreadId,
    ticket: u64,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
}

const MAX_QUEUE_LEN: usize = 1024;
const MAX_CALL_QUEUE_LEN: usize = 256;
const BUSY_LOG_EVERY: u64 = 64;
const IPC_DIRECT_DEBUG_LIMIT: u64 = 128;
const IPC_CALL_DEBUG_LIMIT: u64 = 256;
/// Runtime kill-switch for rendezvous direct delivery.
///
/// Enabled during kernel boot.
static IPC_RENDEZVOUS_DIRECT_ENABLED: AtomicBool = AtomicBool::new(false);
/// Runtime gate for register-only small-message IPC fast path.
///
/// Enabled by default and forced on during kernel boot.
static IPC_REGISTER_FAST_ENABLED: AtomicBool = AtomicBool::new(true);
static IPC_DIRECT_DEBUG_COUNT: AtomicU64 = AtomicU64::new(0);
static IPC_CALL_DEBUG_COUNT: AtomicU64 = AtomicU64::new(0);

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

    /// Number of regular messages pending in the recv queue.
    /// Used by `peek()` for non-destructive readiness queries (poll/select on pipes).
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    fn stats(&self) -> QueueStats {
        QueueStats {
            queue_len: self.queue.len(),
            call_queue_len: self.call_queue.len(),
            waiting_len: self.waiting_receivers.len(),
            callers_len: self.callers_by_cookie.len(),
        }
    }

    fn enqueue_receiver_if_absent(
        &mut self,
        receiver: ThreadId,
        ticket: u64,
        buf_ptr: usize,
        buf_len: usize,
        page_table_root: x86_64::PhysAddr,
    ) {
        // Dedupe by thread_id alone: a fresh sys_recv bumps the receiver's ticket,
        // invalidating any prior waiter entry for the same thread. Matching only on
        // (thread_id, ticket) leaked stale entries on every recv timeout retry —
        // observed as a VecDeque<RecvWaiter> doubling 1.25 MiB → 2.5 MiB during
        // boot once the compositor's deadline-based event loop started running.
        if let Some(waiter) = self
            .waiting_receivers
            .iter_mut()
            .find(|waiter| waiter.thread_id == receiver)
        {
            waiter.ticket = ticket;
            waiter.buf_ptr = buf_ptr;
            waiter.buf_len = buf_len;
            waiter.page_table_root = page_table_root;
            return;
        }
        self.waiting_receivers.push_back(RecvWaiter {
            thread_id: receiver,
            ticket,
            buf_ptr,
            buf_len,
            page_table_root,
        });
    }

    fn wake_one_waiting_sender(&mut self) {
        if let Some(sender_id) = self.waiting_senders.pop_front() {
            crate::sched::ThreadManager::wake_thread(sender_id);
        }
    }

    fn pop_next_receiver_to_wake(&mut self) -> Option<RecvWaiter> {
        while let Some(waiter) = self.waiting_receivers.pop_front() {
            if crate::sched::ThreadManager::is_thread_recv_waiting_ticket(
                waiter.thread_id,
                waiter.ticket,
            ) {
                return Some(waiter);
            }
            crate::telemetry::record_ipc_stale_waiter();
        }
        None
    }

    fn register_receiver_wait(
        &mut self,
        receiver: ThreadId,
        ticket: u64,
        buf_ptr: usize,
        buf_len: usize,
        page_table_root: x86_64::PhysAddr,
    ) {
        self.enqueue_receiver_if_absent(receiver, ticket, buf_ptr, buf_len, page_table_root);
    }
}

impl QueueEndpoint {
    fn send_inner(
        &mut self,
        sender: Option<ThreadId>,
        data: &[u8],
        reply_id: Option<crate::token::ReplyId>,
    ) -> Result<Option<ThreadId>, Error> {
        if self.queue.len() >= MAX_QUEUE_LEN {
            if let Some(sender_id) = crate::sched::ThreadManager::current() {
                self.waiting_senders.push_back(sender_id);
                return Err(Error::WouldBlock);
            }
            return Err(Error::Busy);
        }
        let msg = EndpointMessage::new(data)?;
        let msg_len = msg.len();
        self.queue.push_back(ReceivedMessage {
            message: msg,
            sender,
            reply_id,
        });
        crate::telemetry::record_ipc_queue_enqueue(msg_len);
        let receiver_to_wake = self.pop_next_receiver_to_wake().map(|w| w.thread_id);
        if !self.waiting_senders.is_empty() && self.queue.len() < MAX_QUEUE_LEN {
            self.wake_one_waiting_sender();
        }
        Ok(receiver_to_wake)
    }

    pub fn send_with_reply_id(
        &mut self,
        sender: Option<ThreadId>,
        data: &[u8],
        reply_id: crate::token::ReplyId,
    ) -> Result<Option<ThreadId>, Error> {
        self.send_inner(sender, data, Some(reply_id))
    }
}

impl ByteEndpoint for QueueEndpoint {
    fn send(&mut self, sender: Option<ThreadId>, data: &[u8]) -> Result<Option<ThreadId>, Error> {
        self.send_inner(sender, data, None)
    }

    fn recv(&mut self, receiver: ThreadId) -> Result<Option<ReceivedMessage>, Error> {
        // First check call queue (call messages take priority)
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            crate::telemetry::record_ipc_queue_dequeue(call_msg.message.len());
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(ReceivedMessage {
                message: call_msg.message,
                sender: Some(call_msg.caller),
                reply_id: None,
            }));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            crate::telemetry::record_ipc_queue_dequeue(msg.message.len());
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(msg));
        }
        let ticket = crate::sched::ThreadManager::current_recv_wait_ticket().unwrap_or(0);
        self.enqueue_receiver_if_absent(receiver, ticket, 0, 0, x86_64::PhysAddr::new(0));
        Err(Error::WouldBlock)
    }

    fn recv_nonblocking(&mut self) -> Result<Option<ReceivedMessage>, Error> {
        // First check call queue
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            crate::telemetry::record_ipc_queue_dequeue(call_msg.message.len());
            // Queue now has space - wake a waiting sender if any
            self.wake_one_waiting_sender();
            return Ok(Some(ReceivedMessage {
                message: call_msg.message,
                sender: Some(call_msg.caller),
                reply_id: None,
            }));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            crate::telemetry::record_ipc_queue_dequeue(msg.message.len());
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
        let msg_len = msg.len();
        self.callers_by_cookie.insert(cookie, caller);
        self.call_queue.push_back(CallMessage {
            caller,
            message: msg,
            cookie,
        });
        crate::telemetry::record_ipc_queue_enqueue(msg_len);
        Ok(self.pop_next_receiver_to_wake().map(|w| w.thread_id))
    }

    fn recv_call(
        &mut self,
        receiver: ThreadId,
    ) -> Result<Option<(EndpointMessage, Option<ThreadId>)>, Error> {
        // First check call queue
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            crate::telemetry::record_ipc_queue_dequeue(call_msg.message.len());
            return Ok(Some((call_msg.message, Some(call_msg.caller))));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            crate::telemetry::record_ipc_queue_dequeue(msg.message.len());
            return Ok(Some((msg.message, msg.sender)));
        }
        let ticket = crate::sched::ThreadManager::current_recv_wait_ticket().unwrap_or(0);
        self.enqueue_receiver_if_absent(receiver, ticket, 0, 0, x86_64::PhysAddr::new(0));
        Err(Error::WouldBlock)
    }
}

/// Sharded endpoint repository to reduce lock contention and avoid Arc heap allocations
/// Each shard has its own mutex, endpoints distributed by ID hash
const NUM_ENDPOINT_SHARDS: usize = 16;

struct EndpointShard {
    endpoints: BTreeMap<EndpointId, QueueEndpoint>,
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

/// Dump per-endpoint queue depths for any endpoint exceeding `threshold`
/// pending messages or with any blocked sender/receiver.
///
/// Uses `try_lock` per shard so it's safe to call from inside another
/// shard's lock (e.g. directly after enqueue) — busy shards are skipped
/// rather than waited on, which is fine for diagnostic purposes.
pub fn dump_heavy_endpoints(threshold: usize) {
    klibcluu::warn("ipc-depth: dump start");
    let mut total_busy = 0u64;
    let mut total_skipped = 0u64;
    for shard_idx in 0..NUM_ENDPOINT_SHARDS {
        let shard = &ENDPOINT_SHARDS[shard_idx];
        match shard.try_lock() {
            Some(guard) => {
                for (id, ep) in guard.endpoints.iter() {
                    let q = ep.queue.len();
                    let cq = ep.call_queue.len();
                    let ws = ep.waiting_senders.len();
                    let wr = ep.waiting_receivers.len();
                    if q >= threshold || cq >= threshold || ws > 0 {
                        total_busy += 1;
                        klibcluu::log_dec(klibcluu::LogLevel::Info, "  ep=", id.as_u64());
                        klibcluu::log_dec(klibcluu::LogLevel::Info, "    q=", q as u64);
                        klibcluu::log_dec(klibcluu::LogLevel::Info, "    cq=", cq as u64);
                        klibcluu::log_dec(klibcluu::LogLevel::Info, "    ws=", ws as u64);
                        klibcluu::log_dec(klibcluu::LogLevel::Info, "    wr=", wr as u64);
                    }
                }
            }
            None => {
                total_skipped += 1;
            }
        }
    }
    klibcluu::log_dec(klibcluu::LogLevel::Info, "ipc-depth: heavy_eps=", total_busy);
    klibcluu::log_dec(
        klibcluu::LogLevel::Info,
        "ipc-depth: skipped_shards=",
        total_skipped,
    );
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

/// Global live-endpoint counter.
static TOTAL_ENDPOINT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Hard cap on total live endpoints system-wide.
const MAX_TOTAL_ENDPOINTS: u64 = 4096;

pub fn try_create_endpoint() -> Result<EndpointId, crate::error::Error> {
    loop {
        let current = TOTAL_ENDPOINT_COUNT.load(Ordering::Relaxed);
        if current >= MAX_TOTAL_ENDPOINTS {
            return Err(crate::error::Error::OutOfMemory);
        }
        if TOTAL_ENDPOINT_COUNT
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }
    let id = EndpointId::new(NEXT_ENDPOINT_ID.fetch_add(1, Ordering::SeqCst));
    let shard = get_endpoint_shard(id);
    let mut shard_guard = shard.lock();
    shard_guard
        .endpoints
        .insert(id, QueueEndpoint::new());
    Ok(id)
}

pub fn create_endpoint() -> EndpointId {
    try_create_endpoint().expect("Endpoint limit reached during boot")
}

/// Remove an endpoint from the repository and decrement the global counter.
///
/// WARNING: This does NOT drain message queues or wake blocked threads.
/// Callers must pair with full cleanup logic (Task #5) to avoid
/// permanently stuck threads.
pub fn destroy_endpoint(id: EndpointId) -> bool {
    let shard = get_endpoint_shard(id);
    let mut shard_guard = shard.lock();
    let removed = shard_guard.endpoints.remove(&id).is_some();
    if removed {
        TOTAL_ENDPOINT_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
    removed
}

pub fn endpoint_count_global() -> u64 {
    TOTAL_ENDPOINT_COUNT.load(Ordering::Relaxed)
}

/// Destroy an endpoint, wake all blocked threads, and drop queued messages.
///
/// Called when the last token referencing this endpoint is revoked.
/// After this call, the endpoint no longer exists and any subsequent
/// IPC operations on it will return Error::NotFound.
///
/// Idempotent: returns silently if endpoint was already destroyed.
pub fn destroy_endpoint_full(id: EndpointId) {
    let shard = get_endpoint_shard(id);
    let mut shard_guard = shard.lock();

    let endpoint = match shard_guard.endpoints.remove(&id) {
        Some(ep) => ep,
        None => return, // Already destroyed
    };
    TOTAL_ENDPOINT_COUNT.fetch_sub(1, Ordering::Relaxed);

    // Collect ALL threads that need to be woken
    let receivers: alloc::vec::Vec<crate::sched::ThreadId> = endpoint
        .waiting_receivers
        .into_iter()
        .map(|w| w.thread_id)
        .collect();
    let senders: alloc::vec::Vec<crate::sched::ThreadId> =
        endpoint.waiting_senders.into_iter().collect();
    let callers: alloc::vec::Vec<crate::sched::ThreadId> =
        endpoint.callers_by_cookie.into_values().collect();
    let current = endpoint.current_caller;

    // Messages in queue and call_queue are dropped here (VecDeque Drop impl frees memory)

    // Release shard lock BEFORE waking threads (lock ordering compliance)
    drop(shard_guard);

    // Wake all blocked threads — they'll retry and get Error::NotFound
    for tid in receivers {
        crate::sched::ThreadManager::wake_thread(tid);
    }
    for tid in senders {
        crate::sched::ThreadManager::wake_thread(tid);
    }
    for tid in callers {
        crate::sched::ThreadManager::wake_thread(tid);
    }
    if let Some(tid) = current {
        crate::sched::ThreadManager::wake_thread(tid);
    }
}

/// Number of endpoints currently tracked across all shards.
pub fn endpoint_count() -> usize {
    ENDPOINT_SHARDS
        .iter()
        .map(|shard| shard.lock().endpoints.len())
        .sum()
}

pub fn send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.send(None, data)
}

pub fn try_send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard (non-blocking)
    let mut shard_guard = shard.try_lock().ok_or(Error::WouldBlock)?;
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.send(None, data)
}

pub fn try_send_with_reply_id(
    endpoint: EndpointId,
    data: &[u8],
    reply_id: crate::token::ReplyId,
) -> Result<Option<ThreadId>, Error> {
    let shard = get_endpoint_shard(endpoint);
    let mut shard_guard = shard.try_lock().ok_or(Error::WouldBlock)?;
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.send_with_reply_id(None, data, reply_id)
}

pub fn recv(endpoint: EndpointId, receiver: ThreadId) -> Result<Option<ReceivedMessage>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.recv(receiver)
}

pub fn register_wait(
    endpoint: EndpointId,
    receiver: ThreadId,
    ticket: u64,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<(), Error> {
    let shard = get_endpoint_shard(endpoint);
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.register_receiver_wait(receiver, ticket, buf_ptr, buf_len, page_table_root);
    Ok(())
}

pub fn recv_nonblocking(endpoint: EndpointId) -> Result<Option<ReceivedMessage>, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint (allows concurrent access to endpoints in different shards)
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.recv_nonblocking()
}

/// Non-destructive query of the recv queue depth.
/// Returns the number of pending regular messages (does not include call_queue
/// or waiting senders). Used by `InvokeOp::EndpointPeek` for poll() readiness
/// checks on pipe read-ends.
pub fn peek(endpoint: EndpointId) -> Result<usize, Error> {
    let shard = get_endpoint_shard(endpoint);
    let shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get(&endpoint)
        .ok_or(Error::NotFound)?;
    Ok(ep.pending())
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
    if msg_len > IPC_MESSAGE_MAX {
        return Err(Error::InvalidParameter);
    }
    let payload = copy_user_payload(msg_ptr, msg_len, page_table_root)?;
    send_payload(endpoint, payload.as_slice(), first_send)
}

pub fn send_from_kernel(endpoint: EndpointId, payload: &[u8]) -> Result<(), Error> {
    if payload.len() > IPC_MESSAGE_MAX {
        return Err(Error::InvalidParameter);
    }
    send_payload(endpoint, payload, false)
}

pub fn recv_to_user_nonblocking(
    endpoint: EndpointId,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<(usize, Option<ThreadId>), Error> {
    crate::syscall::userptr::validate_user_buffer(buf_ptr, buf_len)?;
    let received = match recv_nonblocking(endpoint) {
        Ok(Some(msg)) => msg,
        Ok(None) => return Ok((0, None)),
        Err(Error::WouldBlock) => return Err(Error::WouldBlock),
        Err(err) => return Err(err),
    };
    if received.message.len() > buf_len {
        return Err(Error::BufferTooSmall);
    }
    let mut logged = RECV_LOGGED.lock();
    if logged.insert(endpoint) {
        klibcluu::trace("recv_to_user_nonblocking: endpoint_id=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint.as_u64());
        klibcluu::trace("recv_to_user_nonblocking: msg_len=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", received.message.len() as u64);
    }
    let msg_bytes = received.message.bytes();
    // Safety: msg_bytes points to a kernel buffer of msg_bytes.len() bytes.
    unsafe {
        crate::syscall::userptr::copy_to_user(
            buf_ptr,
            msg_bytes.as_ptr(),
            msg_bytes.len(),
            page_table_root,
        )?;
    }

    // Bind reply_id to current (receiving) thread using kernel metadata
    if let Some(reply_id) = received.reply_id {
        if let Some(current) = crate::sched::ThreadManager::current() {
            crate::sched::ThreadManager::bind_call_reply_to_server(reply_id, current);
            crate::sched::ThreadManager::bind_fault_reply_to_server(reply_id, current);
        }
    }

    Ok((msg_bytes.len(), received.sender))
}

/// Send a call message from userspace with reply_id injected
/// Inner call logic: takes an already-filled mutable buffer, injects reply_id, delivers.
fn call_with_reply_id_inner(
    endpoint: EndpointId,
    buffer: &mut [u8],
    payload_len: usize,
    reply_id: crate::token::ReplyId,
) -> Result<(), Error> {
    let sender = crate::sched::ThreadManager::current();

    inject_reply_id(buffer, reply_id);

    let (wake, direct_receiver, waiters_before, queue_len_after, call_queue_len_after) = {
        let shard = get_endpoint_shard(endpoint);
        let mut shard_guard = shard.lock();
        let ep = shard_guard
            .endpoints
            .get_mut(&endpoint)
            .ok_or(Error::NotFound)?;
        let waiters_before = ep.stats().waiting_len;
        let payload_bytes = &buffer[..payload_len];
        let result = match try_direct_deliver_to_waiting_receiver(
            ep,
            endpoint,
            sender,
            payload_bytes,
        )? {
            DirectDelivery::DeliveredWake(receiver_id) => (Some(receiver_id), Some(receiver_id)),
            DirectDelivery::DeliveredNoWake(receiver_id) => (None, Some(receiver_id)),
            DirectDelivery::NotDelivered => {
                match ep.send_with_reply_id(sender, payload_bytes, reply_id) {
                    Ok(wake) => (wake, None),
                    Err(Error::WouldBlock) => {
                        crate::sched::ThreadManager::block_current();
                        crate::architecture::x86_64::syscall::request_resched();
                        return Err(Error::WouldBlock);
                    }
                    Err(Error::Busy) => {
                        log_endpoint_busy(endpoint, ep.stats(), true);
                        return Err(Error::Busy);
                    }
                    Err(err) => return Err(err),
                }
            }
        };
        let stats = ep.stats();
        (
            result.0,
            result.1,
            waiters_before,
            stats.queue_len,
            stats.call_queue_len,
        )
    };

    let dbg_count = IPC_CALL_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed);
    if dbg_count < IPC_CALL_DEBUG_LIMIT && klibcluu::should_log(klibcluu::LogLevel::Trace) {
        klibcluu::trace("ipc-call enqueue");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  endpoint=", endpoint.as_u64());
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  sender=",
            sender.map(|tid| tid.as_u64()).unwrap_or(0),
        );
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  reply_id=", reply_id.as_u64());
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  payload_len=",
            payload_len as u64,
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  waiters_before=",
            waiters_before as u64,
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  wake=",
            wake.map(|tid| tid.as_u64()).unwrap_or(0),
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  direct_receiver=",
            direct_receiver.map(|tid| tid.as_u64()).unwrap_or(0),
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  queue_len_after=",
            queue_len_after as u64,
        );
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  call_queue_len_after=",
            call_queue_len_after as u64,
        );
    }

    if let Some(receiver_id) = direct_receiver {
        crate::sched::ThreadManager::bind_call_reply_to_server(reply_id, receiver_id);
    }
    if let Some(thread_id) = wake {
        crate::sched::ThreadManager::wake_thread(thread_id);
    }
    Ok(())
}

/// Send a call message from userspace with reply_id injected
pub fn call_from_user_with_reply_id(
    endpoint: EndpointId,
    msg_ptr: usize,
    msg_len: usize,
    page_table_root: x86_64::PhysAddr,
    reply_id: crate::token::ReplyId,
) -> Result<(), Error> {
    let dbg_count = IPC_CALL_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed);
    if dbg_count < IPC_CALL_DEBUG_LIMIT && klibcluu::should_log(klibcluu::LogLevel::Trace) {
        klibcluu::trace("ipc-call from-user entry");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  endpoint=", endpoint.as_u64());
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  msg_ptr=", msg_ptr as u64);
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  msg_len=", msg_len as u64);
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  reply_id=", reply_id.as_u64());
    }
    let mut payload = copy_user_payload(msg_ptr, msg_len, page_table_root)?;
    if dbg_count < IPC_CALL_DEBUG_LIMIT && klibcluu::should_log(klibcluu::LogLevel::Trace) {
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  copied_payload_len=",
            payload.as_slice().len() as u64,
        );
    }
    let payload_len = payload.as_slice().len();
    call_with_reply_id_inner(endpoint, payload.as_mut_slice(), payload_len, reply_id)
}

/// Send a call message from a kernel buffer with reply_id injected
pub fn call_from_kernel_with_reply_id(
    endpoint: EndpointId,
    buffer: &mut [u8],
    payload_len: usize,
    reply_id: crate::token::ReplyId,
) -> Result<(), Error> {
    if payload_len > IPC_MESSAGE_MAX {
        return Err(Error::InvalidParameter);
    }
    call_with_reply_id_inner(endpoint, buffer, payload_len, reply_id)
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

fn send_payload(endpoint: EndpointId, payload: &[u8], log_send: bool) -> Result<(), Error> {
    let sender = crate::sched::ThreadManager::current();
    // Try to send - if queue is full, block and return (will be retried from userspace when woken)
    let wake = {
        // Get shard directly (static, no repository lock needed)
        let shard = get_endpoint_shard(endpoint);

        // Lock shard, then endpoint
        let mut shard_guard = shard.lock();
        let ep = shard_guard
            .endpoints
            .get_mut(&endpoint)
            .ok_or(Error::NotFound)?;
        match try_direct_deliver_to_waiting_receiver(ep, endpoint, sender, payload)? {
            DirectDelivery::DeliveredWake(receiver_id) => Some(receiver_id),
            DirectDelivery::DeliveredNoWake(_) => None,
            DirectDelivery::NotDelivered => match ep.send(sender, payload) {
                Ok(wake) => wake, // Success
                Err(Error::WouldBlock) => {
                    // Queue is full - sender was added to waiting_senders, need to block.
                    crate::sched::ThreadManager::block_current();
                    crate::architecture::x86_64::syscall::request_resched();
                    return Err(Error::WouldBlock);
                }
                Err(Error::Busy) => {
                    // Fallback for edge cases (shouldn't happen with backpressure)
                    log_endpoint_busy(endpoint, ep.stats(), false);
                    return Err(Error::Busy);
                }
                Err(err) => return Err(err),
            },
        }
    };

    // Success - wake receiver if any
    if let Some(thread_id) = wake {
        if log_send {
            klibcluu::trace("send_from_user: wake_thread_id=");
            klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
        }
        crate::sched::ThreadManager::wake_thread(thread_id);
    }
    Ok(())
}

fn try_direct_deliver_to_waiting_receiver(
    endpoint: &mut QueueEndpoint,
    endpoint_id: EndpointId,
    sender: Option<ThreadId>,
    data: &[u8],
) -> Result<DirectDelivery, Error> {
    if !rendezvous_direct_enabled() {
        return Ok(DirectDelivery::NotDelivered);
    }

    let max_scan = endpoint.waiting_receivers.len();
    for _ in 0..max_scan {
        let Some(waiter) = endpoint.pop_next_receiver_to_wake() else {
            break;
        };
        direct_debug("attempt", endpoint_id, Some(waiter), data.len());
        let receiver_id = waiter.thread_id;
        if waiter.buf_len == 0 || waiter.buf_ptr == 0 || waiter.page_table_root.is_null() {
            direct_debug("no-recv-buffer", endpoint_id, Some(waiter), data.len());
            crate::telemetry::record_ipc_stale_waiter();
            continue;
        }

        if data.len() > waiter.buf_len {
            direct_debug("buffer-too-small", endpoint_id, Some(waiter), data.len());
            endpoint.waiting_receivers.push_back(waiter);
            continue;
        }

        // Safety: waiter metadata was captured from validated syscall arguments at register_wait.
        let copy_result = unsafe {
            crate::syscall::userptr::copy_to_user(
                waiter.buf_ptr,
                data.as_ptr(),
                data.len(),
                waiter.page_table_root,
            )
        };
        if copy_result.is_err() {
            direct_debug("copy-fail", endpoint_id, Some(waiter), data.len());
            endpoint.waiting_receivers.push_back(waiter);
            continue;
        }

        let stored = crate::sched::ThreadManager::set_recv_wait_delivery(
            receiver_id,
            endpoint_id,
            data.len(),
            sender,
        );
        if !stored {
            direct_debug("store-fail", endpoint_id, Some(waiter), data.len());
            endpoint.waiting_receivers.push_back(waiter);
            continue;
        }
        crate::telemetry::record_ipc_direct_delivery();
        if crate::sched::ThreadManager::is_thread_blocked(receiver_id) {
            direct_debug("delivered-wake", endpoint_id, Some(waiter), data.len());
            return Ok(DirectDelivery::DeliveredWake(receiver_id));
        }
        direct_debug("delivered-nowake", endpoint_id, Some(waiter), data.len());
        return Ok(DirectDelivery::DeliveredNoWake(receiver_id));
    }

    direct_debug("no-waiter", endpoint_id, None, data.len());
    Ok(DirectDelivery::NotDelivered)
}

fn copy_user_payload(
    msg_ptr: usize,
    msg_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<UserPayloadBuffer, Error> {
    crate::syscall::userptr::validate_user_buffer(msg_ptr, msg_len)?;
    if msg_len > IPC_MESSAGE_MAX {
        return Err(Error::InvalidParameter);
    }
    if msg_len <= IPC_INLINE_MAX {
        let mut bytes = [0u8; IPC_INLINE_MAX];
        // Safety: bytes points to a valid kernel buffer of msg_len bytes.
        unsafe {
            crate::syscall::userptr::copy_from_user(
                bytes.as_mut_ptr(),
                msg_ptr,
                msg_len,
                page_table_root,
            )?;
        }
        return Ok(UserPayloadBuffer::Inline {
            len: msg_len,
            bytes,
        });
    }

    let mut bytes = vec![0u8; msg_len];
    // Safety: bytes points to a valid kernel buffer of msg_len bytes.
    unsafe {
        crate::syscall::userptr::copy_from_user(
            bytes.as_mut_ptr(),
            msg_ptr,
            msg_len,
            page_table_root,
        )?;
    }
    Ok(UserPayloadBuffer::Heap(bytes))
}

enum DirectDelivery {
    NotDelivered,
    DeliveredWake(ThreadId),
    DeliveredNoWake(ThreadId),
}

fn direct_debug(event: &str, endpoint: EndpointId, waiter: Option<RecvWaiter>, len: usize) {
    let n = IPC_DIRECT_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= IPC_DIRECT_DEBUG_LIMIT {
        return;
    }
    klibcluu::trace("ipc-direct");
    klibcluu::trace("  event=");
    klibcluu::trace(event);
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "  endpoint=", endpoint.as_u64());
    if let Some(waiter) = waiter {
        klibcluu::log_dec(
            klibcluu::LogLevel::Trace,
            "  waiter_thread=",
            waiter.thread_id.as_u64(),
        );
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "  waiter_ticket=", waiter.ticket);
    }
    klibcluu::log_dec(klibcluu::LogLevel::Trace, "  len=", len as u64);
}

#[inline(always)]
pub fn set_rendezvous_direct_enabled(enabled: bool) {
    IPC_RENDEZVOUS_DIRECT_ENABLED.store(enabled, Ordering::Relaxed);
}

#[inline(always)]
pub fn rendezvous_direct_enabled() -> bool {
    IPC_RENDEZVOUS_DIRECT_ENABLED.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn set_register_fast_enabled(enabled: bool) {
    IPC_REGISTER_FAST_ENABLED.store(enabled, Ordering::Relaxed);
}

#[inline(always)]
pub fn register_fast_enabled() -> bool {
    IPC_REGISTER_FAST_ENABLED.load(Ordering::Relaxed)
}

/// Get the current caller for an endpoint (used by reply)
pub fn take_current_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.take_current_caller().ok_or(Error::InvalidState)
}

/// Get a caller for a specific call cookie.
pub fn take_caller_by_cookie(endpoint: EndpointId, cookie: u64) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.take_caller_by_cookie(cookie)
        .ok_or(Error::InvalidState)
}

pub fn take_any_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    // Get shard directly (static, no repository lock needed)
    let shard = get_endpoint_shard(endpoint);

    // Lock shard, then endpoint
    let mut shard_guard = shard.lock();
    let ep = shard_guard
        .endpoints
        .get_mut(&endpoint)
        .ok_or(Error::NotFound)?;
    ep.take_any_caller().ok_or(Error::InvalidState)
}

/// Tag indicating the message contains a reply ID
pub const REPLY_ID_TAG: u8 = 2;
/// Word index where reply ID is stored
pub const REPLY_ID_WORD: usize = 5;

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

/// Size of a UserMessage in bytes (tag + 6 words).
pub const USER_MESSAGE_SIZE: usize = core::mem::size_of::<UserMessage>();
const _: () = assert!(USER_MESSAGE_SIZE == 56);

/// Inject reply_id into message
fn inject_reply_id(buffer: &mut [u8], reply_id: crate::token::ReplyId) {
    if buffer.len() < core::mem::size_of::<UserMessage>() {
        return;
    }
    let msg = unsafe { &mut *(buffer.as_mut_ptr() as *mut UserMessage) };
    msg.tag.extra = REPLY_ID_TAG;
    msg.words[REPLY_ID_WORD] = reply_id.as_u64() as usize;
}

/// Deliver a reply using pre-verified CallReplyInfo.
///
/// Copies the reply data directly to the caller's reply buffer and wakes them.
pub fn deliver_reply(
    reply_info: crate::sched::CallReplyInfo,
    reply_data: &[u8],
) -> Result<usize, Error> {
    use crate::sched::ThreadManager;

    let caller = reply_info.caller;

    // Validate caller thread exists
    ThreadManager::with_thread(caller, |_| ()).ok_or(Error::NotFound)?;

    // Validate and copy reply to caller's buffer
    if reply_data.len() > reply_info.reply_buf_len {
        return Err(Error::BufferTooSmall);
    }

    unsafe {
        crate::syscall::userptr::copy_to_user(
            reply_info.reply_buf_ptr,
            reply_data.as_ptr(),
            reply_data.len(),
            reply_info.page_table_root,
        )?;
    }

    let bytes_received = reply_data.len();
    ThreadManager::with_thread_mut(caller, |thread| {
        thread.context.rax = bytes_received as u64;
    });

    ThreadManager::wake_thread(caller);
    Ok(bytes_received)
}
