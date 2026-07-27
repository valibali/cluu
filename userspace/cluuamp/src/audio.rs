//! Audio engine: MP3 decode + audiod stream lifecycle + virtio-snd playback.
//!
//! T20 migration: cluuamp talks to audiod for stream lifecycle (open, close,
//! pause, resume, drain) via the PARAM_AUDIOD_EP / registry-brokered endpoint.
//! The actual PCM transport still goes through virtio-snd (audiod's mixer path
//! is stubbed in T17 — same pattern as the SDL2 CLUU audio backend in T18).
//!
//! The decoder writes bounded frames into `pcm_s16` (the producer ring),
//! gated by submit-before-decode so it never exceeds ~1 period + 1 frame.
//! Playback position uses accepted/played byte counters that increment ONLY
//! on confirmed virtio-snd completion — padding bytes from partial EOF
//! periods are excluded via per-slot `actual_bytes` tracking.
//!
//! Single-threaded, non-blocking. The event loop calls `tick()` each frame;
//! tick drains completions (timeout=0), decodes one MP3 frame if the ring
//! has space, and submits PCM periods. Volume/balance applied as PCM scaling
//! before submit (virtio-snd has no mixer).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use libcluu::audio_client::{hz_to_rate, AudioSessionClient, PcmHandle, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::{VfsClient, VfsFile};
use libcluu::registry;
use libcluu::syscall::{space_grant, space_map_range};
use libcluu::types::Message;
use libcluu::{debug_print, Error, Result};

use crate::mp3_ffi::{self, Decoder};
use crate::equalizer::Equalizer;
use crate::gain::{apply_period, Gain};
use crate::id3::{self, TrackMeta};

const PERIOD_BYTES: usize = 4096;
const SCRATCH_VA: usize = 0x7000_0000;
const SCRATCH_PAGES: usize = 24;
const RING_SLOTS: usize = 8;

/// Audiod IPC labels (matching `audiod/src/session.rs`).
const AUDIOD_STREAM_OPEN: u32 = 0x700;
const AUDIOD_STREAM_CLOSE: u32 = 0x701;
const AUDIOD_STREAM_PAUSE: u32 = 0x702;
const AUDIOD_STREAM_RESUME: u32 = 0x703;
const AUDIOD_STREAM_DRAIN: u32 = 0x704;
const READ_CHUNK: usize = 64 * 1024;
const DECODE_BATCH: usize = 4;
const STREAM_BUF_SIZE: usize = 256 * 1024;
const STREAM_REFILL_WATERMARK: usize = 64 * 1024;

const FFT_WINDOW: usize = 512;
const SCOPE_WINDOW: usize = 576;

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
    track_metas: Vec<Option<TrackMeta>>,
    decoder: Decoder,
    audio: Option<AudioSessionClient>,
    vfs_file: Option<VfsFile>,
    stream_buf: Vec<u8>,
    stream_consumed: usize,
    stream_file_offset: usize,
    file_size: usize,
    pcm_frame: Box<[i16]>,
    pcm_s16: Vec<u8>,
    equalizer: Equalizer,
    eq_settings: [i8; 11],
    eq_enabled: bool,
    eq_scratch: Box<[u8]>,
    pcm_mono: Box<[f32]>,
    pcm_scope: Box<[i16]>,
    tap_metadata: Box<[TapMetadata]>,
    actual_bytes: Box<[usize]>,
    ring_slot: usize,
    ring_inflight: u32,
    state: PlaybackState,
    volume: u8,
    balance: i8,
    sample_rate: u32,
    channels: u8,
    pcm_played: u64,
    pcm_total_decoded: u64,
    decode_complete: bool,
    new_pcm_available: bool,
    file_loaded: bool,
    bitrate_kbps: u32,
    needs_refill: bool,
    needs_advance: bool,
    completion_scratch: Vec<(libcluu::audio_client::PcmHandle, Result<()>)>,
    audiod_ep: usize,
    audiod_stream_id: u32,
    audiod_session_id: u32,
}

const _: () = assert!(core::mem::size_of::<AudioEngine>() < 16 * 1024);

