//! Audio engine: MP3 decode + virtio-snd playback + PCM tap for visualization.
//!
//! Single-threaded, non-blocking. The event loop calls `tick()` each frame;
//! tick drains completions (timeout=0), decodes one MP3 frame if the ring
//! has space, and submits PCM periods. Volume/balance applied as PCM scaling
//! before submit (virtio-snd has no mixer).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libcluu::audio_client::{hz_to_rate, AudioSessionClient, PcmHandle, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libcluu::syscall::{space_grant, space_map_range};
use libcluu::{debug_print, Error, Result};

use nanomp3::Decoder;

use crate::equalizer::Equalizer;
use crate::gain::{apply_period, Gain};

const PERIOD_BYTES: usize = 4096;
const SCRATCH_VA: usize = 0x7000_0000;
const SCRATCH_PAGES: usize = 24;
const RING_SLOTS: usize = 8;
const READ_CHUNK: usize = 64 * 1024;

const FFT_WINDOW: usize = 512;
const SCOPE_WINDOW: usize = 576;
const MAX_SAMPLES_PER_FRAME: usize = 2304;

#[derive(Clone, Copy)]
struct TapMetadata {
    handle: Option<PcmHandle>,
    mono: [f32; FFT_WINDOW],
    scope: [i16; SCOPE_WINDOW * 2],
    mono_len: usize,
    scope_len: usize,
}

impl TapMetadata {
    const EMPTY: Self = Self {
        handle: None,
        mono: [0.0; FFT_WINDOW],
        scope: [0; SCOPE_WINDOW * 2],
        mono_len: 0,
        scope_len: 0,
    };
}

const fn submission_target(sample_rate: u32, channels: u8) -> usize {
    let bytes_per_second = sample_rate as u64 * channels as u64 * 2;
    if bytes_per_second == 0 {
        return 2;
    }
    let period_numerator = 13 * bytes_per_second;
    let period_denominator = PERIOD_BYTES as u64 * 1000;
    let periods_for_13ms = (period_numerator + period_denominator - 1) / period_denominator;
    let target = periods_for_13ms as usize + 1;
    if target < 2 {
        2
    } else if target > RING_SLOTS {
        RING_SLOTS
    } else {
        target
    }
}

fn metadata_slot_for_handle(metadata: &[TapMetadata], handle: PcmHandle) -> Option<usize> {
    metadata
        .iter()
        .position(|entry| entry.handle == Some(handle))
}

