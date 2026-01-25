//! Timeserver IPC protocol definitions.
//!
//! The timeserver returns seconds/nanoseconds for realtime and monotonic clocks.

/// Get wall-clock time (seconds + nanoseconds since epoch).
pub const TIME_GETTIMEOFDAY: u32 = 0x510;
/// Get monotonic time (seconds + nanoseconds since boot).
pub const TIME_GETCLOCK: u32 = 0x511;
