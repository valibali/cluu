//! Audio engine: MP3 decode + virtio-snd playback + PCM tap for visualization.
//!
//! Single-threaded, non-blocking. The event loop calls `tick()` each frame;
//! tick drains completions (timeout=0), decodes one MP3 frame if the ring
//! has space, and submits PCM periods. Volume/balance applied as PCM scaling
//! before submit (virtio-snd has no mixer).

use alloc::format;
use alloc::string::String;
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
const SCRATCH_PAGES: usize = 24;
const RING_SLOTS: usize = 8;
const READ_CHUNK: usize = 64 * 1024;

const FFT_WINDOW: usize = 512;
const SCOPE_WINDOW: usize = 576;
const MAX_SAMPLES_PER_FRAME: usize = 2304;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

pub struct AudioEngine {
    playlist: Vec<String>,
    current_index: usize,
    decoder: Decoder,
    audio: Option<AudioSessionClient>,
    mp3_data: Vec<u8>,
    decode_pos: usize,
    pcm_f32: [f32; MAX_SAMPLES_PER_FRAME],
    pcm_s16: Vec<u8>,
    pcm_mono: [f32; FFT_WINDOW],
    pcm_scope: [i16; SCOPE_WINDOW * 2],
    ring_slot: usize,
    ring_inflight: u32,
    state: PlaybackState,
    volume: u8,
    balance: i8,
    sample_rate: u32,
    channels: u8,
    pcm_submitted: u64,
    new_pcm_available: bool,
    file_loaded: bool,
    bitrate_kbps: u32,
}

impl AudioEngine {
    pub fn new(playlist: Vec<String>) -> Self {
        Self {
            playlist,
            current_index: 0,
            decoder: Decoder::new(),
            audio: None,
            mp3_data: Vec::new(),
            decode_pos: 0,
            pcm_f32: [0f32; MAX_SAMPLES_PER_FRAME],
            pcm_s16: Vec::with_capacity(PERIOD_BYTES * 4),
            pcm_mono: [0f32; FFT_WINDOW],
            pcm_scope: [0i16; SCOPE_WINDOW * 2],
            ring_slot: 0,
            ring_inflight: 0,
            state: PlaybackState::Stopped,
            volume: 100,
            balance: 0,
            sample_rate: 44100,
            channels: 2,
            pcm_submitted: 0,
            new_pcm_available: false,
            file_loaded: false,
            bitrate_kbps: 0,
        }
    }

    pub fn playlist(&self) -> &[String] {
        &self.playlist
    }

    pub fn extend_playlist(&mut self, paths: Vec<String>) {
        self.playlist.extend(paths);
    }

