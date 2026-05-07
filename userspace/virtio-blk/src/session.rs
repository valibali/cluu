//! Per-caller block-session state for the BLK_OPEN_SESSION/SUBMIT/COMPLETE
//! IPC protocol. The driver tracks one [`BlkSession`] per opened session;
//! request cookies pack `(session_id << 32) | request_id` so the IRQ-side
//! demux can route completions back to the right caller.

use alloc::collections::BTreeMap;

pub type SessionId = u32;
pub type RequestId = u64;

/// Bookkeeping for a single in-flight request inside a session.
pub struct InFlight {
    pub request_id: RequestId,
    pub completion_endpoint: usize,
    pub bytes_requested: usize,
}

/// Driver-side state for one opened block session.
pub struct BlkSession {
    pub session_id: SessionId,
    pub completion_endpoint: usize,
    pub queue_depth_cap: u16,
    pub in_flight: BTreeMap<RequestId, InFlight>,
}

impl BlkSession {
    pub fn new(session_id: SessionId, completion_endpoint: usize) -> Self {
        Self {
            session_id,
            completion_endpoint,
            queue_depth_cap: 32,
            in_flight: BTreeMap::new(),
        }
    }

    /// True if the session has reached its per-caller depth cap and the
    /// driver should NACK new submits with `Error::Busy`.
    pub fn at_cap(&self) -> bool {
        self.in_flight.len() as u16 >= self.queue_depth_cap
    }
}

/// Pack a (session, request) tuple into the 64-bit virtqueue cookie. The
/// upper 32 bits are the session id; the lower 32 bits are the (truncated)
/// request id. Session 0 is reserved for the legacy FS_READ_GRANT path.
pub fn pack_cookie(sid: SessionId, rid: RequestId) -> u64 {
    ((sid as u64) << 32) | (rid & 0xFFFF_FFFF)
}

/// Inverse of [`pack_cookie`].
pub fn unpack_cookie(cookie: u64) -> (SessionId, RequestId) {
    ((cookie >> 32) as SessionId, cookie & 0xFFFF_FFFF)
}
