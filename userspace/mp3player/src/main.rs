#![no_std]
#![no_main]

//! MP3 player — streams PCM to the virtio-snd audio driver via shared memory.
//!
//! Raw mode:  `mp3player --raw /tmp/rawpcm`  (plays raw S16LE 44100 stereo)
//! MP3 mode:  `mp3player /tmp/test.mp3`      (decodes MP3 via nanomp3)
//!
//! PCM transfer uses a ring of RING_SLOTS grant pages. At session open,
//! the driver returns its space token + grant target VA. mp3player grants
//! RING_SLOTS pages into the driver's address space. Each period: write
//! PCM to the next slot, send metadata-only AUDIO_SUBMIT_PCM with the
//! slot index. Flow control: when all slots are inflight, drain
//! completions before writing more. No per-period blocking wait.

extern crate alloc;
extern crate nanomp3;

use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;

use libcluu::audio_client::{hz_to_rate, AudioSessionClient, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libcluu::syscall::{space_grant, space_map_range};
use libcluu::{debug_print, Error, Result};

use nanomp3::Decoder;

const PERIOD_BYTES: usize = 4096;
const SCRATCH_VA: usize = 0x7000_0000;
const SCRATCH_PAGES: usize = 48;
const RING_SLOTS: usize = 8;
const READ_CHUNK: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("mp3player: error {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    let args = libcluu::args::args();
    if args.len() < 2 {
        debug_print("mp3player: usage: mp3player [--raw] <file>")?;
        return Err(Error::InvalidArgument);
    }

    let raw_mode = args[1] == "--raw";
    let path = if raw_mode {
        if args.len() < 3 {
            debug_print("mp3player: --raw requires a file path")?;
            return Err(Error::InvalidArgument);
        }
        &args[2]
    } else {
        &args[1]
    };

    debug_print(&format!("mp3player: opening {}", path))?;

    let vfs_ep = registry::subscribe_output("vfs", "main")?;
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_ep, client_id);

    let file = vfs.open(path).map_err(|e| {
        let _ = debug_print(&format!("mp3player: open failed {:?}", e));
        e
    })?;

    let file_size = file.size;
    debug_print(&format!("mp3player: file size = {} bytes", file_size))?;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    space_map_range(space_token, SCRATCH_VA, 0, 0x03, SCRATCH_PAGES, 0)?;

    let snd_ep = match registry::subscribe_output("snddev", "main") {
        Ok(ep) => ep,
        Err(e) => {
            debug_print(&format!("mp3player: snddev subscribe failed {:?}", e))?;
            return Err(e);
        }
    };

    debug_print("MP3PLAYER_OPEN")?;

    let mut audio = if raw_mode {
        let params = PcmParams {
            format: PCM_FMT_S16,
            rate: hz_to_rate(44100),
            channels: 2,
        };
        let audio = AudioSessionClient::open(snd_ep, params).map_err(|e| {
            let _ = debug_print(&format!("mp3player: audio open failed {:?}", e));
            e
        })?;
        grant_ring(space_token, &audio)?;
        audio
    } else {
        open_audio_for_mp3(snd_ep, &vfs, file, file_size, space_token)?
    };

    if raw_mode {
        play_raw(&vfs, file, file_size, space_token, &mut audio)?;
    } else {
        play_mp3(&vfs, file, file_size, space_token, &mut audio)?;
    }

    vfs.close(file)?;
    debug_print("MP3PLAYER_DONE")?;
    Ok(())
}

fn grant_ring(space_token: usize, audio: &AudioSessionClient) -> Result<()> {
    for i in 0..RING_SLOTS {
        space_grant(
            space_token,
            audio.driver_space_token,
            SCRATCH_VA + i * PERIOD_BYTES,
            audio.grant_target_va + i * PERIOD_BYTES,
            0,
        )?;
    }
    Ok(())
}

