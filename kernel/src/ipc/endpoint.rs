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

pub const IPC_MESSAGE_MAX: usize = 256;

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
    /// The caller currently being served (for reply routing)
    current_caller: Option<ThreadId>,
    /// Active callers keyed by call cookie.
    callers_by_cookie: BTreeMap<u64, ThreadId>,
}

const MAX_QUEUE_LEN: usize = 1024;
const MAX_CALL_QUEUE_LEN: usize = 256;

impl QueueEndpoint {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            call_queue: VecDeque::new(),
            waiting_receivers: VecDeque::new(),
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
}

impl ByteEndpoint for QueueEndpoint {
    fn send(&mut self, data: &[u8]) -> Result<Option<ThreadId>, Error> {
        if self.queue.len() >= MAX_QUEUE_LEN {
            return Err(Error::Busy);
        }
        let msg = EndpointMessage::new(data)?;
        self.queue.push_back(msg);
        Ok(self.waiting_receivers.pop_front())
    }

    fn recv(&mut self, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error> {
        // First check call queue (call messages take priority)
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            return Ok(Some(call_msg.message));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
            return Ok(Some(msg));
        }
        self.waiting_receivers.push_back(receiver);
        Err(Error::WouldBlock)
    }

    fn recv_nonblocking(&mut self) -> Result<Option<EndpointMessage>, Error> {
        // First check call queue
        if let Some(call_msg) = self.call_queue.pop_front() {
            self.current_caller = Some(call_msg.caller);
            return Ok(Some(call_msg.message));
        }
        // Then check regular queue
        if let Some(msg) = self.queue.pop_front() {
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
        if self.call_queue.len() >= MAX_CALL_QUEUE_LEN {
            return Err(Error::Busy);
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
        self.waiting_receivers.push_back(receiver);
        Err(Error::WouldBlock)
    }
}

pub trait EndpointStore: Send {
    fn create(&mut self) -> EndpointId;
    fn with_endpoint<F, R>(&mut self, id: EndpointId, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut dyn ByteEndpoint) -> Result<R, Error>;
}

pub struct EndpointRepository {
    endpoints: BTreeMap<EndpointId, QueueEndpoint>,
}

impl EndpointRepository {
    fn new() -> Self {
        Self {
            endpoints: BTreeMap::new(),
        }
    }
}

impl EndpointStore for EndpointRepository {
    fn create(&mut self) -> EndpointId {
        let id = EndpointId::new(NEXT_ENDPOINT_ID.fetch_add(1, Ordering::SeqCst));
        self.endpoints.insert(id, QueueEndpoint::new());
        id
    }

    fn with_endpoint<F, R>(&mut self, id: EndpointId, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut dyn ByteEndpoint) -> Result<R, Error>,
    {
        let endpoint = self.endpoints.get_mut(&id).ok_or(Error::NotFound)?;
        f(endpoint)
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

static NEXT_ENDPOINT_ID: AtomicU64 = AtomicU64::new(1);

pub fn create_endpoint() -> EndpointId {
    ENDPOINTS.lock().create()
}

pub fn send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    ENDPOINTS
        .lock()
        .with_endpoint(endpoint, |queue| queue.send(data))
}

pub fn try_send(endpoint: EndpointId, data: &[u8]) -> Result<Option<ThreadId>, Error> {
    let mut guard = ENDPOINTS.try_lock().ok_or(Error::WouldBlock)?;
    guard.with_endpoint(endpoint, |queue| queue.send(data))
}

pub fn recv(endpoint: EndpointId, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error> {
    ENDPOINTS
        .lock()
        .with_endpoint(endpoint, |queue| queue.recv(receiver))
}

pub fn recv_nonblocking(endpoint: EndpointId) -> Result<Option<EndpointMessage>, Error> {
    ENDPOINTS
        .lock()
        .with_endpoint(endpoint, |queue| queue.recv_nonblocking())
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
    let wake = send(endpoint, &buffer[..msg_len])?;
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

    let wake = {
        let mut guard = ENDPOINTS.lock();
        let endpoint_obj = guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
        // Just send as regular message - reply routing via token
        endpoint_obj.send(&buffer[..msg_len])?
    };

    if let Some(thread_id) = wake {
        crate::sched::ThreadManager::wake_thread(thread_id);
    }
    Ok(())
}

/// Get the current caller for an endpoint (used by reply)
pub fn take_current_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    let mut guard = ENDPOINTS.lock();
    let endpoint_obj = guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    endpoint_obj
        .take_current_caller()
        .ok_or(Error::InvalidState)
}

/// Get a caller for a specific call cookie.
pub fn take_caller_by_cookie(endpoint: EndpointId, cookie: u64) -> Result<ThreadId, Error> {
    let mut guard = ENDPOINTS.lock();
    let endpoint_obj = guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    endpoint_obj
        .take_caller_by_cookie(cookie)
        .ok_or(Error::InvalidState)
}

pub fn take_any_caller(endpoint: EndpointId) -> Result<ThreadId, Error> {
    let mut guard = ENDPOINTS.lock();
    let endpoint_obj = guard.endpoints.get_mut(&endpoint).ok_or(Error::NotFound)?;
    endpoint_obj.take_any_caller().ok_or(Error::InvalidState)
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
