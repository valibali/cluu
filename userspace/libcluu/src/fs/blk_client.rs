//! BlkSessionClient — caller-side helper for the BLK_OPEN_SESSION/SUBMIT/
//! COMPLETE raw-block IPC protocol.
//!
//! Open a session, then use either [`BlkSessionClient::read_blocking`]
//! (sync wrapper) or [`BlkSessionClient::submit_async`] +
//! [`BlkSessionClient::drain_completions`] (caller-driven) to issue reads.

use alloc::vec::Vec;

use crate::boot::{process_info, TOKEN_IPC, TOKEN_SPACE};
use crate::ipc::{
    parse_message, BLK_CLOSE_SESSION, BLK_COMPLETE, BLK_OPEN_SESSION, BLK_SUBMIT,
    BLK_SUBMIT_NACK,
};
use crate::syscall::{endpoint_create, ipc_call, ipc_recv_any, ipc_send, virt_to_phys};
use crate::types::Message;
use crate::{Error, Result};

/// Opaque per-request handle returned by [`BlkSessionClient::submit_async`].
/// Callers match this against the `RequestHandle` paired with each result
/// returned by [`BlkSessionClient::drain_completions`] /
/// [`BlkSessionClient::read_blocking`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct RequestHandle(pub u64);

pub struct BlkSessionClient {
    blkdev_endpoint: usize,
    completion_endpoint: usize,
    space_token: usize,
    session_id: u32,
    next_request_id: u64,
    pending_completions: Vec<(RequestHandle, Result<usize>)>,
}

impl BlkSessionClient {
    /// Open a new session against `blkdev_endpoint` (the listen endpoint
    /// of the virtio-blk service, typically obtained via
    /// `registry::subscribe_output("blkdev", "main")`).
    pub fn open(blkdev_endpoint: usize) -> Result<Self> {
        let info = process_info();
        let ipc_token = info.tokens[TOKEN_IPC];
        let space_token = info.tokens[TOKEN_SPACE];
        let completion_endpoint = endpoint_create(ipc_token)?;

        let req = Message::new(
            BLK_OPEN_SESSION,
            [completion_endpoint, 0, 0, 0, 0, 0],
            1,
        );
        let mut reply_buf = [0u8; 64];
        let bytes = ipc_call(blkdev_endpoint, req.as_bytes(), &mut reply_buf)?;
        let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(Error::InvalidState)?;
        if rmsg.tag.label != BLK_OPEN_SESSION || rmsg.words[0] != 0 {
            return Err(Error::InvalidState);
        }
        let session_id = rmsg.words[1] as u32;
        Ok(Self {
            blkdev_endpoint,
            completion_endpoint,
            space_token,
            session_id,
            next_request_id: 1,
            pending_completions: Vec::new(),
        })
    }

    /// Submit a single read; returns a [`RequestHandle`] the caller matches
    /// against later completions. `buf` MUST stay alive and unmoved until the
    /// matching completion arrives — its physical pages are recorded at
    /// submit time.
    ///
    /// **Page alignment constraint**: this method resolves physical pages by
    /// stepping through `buf` in 4096-byte increments starting at
    /// `buf.as_ptr()`. The buffer MUST start at a page boundary, otherwise
    /// the recorded physical addresses will be wrong. Callers should use
    /// `space_map_range`-allocated buffers or otherwise ensure page
    /// alignment.
    pub fn submit_async(&mut self, lba: u64, buf: &mut [u8]) -> Result<RequestHandle> {
        if buf.is_empty() {
            return Err(Error::InvalidArgument);
        }
        let n_pages = buf.len().div_ceil(4096);
        let mut pages_phys: Vec<u64> = Vec::with_capacity(n_pages);
        for i in 0..n_pages {
            let va = buf.as_ptr() as usize + i * 4096;
            pages_phys.push(virt_to_phys(self.space_token, va)?);
        }
        let rid = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);

        let msg = Message::new(
            BLK_SUBMIT,
            [
                self.session_id as usize,
                rid as usize,
                lba as usize,
                (lba >> 32) as usize,
                n_pages,
                buf.len(),
            ],
            6,
        );

        // Encode phys list as payload. Use Vec<u8> to assemble header+payload.
        let header = msg.as_bytes();
        let mut send_buf = Vec::with_capacity(header.len() + n_pages * 8);
        send_buf.extend_from_slice(header);
        for p in pages_phys {
            send_buf.extend_from_slice(&p.to_le_bytes());
        }
        ipc_send(self.blkdev_endpoint, &send_buf)?;
        Ok(RequestHandle(rid))
    }

    /// Non-blocking drain of any completions delivered so far.
    pub fn drain_completions(&mut self) -> Vec<(RequestHandle, Result<usize>)> {
        let mut out = core::mem::take(&mut self.pending_completions);
        let tokens = [self.completion_endpoint];
        let mut buf = [0u8; 128];
        loop {
            // timeout=0 => non-blocking poll.
            match ipc_recv_any(&tokens, &mut buf, 0) {
                Ok((_, len)) => {
                    if let Some((m, _)) = parse_message(&buf[..len]) {
                        out.push(self.decode_completion(&m));
                    }
                }
                Err(_) => break,
            }
        }
        out
    }

    /// Sync read: submit + block until OUR cookie arrives. Out-of-order
    /// completions for other in-flight requests are queued for a later
    /// `drain_completions` call.
    pub fn read_blocking(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize> {
        let h = self.submit_async(lba, buf)?;
        let tokens = [self.completion_endpoint];
        let mut rbuf = [0u8; 128];
        loop {
            let (_, len) = ipc_recv_any(&tokens, &mut rbuf, u64::MAX)?;
            if let Some((m, _)) = parse_message(&rbuf[..len]) {
                let (handle, result) = self.decode_completion(&m);
                if handle == h {
                    return result;
                }
                self.pending_completions.push((handle, result));
            }
        }
    }

    fn decode_completion(&self, m: &Message) -> (RequestHandle, Result<usize>) {
        let h = RequestHandle(m.words[0] as u64);
        let result = match m.tag.label {
            BLK_COMPLETE => {
                let status = m.words[1] as u8;
                let len = m.words[2];
                if status == 0 {
                    Ok(len)
                } else {
                    Err(Error::InvalidState)
                }
            }
            BLK_SUBMIT_NACK => Err(Error::from_errno(m.words[1] as isize)),
            _ => Err(Error::InvalidState),
        };
        (h, result)
    }
}

impl Drop for BlkSessionClient {
    fn drop(&mut self) {
        let msg = Message::new(
            BLK_CLOSE_SESSION,
            [self.session_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc_send(self.blkdev_endpoint, msg.as_bytes());
    }
}
