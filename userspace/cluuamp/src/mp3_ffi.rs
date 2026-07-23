//! Thin Rust FFI bindings to minimp3 (CC0 public domain).
//! Replaces nanomp3 with the original C library that has SSE2 SIMD.

use core::ffi::c_int;

#[repr(C)]
pub struct Mp3dec {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: c_int,
    free_format_bytes: c_int,
    header: [u8; 4],
    reserv_buf: [u8; 511],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Mp3decFrameInfo {
    pub frame_bytes: c_int,
    pub frame_offset: c_int,
    pub channels: c_int,
    pub hz: c_int,
    pub layer: c_int,
    pub bitrate_kbps: c_int,
}

pub const MAX_SAMPLES_PER_FRAME: usize = 1152 * 2;

extern "C" {
    fn mp3dec_init(dec: *mut Mp3dec) -> c_int;
    fn mp3dec_decode_frame(
        dec: *mut Mp3dec,
        mp3: *const u8,
        mp3_bytes: c_int,
        pcm: *mut i16,
        info: *mut Mp3decFrameInfo,
    ) -> c_int;
}

pub struct Decoder {
    dec: Mp3dec,
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            dec: Mp3dec {
                mdct_overlap: [[0.0; 288]; 2],
                qmf_state: [0.0; 960],
                reserv: 0,
                free_format_bytes: 0,
                header: [0; 4],
                reserv_buf: [0; 511],
            },
        }
    }

    pub fn init(&mut self) {
        unsafe { mp3dec_init(&mut self.dec as *mut Mp3dec); }
    }

    pub fn decode(&mut self, mp3: &[u8], pcm: &mut [i16]) -> (usize, Option<FrameInfo>) {
        let mut info = Mp3decFrameInfo::default();
        let samples = unsafe {
            mp3dec_decode_frame(
                &mut self.dec as *mut Mp3dec,
                mp3.as_ptr(),
                mp3.len() as c_int,
                pcm.as_mut_ptr(),
                &mut info as *mut Mp3decFrameInfo,
            )
        };
        if samples > 0 && info.frame_bytes > 0 {
            let fi = FrameInfo {
                sample_rate: info.hz as u32,
                channels: info.channels as u8,
                bitrate: info.bitrate_kbps as u32,
                samples_produced: samples as usize,
            };
            (info.frame_bytes as usize, Some(fi))
        } else if info.frame_bytes > 0 {
            (info.frame_bytes as usize, None)
        } else {
            (0, None)
        }
    }
}

pub struct FrameInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub bitrate: u32,
    pub samples_produced: usize,
}

impl FrameInfo {
    pub fn channels_num(&self) -> u8 {
        self.channels
    }
}
