//! AudioSessionClient — caller-side helper for the AUDIO_OPEN_SESSION /
//! SUBMIT_PCM / COMPLETE / CLOSE PCM streaming IPC protocol.
//!
//! Mirrors `BlkSessionClient`. Open a session against the virtio-snd
//! driver's listen endpoint (obtained via
//! `registry::subscribe_output("snddev", "main")`), then submit PCM
//! periods and drain completions.
//!
//! PCM transfer uses shared memory (grant): at session open the driver
//! returns its space token + a grant target VA. The caller grants its
//! PCM buffer page into the driver's address space once. Each
//! `submit_grant` call sends metadata only (session_id, period_id, len);
//! the driver reads PCM from the pre-granted page.

use alloc::vec::Vec;

use crate::boot::{process_info, TOKEN_IPC};
use crate::ipc::{
    parse_message, AUDIO_CLOSE, AUDIO_COMPLETE, AUDIO_OPEN_SESSION, AUDIO_QUERY_CAPS,
    AUDIO_SUBMIT_PCM,
};
use crate::syscall::{endpoint_create, ipc_call, ipc_recv_any, ipc_send};
use crate::types::Message;
use crate::{Error, Result};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct PcmHandle(pub u64);

pub struct AudioSessionClient {
    driver_endpoint: usize,
    completion_endpoint: usize,
    session_id: u32,
    next_period_id: u64,
    pending: Vec<(PcmHandle, Result<()>)>,
    pub grant_target_va: usize,
    pub driver_space_token: usize,
    /// Actual period size in bytes, as accepted/clamped by the driver.
    /// Callers must use this for slot VA math and submit_grant lengths.
    pub period_bytes: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct PcmParams {
    pub format: u8,
    pub rate: u8,
    pub channels: u8,
    /// Requested period size in bytes. Driver may clamp; see
    /// `AudioSessionClient::period_bytes` for the actual value.
    pub period_bytes: u32,
}

impl Default for PcmParams {
    fn default() -> Self {
        Self {
            format: PCM_FMT_S16,
            rate: PCM_RATE_44100,
            channels: 2,
            period_bytes: 2048,
        }
    }
}

pub const PCM_FMT_S16: u8 = 5;
pub const PCM_RATE_5512: u8 = 0;
pub const PCM_RATE_8000: u8 = 1;
pub const PCM_RATE_11025: u8 = 2;
pub const PCM_RATE_16000: u8 = 3;
pub const PCM_RATE_22050: u8 = 4;
pub const PCM_RATE_32000: u8 = 5;
pub const PCM_RATE_44100: u8 = 6;
pub const PCM_RATE_48000: u8 = 7;
pub const PCM_RATE_64000: u8 = 8;
pub const PCM_RATE_88200: u8 = 9;
pub const PCM_RATE_96000: u8 = 10;

pub fn hz_to_rate(hz: u32) -> u8 {
    match hz {
        5512 => PCM_RATE_5512,
        8000 => PCM_RATE_8000,
        11025 => PCM_RATE_11025,
        16000 => PCM_RATE_16000,
        22050 => PCM_RATE_22050,
        32000 => PCM_RATE_32000,
        44100 => PCM_RATE_44100,
        48000 => PCM_RATE_48000,
        64000 => PCM_RATE_64000,
        88200 => PCM_RATE_88200,
        96000 => PCM_RATE_96000,
        _ => PCM_RATE_44100,
    }
}

pub fn rate_to_hz(r: u8) -> u32 {
    match r {
        PCM_RATE_5512 => 5512,
        PCM_RATE_8000 => 8000,
        PCM_RATE_11025 => 11025,
        PCM_RATE_16000 => 16000,
        PCM_RATE_22050 => 22050,
        PCM_RATE_32000 => 32000,
        PCM_RATE_44100 => 44100,
        PCM_RATE_48000 => 48000,
        PCM_RATE_64000 => 64000,
        PCM_RATE_88200 => 88200,
        PCM_RATE_96000 => 96000,
        _ => 44100,
    }
}

#[derive(Copy, Clone, Debug)]
pub struct DriverCaps {
    pub formats: u64,
    pub rates: u64,
    pub channels: u64,
}

impl DriverCaps {
    pub fn supports_format(&self, fmt: u8) -> bool {
        (self.formats & (1u64 << fmt)) != 0
    }
    pub fn supports_rate(&self, rate_hz: u32) -> bool {
        let r = hz_to_rate(rate_hz);
        (self.rates & (1u64 << r)) != 0
    }
    pub fn supports_channels(&self, ch: u8) -> bool {
        (self.channels & (1u64 << ch)) != 0
    }
}

pub fn query_driver_caps(driver_endpoint: usize) -> Result<DriverCaps> {
    let req = Message::new(AUDIO_QUERY_CAPS, [0, 0, 0, 0, 0, 0], 0);
    let mut reply_buf = [0u8; 64];
    let bytes = ipc_call(driver_endpoint, req.as_bytes(), &mut reply_buf)?;
    let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(Error::InvalidState)?;
    if rmsg.tag.label != AUDIO_QUERY_CAPS || rmsg.words[0] != 0 {
        return Err(Error::InvalidState);
    }
    Ok(DriverCaps {
        formats: rmsg.words[1] as u64,
        rates: rmsg.words[2] as u64,
        channels: rmsg.words[3] as u64,
    })
}

impl AudioSessionClient {
    pub fn open(driver_endpoint: usize, params: PcmParams) -> Result<Self> {
        let info = process_info();
        let ipc_token = info.tokens[TOKEN_IPC];
        let completion_endpoint = endpoint_create(ipc_token)?;

        let req = Message::new(
            AUDIO_OPEN_SESSION,
            [
                completion_endpoint,
                params.format as usize,
                params.rate as usize,
                params.channels as usize,
                params.period_bytes as usize,
                0,
            ],
            5,
        );
        let mut reply_buf = [0u8; 64];
        let bytes = ipc_call(driver_endpoint, req.as_bytes(), &mut reply_buf)?;
        let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(Error::InvalidState)?;
        if rmsg.tag.label != AUDIO_OPEN_SESSION || rmsg.words[0] != 0 {
            return Err(Error::InvalidState);
        }
        let session_id = rmsg.words[1] as u32;
        let driver_space_token = rmsg.words[2];
        let grant_target_va = rmsg.words[3];
        let actual_period_bytes = rmsg.words[4];
        Ok(Self {
            driver_endpoint,
            completion_endpoint,
            session_id,
            next_period_id: 1,
            pending: Vec::new(),
            grant_target_va,
            driver_space_token,
            period_bytes: actual_period_bytes,
        })
    }

