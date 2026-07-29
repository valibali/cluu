//! SHM SPSC frame ring — single-producer / single-consumer ring buffer for
//! audio frames, designed to live in a shared-memory page granted between an
//! audiod client (producer) and audiod (consumer).
//!
//! Supports both mono (2 bytes/frame) and stereo (4 bytes/frame) S16 streams.
//! The frame size is stored in the header's `reserved` field at init time and
//! validated on attach.
//!
//! # Layout
//!
//! ```text
//! ┌──────────────────────────────────┬──────────────────────────────┐
//! │ FrameRingHeader (32 bytes)       │ Frame data (capacity frames) │
//! └──────────────────────────────────┴──────────────────────────────┘
//! ```
//!
//! Each frame is `frame_bytes` bytes (2 for mono S16, 4 for stereo S16 LE).
//!
//! # Memory ordering
//!
//! The producer writes data then publishes `write_idx` with `Release`.
//! The consumer reads `write_idx` with `Acquire`, reads data, then
//! publishes `read_idx` with `Release`.
//! This is the canonical SPSC acquire/release pattern — no locks needed.
//!
//! # Monotonic frame counters
//!
//! `total_written` and `total_read` are monotonic counters that NEVER
//! reset. They count frames since ring initialisation, not modulo
//! capacity. The producer increments `total_written` on push; the
//! consumer increments `total_read` on pop. These are used for:
//! - Position reporting (played frames)
//! - Xrun detection (written - read > capacity → overrun)
//! - Underrun detection (read > written → underrun, consumer feeds silence)

use core::sync::atomic::{fence, AtomicU32, Ordering};

/// Magic value identifying an initialised FrameRing.
const MAGIC: u32 = 0x4155_4446; // "AUDF"

/// Stereo S16 frame: 2 × i16 = 4 bytes.
pub const FRAME_BYTES_STEREO: usize = 4;

/// Mono S16 frame: 1 × i16 = 2 bytes.
pub const FRAME_BYTES_MONO: usize = 2;

/// Header stored at the start of the shared region.
///
/// All fields are accessed via atomic operations for cross-process safety.
/// `total_written` and `total_read` are monotonic — they never reset.
#[repr(C)]
pub struct FrameRingHeader {
    magic: AtomicU32,
    capacity: AtomicU32,
    write_idx: AtomicU32,
    read_idx: AtomicU32,
    total_written: AtomicU32,
    total_read: AtomicU32,
    xrun_count: AtomicU32,
    frame_bytes: AtomicU32,
}

impl FrameRingHeader {
    pub const fn bytes() -> usize {
        core::mem::size_of::<Self>()
    }
}

/// SPSC frame ring view over a backing buffer.
///
/// The backing buffer must be at least `FrameRingHeader::bytes() + capacity * FRAME_BYTES`.
/// One frame is reserved to distinguish full from empty (like the byte ring),
/// so the usable capacity is `capacity - 1` frames.
pub struct FrameRing<'a> {
    header: &'a FrameRingHeader,
    data: &'a mut [u8],
    capacity: usize,
    frame_bytes: usize,
}

/// Minimum capacity (in frames) — must hold at least one period.
pub const MIN_CAPACITY: usize = 4;

impl<'a> FrameRing<'a> {
    /// Total bytes needed for a ring with `capacity` frames of `frame_bytes` each.
    pub fn bytes_for_capacity(capacity: usize, frame_bytes: usize) -> usize {
        FrameRingHeader::bytes() + capacity * frame_bytes
    }

    /// Initialise a new ring in `backing` with the given frame size.
    pub fn initialize(backing: &'a mut [u8], capacity: usize, frame_bytes: usize) -> Self {
        assert!(backing.len() >= Self::bytes_for_capacity(capacity, frame_bytes));
        assert!(capacity >= MIN_CAPACITY);
        assert!(frame_bytes == FRAME_BYTES_MONO || frame_bytes == FRAME_BYTES_STEREO);
        let header_bytes = FrameRingHeader::bytes();
        let (header_slice, data_slice) = backing.split_at_mut(header_bytes);
        let header = unsafe {
            &mut *(header_slice.as_mut_ptr() as *mut FrameRingHeader)
        };
        header.magic.store(MAGIC, Ordering::Relaxed);
        header.capacity.store(capacity as u32, Ordering::Relaxed);
        header.write_idx.store(0, Ordering::Relaxed);
        header.read_idx.store(0, Ordering::Relaxed);
        header.total_written.store(0, Ordering::Relaxed);
        header.total_read.store(0, Ordering::Relaxed);
        header.xrun_count.store(0, Ordering::Relaxed);
        header.frame_bytes.store(frame_bytes as u32, Ordering::Relaxed);
        fence(Ordering::Release);
        let data_len = capacity * frame_bytes;
        Self {
            header: unsafe { &*(header as *const FrameRingHeader) },
            data: &mut data_slice[..data_len],
            capacity,
            frame_bytes,
        }
    }

