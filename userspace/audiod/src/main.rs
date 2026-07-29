#![no_std]
#![no_main]

//! audiod — CLUU audio daemon.
//!
//! Sole holder of the private virtio-snd endpoint. Mixes N streams from
//! multiple sessions and outputs stereo S16 at 44100 Hz to virtio-snd.
//!
//! # Boot path
//!
//! audiod is started by root-procmgr via `/etc/system.toml` (same pattern
//! as displayd). It subscribes to "snddev:main" (virtio-snd) as the sole
//! client, then registers "audiod:main" so session binaries can resolve
//! it via `PARAM_AUDIOD_EP` (installed by root-procmgr at session spawn).
//!
//! # Session authority
//!
//! root-procmgr is the sole holder of the global audiod control endpoint
//! (AGENTS.md §6 root-godmode). Per-session stream-creation endpoints are
//! installed via `PARAM_AUDIOD_EP` at session spawn. Per-stream endpoints
//! are returned on `stream_open` and are the sole authority for stream
//! control. No runtime sender-TID ACL (AGENTS.md §3).
//!
//! # Audio path
//!
//! Producer → SHM SPSC ring → audiod reads → resample to 44100 stereo S16
//! → i32 mix with single saturation → submit to virtio-snd.

extern crate alloc;

use audiod::mixer::{saturate_i16, Gain, MAX_PERIOD_FRAMES};
use audiod::ring::{FrameRing, FRAME_BYTES_STEREO};
use audiod::session::{
    Stream, StreamRegistry, StreamState, StreamStatus, AUDIOD_SESSION_DESTROYED,
    AUDIOD_STREAM_CLOSE, AUDIOD_STREAM_DRAIN, AUDIOD_STREAM_GAIN, AUDIOD_STREAM_OPEN,
    AUDIOD_STREAM_PANORAMA, AUDIOD_STREAM_PAUSE, AUDIOD_STREAM_RESUME, AUDIOD_STREAM_STATUS,
    AUDIOD_QUERY_CAPS, CAPS_FMT_S16, CAPS_CH_MONO, CAPS_CH_STEREO, CAPS_RATES_ALL,
};

use alloc::format;
use alloc::vec::Vec;

use libcluu::audio_client::{query_driver_caps, AudioSessionClient, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::{extract_reply_id, reply};
use libcluu::registry;
use libcluu::syscall::{
    endpoint_create, ipc_recv_any_with_sender, space_grant, space_map_auto, space_map_range, space_unmap,
    InvokeOp, MAP_FRAME_TOKEN,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

const OUTPUT_RATE_PREFERRED: u32 = 44100;
const OUTPUT_RATE_FALLBACK: u32 = 48000;
const OUTPUT_CHANNELS: u8 = 2;

pub const PERIOD_BYTES: usize = 2048;

pub const BUFFER_BYTES: usize = PERIOD_BYTES * 4;

const RING_CAPACITY_FRAMES: usize = 6144;

const RING_SLOTS: usize = 8;
const SCRATCH_VA: usize = 0x7000_0000;

const SHM_VA_BASE: usize = 0x7100_0000;
const MAX_STREAMS: usize = 16;
const PAGE_SIZE: usize = 4096;
const FLAGS_USER_RW: usize = 0x07;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("audiod: error {:?}", e));
            -1
        }
    }
}

struct Audiod {
    snd: AudioSessionClient,
    streams: StreamRegistry,
    listen_ep: usize,
    space_token: usize,
    mix_buf: [[i16; 2]; MAX_PERIOD_FRAMES],
    stream_buf: [[i16; 2]; MAX_PERIOD_FRAMES],
    mono_stream_buf: [i16; MAX_PERIOD_FRAMES],
    resample_out: [[i16; 2]; MAX_PERIOD_FRAMES],
    next_period_id: u64,
    page_index: usize,
    ring_slot_bitmap: u64,
    ring_pages_per_stream: usize,
    ring_total_bytes: usize,
    inflight_slots: u64,
    output_rate: u32,
    period_bytes: usize,
    period_frames: usize,
    slot_stride: usize,
    completion_scratch: alloc::vec::Vec<(libcluu::audio_client::PcmHandle, libcluu::Result<()>)>,
}