impl AudioEngine {
    pub fn new(playlist: Vec<String>) -> Self {
        let metas = vec![None; playlist.len()];
        Self {
            playlist,
            current_index: 0,
            track_metas: metas,
            decoder: {
                let mut d = Decoder::new();
                d.init();
                d
            },
            audio: None,
            vfs_file: None,
            stream_buf: Vec::with_capacity(STREAM_BUF_SIZE),
            stream_consumed: 0,
            stream_file_offset: 0,
            file_size: 0,
            pcm_frame: vec![0i16; mp3_ffi::MAX_SAMPLES_PER_FRAME].into_boxed_slice(),
            pcm_s16: Vec::with_capacity(PERIOD_BYTES * 4),
            equalizer: Equalizer::new(),
            eq_settings: [0; 11],
            eq_enabled: false,
            eq_scratch: vec![0; PERIOD_BYTES].into_boxed_slice(),
            pcm_mono: vec![0.0; FFT_WINDOW].into_boxed_slice(),
            pcm_scope: vec![0; SCOPE_WINDOW * 2].into_boxed_slice(),
            tap_metadata: vec![TapMetadata::EMPTY; RING_SLOTS].into_boxed_slice(),
            actual_bytes: vec![0usize; RING_SLOTS].into_boxed_slice(),
            ring_slot: 0,
            ring_inflight: 0,
            state: PlaybackState::Stopped,
            volume: 100,
            balance: 0,
            sample_rate: 44100,
            channels: 2,
            pcm_played: 0,
            pcm_total_decoded: 0,
            decode_complete: false,
            new_pcm_available: false,
            file_loaded: false,
            bitrate_kbps: 0,
            needs_refill: false,
            needs_advance: false,
            completion_scratch: Vec::new(),
            audiod_ep: 0,
            audiod_stream_id: 0,
            audiod_session_id: 0,
        }
    }

    pub fn playlist(&self) -> &[String] {
        &self.playlist
    }

    pub fn extend_playlist(&mut self, paths: Vec<String>) {
        self.track_metas.extend(paths.iter().map(|_| None));
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
        let pos = self.pcm_played / bytes_per_ms;
        let dur = self.duration_ms();
        pos.min(dur)
    }

    pub fn duration_ms(&self) -> u64 {
        let bytes_per_ms = (self.sample_rate as u64 * 2 * self.channels as u64) / 1000;
        if bytes_per_ms == 0 {
            return 0;
        }
        if self.decode_complete {
            return self.pcm_total_decoded / bytes_per_ms;
        }
        let bitrate_bps = if self.bitrate_kbps > 0 {
            self.bitrate_kbps as u64 * 1000
        } else {
            44100 * 2
        };
        (self.file_size as u64 * 8 * 1000) / bitrate_bps / bytes_per_ms * bytes_per_ms
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

    pub fn track_meta(&self, idx: usize) -> Option<&TrackMeta> {
        self.track_metas.get(idx).and_then(|m| m.as_ref())
    }

    pub fn current_meta(&self) -> Option<&TrackMeta> {
        self.track_meta(self.current_index)
    }

    pub fn set_track_meta(&mut self, idx: usize, meta: TrackMeta) {
        if idx < self.track_metas.len() {
            self.track_metas[idx] = Some(meta);
        }
    }

    pub fn track_duration_ms(&self, idx: usize) -> u64 {
        self.track_metas.get(idx).and_then(|m| m.as_ref()).map_or(0, |m| m.duration_ms)
    }

    pub fn next_unparsed_meta(&self) -> Option<usize> {
        self.track_metas.iter().position(|m| m.is_none())
    }

    pub fn write_display_title(&self, idx: usize, out: &mut String) {
        out.clear();
        if let Some(meta) = self.track_meta(idx) {
            if !meta.title.is_empty() {
                if !meta.artist.is_empty() {
                    use core::fmt::Write;
                    let _ = write!(out, "{} - {}", meta.artist, meta.title);
                    return;
                }
                out.push_str(&meta.title);
                return;
            }
        }
        out.push_str(self.filename(idx));
    }

    pub fn filename(&self, idx: usize) -> &str {
        if idx < self.playlist.len() {
            let path = &self.playlist[idx];
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
            self.send_audiod_resume();
            return Ok(());
        }
        if self.state == PlaybackState::Stopped {
            self.close_audio();
        }
        if !self.file_loaded {
            self.load_current()?;
        }
        self.state = PlaybackState::Playing;
        let _ = debug_print("CLUUAMP_SOLO_OK\n");
        Ok(())
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
            self.send_audiod_pause();
        } else if self.state == PlaybackState::Paused {
            self.state = PlaybackState::Playing;
            self.send_audiod_resume();
        }
    }

