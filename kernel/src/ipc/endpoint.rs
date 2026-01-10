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

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn raw_bytes(&self) -> &[u8; IPC_MESSAGE_MAX] {
        &self.bytes
    }
}

pub trait ByteEndpoint: Send {
    fn send(&mut self, data: &[u8]) -> Result<Option<ThreadId>, Error>;
    fn recv(&mut self, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error>;
    fn recv_nonblocking(&mut self) -> Result<Option<EndpointMessage>, Error>;
}

pub struct QueueEndpoint {
    queue: VecDeque<EndpointMessage>,
    waiting_receivers: VecDeque<ThreadId>,
}

impl QueueEndpoint {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            waiting_receivers: VecDeque::new(),
        }
    }
}

impl ByteEndpoint for QueueEndpoint {
    fn send(&mut self, data: &[u8]) -> Result<Option<ThreadId>, Error> {
        let msg = EndpointMessage::new(data)?;
        self.queue.push_back(msg);
        Ok(self.waiting_receivers.pop_front())
    }

    fn recv(&mut self, receiver: ThreadId) -> Result<Option<EndpointMessage>, Error> {
        if let Some(msg) = self.queue.pop_front() {
            return Ok(Some(msg));
        }
        self.waiting_receivers.push_back(receiver);
        Err(Error::WouldBlock)
    }

    fn recv_nonblocking(&mut self) -> Result<Option<EndpointMessage>, Error> {
        if let Some(msg) = self.queue.pop_front() {
            return Ok(Some(msg));
        }
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
    crate::syscall::userptr::copy_from_user(
        buffer.as_mut_ptr(),
        msg_ptr,
        msg_len,
        page_table_root,
    )?;
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
    use core::sync::atomic::{AtomicU64, Ordering};

    static IPC_RECV_DELIVERED_LOGGED: AtomicU64 = AtomicU64::new(0);
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
    crate::syscall::userptr::copy_to_user(
        buf_ptr,
        msg.raw_bytes().as_ptr(),
        msg.len(),
        page_table_root,
    )?;
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
    crate::syscall::userptr::copy_to_user(
        buf_ptr,
        msg.raw_bytes().as_ptr(),
        msg.len(),
        page_table_root,
    )?;
    Ok(msg.len())
}