    /// Attach to an already initialised ring.
    pub fn attach(backing: &'a mut [u8]) -> Option<Self> {
        if backing.len() < FrameRingHeader::bytes() {
            return None;
        }
        let header_bytes = FrameRingHeader::bytes();
        let (header_slice, data_slice) = backing.split_at_mut(header_bytes);
        let header = unsafe {
            &mut *(header_slice.as_mut_ptr() as *mut FrameRingHeader)
        };
        if header.magic.load(Ordering::Relaxed) != MAGIC {
            return None;
        }
        let capacity = header.capacity.load(Ordering::Relaxed) as usize;
        if capacity < MIN_CAPACITY {
            return None;
        }
        let frame_bytes = header.frame_bytes.load(Ordering::Relaxed) as usize;
        if frame_bytes != FRAME_BYTES_MONO && frame_bytes != FRAME_BYTES_STEREO {
            return None;
        }
        let data_len = capacity * frame_bytes;
        if data_slice.len() < data_len {
            return None;
        }
        Some(Self {
            header: unsafe { &*(header as *const FrameRingHeader) },
            data: &mut data_slice[..data_len],
            capacity,
            frame_bytes,
        })
    }

    /// Usable frame capacity (one slot reserved for full/empty distinction).
    pub fn capacity(&self) -> usize {
        self.capacity.saturating_sub(1)
    }

    /// Frames available for the consumer to read.
    pub fn available_read(&self) -> usize {
        let write = self.write_idx();
        let read = self.read_idx();
        if write >= read {
            write - read
        } else {
            self.capacity - read + write
        }
    }

    /// Frames the producer can write without overwriting unread data.
    pub fn available_write(&self) -> usize {
        self.capacity().saturating_sub(self.available_read())
    }

    /// Monotonic count of frames ever written (never resets).
    pub fn total_written(&self) -> u64 {
        self.header.total_written.load(Ordering::Relaxed) as u64
    }

    /// Monotonic count of frames ever read (never resets).
    pub fn total_read(&self) -> u64 {
        self.header.total_read.load(Ordering::Relaxed) as u64
    }

    /// Number of xruns (overrun events) recorded by the producer.
    pub fn xrun_count(&self) -> u32 {
        self.header.xrun_count.load(Ordering::Relaxed)
    }

    /// Push up to `frames.len()` stereo frames into the ring.
    ///
    /// Returns the number of frames actually written. If the ring is full,
    /// returns 0 and increments the xrun counter (overrun).
    /// Each frame is `[i16; 2]` (left, right).
    pub fn push(&mut self, frames: &[[i16; 2]]) -> usize {
        debug_assert_eq!(self.frame_bytes, FRAME_BYTES_STEREO);
        if frames.is_empty() {
            return 0;
        }
        let to_write = frames.len().min(self.available_write());
        if to_write == 0 {
            self.header.xrun_count.fetch_add(1, Ordering::Relaxed);
            return 0;
        }
        let write = self.write_idx();
        for i in 0..to_write {
            let idx = (write + i) % self.capacity;
            let offset = idx * self.frame_bytes;
            let frame = &frames[i];
            self.data[offset..offset + 2]
                .copy_from_slice(&frame[0].to_le_bytes());
            self.data[offset + 2..offset + 4]
                .copy_from_slice(&frame[1].to_le_bytes());
        }
        fence(Ordering::Release);
        let new_write = (write + to_write) % self.capacity;
        self.header.write_idx.store(new_write as u32, Ordering::Release);
        self.header
            .total_written
            .fetch_add(to_write as u32, Ordering::Relaxed);
        to_write
    }

