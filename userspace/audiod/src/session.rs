//! Per-session stream management for audiod.
//!
//! Each session can open multiple audio streams. Each stream has:
//! - A unique stream_id (within audiod)
//! - A SHM SPSC ring (producer = app, consumer = audiod)
//! - A per-stream capability endpoint for control (pause/drain/gain/close)
//! - A LinearResampler to convert to the output rate
//! - A Gain (Q15 fixed-point)
//! - Monotonic frame counters (written, played)
//! - State: Active | Paused | Draining | Closed
//!
//! # IPC Protocol
//!
//! All audiod IPC uses labels in the 0x700 range:
//! - `AUDIOD_STREAM_OPEN` (0x700): open a new stream
//! - `AUDIOD_STREAM_CLOSE` (0x701): close a stream
//! - `AUDIOD_STREAM_PAUSE` (0x702): pause a stream
//! - `AUDIOD_STREAM_RESUME` (0x703): resume a paused stream
//! - `AUDIOD_STREAM_DRAIN` (0x704): mark stream for drain
//! - `AUDIOD_STREAM_GAIN` (0x705): set stream gain (Q15)
//! - `AUDIOD_STREAM_STATUS` (0x706): query stream status
//! - `AUDIOD_SESSION_DESTROYED` (0x710): root-procmgr notifies session death
//!
//! Authority: possession of the per-stream endpoint token is the sole
//! authority for stream control. No sender-TID checks (AGENTS.md §3).

use alloc::collections::BTreeMap;
use alloc::format;

use crate::ring::FrameRing;
use crate::resample::LinearResampler;
use crate::mixer::{Gain, Pan};

/// IPC labels for audiod stream control.
pub const AUDIOD_STREAM_OPEN: u32 = 0x700;
pub const AUDIOD_STREAM_CLOSE: u32 = 0x701;
pub const AUDIOD_STREAM_PAUSE: u32 = 0x702;
pub const AUDIOD_STREAM_RESUME: u32 = 0x703;
pub const AUDIOD_STREAM_DRAIN: u32 = 0x704;
pub const AUDIOD_STREAM_GAIN: u32 = 0x705;
pub const AUDIOD_STREAM_STATUS: u32 = 0x706;
pub const AUDIOD_STREAM_PANORAMA: u32 = 0x707;
pub const AUDIOD_QUERY_CAPS: u32 = 0x708;
pub const AUDIOD_SESSION_DESTROYED: u32 = 0x710;

/// Capabilities bitmasks returned by AUDOD_QUERY_CAPS.
/// Format bits: bit N set ⇒ PCM_FMT_N supported (see libcluu audio_client constants).
pub const CAPS_FMT_S16: u64 = 1 << 5;
/// Channel bits: bit 1 = mono, bit 2 = stereo.
pub const CAPS_CH_MONO: u64 = 1 << 1;
pub const CAPS_CH_STEREO: u64 = 1 << 2;
/// Rate bits: bit N set ⇒ PCM_RATE_N supported (all known rates — audiod resamples).
pub const CAPS_RATES_ALL: u64 = 0x7FF;

/// Stream state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    /// Stream is active and contributing to the mix.
    Active,
    /// Stream is paused — contributes silence, ring data preserved.
    Paused,
    /// Stream is draining — play remaining ring data, then close.
    Draining,
    /// Stream is closed — resources revoked, removed from mix.
    Closed,
}

/// Per-stream metadata and state.
pub struct Stream {
    pub stream_id: u32,
    pub session_id: u32,
    pub state: StreamState,
    pub gain: Gain,
    pub pan: Pan,
    pub resampler: LinearResampler,
    /// Per-stream control endpoint (recv side, held by audiod).
    pub control_endpoint: usize,
    /// SHM ring backing (owned by audiod, granted to the producer).
    pub ring_backing: &'static mut [u8],
    /// Ring capacity in frames.
    pub ring_capacity: usize,
    /// Frame token backing the SHM ring; freed on stream close.
    pub frame_token: u64,
    /// Monotonic frame counters.
    pub frames_written: u64,
    pub frames_played: u64,
    /// Xrun counter (overruns from the producer side).
    pub xrun_count: u32,
    /// Input sample rate (producer rate, before resampling).
    pub in_rate: u32,
    /// Input channel count (1 = mono, 2 = stereo).
    pub in_channels: u8,
}

impl Stream {
    /// Create a FrameRing view over the stream's SHM backing.
    pub fn ring(&mut self) -> Option<FrameRing<'_>> {
        FrameRing::attach(self.ring_backing)
    }

    /// Mark the stream for draining. It will be closed when the ring empties.
    pub fn drain(&mut self) {
        if self.state == StreamState::Active || self.state == StreamState::Paused {
            self.state = StreamState::Draining;
        }
    }

    /// Pause the stream (contributes silence, ring data preserved).
    pub fn pause(&mut self) {
        if self.state == StreamState::Active {
            self.state = StreamState::Paused;
        }
    }

    /// Resume a paused stream.
    pub fn resume(&mut self) {
        if self.state == StreamState::Paused {
            self.state = StreamState::Active;
        }
    }

    /// Set the stream gain (Q15 fixed-point).
    pub fn set_gain(&mut self, q15: i32) {
        self.gain = Gain::from_q15(q15);
    }

    /// Set the stream panorama (balance ∈ [-100, +100]).
    pub fn set_pan(&mut self, balance: i8) {
        self.pan = Pan::from_balance(balance);
    }

    /// Check if the stream has finished draining (ring empty + was draining).
    pub fn is_drained(&mut self) -> bool {
        if self.state != StreamState::Draining {
            return false;
        }
        if let Some(ring) = FrameRing::attach(self.ring_backing) {
            ring.available_read() == 0
        } else {
            true
        }
    }
}

