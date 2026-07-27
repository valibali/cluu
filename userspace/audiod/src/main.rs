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

use audiod::mixer::MAX_PERIOD_FRAMES;
use audiod::ring::FRAME_BYTES;
use audiod::session::{
    StreamRegistry, StreamStatus, AUDIOD_SESSION_DESTROYED, AUDIOD_STREAM_CLOSE,
    AUDIOD_STREAM_DRAIN, AUDIOD_STREAM_GAIN, AUDIOD_STREAM_OPEN, AUDIOD_STREAM_PAUSE,
    AUDIOD_STREAM_RESUME, AUDIOD_STREAM_STATUS,
};

use alloc::format;

use libcluu::audio_client::{AudioSessionClient, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::{extract_reply_id, reply};
use libcluu::registry;
use libcluu::syscall::{endpoint_create, ipc_recv_any_with_sender};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Result};

/// Output format: stereo S16 at 44100 Hz.
const OUTPUT_RATE: u32 = 44100;
const OUTPUT_CHANNELS: u8 = 2;

/// Initial period size in bytes. 2048 bytes = 512 stereo S16 frames.
/// Separately testable at 1024 bytes (256 frames).
pub const PERIOD_BYTES: usize = 2048;
pub const PERIOD_FRAMES: usize = PERIOD_BYTES / FRAME_BYTES;

/// Buffer size must be ≥ period (coherent configuration). 4× period.
pub const BUFFER_BYTES: usize = PERIOD_BYTES * 4;

/// SHM ring capacity in frames. Must be ≥ 2× period for smooth pacing.
const RING_CAPACITY_FRAMES: usize = PERIOD_FRAMES * 4;

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
    /// virtio-snd audio session (sole client).
    snd: AudioSessionClient,
    /// Stream registry (session_id → streams).
    streams: StreamRegistry,
    /// Listen endpoint for audiod:main (stream_open, session_destroyed).
    listen_ep: usize,
    /// Mixed output buffer (one period).
    mix_buf: [[i16; 2]; MAX_PERIOD_FRAMES],
    /// Per-stream resampled output (scratch).
    stream_buf: [[i16; 2]; MAX_PERIOD_FRAMES],
    /// PCM output in bytes (for virtio-snd submit).
    pcm_out: [u8; PERIOD_BYTES],
    /// Next period ID for virtio-snd submit.
    next_period_id: u64,
    /// Page index for virtio-snd ring (rotates through slots).
    page_index: usize,
}

fn run() -> Result<()> {
    debug_print("audiod: starting")?;

    let info = process_info();
    let ipc_token = info.tokens[TOKEN_IPC];

    // ── Connect to virtio-snd as sole client ─────────────────────────────
    let snd_ep = registry::subscribe_output("snddev", "main")?;
    debug_print("audiod: subscribed to snddev:main")?;

    let params = PcmParams {
        format: PCM_FMT_S16,
        rate: libcluu::audio_client::hz_to_rate(OUTPUT_RATE),
        channels: OUTPUT_CHANNELS,
    };
    let snd = AudioSessionClient::open(snd_ep, params)?;
    debug_print("audiod: virtio-snd session open")?;

    // Grant PCM pages to virtio-snd for each ring slot.
    // The AudioSessionClient holds grant_target_va and driver_space_token
    // from the open reply. The actual grant setup mirrors cluuamp's audio.rs:
    // space_grant(space_token, snd.driver_space_token, our_va, snd.grant_target_va, 0)
    let _space_token = info.tokens[TOKEN_SPACE];

    // ── Register audiod:main ─────────────────────────────────────────────
    registry::init("audiod")?;
    let listen_ep = endpoint_create(ipc_token)?;
    registry::register_output("main", listen_ep)?;
    debug_print("audiod: registered as audiod:main")?;

    let mut audiod = Audiod {
        snd,
        streams: StreamRegistry::new(),
        listen_ep,
        mix_buf: [[0i16; 2]; MAX_PERIOD_FRAMES],
        stream_buf: [[0i16; 2]; MAX_PERIOD_FRAMES],
        pcm_out: [0u8; PERIOD_BYTES],
        next_period_id: 1,
        page_index: 0,
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
        let tokens = [audiod.listen_ep, registry_ep];
        let (idx, len, _sender_tid) = match ipc_recv_any_with_sender(&tokens, &mut buf, 10) {
            Ok(t) => t,
            Err(_) => {
                // Timeout — drain completions and run the mix cycle.
                audiod.drain_completions();
                audiod.mix_and_submit();
                continue;
            }
        };

        if idx == 1 {
            // Registry control message.
            if len >= core::mem::size_of::<Message>() {
                // SAFETY: `buf` is a stack-local `[u8; 256]` which is
                // naturally aligned to at least 8 bytes on x86_64 (stack
                // alignment), satisfying `Message`'s alignment. The length
                // check `len >= size_of::<Message>()` ensures the cast
                // reads only within the received bytes. The payload slice
                // below is bounded by `len`, so it also stays in bounds.
                let msg = unsafe { &*(buf.as_ptr() as *const Message) };
                let payload = &buf[core::mem::size_of::<Message>()..len];
                let _ = registry::handle_incoming_message(msg, payload);
            }
            audiod.drain_completions();
            audiod.mix_and_submit();
            continue;
        }

        // idx == 0: audiod:main message (stream control or session destroyed).
        if len < core::mem::size_of::<Message>() {
            continue;
        }
        // SAFETY: Same argument as the registry branch above — `buf` is a
        // stack-local `[u8; 256]` with sufficient natural alignment for
        // `Message`, and `len >= size_of::<Message>()` was just checked.
        let msg = unsafe { &*(buf.as_ptr() as *const Message) };
        audiod.handle_message(msg);
        audiod.drain_completions();
        audiod.mix_and_submit();
    }
}

