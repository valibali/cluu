//! IPC endpoint repository and message queue.
//!
//! This provides a minimal queue-based endpoint implementation so userspace
//! can send exit notifications to a parent process manager.

use crate::error::Error;
use crate::token::EndpointId;
use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

pub const IPC_MESSAGE_MAX: usize = 64;

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
    fn send(&mut self, data: &[u8]) -> Result<(), Error>;
    fn recv(&mut self) -> Result<Option<EndpointMessage>, Error>;
}

pub struct QueueEndpoint {
    queue: VecDeque<EndpointMessage>,
}

impl QueueEndpoint {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }
}

impl ByteEndpoint for QueueEndpoint {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        let msg = EndpointMessage::new(data)?;
        self.queue.push_back(msg);
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<EndpointMessage>, Error> {
        Ok(self.queue.pop_front())
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

static NEXT_ENDPOINT_ID: AtomicU64 = AtomicU64::new(1);

pub fn create_endpoint() -> EndpointId {
    ENDPOINTS.lock().create()
}

pub fn send(endpoint: EndpointId, data: &[u8]) -> Result<(), Error> {
    ENDPOINTS.lock().with_endpoint(endpoint, |queue| queue.send(data))?;
    Ok(())
}

pub fn recv(endpoint: EndpointId) -> Result<Option<EndpointMessage>, Error> {
    ENDPOINTS.lock().with_endpoint(endpoint, |queue| queue.recv())
}

pub fn send_from_user(
    endpoint: EndpointId,
    msg_ptr: usize,
    msg_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<(), Error> {
    crate::syscall::userptr::validate_user_buffer(msg_ptr, msg_len)?;
    let mut buffer = [0u8; IPC_MESSAGE_MAX];
    if msg_len > buffer.len() {
        return Err(Error::InvalidParameter);
    }
    crate::syscall::userptr::copy_from_user(buffer.as_mut_ptr(), msg_ptr, msg_len, page_table_root)?;
    send(endpoint, &buffer[..msg_len])
}

pub fn recv_to_user(
    endpoint: EndpointId,
    buf_ptr: usize,
    buf_len: usize,
    page_table_root: x86_64::PhysAddr,
) -> Result<usize, Error> {
    crate::syscall::userptr::validate_user_buffer(buf_ptr, buf_len)?;
    let Some(msg) = recv(endpoint)? else {
        return Ok(0);
    };
    if msg.len() > buf_len {
        return Err(Error::BufferTooSmall);
    }
    crate::syscall::userptr::copy_to_user(buf_ptr, msg.raw_bytes().as_ptr(), msg.len(), page_table_root)?;
    Ok(msg.len())
}