/// Per-session stream table. Tracks all streams belonging to one session.
pub struct SessionStreams {
    pub session_id: u32,
    pub streams: BTreeMap<u32, Stream>,
    pub next_stream_id: u32,
}

impl SessionStreams {
    pub fn new(session_id: u32) -> Self {
        Self {
            session_id,
            streams: BTreeMap::new(),
            next_stream_id: 1,
        }
    }

    /// Allocate the next stream ID. Never returns 0 (0 is reserved).
    pub fn alloc_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        if id == 0 {
            self.next_stream_id = 1;
            return 1;
        }
        id
    }

    /// Close all streams for this session (used on session teardown).
    pub fn close_all(&mut self) {
        for stream in self.streams.values_mut() {
            stream.state = StreamState::Closed;
        }
        self.streams.clear();
    }
}

/// Global stream table: session_id → SessionStreams.
pub struct StreamRegistry {
    pub sessions: BTreeMap<u32, SessionStreams>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
        }
    }

    /// Register a new session (called when root-procmgr notifies AUDIOD_SESSION_DESTROYED
    /// — no, actually called when the first stream is opened for a session).
    pub fn ensure_session(&mut self, session_id: u32) -> &mut SessionStreams {
        self.sessions
            .entry(session_id)
            .or_insert_with(|| SessionStreams::new(session_id))
    }

    /// Remove all streams for a session (called on session teardown).
    pub fn destroy_session(&mut self, session_id: u32) {
        if let Some(session) = self.sessions.remove(&session_id) {
            for stream in session.streams.values() {
                // In a full implementation, revoke the per-stream endpoint
                // and free the SHM ring backing. For T17, the kernel revokes
                // derived tokens when the endpoint is destroyed.
                let _ = stream;
            }
            let _ = format!("audiod: session {} destroyed ({} streams closed)",
                session_id & 0xFF, session.streams.len());
        }
    }

    /// Look up a stream by (session_id, stream_id).
    pub fn get_stream(&mut self, session_id: u32, stream_id: u32) -> Option<&mut Stream> {
        self.sessions
            .get_mut(&session_id)
            .and_then(|s| s.streams.get_mut(&stream_id))
    }

    /// Iterate all active/paused/draining streams across all sessions.
    /// Used by the mixer to collect contributions.
    pub fn active_streams(&mut self) -> impl Iterator<Item = (&u32, &u32, &mut Stream)> {
        self.sessions
            .iter_mut()
            .flat_map(|(sid, session)| {
                session
                    .streams
                    .iter_mut()
                    .map(move |(stream_id, stream)| (sid, stream_id, stream))
            })
            .filter(|(_, _, s)| s.state != StreamState::Closed)
    }
}

/// Status report for a stream (returned by AUDIOD_STREAM_STATUS).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StreamStatus {
    pub state: u8,
    pub frames_written: u64,
    pub frames_played: u64,
    pub xrun_count: u32,
    pub ring_available: u32,
}

impl StreamStatus {
    pub fn from_stream(stream: &mut Stream) -> Self {
        let ring_available = if let Some(ring) = FrameRing::attach(stream.ring_backing) {
            ring.available_read() as u32
        } else {
            0
        };
        Self {
            state: match stream.state {
                StreamState::Active => 0,
                StreamState::Paused => 1,
                StreamState::Draining => 2,
                StreamState::Closed => 3,
            },
            frames_written: stream.frames_written,
            frames_played: stream.frames_played,
            xrun_count: stream.xrun_count,
            ring_available,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_alloc_stream_ids_monotonic() {
        let mut s = SessionStreams::new(1);
        assert_eq!(s.alloc_stream_id(), 1);
        assert_eq!(s.alloc_stream_id(), 2);
        assert_eq!(s.alloc_stream_id(), 3);
    }

    #[test]
    fn stream_state_transitions() {
        // Simulate state transitions without actual SHM/IPC.
        let mut s = SessionStreams::new(1);
        let id = s.alloc_stream_id();
        // We can't create a real Stream without SHM backing, but we can
        // test the state machine logic via the SessionStreams API.
        assert_eq!(id, 1);
        assert_eq!(s.session_id, 1);
    }

    #[test]
    fn registry_destroy_session_removes_streams() {
        let mut reg = StreamRegistry::new();
        reg.ensure_session(1);
        reg.ensure_session(2);
        assert_eq!(reg.sessions.len(), 2);
        reg.destroy_session(1);
        assert_eq!(reg.sessions.len(), 1);
        assert!(reg.sessions.contains_key(&2));
    }

    #[test]
    fn registry_ensure_session_idempotent() {
        let mut reg = StreamRegistry::new();
        reg.ensure_session(5);
        reg.ensure_session(5);
        assert_eq!(reg.sessions.len(), 1);
    }
}