    pub fn current_index(&self) -> usize {
        self.current_index
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn volume(&self) -> u8 {
        self.volume
    }

    pub fn balance(&self) -> i8 {
        self.balance
    }

    pub fn set_volume(&mut self, v: u8) {
        self.volume = v.min(100);
    }

    pub fn set_balance(&mut self, b: i8) {
        self.balance = b.clamp(-50, 50);
    }

    pub fn pcm_mono(&self) -> &[f32] {
        &self.pcm_mono
    }

    pub fn pcm_scope(&self) -> &[i16] {
        &self.pcm_scope
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Current track's MP3 bitrate in kbps; 0 before the first probe.
    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    /// Remove a playlist entry. Removing the playing/current track stops
    /// playback and closes the audio session; indices behind the removal
    /// shift down. current_index clamps to the new playlist end.
    pub fn remove_track(&mut self, idx: usize) {
        if idx >= self.playlist.len() {
            return;
        }
        self.playlist.remove(idx);
        if self.playlist.is_empty() {
            self.stop();
            self.close_audio();
            self.current_index = 0;
            return;
        }
        if idx == self.current_index {
            self.stop();
            self.close_audio();
            if self.current_index >= self.playlist.len() {
                self.current_index = self.playlist.len() - 1;
            }
        } else if idx < self.current_index {
            self.current_index -= 1;
        }
    }

    pub fn has_new_pcm(&self) -> bool {
        self.new_pcm_available
    }

    pub fn clear_new_pcm(&mut self) {
        self.new_pcm_available = false;
    }

    pub fn position_ms(&self) -> u64 {
        let bytes_per_ms = (self.sample_rate as u64 * 2 * self.channels as u64) / 1000;
        if bytes_per_ms == 0 {
            return 0;
        }
        self.pcm_submitted / bytes_per_ms
    }

    pub fn duration_ms(&self) -> u64 {
        let bytes_per_ms = (self.sample_rate as u64 * 2 * self.channels as u64) / 1000;
        if bytes_per_ms == 0 {
            return 0;
        }
        (self.mp3_data.len() as u64 * 8 * 1000) / (44100 * 2) / bytes_per_ms * bytes_per_ms
    }

    pub fn current_title(&self) -> &str {
        if self.current_index < self.playlist.len() {
            let path = &self.playlist[self.current_index];
            if let Some(pos) = path.rfind('/') {
                &path[pos + 1..]
            } else {
                path
            }
        } else {
            ""
        }
    }

    pub fn play(&mut self) -> Result<()> {
        if self.state == PlaybackState::Paused {
            self.state = PlaybackState::Playing;
            return Ok(());
        }
        if !self.file_loaded {
            self.load_current()?;
        }
        self.state = PlaybackState::Playing;
        Ok(())
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        } else if self.state == PlaybackState::Paused {
            self.state = PlaybackState::Playing;
        }
    }

    pub fn stop(&mut self) {
        self.state = PlaybackState::Stopped;
        self.decode_pos = 0;
        self.pcm_s16.clear();
        self.pcm_submitted = 0;
        self.decoder = Decoder::new();
    }

    pub fn next(&mut self) -> Result<()> {
        if self.current_index + 1 < self.playlist.len() {
            self.current_index += 1;
            self.close_audio();
            self.file_loaded = false;
            self.state = PlaybackState::Stopped;
            self.play()
        } else {
            self.state = PlaybackState::Stopped;
            Ok(())
        }
    }

    pub fn prev(&mut self) -> Result<()> {
        if self.current_index > 0 {
            self.current_index -= 1;
            self.close_audio();
            self.file_loaded = false;
            self.state = PlaybackState::Stopped;
            self.play()
        } else {
            Ok(())
        }
    }

    pub fn select_track(&mut self, index: usize) -> Result<()> {
        if index < self.playlist.len() {
            self.current_index = index;
            self.close_audio();
            self.file_loaded = false;
            self.state = PlaybackState::Stopped;
            self.play()
        } else {
            Ok(())
        }
    }

    fn close_audio(&mut self) {
        self.audio = None;
        self.mp3_data.clear();
        self.decode_pos = 0;
        self.pcm_s16.clear();
        self.pcm_submitted = 0;
        self.ring_slot = 0;
        self.ring_inflight = 0;
        self.file_loaded = false;
    }

    fn load_current(&mut self) -> Result<()> {
        if self.current_index >= self.playlist.len() {
            return Err(Error::NotFound);
        }
        let path = &self.playlist[self.current_index];
        debug_print(&format!("cluuamp: opening {}\n", path));

        let info = process_info();
        let space_token = info.tokens[TOKEN_SPACE];
        space_map_range(space_token, SCRATCH_VA, 0, 0x03, SCRATCH_PAGES, 0)?;

        let vfs_ep = registry::subscribe_output("vfs", "main")?;
        let client_id = registry::control_endpoint();
        let vfs = VfsClient::new(vfs_ep, client_id);
        let file = vfs.open(path)?;
        let file_size = file.size;

        let mut mp3_data: Vec<u8> = Vec::with_capacity(file_size);
        let mut offset = 0usize;
        let vfs_scratch = SCRATCH_VA + RING_SLOTS * PERIOD_BYTES;
        while offset < file_size {
            let want = if file_size - offset > READ_CHUNK {
                READ_CHUNK
            } else {
                file_size - offset
            };
            let grant = vfs.read_grant(file, offset, want, space_token, vfs_scratch)?;
            let src = unsafe {
                core::slice::from_raw_parts(grant.base as *const u8, grant.len)
            };
            mp3_data.extend_from_slice(src);
            offset += grant.len;
        }
        vfs.close(file)?;

        let (rate, channels, bitrate) = self.probe_format(&mp3_data)?;
        self.sample_rate = rate;
        self.channels = channels;
        self.bitrate_kbps = bitrate;

        let snd_ep = registry::subscribe_output("snddev", "main")?;
        let params = PcmParams {
            format: PCM_FMT_S16,
            rate: hz_to_rate(rate),
            channels,
        };
        let audio = AudioSessionClient::open(snd_ep, params)?;

        for i in 0..RING_SLOTS {
            space_grant(
                space_token,
                audio.driver_space_token,
                SCRATCH_VA + i * PERIOD_BYTES,
                audio.grant_target_va + i * PERIOD_BYTES,
                0,
            )?;
        }

        self.audio = Some(audio);
        self.mp3_data = mp3_data;
        self.decode_pos = 0;
        self.decoder = Decoder::new();
        self.file_loaded = true;
        debug_print("cluuamp: audio session open\n");
        Ok(())
    }

    fn probe_format(&mut self, data: &[u8]) -> Result<(u32, u8, u32)> {
        let mut decoder = Decoder::new();
        let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];
        let mut pos = 0;
        for _ in 0..200 {
            if pos >= data.len() {
                break;
            }
            let (consumed, info) = decoder.decode(&data[pos..], &mut pcm);
            if consumed > 0 {
                pos += consumed;
            }
            if let Some(fi) = info {
                return Ok((fi.sample_rate, fi.channels.num(), fi.bitrate));
            }
        }
        Err(Error::InvalidState)
    }

