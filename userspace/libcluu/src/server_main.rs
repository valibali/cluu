//! Generic async server main-loop skeleton.
//!
//! Generalizes the VFS/session-procmgr recv-spawn-poll pattern so that
//! single-threaded IPC servers can adopt the async runtime without each
//! reimplementing the loop. Skeleton only — not adopted by any server yet.
//!
//! # Usage
//!
//! ```ignore
//! let mut server = AsyncServerMain::new(token_self, server_ep);
//! server.init();
//! loop {
//!     server.poll_ready();
//!     let tokens = [server_ep, server.reply_endpoint()];
//!     match server.recv_any(&tokens, &mut buf, timeout_ms) {
//!         Ok(msg) => { server.dispatch(msg); }
//!         Err(_) => {}
//!     }
//!     server.drain_completions();
//! }
//! ```

extern crate alloc;
use alloc::vec::Vec;
use crate::async_runtime::Runtime;
use crate::error::{Error, Result};
use crate::syscall;
use crate::types::Message;

pub struct AsyncServerMain {
    rt: Runtime,
}

impl AsyncServerMain {
    pub fn new(token_self: usize, _server_ep: usize) -> Result<Self> {
        let rt = Runtime::new(token_self)?;
        Ok(Self { rt })
    }

    pub fn reply_endpoint(&self) -> usize {
        self.rt.reply_endpoint()
    }

    pub fn poll_ready(&mut self) {
        self.rt.poll_ready();
    }

    pub fn recv_any(
        &self,
        tokens: &[usize],
        buf: &mut [u8],
        timeout_ms: u64,
    ) -> Result<(Message, Vec<u8>, usize, usize)> {
        let (idx, len) = syscall::ipc_recv_any(tokens, buf, timeout_ms)?;
        let (msg, payload) = crate::ipc::parse_message(&buf[..len])
            .ok_or(Error::InvalidArgument)?;
        Ok((msg, payload.to_vec(), len, idx))
    }

    pub fn deliver_reply(&mut self, cookie: usize, msg: Message, payload: Vec<u8>) {
        self.rt.deliver_reply(cookie, msg, payload);
    }

    pub fn cancel_endpoint(&mut self, endpoint: usize) {
        self.rt.cancel_endpoint(endpoint);
    }

    pub fn drain_completions(&mut self) {
        while self.rt.pop_completion().is_some() {}
    }

    pub fn runtime(&mut self) -> &mut Runtime {
        &mut self.rt
    }

    pub fn is_reply(&self, idx: usize, tokens_len: usize) -> bool {
        idx + 1 == tokens_len
    }
}