    pub fn completion_endpoint(&self) -> usize {
        self.completion_endpoint
    }

    pub fn submit_grant(&mut self, page_index: usize, len: usize) -> Result<PcmHandle> {
        if len == 0 {
            return Err(Error::InvalidArgument);
        }
        let pid = self.next_period_id;
        self.next_period_id = self.next_period_id.wrapping_add(1);

        let msg = Message::new(
            AUDIO_SUBMIT_PCM,
            [self.session_id as usize, pid as usize, len, page_index, 0, 0],
            4,
        );
        ipc_send(self.driver_endpoint, msg.as_bytes())?;
        Ok(PcmHandle(pid))
    }

    pub fn wait_next_completion(&mut self) {
        let tokens = [self.completion_endpoint];
        let mut buf = [0u8; 128];
        if let Ok((_, len)) = ipc_recv_any(&tokens, &mut buf, 100) {
            if let Some((m, _)) = parse_message(&buf[..len]) {
                self.pending.push(self.decode_completion(&m));
            }
        }
    }

    pub fn drain_completions_into(&mut self, out: &mut Vec<(PcmHandle, Result<()>)>) {
        out.clear();
        core::mem::swap(out, &mut self.pending);
        let tokens = [self.completion_endpoint];
        let mut buf = [0u8; 128];
        loop {
            match ipc_recv_any(&tokens, &mut buf, 0) {
                Ok((_, len)) => {
                    if let Some((m, _)) = parse_message(&buf[..len]) {
                        out.push(self.decode_completion(&m));
                    }
                }
                Err(_) => break,
            }
        }
    }

    pub fn drain_completions(&mut self) -> Vec<(PcmHandle, Result<()>)> {
        let mut out = Vec::new();
        self.drain_completions_into(&mut out);
        out
    }

    pub fn wait_for(&mut self, handle: PcmHandle) -> Result<()> {
        let tokens = [self.completion_endpoint];
        let mut rbuf = [0u8; 128];
        loop {
            let (_, len) = ipc_recv_any(&tokens, &mut rbuf, u64::MAX)?;
            if let Some((m, _)) = parse_message(&rbuf[..len]) {
                let (h, result) = self.decode_completion(&m);
                if h == handle {
                    return result;
                }
                self.pending.push((h, result));
            }
        }
    }

    fn decode_completion(&self, m: &Message) -> (PcmHandle, Result<()>) {
        let h = PcmHandle(m.words[0] as u64);
        let result = match m.tag.label {
            AUDIO_COMPLETE => {
                let status = m.words[1] as u32;
                if status == 0x8000 {
                    Ok(())
                } else {
                    Err(Error::InvalidState)
                }
            }
            _ => Err(Error::InvalidState),
        };
        (h, result)
    }
}

impl Drop for AudioSessionClient {
    fn drop(&mut self) {
        let msg = Message::new(
            AUDIO_CLOSE,
            [self.session_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc_send(self.driver_endpoint, msg.as_bytes());
    }
}
