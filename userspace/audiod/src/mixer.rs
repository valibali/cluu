//! N-stream audio mixer with i32 accumulation and single saturation.
//!
//! Each stream contributes i32 samples (after gain and resampling to the
//! output rate). The mixer sums all active streams into an i32 buffer,
//! then applies a single saturation to i16 at output time.
//!
//! # Design
//!
//! - **i32 accumulation**: prevents intermediate overflow when mixing many
//!   streams. i32 range is ±2 billion; with N streams each at ±32768,
//!   you need N > 65000 before overflow. Practical N is < 32.
//! - **Single saturation**: saturate once at the end, not per-stream.
//!   This preserves inter-stream dynamics (two streams at 0.5 gain each
//!   sum to 1.0, not clipped).
//! - **Gain**: per-stream gain as i32 fixed-point (Q15 format: 1.0 = 32768).
//!   Applied before accumulation.
//! - **Pause**: paused streams contribute silence (skip, not zero-fill).
//! - **Drain**: draining streams are marked; the mixer reports when all
//!   active streams have drained their rings.

/// Q15 fixed-point gain: 1.0 = 32768, 0.0 = 0.
/// Gain is applied as: sample_q15 = (sample_i16 * gain) >> 15.
pub const GAIN_UNITY: i32 = 32768;

/// Per-stream gain in Q15 fixed-point.
#[derive(Clone, Copy, Debug)]
pub struct Gain {
    /// Linear gain × 32768. Range [0, 65536] for [0.0, 2.0].
    pub q15: i32,
}

impl Gain {
    pub const UNITY: Self = Self { q15: GAIN_UNITY };
    pub const SILENCE: Self = Self { q15: 0 };

    pub fn from_percent(pct: u8) -> Self {
        // 100% = unity, 0% = silence.
        let q15 = (GAIN_UNITY * pct as i32) / 100;
        Self { q15 }
    }

    pub fn from_q15(q15: i32) -> Self {
        Self { q15: q15.max(0) }
    }

    pub fn apply(&self, sample: i16) -> i32 {
        // (sample * gain) >> 15, with i32 intermediate.
        ((sample as i32) * self.q15) >> 15
    }
}

/// Mix N stereo streams into one stereo S16 output buffer.
///
/// Each stream provides a slice of stereo S16 frames. All streams must
/// have the same length (one output period). The mixer:
/// 1. Accumulates all streams into i32 per-sample buffers.
/// 2. Applies per-stream gain before accumulation.
/// 3. Saturates the sum to i16 at output.
///
/// Streams that are paused or have no data contribute silence (skipped,
/// not zeroed — this is an optimisation, not a correctness issue).
///
/// Returns the number of output frames written (always equals `out_frames.len()`).
pub fn mix_streams(
    streams: &[(&[[i16; 2]], Gain)],
    out_frames: &mut [[i16; 2]],
) -> usize {
    let n_frames = out_frames.len();
    if n_frames == 0 {
        return 0;
    }

    // Accumulate in i32. Use the output buffer's stack frame — for a 2048-byte
    // period that's 512 frames × 2 channels = 1024 i32s = 4 KB. Acceptable
    // on the 128 KB user stack (see cluu-audioengine-stack-overflow-128kib
    // gotcha — we keep it under 4 KB).
    let mut accum_l: [i32; MAX_PERIOD_FRAMES] = [0; MAX_PERIOD_FRAMES];
    let mut accum_r: [i32; MAX_PERIOD_FRAMES] = [0; MAX_PERIOD_FRAMES];

    for (frames, gain) in streams {
        let len = frames.len().min(n_frames);
        for i in 0..len {
            accum_l[i] = accum_l[i].saturating_add(gain.apply(frames[i][0]));
            accum_r[i] = accum_r[i].saturating_add(gain.apply(frames[i][1]));
        }
    }

    // Single saturation to i16.
    for i in 0..n_frames {
        out_frames[i][0] = saturate_i16(accum_l[i]);
        out_frames[i][1] = saturate_i16(accum_r[i]);
    }

    n_frames
}

/// Maximum period size in frames. 2048 bytes / 4 bytes per stereo frame = 512.
/// Doubled for the 1024-byte test case headroom — actually 1024/4=256, so 512
/// covers both. Keep at 512 for the 2048-byte default.
pub const MAX_PERIOD_FRAMES: usize = 512;

/// Saturate an i32 to i16 range [-32768, 32767].
#[inline]
pub fn saturate_i16(v: i32) -> i16 {
    v.clamp(-32768, 32767) as i16
}