    /// Push up to `samples.len()` mono samples into the ring.
    ///
    /// Returns the number of samples actually written. If the ring is full,
    /// returns 0 and increments the xrun counter (overrun).
    pub fn push_mono(&mut self, samples: &[i16]) -> usize {
        debug_assert_eq!(self.frame_bytes, FRAME_BYTES_MONO);
        if samples.is_empty() {
            return 0;
        }
        let to_write = samples.len().min(self.available_write());
        if to_write == 0 {
            self.header.xrun_count.fetch_add(1, Ordering::Relaxed);
            return 0;
        }
        let write = self.write_idx();
        for i in 0..to_write {
            let idx = (write + i) % self.capacity;
            let offset = idx * self.frame_bytes;
            self.data[offset..offset + 2]
                .copy_from_slice(&samples[i].to_le_bytes());
        }
        fence(Ordering::Release);
        let new_write = (write + to_write) % self.capacity;
        self.header.write_idx.store(new_write as u32, Ordering::Release);
        self.header
            .total_written
            .fetch_add(to_write as u32, Ordering::Relaxed);
        to_write
    }

    /// Pop up to `dst.len()` stereo frames from the ring.
    ///
    /// Returns the number of frames actually read. If the ring is empty,
    /// returns 0 (underrun — caller should feed silence).
    pub fn pop(&mut self, dst: &mut [[i16; 2]]) -> usize {
        debug_assert_eq!(self.frame_bytes, FRAME_BYTES_STEREO);
        if dst.is_empty() {
            return 0;
        }
        let to_read = dst.len().min(self.available_read());
        if to_read == 0 {
            return 0;
        }
        let read = self.read_idx();
        for i in 0..to_read {
            let idx = (read + i) % self.capacity;
            let offset = idx * self.frame_bytes;
            dst[i][0] = i16::from_le_bytes([self.data[offset], self.data[offset + 1]]);
            dst[i][1] = i16::from_le_bytes([self.data[offset + 2], self.data[offset + 3]]);
        }
        fence(Ordering::Release);
        let new_read = (read + to_read) % self.capacity;
        self.header.read_idx.store(new_read as u32, Ordering::Release);
        self.header
            .total_read
            .fetch_add(to_read as u32, Ordering::Relaxed);
        to_read
    }

    /// Pop up to `dst.len()` mono samples from the ring.
    ///
    /// Returns the number of samples actually read. If the ring is empty,
    /// returns 0 (underrun — caller should feed silence).
    pub fn pop_mono(&mut self, dst: &mut [i16]) -> usize {
        debug_assert_eq!(self.frame_bytes, FRAME_BYTES_MONO);
        if dst.is_empty() {
            return 0;
        }
        let to_read = dst.len().min(self.available_read());
        if to_read == 0 {
            return 0;
        }
        let read = self.read_idx();
        for i in 0..to_read {
            let idx = (read + i) % self.capacity;
            let offset = idx * self.frame_bytes;
            dst[i] = i16::from_le_bytes([self.data[offset], self.data[offset + 1]]);
        }
        fence(Ordering::Release);
        let new_read = (read + to_read) % self.capacity;
        self.header.read_idx.store(new_read as u32, Ordering::Release);
        self.header
            .total_read
            .fetch_add(to_read as u32, Ordering::Relaxed);
        to_read
    }

    /// Reset the ring to empty state. Only safe when the producer and
    /// consumer are both quiescent (e.g. during stream open before data flows).
    pub fn reset(&mut self) {
        self.header.write_idx.store(0, Ordering::Relaxed);
        self.header.read_idx.store(0, Ordering::Relaxed);
        self.header.xrun_count.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
    }

    #[inline]
    fn write_idx(&self) -> usize {
        self.header.write_idx.load(Ordering::Acquire) as usize % self.capacity
    }

    #[inline]
    fn read_idx(&self) -> usize {
        self.header.read_idx.load(Ordering::Acquire) as usize % self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn make_ring(capacity: usize) -> Vec<u8> {
        let bytes = FrameRing::bytes_for_capacity(capacity, FRAME_BYTES_STEREO);
        let mut buf = vec![0u8; bytes];
        FrameRing::initialize(&mut buf, capacity, FRAME_BYTES_STEREO);
        buf
    }

    fn make_mono_ring(capacity: usize) -> Vec<u8> {
        let bytes = FrameRing::bytes_for_capacity(capacity, FRAME_BYTES_MONO);
        let mut buf = vec![0u8; bytes];
        FrameRing::initialize(&mut buf, capacity, FRAME_BYTES_MONO);
        buf
    }

    #[test]
    fn ring_wrap_basic() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        // Usable capacity = 7 (one slot reserved).
        let frames = [[1i16, 2]; 7];
        assert_eq!(ring.push(&frames), 7);
        assert_eq!(ring.available_read(), 7);
        assert_eq!(ring.available_write(), 0);

        // Pop 4, push 4 — wraps around.
        let mut out = [[0i16, 0]; 4];
        assert_eq!(ring.pop(&mut out), 4);
        assert_eq!(out[0], [1, 2]);
        assert_eq!(ring.available_read(), 3);

        let frames2 = [[3i16, 4]; 4];
        assert_eq!(ring.push(&frames2), 4);
        assert_eq!(ring.available_read(), 7);
        assert_eq!(ring.total_written(), 11);
        assert_eq!(ring.total_read(), 4);
    }