    pub fn stop(&mut self) {
        if self.state != PlaybackState::Stopped {
            self.send_audiod_drain();
        }
        self.state = PlaybackState::Stopped;
        self.stream_consumed = 0;
        self.pcm_s16.clear();
        self.pcm_played = 0;
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
        if self.audiod_stream_id != 0 {
            self.send_audiod_close();
            self.audiod_stream_id = 0;
            self.audiod_session_id = 0;
        }
        if let Some(file) = self.vfs_file.take() {
            let vfs_ep = registry::subscribe_output("vfs", "main").ok();
            if let Some(ep) = vfs_ep {
                let cid = registry::control_endpoint();
                let vfs = VfsClient::new(ep, cid);
                let _ = vfs.close(file);
            }
        }
        self.audio = None;
        self.stream_buf.clear();
        self.stream_consumed = 0;
        self.stream_file_offset = 0;
        self.file_size = 0;
        self.pcm_s16.clear();
        self.pcm_played = 0;
        self.pcm_total_decoded = 0;
        self.decode_complete = false;
        self.ring_slot = 0;
        self.ring_inflight = 0;
        self.tap_metadata.fill(TapMetadata::EMPTY);
        self.actual_bytes.fill(0);
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
        self.vfs_file = Some(file);
        self.file_size = file_size;

        self.stream_buf.clear();
        self.stream_consumed = 0;
        self.stream_file_offset = 0;
        self.refill_stream(&vfs, space_token)?;

        let (rate, channels, bitrate) = Self::probe_format(&self.stream_buf)?;
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
        self.decoder = Decoder::new();
        self.file_loaded = true;

        self.open_audiod_stream();

        let meta = id3::parse(&self.stream_buf);
        if !meta.is_empty() {
            self.track_metas[self.current_index] = Some(meta);
        }

        debug_print("cluuamp: audio session open\n");
        Ok(())
    }

    fn refill_stream(&mut self, vfs: &VfsClient, space_token: usize) -> Result<()> {
        let vfs_scratch = SCRATCH_VA + RING_SLOTS * PERIOD_BYTES;
        while self.stream_buf.len() < STREAM_BUF_SIZE && self.stream_file_offset < self.file_size {
            let space = STREAM_BUF_SIZE - self.stream_buf.len();
            let want = if self.file_size - self.stream_file_offset > READ_CHUNK {
                READ_CHUNK
            } else {
                self.file_size - self.stream_file_offset
            };
            let want = want.min(space);
            let file = self.vfs_file.ok_or(Error::InvalidState)?;
            let grant = vfs.read_grant(file, self.stream_file_offset, want, space_token, vfs_scratch)?;
            let src = unsafe { core::slice::from_raw_parts(grant.base as *const u8, grant.len) };
            self.stream_buf.extend_from_slice(src);
            self.stream_file_offset += grant.len;
        }
        Ok(())
    }

    fn stream_avail(&self) -> usize {
        self.stream_buf.len().saturating_sub(self.stream_consumed)
    }

    fn stream_at_eof(&self) -> bool {
        self.stream_file_offset >= self.file_size && self.stream_consumed >= self.stream_buf.len()
    }

    fn ensure_stream_data(&mut self) -> Result<()> {
        if self.stream_avail() >= STREAM_REFILL_WATERMARK || self.stream_file_offset >= self.file_size {
            return Ok(());
        }
        if self.stream_consumed > 0 {
            self.stream_buf.drain(0..self.stream_consumed);
            self.stream_consumed = 0;
        }
        let info = process_info();
        let space_token = info.tokens[TOKEN_SPACE];
        let vfs_ep = registry::subscribe_output("vfs", "main")?;
        let client_id = registry::control_endpoint();
        let vfs = VfsClient::new(vfs_ep, client_id);
        self.refill_stream(&vfs, space_token)?;
        Ok(())
    }

