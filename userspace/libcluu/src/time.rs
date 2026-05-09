//! Timeserver IPC protocol definitions.
//!
//! The timeserver returns seconds/nanoseconds for realtime and monotonic clocks.

use crate::types::Message;
use crate::{ipc, registry, Error, IpcFlags};

/// Get wall-clock time (seconds + nanoseconds since epoch).
pub const TIME_GETTIMEOFDAY: u32 = 0x510;
/// Get monotonic time (seconds + nanoseconds since boot).
pub const TIME_GETCLOCK: u32 = 0x511;

/// Query the timeserver for `(seconds, nanoseconds)` of the given clock.
///
/// `label` should be one of `TIME_GETTIMEOFDAY` or `TIME_GETCLOCK`.
/// Returns `Error::NotFound` if the timeserver isn't registered yet.
pub fn query(label: u32) -> Result<(u64, u64), Error> {
    let endpoint = registry::lookup_service("timeserver:main").ok_or(Error::NotFound)?;
    let mut msg = Message::new(label, [0; 6], 1);
    ipc::call(endpoint, &mut msg, IpcFlags::empty())?;
    let status = msg.words[0] as isize;
    if status < 0 {
        return Err(Error::from_errno(status));
    }
    Ok((msg.words[1] as u64, msg.words[2] as u64))
}