fn ring_bytes_for_stream() -> usize {
    let raw = FrameRing::bytes_for_capacity(RING_CAPACITY_FRAMES, FRAME_BYTES_STEREO);
    (raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn alloc_frame(space_token: usize, bytes: usize) -> Result<(u64, usize)> {
    let rounded = (bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let token = unsafe { libcluu::syscall::invoke(space_token, InvokeOp::FrameAllocate, rounded, 0, 0, 0)? };
    Ok((token as u64, rounded))
}

fn free_frame(token: u64) -> Result<()> {
    if token == 0 {
        return Ok(());
    }
    unsafe { libcluu::syscall::invoke(token as usize, InvokeOp::FrameFree, 0, 0, 0, 0)? };
    Ok(())
}

fn map_frame_rw(space_token: usize, va: usize, token: u64, size: usize) -> Result<()> {
    let num_pages = size / PAGE_SIZE;
    space_map_range(
        space_token,
        va,
        token as usize,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    )?;
    Ok(())
}

fn run() -> Result<()> {
    debug_print("audiod: starting")?;

    let info = process_info();
    let ipc_token = info.tokens[TOKEN_IPC];
    let space_token = info.tokens[TOKEN_SPACE];

    registry::init("audiod")?;

    // ── Connect to virtio-snd as sole client ─────────────────────────────
    let snd_ep = registry::subscribe_output("snddev", "main")?;
    debug_print("audiod: subscribed to snddev:main")?;

    let caps = query_driver_caps(snd_ep)?;
    let output_rate = if caps.supports_rate(OUTPUT_RATE_PREFERRED) {
        OUTPUT_RATE_PREFERRED
    } else if caps.supports_rate(OUTPUT_RATE_FALLBACK) {
        OUTPUT_RATE_FALLBACK
    } else {
        return Err(Error::InvalidState);
    };
    debug_print(&format!("audiod: virtio-snd caps fmts={:#x} rates={:#x} ch={:#x} → picked {}Hz",
        caps.formats, caps.rates, caps.channels, output_rate))?;

    let params = PcmParams {
        format: PCM_FMT_S16,
        rate: libcluu::audio_client::hz_to_rate(output_rate),
        channels: OUTPUT_CHANNELS,
        period_bytes: PERIOD_BYTES as u32,
    };
    let snd = AudioSessionClient::open(snd_ep, params)?;
    let period_bytes = snd.period_bytes;
    let period_frames = period_bytes / FRAME_BYTES_STEREO;
    let slot_stride = (period_bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let scratch_pages = RING_SLOTS * (slot_stride / PAGE_SIZE);
    debug_print(&format!("audiod: virtio-snd session open period_bytes={}", period_bytes))?;

    space_map_range(space_token, SCRATCH_VA, 0, 0x03, scratch_pages, 0)?;
    for i in 0..RING_SLOTS {
        space_grant(
            space_token,
            snd.driver_space_token,
            SCRATCH_VA + i * slot_stride,
            snd.grant_target_va + i * slot_stride,
            0,
        )?;
    }
    debug_print("audiod: PCM ring granted to virtio-snd")?;

    let listen_ep = endpoint_create(ipc_token)?;
    registry::register_output("main", listen_ep)?;
    debug_print("audiod: registered as audiod:main")?;

    let ring_total_bytes = ring_bytes_for_stream();
    let ring_pages_per_stream = ring_total_bytes / PAGE_SIZE;

    let completion_ep = snd.completion_endpoint();
    let mut audiod = Audiod {
        snd,
        streams: StreamRegistry::new(),
        listen_ep,
        space_token,
        mix_buf: [[0i16; 2]; MAX_PERIOD_FRAMES],
        stream_buf: [[0i16; 2]; MAX_PERIOD_FRAMES],
        mono_stream_buf: [0i16; MAX_PERIOD_FRAMES],
        resample_out: [[0i16; 2]; MAX_PERIOD_FRAMES],
        next_period_id: 1,
        page_index: 0,
        ring_slot_bitmap: 0,
        ring_pages_per_stream,
        ring_total_bytes,
        inflight_slots: 0,
        output_rate,
        period_bytes,
        period_frames,
        slot_stride,
        completion_scratch: alloc::vec::Vec::new(),
    };

    debug_print("AUDIOD_READY")?;

    // ── Main loop ────────────────────────────────────────────────────────
    //
    // # Periodic wakeup (AGENTS.md §7)
    //
    // The 10 ms recv timeout is NOT polling — it is the audio period.
    // audiod is a mixer service that MUST wake periodically to mix active
    // streams and submit PCM periods to virtio-snd. Audio has real-time
    // requirements: periodic wakeup is the standard ALSA/JACK/PulseAudio
    // pattern. The timeout drives `mix_and_submit` on each wake.
    //
    // This is NOT a deadlock risk because audiod has no downstream IPC
    // dependencies — it talks only to virtio-snd (a leaf driver with no
    // downstream IPC) via the `AudioSessionClient`. There is no
    // mutual-blocking IPC cycle.
    let registry_ep = registry::control_endpoint();
    let mut buf = [0u8; 256];
    loop {
        let tokens = [audiod.listen_ep, registry_ep, completion_ep];
        let (idx, len, _sender_tid) = match ipc_recv_any_with_sender(&tokens, &mut buf, 100) {
            Ok(t) => t,
            Err(_) => {
                audiod.tick();
                continue;
            }
        };

        if idx == 2 {
            if len >= core::mem::size_of::<Message>() {
                let msg = unsafe { &*(buf.as_ptr() as *const Message) };
                audiod.process_completion(msg);
            }
            audiod.tick();
            continue;
        }

        if idx == 1 {
            if len >= core::mem::size_of::<Message>() {
                let msg = unsafe { &*(buf.as_ptr() as *const Message) };
                let payload = &buf[core::mem::size_of::<Message>()..len];
                let _ = registry::handle_incoming_message(msg, payload);
            }
            audiod.tick();
            continue;
        }

        if len < core::mem::size_of::<Message>() {
            continue;
        }
        let msg = unsafe { &*(buf.as_ptr() as *const Message) };
        audiod.handle_message(msg);
        audiod.tick();
    }
}

impl Audiod {
    fn alloc_slot(&mut self) -> Option<usize> {
        for i in 0..MAX_STREAMS {
            if (self.ring_slot_bitmap & (1u64 << i)) == 0 {
                self.ring_slot_bitmap |= 1u64 << i;
                return Some(i);
            }
        }
        None
    }

    fn free_slot(&mut self, slot_idx: usize) {
        if slot_idx < MAX_STREAMS {
            self.ring_slot_bitmap &= !(1u64 << slot_idx);
        }
    }

    fn handle_message(&mut self, msg: &Message) {
        let reply_token = extract_reply_id(msg);
        match msg.tag.label {
            AUDIOD_STREAM_OPEN => self.handle_stream_open(msg, reply_token),
            AUDIOD_STREAM_CLOSE => self.handle_stream_close(msg, reply_token),
            AUDIOD_STREAM_PAUSE => self.handle_stream_pause(msg, reply_token),
            AUDIOD_STREAM_RESUME => self.handle_stream_resume(msg, reply_token),
            AUDIOD_STREAM_DRAIN => self.handle_stream_drain(msg, reply_token),
            AUDIOD_STREAM_GAIN => self.handle_stream_gain(msg, reply_token),
            AUDIOD_STREAM_PANORAMA => self.handle_stream_panorama(msg, reply_token),
            AUDIOD_STREAM_STATUS => self.handle_stream_status(msg, reply_token),
            AUDIOD_QUERY_CAPS => Self::handle_query_caps(reply_token),
            AUDIOD_SESSION_DESTROYED => self.handle_session_destroyed(msg, reply_token),
            _ => {
            }
        }
    }

    fn handle_query_caps(reply_token: Option<usize>) {
        let rmsg = Message::new(
            AUDIOD_QUERY_CAPS,
            [0, CAPS_FMT_S16 as usize, CAPS_RATES_ALL as usize, (CAPS_CH_MONO | CAPS_CH_STEREO) as usize, 0, 0],
            5,
        );
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_open(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let in_rate = msg.words[1] as u32;
        let in_channels = msg.words[2] as u8;
        let _requested_period_bytes = msg.words[3];
        let format = msg.words[4] as u8;

        let fmt_bit = 1u64 << format;
        if (CAPS_FMT_S16 & fmt_bit) == 0 {
            let rmsg = Message::new(AUDIOD_STREAM_OPEN, [4, 0, 0, 0, 0, 0], 1);
            if let Some(rt) = reply_token {
                let _ = reply(rt, &rmsg, IpcFlags::empty());
            }
            return;
        }
        let ch_bit = match in_channels {
            1 => CAPS_CH_MONO,
            2 => CAPS_CH_STEREO,
            _ => 0,
        };
        if ch_bit == 0 {
            let rmsg = Message::new(AUDIOD_STREAM_OPEN, [6, 0, 0, 0, 0, 0], 1);
            if let Some(rt) = reply_token {
                let _ = reply(rt, &rmsg, IpcFlags::empty());
            }
            return;
        }

        let slot_idx = match self.alloc_slot() {
            Some(s) => s,
            None => {
                let rmsg = Message::new(
                    AUDIOD_STREAM_OPEN,
                    [1, 0, 0, 0, 0, 0],
                    1,
                );
                if let Some(rt) = reply_token {
                    let _ = reply(rt, &rmsg, IpcFlags::empty());
                }
                return;
            }
        };

        let (frame_token, ring_bytes) = match alloc_frame(self.space_token, self.ring_total_bytes) {
            Ok(t) => t,
            Err(e) => {
                self.free_slot(slot_idx);
                let _ = debug_print(&format!("audiod: FrameAllocate failed: {:?}\n", e));
                let rmsg = Message::new(AUDIOD_STREAM_OPEN, [2, 0, 0, 0, 0, 0], 1);
                if let Some(rt) = reply_token {
                    let _ = reply(rt, &rmsg, IpcFlags::empty());
                }
                return;
            }
        };

        let ring_pages = ring_bytes / PAGE_SIZE;
        let va = match space_map_auto(self.space_token, frame_token as usize, 0x03, ring_pages) {
            Ok(va) => va,
            Err(e) => {
                let _ = free_frame(frame_token);
                self.free_slot(slot_idx);
                let _ = debug_print(&format!("audiod: space_map_auto failed: {:?}\n", e));
                let rmsg = Message::new(AUDIOD_STREAM_OPEN, [3, 0, 0, 0, 0, 0], 1);
                if let Some(rt) = reply_token {
                    let _ = reply(rt, &rmsg, IpcFlags::empty());
                }
                return;
            }
        };

        let backing = unsafe {
            core::slice::from_raw_parts_mut(va as *mut u8, self.ring_total_bytes)
        };
        let frame_bytes = (in_channels as usize) * 2;
        FrameRing::initialize(backing, RING_CAPACITY_FRAMES, frame_bytes);

        let session = self.streams.ensure_session(session_id);
        let stream_id = session.alloc_stream_id();
        let stream = Stream {
            stream_id,
            session_id,
            state: StreamState::Active,
            gain: Gain::UNITY,
            pan: audiod::mixer::Pan::CENTER,
            resampler: audiod::resample::LinearResampler::new(in_rate, self.output_rate, in_channels),
            control_endpoint: 0,
            ring_backing: unsafe {
                core::slice::from_raw_parts_mut(va as *mut u8, self.ring_total_bytes)
            },
            ring_capacity: RING_CAPACITY_FRAMES,
            frame_token,
            frames_written: 0,
            frames_played: 0,
            xrun_count: 0,
            in_rate,
            in_channels,
        };
        session.streams.insert(stream_id, stream);

        let rmsg = Message::new(
            AUDIOD_STREAM_OPEN,
            [0, stream_id as usize, session_id as usize, frame_token as usize, ring_bytes, self.period_bytes],
            6,
        );
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
        let _ = debug_print(&format!(
            "audiod: stream_open sid={} stream_id={} rate={} ch={} slot={} va={:#x} period_bytes={}\n",
            session_id & 0xFF, stream_id, in_rate, in_channels, slot_idx, va, self.period_bytes
        ));

        self.bootstrap();
    }

    fn handle_stream_close(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        if let Some(session) = self.streams.sessions.get_mut(&session_id) {
            if let Some(stream) = session.streams.remove(&stream_id) {
                let va = stream.ring_backing.as_ptr() as usize;
                let _ = space_unmap(self.space_token, va, self.ring_pages_per_stream);
                let _ = free_frame(stream.frame_token);
                let slot_idx = (va - SHM_VA_BASE) / (self.ring_pages_per_stream * PAGE_SIZE);
                self.free_slot(slot_idx);
            }
        }
        let rmsg = Message::new(AUDIOD_STREAM_CLOSE, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_pause(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        if let Some(s) = self.streams.get_stream(session_id, stream_id) {
            s.pause();
        }
        let rmsg = Message::new(AUDIOD_STREAM_PAUSE, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_resume(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        if let Some(s) = self.streams.get_stream(session_id, stream_id) {
            s.resume();
        }
        let rmsg = Message::new(AUDIOD_STREAM_RESUME, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_drain(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        if let Some(s) = self.streams.get_stream(session_id, stream_id) {
            s.drain();
        }
        let rmsg = Message::new(AUDIOD_STREAM_DRAIN, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_gain(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        let q15 = msg.words[2] as i32;
        if let Some(s) = self.streams.get_stream(session_id, stream_id) {
            s.set_gain(q15);
        }
        let rmsg = Message::new(AUDIOD_STREAM_GAIN, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_panorama(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        let balance = msg.words[2] as i8;
        if let Some(s) = self.streams.get_stream(session_id, stream_id) {
            s.set_pan(balance);
        }
        let rmsg = Message::new(AUDIOD_STREAM_PANORAMA, [0, 0, 0, 0, 0, 0], 1);
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_stream_status(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        let (state, written, played, xruns, avail) =
            if let Some(s) = self.streams.get_stream(session_id, stream_id) {
                let status = StreamStatus::from_stream(s);
                (status.state, status.frames_written, status.frames_played,
                 status.xrun_count, status.ring_available)
            } else {
                (0xFFu8, 0u64, 0u64, 0u32, 0u32)
            };
        let rmsg = Message::new(
            AUDIOD_STREAM_STATUS,
            [state as usize, written as usize, played as usize, xruns as usize, avail as usize, 0],
            5,
        );
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
    }

    fn handle_session_destroyed(&mut self, msg: &Message, _reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        if let Some(session) = self.streams.sessions.remove(&session_id) {
            for stream in session.streams.values() {
                let va = stream.ring_backing.as_ptr() as usize;
                let _ = space_unmap(self.space_token, va, self.ring_pages_per_stream);
                let _ = free_frame(stream.frame_token);
                let slot_idx = (va - SHM_VA_BASE) / (self.ring_pages_per_stream * PAGE_SIZE);
                self.free_slot(slot_idx);
            }
            let _ = debug_print(&format!(
                "audiod: session {} destroyed — {} streams killed\n",
                session_id & 0xFF, session.streams.len()
            ));
        }
    }

    fn process_completion(&mut self, msg: &Message) {
        use libcluu::ipc::AUDIO_COMPLETE;
        if msg.tag.label == AUDIO_COMPLETE {
            self.inflight_slots = self.inflight_slots.saturating_sub(1);
        }
    }

    fn drain_completions(&mut self) -> usize {
        self.snd.drain_completions_into(&mut self.completion_scratch);
        let n = self.completion_scratch.len();
        for (_handle, _result) in self.completion_scratch.drain(..) {
            self.inflight_slots = self.inflight_slots.saturating_sub(1);
        }
        n
    }

    fn tick(&mut self) {
        let _freed = self.drain_completions();
        let target = self.target_inflight();
        while self.inflight_slots < target {
            self.mix_and_submit_one();
        }
    }


    fn target_inflight(&self) -> u64 {
        const TARGET: u64 = 3;
        let has_active = self.streams.sessions.values()
            .any(|s| s.streams.values().any(|st| st.state == StreamState::Active));
        if has_active { TARGET } else { 0 }
    }

    fn bootstrap(&mut self) {
        const BOOTSTRAP_SLOTS: u64 = 3;
        while self.inflight_slots < BOOTSTRAP_SLOTS {
            self.mix_and_submit_one();
        }
    }

    fn mix_and_submit_one(&mut self) {
        let n_frames = self.period_frames;
        let mut accum_l: [i32; MAX_PERIOD_FRAMES] = [0; MAX_PERIOD_FRAMES];
        let mut accum_r: [i32; MAX_PERIOD_FRAMES] = [0; MAX_PERIOD_FRAMES];

        let stream_keys: Vec<(u32, u32)> = self
            .streams
            .sessions
            .iter()
            .flat_map(|(sid, session)| {
                session.streams.iter().map(move |(stream_id, s)| (*sid, *stream_id, s.state))
            })
            .filter(|(_, _, state)| *state != StreamState::Closed && *state != StreamState::Paused)
            .map(|(sid, stream_id, _)| (sid, stream_id))
            .collect();

        for (sid, stream_id) in stream_keys {
            let stream = match self.streams.get_stream(sid, stream_id) {
                Some(s) => s,
                None => continue,
            };
            let gain = stream.gain;
            let pan = stream.pan;
            let in_rate = stream.in_rate;

            if in_rate == self.output_rate {
                let popped = {
                    let mut ring = match FrameRing::attach(stream.ring_backing) {
                        Some(r) => r,
                        None => continue,
                    };
                    if stream.in_channels == 1 {
                        ring.pop_mono(&mut self.mono_stream_buf[..n_frames])
                    } else {
                        ring.pop(&mut self.stream_buf[..n_frames])
                    }
                };
                stream.frames_played = stream.frames_played.wrapping_add(popped as u64);
                if stream.in_channels == 1 {
                    for i in 0..popped {
                        let s = self.mono_stream_buf[i];
                        let l = pan.apply_l(gain.apply(s));
                        let r = pan.apply_r(gain.apply(s));
                        accum_l[i] = accum_l[i].saturating_add(l);
                        accum_r[i] = accum_r[i].saturating_add(r);
                    }
                } else {
                    for i in 0..popped {
                        let l = pan.apply_l(gain.apply(self.stream_buf[i][0]));
                        let r = pan.apply_r(gain.apply(self.stream_buf[i][1]));
                        accum_l[i] = accum_l[i].saturating_add(l);
                        accum_r[i] = accum_r[i].saturating_add(r);
                    }
                }
            } else {
                let ring_va = stream.ring_backing.as_ptr() as usize;
                let ring_len = stream.ring_backing.len();
                let resampler = &mut stream.resampler;
                let frames_played = &mut stream.frames_played;
                let in_rate = stream.in_rate;
                let is_mono = stream.in_channels == 1;

                let mut out_filled = 0usize;
                const CHUNK: usize = 32;
                let mut input_frames = [[0i16; 2]; CHUNK];
                let mut input_mono = [0i16; CHUNK];

                while out_filled < n_frames {
                    let want_in = ((n_frames - out_filled) as u64 * in_rate as u64
                        / self.output_rate as u64) as usize;
                    let want_in = want_in.min(CHUNK).max(1);

                    let backing = unsafe { core::slice::from_raw_parts_mut(ring_va as *mut u8, ring_len) };
                    let popped = match FrameRing::attach(backing) {
                        Some(mut r) => {
                            let avail = r.available_read().min(want_in);
                            if avail == 0 { 0 }
                            else if is_mono { r.pop_mono(&mut input_mono[..avail]) }
                            else { r.pop(&mut input_frames[..avail]) }
                        }
                        None => break,
                    };
                    if popped == 0 { break; }

                    let input_i16: &[i16] = if is_mono {
                        &input_mono[..popped]
                    } else {
                        unsafe {
                            core::slice::from_raw_parts(
                                input_frames[..popped].as_ptr() as *const i16,
                                popped * 2,
                            )
                        }
                    };
                    let out_space = &mut self.resample_out[out_filled..n_frames];
                    let produced = resampler.process(input_i16, out_space);
                    *frames_played = frames_played.wrapping_add(popped as u64);

                    for i in 0..produced {
                        let idx = out_filled + i;
                        let l = pan.apply_l(gain.apply(self.resample_out[idx][0]));
                        let r = pan.apply_r(gain.apply(self.resample_out[idx][1]));
                        accum_l[idx] = accum_l[idx].saturating_add(l);
                        accum_r[idx] = accum_r[idx].saturating_add(r);
                    }
                    out_filled += produced;
                    if produced == 0 { break; }
                }
            }
        }

        for i in 0..n_frames {
            self.mix_buf[i][0] = saturate_i16(accum_l[i]);
            self.mix_buf[i][1] = saturate_i16(accum_r[i]);
        }

        let slot_va = SCRATCH_VA + self.page_index * self.slot_stride;
        let scratch = unsafe { core::slice::from_raw_parts_mut(slot_va as *mut u8, self.period_bytes) };
        for i in 0..n_frames {
            let offset = i * FRAME_BYTES_STEREO;
            scratch[offset..offset + 2]
                .copy_from_slice(&self.mix_buf[i][0].to_le_bytes());
            scratch[offset + 2..offset + 4]
                .copy_from_slice(&self.mix_buf[i][1].to_le_bytes());
        }

        if self.snd.submit_grant(self.page_index, self.period_bytes).is_ok() {
            self.inflight_slots += 1;
            self.page_index = (self.page_index + 1) % RING_SLOTS;
        }
    }
}