    fn probe_format(data: &[u8]) -> Result<(u32, u8, u32)> {
        let mut decoder = {
            let mut d = Decoder::new();
            d.init();
            d
        };
        let mut pcm = vec![0i16; mp3_ffi::MAX_SAMPLES_PER_FRAME];
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
                return Ok((fi.sample_rate, fi.channels_num(), fi.bitrate));
            }
        }
        Err(Error::InvalidState)
    }

    pub fn estimate_duration_ms(head: &[u8], file_size: usize) -> u64 {
        if file_size == 0 {
            return 0;
        }
        let bitrate_bps = match Self::probe_format(head) {
            Ok((_rate, _ch, bitrate)) => bitrate as u64 * 1000,
            Err(_) => return 0,
        };
        if bitrate_bps == 0 {
            return 0;
        }
        file_size as u64 * 8 * 1000 / bitrate_bps
    }

    pub fn tick(&mut self) -> Result<()> {
        if self.state != PlaybackState::Playing {
            return Ok(());
        }
        if self.audio.is_none() {
            return Ok(());
        }

        self.drain_completions();

        if self.stream_avail() < STREAM_REFILL_WATERMARK && !self.stream_at_eof() {
            self.needs_refill = true;
        }

        while self.pcm_s16.len() >= PERIOD_BYTES && self.ring_inflight < RING_SLOTS as u32 {
            self.submit_period()?;
        }

        let mut decoded = 0usize;
        while self.ring_inflight < RING_SLOTS as u32
            && decoded < DECODE_BATCH
            && self.pcm_s16.len() < PERIOD_BYTES
        {
            if self.stream_avail() == 0 {
                if self.stream_at_eof() {
                    break;
                }
                return Ok(());
            }
            self.decode_one_frame()?;
            decoded += 1;
        }

        while self.pcm_s16.len() >= PERIOD_BYTES && self.ring_inflight < RING_SLOTS as u32 {
            self.submit_period()?;
        }

        let at_eof = self.stream_at_eof();
        if at_eof {
            self.decode_complete = true;
        }
        if at_eof && !self.pcm_s16.is_empty() && self.ring_inflight < RING_SLOTS as u32 {
            self.submit_period()?;
        }
        if at_eof && self.pcm_s16.is_empty() && self.ring_inflight == 0 {
            self.needs_advance = true;
        }

        Ok(())
    }

    pub fn ring_saturated(&self) -> bool {
        self.state == PlaybackState::Playing && self.ring_inflight >= RING_SLOTS as u32
    }

    pub fn service_pending(&mut self) -> Result<()> {
        if self.needs_refill {
            self.needs_refill = false;
            self.ensure_stream_data()?;
        }
        if self.needs_advance {
            self.needs_advance = false;
            self.advance_to_next()?;
        }
        Ok(())
    }

    fn drain_completions(&mut self) {
        let audio = match self.audio.as_mut() {
            Some(a) => a,
            None => return,
        };
        audio.drain_completions_into(&mut self.completion_scratch);
        for (handle, result) in self.completion_scratch.drain(..) {
            self.ring_inflight = self.ring_inflight.saturating_sub(1);
            if let Some(slot) = metadata_slot_for_handle(&self.tap_metadata, handle) {
                let metadata = self.tap_metadata[slot];
                let actual = self.actual_bytes[slot];
                self.tap_metadata[slot] = TapMetadata::EMPTY;
                self.actual_bytes[slot] = 0;
                self.pcm_played += actual as u64;
                if result.is_ok() {
                    self.pcm_mono.copy_from_slice(&metadata.mono);
                    self.pcm_scope.copy_from_slice(&metadata.scope);
                    self.new_pcm_available = metadata.mono_len > 0 || metadata.scope_len > 0;
                }
            }
        }
    }

    fn decode_one_frame(&mut self) -> Result<()> {
        if self.stream_avail() == 0 {
            return Ok(());
        }
        let (consumed, info) = self
            .decoder
            .decode(&self.stream_buf[self.stream_consumed..], &mut self.pcm_frame);
        if consumed == 0 && info.is_none() {
            self.stream_consumed += 1;
            return Ok(());
        }
        self.stream_consumed += consumed;

        if let Some(fi) = info {
            let total_samples = fi.samples_produced * fi.channels_num() as usize;
            for i in 0..total_samples {
                self.pcm_s16.extend_from_slice(&self.pcm_frame[i].to_le_bytes());
            }
            self.pcm_total_decoded += (total_samples * 2) as u64;
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
        self.actual_bytes[slot] = to_copy;
        self.pcm_s16.drain(..to_copy);
        self.ring_slot = (self.ring_slot + 1) % RING_SLOTS;
        self.ring_inflight += 1;
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

    fn open_audiod_stream(&mut self) {
        let ep = match registry::subscribe_output("audiod", "main") {
            Ok(ep) => ep,
            Err(_) => {
                let _ = debug_print("cluuamp: audiod not available — direct virtio-snd mode\n");
                return;
            }
        };
        self.audiod_ep = ep;

        let req = Message::new(
            AUDIOD_STREAM_OPEN,
            [0, self.sample_rate as usize, self.channels as usize, 0, 0, 0],
            3,
        );
        let mut reply_buf = [0u8; 64];
        let bytes = match libcluu::syscall::ipc_call(ep, req.as_bytes(), &mut reply_buf) {
            Ok(n) => n,
            Err(_) => {
                self.audiod_ep = 0;
                return;
            }
        };
        let (rmsg, _) = match libcluu::ipc::parse_message(&reply_buf[..bytes]) {
            Some(parsed) => parsed,
            None => {
                self.audiod_ep = 0;
                return;
            }
        };
        if rmsg.tag.label != AUDIOD_STREAM_OPEN || rmsg.words[0] != 0 {
            self.audiod_ep = 0;
            return;
        }
        self.audiod_stream_id = rmsg.words[1] as u32;
        self.audiod_session_id = rmsg.words[2] as u32;
        let _ = debug_print(&format!(
            "cluuamp: audiod stream open id={} session={}\n",
            self.audiod_stream_id, self.audiod_session_id & 0xFF
        ));
    }

    fn send_audiod_close(&mut self) {
        if self.audiod_ep == 0 || self.audiod_stream_id == 0 {
            return;
        }
        let msg = Message::new(
            AUDIOD_STREAM_CLOSE,
            [self.audiod_session_id as usize, self.audiod_stream_id as usize, 0, 0, 0, 0],
            2,
        );
        let _ = libcluu::syscall::ipc_send(self.audiod_ep, msg.as_bytes());
    }

    fn send_audiod_pause(&mut self) {
        if self.audiod_ep == 0 || self.audiod_stream_id == 0 {
            return;
        }
        let msg = Message::new(
            AUDIOD_STREAM_PAUSE,
            [self.audiod_session_id as usize, self.audiod_stream_id as usize, 0, 0, 0, 0],
            2,
        );
        let _ = libcluu::syscall::ipc_send(self.audiod_ep, msg.as_bytes());
    }

    fn send_audiod_resume(&mut self) {
        if self.audiod_ep == 0 || self.audiod_stream_id == 0 {
            return;
        }
        let msg = Message::new(
            AUDIOD_STREAM_RESUME,
            [self.audiod_session_id as usize, self.audiod_stream_id as usize, 0, 0, 0, 0],
            2,
        );
        let _ = libcluu::syscall::ipc_send(self.audiod_ep, msg.as_bytes());
    }

    fn send_audiod_drain(&mut self) {
        if self.audiod_ep == 0 || self.audiod_stream_id == 0 {
            return;
        }
        let msg = Message::new(
            AUDIOD_STREAM_DRAIN,
            [self.audiod_session_id as usize, self.audiod_stream_id as usize, 0, 0, 0, 0],
            2,
        );
        let _ = libcluu::syscall::ipc_send(self.audiod_ep, msg.as_bytes());
    }
}