fn open_audio_for_mp3(
    snd_ep: usize,
    vfs: &VfsClient,
    file: libcluu::fs::client::VfsFile,
    file_size: usize,
    space_token: usize,
) -> Result<AudioSessionClient> {
    let mut decoder = Decoder::new();
    let mut mp3_buf: Vec<u8> = Vec::new();
    let mut pcm_f32 = [0f32; nanomp3::MAX_SAMPLES_PER_FRAME];
    let mut offset = 0usize;
    let vfs_scratch = SCRATCH_VA + RING_SLOTS * PERIOD_BYTES;

    let (rate, channels) = 'outer: loop {
        if offset >= file_size {
            debug_print("mp3player: no MP3 frames found")?;
            return Err(Error::InvalidArgument);
        }
        let want = READ_CHUNK.min(file_size - offset);
        let grant = vfs.read_grant(file, offset, want, space_token, vfs_scratch)?;
        let chunk = unsafe {
            core::slice::from_raw_parts((grant.base + grant.offset) as *const u8, grant.len)
        };
        mp3_buf.extend_from_slice(chunk);
        offset += want;

        loop {
            let (consumed, frame_info) = decoder.decode(&mp3_buf, &mut pcm_f32);
            if consumed == 0 {
                break;
            }
            mp3_buf.drain(..consumed);
            if let Some(info) = frame_info {
                let ch = match info.channels {
                    nanomp3::Channels::Mono => 1,
                    nanomp3::Channels::Stereo => 2,
                };
                debug_print(&format!(
                    "mp3player: stream rate={}Hz channels={}",
                    info.sample_rate, ch
                ))?;
                break 'outer (hz_to_rate(info.sample_rate), ch);
            }
        }
    };

    let params = PcmParams {
        format: PCM_FMT_S16,
        rate,
        channels,
    };
    let audio = AudioSessionClient::open(snd_ep, params).map_err(|e| {
        let _ = debug_print(&format!("mp3player: audio open failed {:?}", e));
        e
    })?;

    grant_ring(space_token, &audio)?;

    Ok(audio)
}

struct RingState {
    slot: usize,
    inflight: u32,
}

impl RingState {
    fn flow_control(&mut self, audio: &mut AudioSessionClient) {
        while self.inflight >= RING_SLOTS as u32 {
            let completed = audio.drain_completions();
            if completed.is_empty() {
                audio.wait_next_completion();
            } else {
                self.inflight = self.inflight.saturating_sub(completed.len() as u32);
            }
        }
    }

    fn submit(&mut self, audio: &mut AudioSessionClient, len: usize) -> Result<()> {
        let slot = self.slot;
        let _handle = audio.submit_grant(slot, len)?;
        self.inflight += 1;
        self.slot = (self.slot + 1) % RING_SLOTS;
        Ok(())
    }
}

fn drain_all(audio: &mut AudioSessionClient, ring: &mut RingState) {
    while ring.inflight > 0 {
        let completed = audio.drain_completions();
        if completed.is_empty() {
            audio.wait_next_completion();
        } else {
            ring.inflight = ring.inflight.saturating_sub(completed.len() as u32);
        }
    }
}

fn play_raw(
    vfs: &VfsClient,
    file: libcluu::fs::client::VfsFile,
    file_size: usize,
    space_token: usize,
    audio: &mut AudioSessionClient,
) -> Result<()> {
    let vfs_scratch = SCRATCH_VA + RING_SLOTS * PERIOD_BYTES;
    let mut raw_data: Vec<u8> = Vec::with_capacity(file_size);
    {
        let mut offset = 0usize;
        while offset < file_size {
            let want = READ_CHUNK.min(file_size - offset);
            let grant = vfs.read_grant(file, offset, want, space_token, vfs_scratch)?;
            let chunk = unsafe {
                core::slice::from_raw_parts((grant.base + grant.offset) as *const u8, grant.len)
            };
            raw_data.extend_from_slice(chunk);
            offset += want;
        }
    }
    debug_print(&format!("mp3player: raw file loaded {} bytes", raw_data.len()))?;

    let mut periods = 0u32;
    let mut ring = RingState { slot: 0, inflight: 0 };
    let mut offset = 0usize;

    while offset < raw_data.len() {
        ring.flow_control(audio);

        let slot_va = SCRATCH_VA + ring.slot * PERIOD_BYTES;
        let scratch = unsafe {
            core::slice::from_raw_parts_mut(slot_va as *mut u8, PERIOD_BYTES)
        };
        let remaining = raw_data.len() - offset;
        let copy_len = remaining.min(PERIOD_BYTES);
        scratch[..copy_len].copy_from_slice(&raw_data[offset..offset + copy_len]);
        if copy_len < PERIOD_BYTES {
            for b in &mut scratch[copy_len..] {
                *b = 0;
            }
        }

        ring.submit(audio, copy_len)?;
        periods += 1;
        offset += copy_len;
    }

    debug_print(&format!("mp3player: raw {} periods played", periods))?;
    drain_all(audio, &mut ring);
    Ok(())
}

