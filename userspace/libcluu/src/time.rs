//! Timeserver IPC protocol definitions.
//!
//! The timeserver returns seconds/nanoseconds for realtime and monotonic clocks.

use crate::types::Message;
use crate::{ipc, registry, Error, IpcFlags};

/// Get wall-clock time (seconds + nanoseconds since epoch).
pub const TIME_GETTIMEOFDAY: u32 = 0x510;
/// Get monotonic time (seconds + nanoseconds since boot).
pub const TIME_GETCLOCK: u32 = 0x511;

// --- Timeserver push-mode (periodic-tick subscriptions). ---
// Subscribe to periodic ticks. Words: [period_ms: u32, notify_ep: u64].
// Reply words[0]: errno (0 ok, EINVAL if period_ms == 0 or > 60_000).
pub const TIME_SUBSCRIBE_PERIODIC_LABEL: u32 = 120;
// Unsubscribe. Timeserver matches on sender_tid. Words: [].
pub const TIME_UNSUBSCRIBE_LABEL: u32 = 121;
// Push from timeserver. Words: [tick_count_since_subscribe: u64, now_monotonic_ms: u64].
// Fire-and-forget; subscriber MUST NOT reply.
pub const TIME_TICK_LABEL: u32 = 122;

/// Query the timeserver for `(seconds, nanoseconds)` of the given clock.
///
/// `label` should be one of `TIME_GETTIMEOFDAY` or `TIME_GETCLOCK`.
/// Returns `Error::NotFound` if the timeserver isn't registered yet.
pub fn query(label: u32) -> Result<(u64, u64), Error> {
    let endpoint = registry::lookup_service("timeserver:main").ok_or(Error::NotFound)?;
    query_endpoint(endpoint, label)
}

/// Query the timeserver using a pre-resolved endpoint (avoids repeated registry lookups).
///
/// Returns `Error::NotFound` if `endpoint` is 0.
pub fn query_endpoint(endpoint: usize, label: u32) -> Result<(u64, u64), Error> {
    if endpoint == 0 {
        return Err(Error::NotFound);
    }
    let mut msg = Message::new(label, [0; 6], 1);
    ipc::call(endpoint, &mut msg, IpcFlags::empty())?;
    let status = msg.words[0] as isize;
    if status < 0 {
        return Err(Error::from_errno(status));
    }
    Ok((msg.words[1] as u64, msg.words[2] as u64))
}