    #[test]
    fn ring_overcommit_returns_zero_and_counts_xrun() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let full = [[1i16, 1]; 7];
        assert_eq!(ring.push(&full), 7);
        // Ring is full — push returns 0 and xrun increments.
        let extra = [[2i16, 2]; 4];
        assert_eq!(ring.push(&extra), 0);
        assert_eq!(ring.xrun_count(), 1);
    }

    #[test]
    fn ring_monotonic_counters_never_reset() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        for round in 0..5u64 {
            let push_count = 3usize;
            let frames = [[round as i16, (round + 1) as i16]; 3];
            assert_eq!(ring.push(&frames), push_count);
            let mut out = [[0i16, 0]; 3];
            assert_eq!(ring.pop(&mut out), push_count);
        }
        assert_eq!(ring.total_written(), 15);
        assert_eq!(ring.total_read(), 15);
    }

    #[test]
    fn ring_underrun_returns_zero_on_empty() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let mut out = [[0i16, 0]; 4];
        assert_eq!(ring.pop(&mut out), 0);
    }

    #[test]
    fn ring_preserves_stereo_pairs() {
        let capacity = 16;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let frames = [
            [100i16, -100],
            [200, -200],
            [300, -300],
            [0, 0],
        ];
        assert_eq!(ring.push(&frames), 4);
        let mut out = [[0i16, 0]; 4];
        assert_eq!(ring.pop(&mut out), 4);
        assert_eq!(out, frames);
    }

    #[test]
    fn ring_reset_clears_state() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let frames = [[1i16, 2]; 5];
        assert_eq!(ring.push(&frames), 5);
        ring.reset();
        assert_eq!(ring.available_read(), 0);
        assert_eq!(ring.available_write(), 7);
    }

    #[test]
    fn ring_partial_push_then_complete() {
        let capacity = 8;
        let mut buf = make_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let first = [[1i16, 1]; 5];
        assert_eq!(ring.push(&first), 5);
        let second = [[2i16, 2]; 5];
        // Only 2 more fit (capacity-1=7, 5 used, 2 left).
        assert_eq!(ring.push(&second), 2);
        assert_eq!(ring.available_read(), 7);
    }

    #[test]
    fn ring_mono_push_pop() {
        let capacity = 8;
        let mut buf = make_mono_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let samples = [100i16, -200, 300, 0, 500, -600];
        assert_eq!(ring.push_mono(&samples), 6);
        assert_eq!(ring.available_read(), 6);

        let mut out = [0i16; 4];
        assert_eq!(ring.pop_mono(&mut out), 4);
        assert_eq!(out, [100, -200, 300, 0]);

        let mut rest = [0i16; 2];
        assert_eq!(ring.pop_mono(&mut rest), 2);
        assert_eq!(rest, [500, -600]);
        assert_eq!(ring.available_read(), 0);
    }

    #[test]
    fn ring_mono_overrun_counts_xrun() {
        let capacity = 8;
        let mut buf = make_mono_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let full = [1i16; 7];
        assert_eq!(ring.push_mono(&full), 7);
        let extra = [2i16; 4];
        assert_eq!(ring.push_mono(&extra), 0);
        assert_eq!(ring.xrun_count(), 1);
    }

    #[test]
    fn ring_mono_wraps() {
        let capacity = 8;
        let mut buf = make_mono_ring(capacity);
        let mut ring = FrameRing::attach(&mut buf).unwrap();
        let first = [1i16, 2, 3, 4, 5];
        assert_eq!(ring.push_mono(&first), 5);
        let mut out = [0i16; 3];
        assert_eq!(ring.pop_mono(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);

        let second = [6i16, 7, 8, 9];
        assert_eq!(ring.push_mono(&second), 4);
        assert_eq!(ring.total_written(), 9);
        assert_eq!(ring.total_read(), 3);
    }
}