    pub fn tick(&mut self) -> Result<()> {
        if self.state != PlaybackState::Playing {
            return Ok(());
        }
        if self.audio.is_none() {
            return Ok(());
        }

        self.drain_completions();

        if self.ring_inflight < RING_SLOTS as u32 {
            self.decode_one_frame()?;
        }

        while self.pcm_s16.len() >= PERIOD_BYTES && self.ring_inflight < RING_SLOTS as u32 {
            self.submit_period()?;
        }

        Ok(())
    }

    fn drain_completions(&mut self) {
        if self.audio.is_none() {
            return;
        }
        let audio = self.audio.as_mut().unwrap();
        let completed = audio.drain_completions();
        if !completed.is_empty() {
            let n = completed.len() as u32;
            self.ring_inflight = self.ring_inflight.saturating_sub(n);
        }
    }

    fn decode_one_frame(&mut self) -> Result<()> {
        if self.decode_pos >= self.mp3_data.len() {
            self.advance_to_next()?;
            return Ok(());
        }
        let (consumed, info) =
            self.decoder
                .decode(&self.mp3_data[self.decode_pos..], &mut self.pcm_f32);
        if consumed == 0 && info.is_none() {
            self.advance_to_next()?;
            return Ok(());
        }
        self.decode_pos += consumed;

        if let Some(fi) = info {
            let total_samples = fi.samples_produced * fi.channels.num() as usize;
            let ch = fi.channels.num() as usize;
            self.update_pcm_tap(total_samples, ch);
            let vol = self.volume as f32 / 100.0;
            let bal_l = if self.balance < 0 {
                1.0
            } else {
                1.0 - self.balance as f32 / 50.0
            };
            let bal_r = if self.balance > 0 {
                1.0
            } else {
                1.0 + self.balance as f32 / 50.0
            };
            for i in 0..total_samples {
                let clamped = self.pcm_f32[i].max(-1.0).min(1.0);
                let channel = i % ch;
                let scale = vol * if channel == 0 { bal_l } else { bal_r };
                let s = ((clamped * scale) * 32767.0) as i16;
                self.pcm_s16.extend_from_slice(&s.to_le_bytes());
            }
            self.new_pcm_available = true;
        }
        Ok(())
    }

    fn update_pcm_tap(&mut self, total_samples: usize, channels: usize) {
        if channels == 0 {
            return;
        }
        let mono_count = total_samples.min(FFT_WINDOW * channels);
        let copy_samples = mono_count / channels;
        for i in 0..copy_samples.min(FFT_WINDOW) {
            let mut sum = 0.0f32;
            for c in 0..channels {
                let idx = i * channels + c;
                if idx < total_samples {
                    sum += self.pcm_f32[idx];
                }
            }
            self.pcm_mono[i] = sum;
        }

        let scope_samples = total_samples.min(SCOPE_WINDOW * channels);
        for i in 0..scope_samples {
            let clamped = self.pcm_f32[i].max(-1.0).min(1.0);
            self.pcm_scope[i] = (clamped * 32767.0) as i16;
        }
    }

    fn submit_period(&mut self) -> Result<()> {
        let audio = self.audio.as_mut().unwrap();
        let slot = self.ring_slot;
        let slot_va = SCRATCH_VA + slot * PERIOD_BYTES;
        let scratch = unsafe {
            core::slice::from_raw_parts_mut(slot_va as *mut u8, PERIOD_BYTES)
        };
        let to_copy = self.pcm_s16.len().min(PERIOD_BYTES);
        scratch[..to_copy].copy_from_slice(&self.pcm_s16[..to_copy]);
        if to_copy < PERIOD_BYTES {
            for b in &mut scratch[to_copy..] {
                *b = 0;
            }
        }
        self.pcm_s16.drain(..to_copy);
        audio.submit_grant(slot, PERIOD_BYTES)?;
        self.ring_slot = (self.ring_slot + 1) % RING_SLOTS;
        self.ring_inflight += 1;
        self.pcm_submitted += PERIOD_BYTES as u64;
        Ok(())
    }

    fn advance_to_next(&mut self) -> Result<()> {
        if self.current_index + 1 < self.playlist.len() {
            self.current_index += 1;
            self.close_audio();
            self.play()?;
        } else {
            self.state = PlaybackState::Stopped;
        }
        Ok(())
    }
}