fn play_mp3(
    vfs: &VfsClient,
    file: libcluu::fs::client::VfsFile,
    file_size: usize,
    space_token: usize,
    audio: &mut AudioSessionClient,
) -> Result<()> {
    // Read entire file into memory first — eliminates 9p I/O stalls during
    // playback (which cause device underruns and perceived speed-up).
    let vfs_scratch = SCRATCH_VA + RING_SLOTS * PERIOD_BYTES;
    let mut mp3_data: Vec<u8> = Vec::with_capacity(file_size);
    {
        let mut offset = 0usize;
        while offset < file_size {
            let want = READ_CHUNK.min(file_size - offset);
            let grant = vfs.read_grant(file, offset, want, space_token, vfs_scratch)?;
            let chunk = unsafe {
                core::slice::from_raw_parts((grant.base + grant.offset) as *const u8, grant.len)
            };
            mp3_data.extend_from_slice(chunk);
            offset += want;
        }
    }
    debug_print(&format!("mp3player: file loaded {} bytes", mp3_data.len()))?;

    let mut decoder = Decoder::new();
    let mut pcm_f32 = [0f32; nanomp3::MAX_SAMPLES_PER_FRAME];
    let mut pcm_s16: Vec<u8> = Vec::with_capacity(PERIOD_BYTES);
    let mut pos = 0usize;
    let mut frames_decoded = 0u32;
    let mut ring = RingState { slot: 0, inflight: 0 };

    while pos < mp3_data.len() {
        let (consumed, frame_info) = decoder.decode(&mp3_data[pos..], &mut pcm_f32);
        if consumed == 0 {
            break;
        }
        pos += consumed;

        if let Some(info) = frame_info {
            frames_decoded += 1;
            let n_samples = info.samples_produced;
            let ch = match info.channels {
                nanomp3::Channels::Mono => 1,
                nanomp3::Channels::Stereo => 2,
            };
            let total_samples = n_samples * ch;

            for i in 0..total_samples {
                let clamped = pcm_f32[i].max(-1.0).min(1.0);
                let s = (clamped * 32767.0) as i16;
                pcm_s16.extend_from_slice(&s.to_le_bytes());
            }

            while pcm_s16.len() >= PERIOD_BYTES {
                ring.flow_control(audio);

                let slot_va = SCRATCH_VA + ring.slot * PERIOD_BYTES;
                let scratch = unsafe {
                    core::slice::from_raw_parts_mut(slot_va as *mut u8, PERIOD_BYTES)
                };
                scratch.copy_from_slice(&pcm_s16[..PERIOD_BYTES]);
                pcm_s16.drain(..PERIOD_BYTES);

                ring.submit(audio, PERIOD_BYTES)?;
            }
        }

        if frames_decoded % 64 == 0 && frames_decoded > 0 {
            debug_print(&format!("MP3PLAYER_DECODED_{}", frames_decoded))?;
        }
    }

    if !pcm_s16.is_empty() {
        ring.flow_control(audio);
        let slot_va = SCRATCH_VA + ring.slot * PERIOD_BYTES;
        let scratch = unsafe {
            core::slice::from_raw_parts_mut(slot_va as *mut u8, PERIOD_BYTES)
        };
        let copy_len = pcm_s16.len().min(PERIOD_BYTES);
        scratch[..copy_len].copy_from_slice(&pcm_s16[..copy_len]);
        if copy_len < PERIOD_BYTES {
            for b in &mut scratch[copy_len..] {
                *b = 0;
            }
        }
        ring.submit(audio, copy_len)?;
    }

    debug_print(&format!("mp3player: {} MP3 frames decoded", frames_decoded))?;
    drain_all(audio, &mut ring);
    Ok(())
}