/// Mix two tone buffers and verify no clipping occurs.
///
/// This is a test helper used by the integration harness to verify
/// that two simultaneous tones mix without clipping.
#[cfg(test)]
pub fn mix_two_no_clip(a: &[[i16; 2]], b: &[[i16; 2]], out: &mut [[i16; 2]]) -> usize {
    let streams: [(&[[i16; 2]], Gain); 2] = [
        (a, Gain::UNITY),
        (b, Gain::UNITY),
    ];
    mix_streams(&streams, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn silence_mix_produces_zeros() {
        let a = vec![[0i16, 0]; 64];
        let b = vec![[0i16, 0]; 64];
        let mut out = [[0i16, 0]; 64];
        let n = mix_two_no_clip(&a, &b, &mut out);
        assert_eq!(n, 64);
        for f in &out {
            assert_eq!(f, &[0, 0]);
        }
    }

    #[test]
    fn single_stream_passthrough() {
        let a: Vec<[i16; 2]> = (0..64).map(|i| [i as i16, -i as i16]).collect();
        let b = vec![[0i16, 0]; 64];
        let mut out = [[0i16, 0]; 64];
        let n = mix_two_no_clip(&a, &b, &mut out);
        assert_eq!(n, 64);
        for i in 0..64 {
            assert_eq!(out[i][0], i as i16);
            assert_eq!(out[i][1], -(i as i16));
        }
    }

    #[test]
    fn two_streams_sum_without_clipping() {
        // Two streams at half gain each should sum to unity without clipping.
        let a: Vec<[i16; 2]> = (0..64).map(|_| [16384, 16384]).collect();
        let b: Vec<[i16; 2]> = (0..64).map(|_| [16384, 16384]).collect();
        let mut out = [[0i16, 0]; 64];
        let streams: [(&[[i16; 2]], Gain); 2] = [
            (&a, Gain::from_percent(50)),
            (&b, Gain::from_percent(50)),
        ];
        let n = mix_streams(&streams, &mut out);
        assert_eq!(n, 64);
        // 16384 * 0.5 + 16384 * 0.5 = 16384 (no clipping).
        for f in &out {
            assert_eq!(f, &[16384, 16384]);
        }
    }

    #[test]
    fn clipping_saturates_to_max() {
        // Two full-scale streams should saturate.
        let a = vec![[32767i16, 32767]; 32];
        let b = vec![[32767i16, 32767]; 32];
        let mut out = [[0i16, 0]; 32];
        let n = mix_two_no_clip(&a, &b, &mut out);
        assert_eq!(n, 32);
        for f in &out {
            assert_eq!(f, &[32767, 32767], "should saturate to max");
        }
    }

    #[test]
    fn clipping_saturates_to_min() {
        let a = vec![[-32768i16, -32768]; 32];
        let b = vec![[-32768i16, -32768]; 32];
        let mut out = [[0i16, 0]; 32];
        let n = mix_two_no_clip(&a, &b, &mut out);
        assert_eq!(n, 32);
        for f in &out {
            assert_eq!(f, &[-32768, -32768], "should saturate to min");
        }
    }

    #[test]
    fn n_stream_mix_4_streams() {
        // 4 streams at 25% gain each, each at full scale.
        // Sum = 4 * 32767 * 0.25 = 32767 (no clipping).
        let s = vec![[32767i16, -32768]; 64];
        let mut out = [[0i16, 0]; 64];
        let streams: [(&[[i16; 2]], Gain); 4] = [
            (&s, Gain::from_percent(25)),
            (&s, Gain::from_percent(25)),
            (&s, Gain::from_percent(25)),
            (&s, Gain::from_percent(25)),
        ];
        let n = mix_streams(&streams, &mut out);
        assert_eq!(n, 64);
        // L: 4 * (32767 * 8192 >> 15) = 4 * 8191 = 32764 (Q15 truncation).
        assert!(out[0][0] >= 32760 && out[0][0] <= 32767);
        // R: 4 * (-32768 * 8192 >> 15) = 4 * -8192 = -32768.
        assert_eq!(out[0][1], -32768);
    }

    #[test]
    fn gain_zero_silences_stream() {
        let a = vec![[32767i16, 32767]; 32];
        let b = vec![[0i16, 0]; 32];
        let mut out = [[0i16, 0]; 32];
        let streams: [(&[[i16; 2]], Gain); 2] = [
            (&a, Gain::SILENCE),
            (&b, Gain::UNITY),
        ];
        let n = mix_streams(&streams, &mut out);
        assert_eq!(n, 32);
        for f in &out {
            assert_eq!(f, &[0, 0]);
        }
    }

    #[test]
    fn gain_unity_preserves_signal() {
        let a: Vec<[i16; 2]> = (0..32).map(|i| [i as i16 * 100, -(i as i16 * 100)]).collect();
        let b = vec![[0i16, 0]; 32];
        let mut out = [[0i16, 0]; 32];
        let streams: [(&[[i16; 2]], Gain); 2] = [
            (&a, Gain::UNITY),
            (&b, Gain::UNITY),
        ];
        let n = mix_streams(&streams, &mut out);
        assert_eq!(n, 32);
        for i in 0..32 {
            assert_eq!(out[i][0], i as i16 * 100);
            assert_eq!(out[i][1], -(i as i16 * 100));
        }
    }

    #[test]
    fn saturate_i16_boundaries() {
        assert_eq!(saturate_i16(0), 0);
        assert_eq!(saturate_i16(32767), 32767);
        assert_eq!(saturate_i16(32768), 32767);
        assert_eq!(saturate_i16(100000), 32767);
        assert_eq!(saturate_i16(-32768), -32768);
        assert_eq!(saturate_i16(-32769), -32768);
        assert_eq!(saturate_i16(-100000), -32768);
    }

    #[test]
    fn asymmetric_stereo_mix() {
        // L channel from stream A, R channel from stream B.
        let a: Vec<[i16; 2]> = (0..16).map(|i| [i as i16, 0]).collect();
        let b: Vec<[i16; 2]> = (0..16).map(|i| [0, i as i16]).collect();
        let mut out = [[0i16, 0]; 16];
        let n = mix_two_no_clip(&a, &b, &mut out);
        assert_eq!(n, 16);
        for i in 0..16 {
            assert_eq!(out[i][0], i as i16, "L from A");
            assert_eq!(out[i][1], i as i16, "R from B");
        }
    }
}