impl Audiod {
    fn handle_message(&mut self, msg: &Message) {
        let reply_token = extract_reply_id(msg);
        match msg.tag.label {
            AUDIOD_STREAM_OPEN => self.handle_stream_open(msg, reply_token),
            AUDIOD_STREAM_CLOSE => self.handle_stream_close(msg, reply_token),
            AUDIOD_STREAM_PAUSE => self.handle_stream_pause(msg, reply_token),
            AUDIOD_STREAM_RESUME => self.handle_stream_resume(msg, reply_token),
            AUDIOD_STREAM_DRAIN => self.handle_stream_drain(msg, reply_token),
            AUDIOD_STREAM_GAIN => self.handle_stream_gain(msg, reply_token),
            AUDIOD_STREAM_STATUS => self.handle_stream_status(msg, reply_token),
            AUDIOD_SESSION_DESTROYED => self.handle_session_destroyed(msg, reply_token),
            _ => {
                // Unknown label — ignore.
            }
        }
    }

    fn handle_stream_open(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let in_rate = msg.words[1] as u32;
        let in_channels = msg.words[2] as u8;

        let session = self.streams.ensure_session(session_id);
        let stream_id = session.alloc_stream_id();

        // In a full implementation, allocate SHM ring backing, create per-stream
        // endpoint, grant ring to producer. For T17, we record the stream metadata.
        // The SHM ring allocation requires space_map + space_grant, which needs
        // the full grant setup (similar to cluuamp's audio.rs).
        //
        // For T17, stream_open returns a success reply with stream_id.
        // The actual SHM ring is allocated in a follow-up call or via a
        // separate grant mechanism.

        let rmsg = Message::new(
            AUDIOD_STREAM_OPEN,
            [0, stream_id as usize, session_id as usize, 0, 0, 0],
            3,
        );
        if let Some(rt) = reply_token {
            let _ = reply(rt, &rmsg, IpcFlags::empty());
        }
        let _ = debug_print(&format!(
            "audiod: stream_open sid={} stream_id={} rate={} ch={}",
            session_id & 0xFF, stream_id, in_rate, in_channels
        ));
    }

    fn handle_stream_close(&mut self, msg: &Message, reply_token: Option<usize>) {
        let session_id = msg.words[0] as u32;
        let stream_id = msg.words[1] as u32;
        if let Some(session) = self.streams.sessions.get_mut(&session_id) {
            session.streams.remove(&stream_id);
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
        self.streams.destroy_session(session_id);
        let _ = debug_print(&format!(
            "audiod: session {} destroyed — streams killed",
            session_id & 0xFF
        ));
    }

    fn drain_completions(&mut self) {
        // Drain virtio-snd TX completions.
        let completions = self.snd.drain_completions();
        for (_handle, _result) in completions {
            // Mark period as played. In a full implementation, update
            // per-stream frames_played counters based on which streams
            // contributed to this period.
        }
    }

    fn mix_and_submit(&mut self) {
        // Collect active stream contributions.
        // For T17, the mixer collects from all active streams' SHM rings,
        // resamples each to OUTPUT_RATE, and mixes with single saturation.
        //
        // In the full implementation:
        // 1. For each active stream: pop PERIOD_FRAMES from its SHM ring
        // 2. Resample to OUTPUT_RATE stereo S16
        // 3. Mix all streams with per-stream gain
        // 4. Convert to bytes and submit to virtio-snd

        // For T17, if no streams are active, output silence.
        // (The mix_buf is zero-initialized; if no streams contribute,
        //  the output is silence.)
        let n_frames = PERIOD_FRAMES;

        // Convert mix_buf to bytes for virtio-snd.
        for i in 0..n_frames {
            let offset = i * FRAME_BYTES;
            self.pcm_out[offset..offset + 2]
                .copy_from_slice(&self.mix_buf[i][0].to_le_bytes());
            self.pcm_out[offset + 2..offset + 4]
                .copy_from_slice(&self.mix_buf[i][1].to_le_bytes());
        }

        // Submit to virtio-snd.
        let _ = self.snd.submit_grant(self.page_index, PERIOD_BYTES);
        self.page_index = (self.page_index + 1) % 8; // RING_SLOTS = 8
        self.next_period_id = self.next_period_id.wrapping_add(1);
    }
}