fn tap_metadata(pcm: &[u8], byte_count: usize, channels: u8) -> TapMetadata {
    let mut metadata = TapMetadata::EMPTY;
    let sample_count = (byte_count.min(pcm.len()) & !1) / 2;
    let channels = usize::from(channels.max(1));
    let frames = sample_count / channels;
    metadata.mono_len = frames.min(FFT_WINDOW);
    let mono_start = frames.saturating_sub(metadata.mono_len);
    for frame in 0..metadata.mono_len {
        let mut sum = 0i32;
        for channel in 0..channels {
            let sample = (mono_start + frame) * channels + channel;
            let offset = sample * 2;
            sum += i32::from(i16::from_le_bytes([pcm[offset], pcm[offset + 1]]));
        }
        metadata.mono[frame] = sum as f32 / (channels as f32 * 32768.0);
    }
    metadata.scope_len = sample_count.min(SCOPE_WINDOW * channels);
    let scope_start = sample_count.saturating_sub(metadata.scope_len);
    for sample in 0..metadata.scope_len {
        let offset = (scope_start + sample) * 2;
        metadata.scope[sample] = i16::from_le_bytes([pcm[offset], pcm[offset + 1]]);
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::{
        metadata_slot_for_handle, submission_target, tap_metadata, TapMetadata, FFT_WINDOW,
        RING_SLOTS, SCOPE_WINDOW,
    };
    use libcluu::audio_client::PcmHandle;

    #[test]
    fn submission_target_keeps_low_rate_stereo_at_two_periods() {
        assert_eq!(submission_target(5512, 2), 2);
        assert_eq!(submission_target(48000, 2), 2);
    }

    #[test]
    fn submission_target_uses_three_periods_for_96khz_stereo() {
        assert_eq!(submission_target(96000, 2), 3);
    }

    #[test]
    fn metadata_lookup_matches_completed_handles_out_of_order() {
        let mut metadata = [TapMetadata::EMPTY; RING_SLOTS];
        metadata[1].handle = Some(PcmHandle(11));
        metadata[6].handle = Some(PcmHandle(22));

        assert_eq!(metadata_slot_for_handle(&metadata, PcmHandle(22)), Some(6));
        assert_eq!(metadata_slot_for_handle(&metadata, PcmHandle(11)), Some(1));
        assert_eq!(metadata_slot_for_handle(&metadata, PcmHandle(33)), None);
    }

    #[test]
    fn tap_metadata_averages_stereo_final_pcm() {
        let pcm = [0x00, 0x40, 0x00, 0xc0];

        let metadata = tap_metadata(&pcm, pcm.len(), 2);

        assert_eq!(metadata.mono_len, 1);
        assert_eq!(metadata.mono[0], 0.0);
        assert_eq!(metadata.scope_len, 2);
        assert_eq!(&metadata.scope[..2], &[16_384, -16_384]);
    }

    #[test]
    fn tap_metadata_uses_period_tail_for_visual_alignment() {
        let mut pcm = [0u8; (SCOPE_WINDOW + 1) * 4];
        pcm[..4].copy_from_slice(&[1, 0, 1, 0]);
        let fft_tail_start = (SCOPE_WINDOW + 1 - FFT_WINDOW) * 4;
        pcm[fft_tail_start..fft_tail_start + 4].copy_from_slice(&[2, 0, 2, 0]);
        pcm[4..8].copy_from_slice(&[3, 0, 3, 0]);

        let metadata = tap_metadata(&pcm, pcm.len(), 2);

        assert_eq!(metadata.mono_len, FFT_WINDOW);
        assert_eq!(metadata.mono[0], 2.0 / 32768.0);
        assert_eq!(&metadata.scope[..2], &[3, 3]);
    }
}

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
    pcm_f32: Box<[f32]>,
    pcm_s16: Vec<u8>,
    equalizer: Equalizer,
    eq_settings: [i8; 11],
    eq_enabled: bool,
    eq_scratch: Box<[u8]>,
    pcm_mono: Box<[f32]>,
    pcm_scope: Box<[i16]>,
    tap_metadata: Box<[TapMetadata]>,
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

const _: () = assert!(core::mem::size_of::<AudioEngine>() < 16 * 1024);

impl AudioEngine {
    pub fn new(playlist: Vec<String>) -> Self {
        Self {
            playlist,
            current_index: 0,
            decoder: Decoder::new(),
            audio: None,
            mp3_data: Vec::new(),
            decode_pos: 0,
            pcm_f32: vec![0.0; MAX_SAMPLES_PER_FRAME].into_boxed_slice(),
            pcm_s16: Vec::with_capacity(PERIOD_BYTES * 4),
            equalizer: Equalizer::new(),
            eq_settings: [0; 11],
            eq_enabled: false,
            eq_scratch: vec![0; PERIOD_BYTES].into_boxed_slice(),
            pcm_mono: vec![0.0; FFT_WINDOW].into_boxed_slice(),
            pcm_scope: vec![0; SCOPE_WINDOW * 2].into_boxed_slice(),
            tap_metadata: vec![TapMetadata::EMPTY; RING_SLOTS].into_boxed_slice(),
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

    pub fn set_equalizer(&mut self, enabled: bool, settings: [i8; 11]) {
        self.eq_enabled = enabled;
        self.eq_settings = settings;
        self.equalizer = Equalizer::new();
        self.equalizer
            .configure(self.eq_settings, self.sample_rate, self.channels);
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
        // Use the probed bitrate (kbps) when available; before the first
        // probe (bitrate_kbps == 0) fall back to the old ~88.2 kbps
        // assumption (44100 * 2 bits/sec) so pre-probe behavior is unchanged.
        let bitrate_bps = if self.bitrate_kbps > 0 {
            self.bitrate_kbps as u64 * 1000
        } else {
            44100 * 2
        };
        (self.mp3_data.len() as u64 * 8 * 1000) / bitrate_bps / bytes_per_ms * bytes_per_ms
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
        self.tap_metadata.fill(TapMetadata::EMPTY);
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
            let src = unsafe { core::slice::from_raw_parts(grant.base as *const u8, grant.len) };
            mp3_data.extend_from_slice(src);
            offset += grant.len;
        }
        vfs.close(file)?;

        let (rate, channels, bitrate) = self.probe_format(&mp3_data)?;
        self.sample_rate = rate;
        self.channels = channels;
        self.bitrate_kbps = bitrate;
        self.equalizer = Equalizer::new();
        self.equalizer
            .configure(self.eq_settings, self.sample_rate, self.channels);

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

        let submission_target = submission_target(self.sample_rate, self.channels) as u32;
        if self.ring_inflight < submission_target {
            self.decode_one_frame()?;
        }

        while self.pcm_s16.len() >= PERIOD_BYTES && self.ring_inflight < submission_target {
            self.submit_period()?;
        }

        let at_eof = self.decode_pos >= self.mp3_data.len();
        if at_eof && !self.pcm_s16.is_empty() && self.ring_inflight < RING_SLOTS as u32 {
            self.submit_period()?;
        }
        if at_eof && self.pcm_s16.is_empty() {
            self.advance_to_next()?;
        }

        Ok(())
    }

    fn drain_completions(&mut self) {
        let completed = match self.audio.as_mut() {
            Some(audio) => audio.drain_completions(),
            None => return,
        };
        for (handle, result) in completed {
            self.ring_inflight = self.ring_inflight.saturating_sub(1);
            if let Some(slot) = metadata_slot_for_handle(&self.tap_metadata, handle) {
                let metadata = self.tap_metadata[slot];
                self.tap_metadata[slot] = TapMetadata::EMPTY;
                if result.is_ok() {
                    self.pcm_mono.copy_from_slice(&metadata.mono);
                    self.pcm_scope.copy_from_slice(&metadata.scope);
                    self.new_pcm_available = metadata.mono_len > 0 || metadata.scope_len > 0;
                }
            }
        }
    }

    fn decode_one_frame(&mut self) -> Result<()> {
        if self.decode_pos >= self.mp3_data.len() {
            return Ok(());
        }
        let (consumed, info) = self
            .decoder
            .decode(&self.mp3_data[self.decode_pos..], &mut self.pcm_f32);
        if consumed == 0 && info.is_none() {
            self.decode_pos = self.mp3_data.len();
            return Ok(());
        }
        self.decode_pos += consumed;

        if let Some(fi) = info {
            let total_samples = fi.samples_produced * fi.channels.num() as usize;
            for i in 0..total_samples {
                let clamped = self.pcm_f32[i].max(-1.0).min(1.0);
                let s = (clamped * 32767.0) as i16;
                self.pcm_s16.extend_from_slice(&s.to_le_bytes());
            }
        }
        Ok(())
    }

    fn submit_period(&mut self) -> Result<()> {
        let slot = self.next_free_slot().ok_or(Error::InvalidState)?;
        let slot_va = SCRATCH_VA + slot * PERIOD_BYTES;
        let scratch = unsafe { core::slice::from_raw_parts_mut(slot_va as *mut u8, PERIOD_BYTES) };
        let to_copy = self.pcm_s16.len().min(PERIOD_BYTES);
        self.equalizer.process_period(
            &self.pcm_s16[..to_copy],
            &mut self.eq_scratch,
            self.eq_enabled,
        );
        let metadata = tap_metadata(&self.eq_scratch[..to_copy], to_copy, self.channels);
        apply_period(
            &self.eq_scratch[..to_copy],
            scratch,
            Gain::new(self.volume, self.balance, self.channels),
        );
        let handle = match self.audio.as_mut() {
            Some(audio) => audio.submit_grant(slot, PERIOD_BYTES)?,
            None => return Err(Error::InvalidState),
        };
        self.tap_metadata[slot] = TapMetadata {
            handle: Some(handle),
            ..metadata
        };
        self.pcm_s16.drain(..to_copy);
        self.ring_slot = (self.ring_slot + 1) % RING_SLOTS;
        self.ring_inflight += 1;
        self.pcm_submitted += PERIOD_BYTES as u64;
        Ok(())
    }

    fn next_free_slot(&self) -> Option<usize> {
        (0..RING_SLOTS)
            .map(|offset| (self.ring_slot + offset) % RING_SLOTS)
            .find(|&slot| self.tap_metadata[slot].handle.is_none())
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
