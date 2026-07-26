//! Linear resampling and channel conversion for audio streams.
//!
//! All audiod streams are mixed at a fixed output format (stereo S16 at the
//! hardware rate, 44100 Hz). Producers may submit PCM at a different sample
//! rate or channel count. This module converts producer PCM to the output
//! format using linear interpolation.
//!
//! # Design
//!
//! - **Linear resampling**: interpolates between adjacent input samples.
//!   Good enough for quality; the spec says "linear resampling".
//! - **Mono → stereo**: duplicates the mono sample to both channels.
//! - **Stereo → mono**: averages L and R.
//! - **No float in the hot path**: all arithmetic is i32 to avoid FPU
//!   issues in no_std. The interpolation uses fixed-point arithmetic.
//!
//! # Continuity
//!
//! The resampler holds a fractional read position and the last input sample
//! across calls. This ensures continuous output across period boundaries —
//! no clicks or discontinuities when the producer submits PCM in chunks
//! that don't align with the output period.

/// Fixed-point fractional precision for resampling (16 bits = 65536).
const FRAC_BITS: u32 = 16;
const FRAC_ONE: u64 = 1u64 << FRAC_BITS;
const FRAC_MASK: u64 = FRAC_ONE - 1;

/// A linear resampler that converts input PCM (any rate, mono or stereo)
/// to stereo S16 at the output rate.
pub struct LinearResampler {
    in_rate: u32,
    out_rate: u32,
    channels: u8,
    /// Fractional read position in the input buffer (fixed-point).
    /// Integer part = sample index, fractional part = interpolation weight.
    frac_pos: u64,
    /// Last input sample(s) for cross-boundary interpolation.
    /// For mono: [sample, 0]. For stereo: [L, R].
    last_sample: [i16; 2],
    /// Accumulated fractional position carried across calls.
    carry: u64,
}

impl LinearResampler {
    /// Create a new resampler.
    ///
    /// `in_rate` is the producer's sample rate (e.g. 48000).
    /// `out_rate` is the hardware output rate (e.g. 44100).
    /// `channels` is the producer's channel count (1 = mono, 2 = stereo).
    pub fn new(in_rate: u32, out_rate: u32, channels: u8) -> Self {
        Self {
            in_rate,
            out_rate,
            channels,
            frac_pos: 0,
            last_sample: [0, 0],
            carry: FRAC_ONE,
        }
    }

    /// Reset the resampler to initial state (silence, no carry).
    pub fn reset(&mut self) {
        self.frac_pos = 0;
        self.last_sample = [0, 0];
        self.carry = FRAC_ONE;
    }

    /// Process input PCM (interleaved S16) and produce output stereo S16 frames.
    ///
    /// `input` is raw interleaved S16 bytes (LE). `output` receives stereo
    /// frames as `[[i16; 2]]`. Returns the number of output frames written.
    ///
    /// The resampler maintains continuity across calls: the fractional
    /// position and last sample are carried forward. The effective input
    /// is `[last_sample, input[0], input[1], ...]` so cross-boundary
    /// interpolation uses the previous buffer's final sample.
    pub fn process(&mut self, input: &[i16], output: &mut [[i16; 2]]) -> usize {
        if self.in_rate == 0 || self.out_rate == 0 {
            return 0;
        }
        let input_frames = if self.channels == 2 {
            input.len() / 2
        } else {
            input.len()
        };
        if input_frames == 0 {
            return 0;
        }

        let step = (self.in_rate as u64 * FRAC_ONE) / self.out_rate as u64;
        let eff_len = input_frames + 1;

        let mut out_idx = 0usize;
        let mut pos = self.carry;

        while out_idx < output.len() {
            let src_idx = (pos >> FRAC_BITS) as usize;
            let frac = (pos & FRAC_MASK) as u32;

            if src_idx + 1 >= eff_len {
                break;
            }

            let (cur_l, cur_r) = self.read_eff(input, src_idx);
            let (next_l, next_r) = self.read_eff(input, src_idx + 1);

            let l = interpolate(cur_l, next_l, frac);
            let r = interpolate(cur_r, next_r, frac);

            output[out_idx] = [l, r];
            out_idx += 1;
            pos += step;
        }

        let fully_consumed = (pos >> FRAC_BITS) as usize >= input_frames;
        if fully_consumed {
            let (l, r) = self.read_sample(input, input_frames - 1);
            self.last_sample = [l, r];
            self.carry = pos.saturating_sub(input_frames as u64 * FRAC_ONE);
        } else {
            self.carry = pos;
        }

        out_idx
    }

    fn read_eff(&self, input: &[i16], eff_idx: usize) -> (i16, i16) {
        if eff_idx == 0 {
            (self.last_sample[0], self.last_sample[1])
        } else {
            self.read_sample(input, eff_idx - 1)
        }
    }

    /// Read a stereo sample from the input at the given frame index.
    fn read_sample(&self, input: &[i16], frame_idx: usize) -> (i16, i16) {
        if self.channels == 2 {
            let offset = frame_idx * 2;
            if offset + 1 < input.len() {
                (input[offset], input[offset + 1])
            } else {
                (0, 0)
            }
        } else {
            // Mono → duplicate to both channels.
            if frame_idx < input.len() {
                (input[frame_idx], input[frame_idx])
            } else {
                (0, 0)
            }
        }
    }

    /// Generate silence into the output buffer. Used during underrun.
    pub fn fill_silence(output: &mut [[i16; 2]]) {
        for frame in output.iter_mut() {
            frame[0] = 0;
            frame[1] = 0;
        }
    }
}

/// Linear interpolation between two i16 samples.
/// Returns `cur + (next - cur) * frac / FRAC_ONE` with i32 arithmetic.
#[inline]
fn interpolate(cur: i16, next: i16, frac: u32) -> i16 {
    let diff = (next as i32) - (cur as i32);
    let interpolated = (cur as i32) + ((diff * frac as i32) >> FRAC_BITS);
    interpolated.clamp(-32768, 32767) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn silence_produces_zeros() {
        let input = [0i16; 256];
        let mut output = [[0i16, 0]; 128];
        let mut r = LinearResampler::new(44100, 44100, 2);
        let n = r.process(&input, &mut output);
        assert!(n > 0);
        for frame in &output[..n] {
            assert_eq!(frame, &[0, 0]);
        }
    }

    #[test]
    fn mono_to_stereo_duplicates_channel() {
        // Mono input: alternating 100, -100
        let input: Vec<i16> = (0..256).map(|i| if i % 2 == 0 { 100 } else { -100 }).collect();
        let mut output = [[0i16, 0]; 100];
        let mut r = LinearResampler::new(44100, 44100, 1);
        let n = r.process(&input, &mut output);
        assert!(n > 0);
        // Each output frame should have L == R (mono duplicated).
        for frame in &output[..n] {
            assert_eq!(frame[0], frame[1], "mono should be duplicated to both channels");
        }
    }

    #[test]
    fn passthrough_44100_preserves_signal() {
        // Same rate, stereo — should be near-lossless (step = 1.0).
        let input: Vec<i16> = (0..512).map(|i| (i as i16).wrapping_mul(100)).collect();
        let mut output = [[0i16, 0]; 200];
        let mut r = LinearResampler::new(44100, 44100, 2);
        let n = r.process(&input, &mut output);
        assert!(n > 0);
        // First output should match first input (step=1, frac=0).
        assert_eq!(output[0][0], input[0]);
        assert_eq!(output[0][1], input[1]);
        assert_eq!(output[1][0], input[2]);
    }

    #[test]
    fn resample_continuity_across_calls() {
        // 48000 → 44100 downsampling. Two consecutive calls should produce
        // continuous output — no discontinuity at the boundary.
        let mut r = LinearResampler::new(48000, 44100, 2);

        // Sine-like input: 100 * sin(2π * f * t) approximated by i16.
        let make_input = |offset: usize, len: usize| -> Vec<i16> {
            (0..len)
                .map(|i| {
                    let t = (offset + i) as f64 / 48000.0;
                    let v = (100.0 * (2.0 * core::f64::consts::PI * 440.0 * t).sin()) as i16;
                    v
                })
                .flat_map(|v| [v, v])
                .collect::<Vec<i16>>()
        };

        let input1 = make_input(0, 480);
        let input2 = make_input(480, 480);
        let mut out1 = [[0i16, 0]; 500];
        let mut out2 = [[0i16, 0]; 500];
        let n1 = r.process(&input1, &mut out1);
        let n2 = r.process(&input2, &mut out2);
        assert!(n1 > 0);
        assert!(n2 > 0);

        // Check continuity: the last sample of out1 and first of out2
        // should be close (within a few units) — no large jump.
        let last = out1[n1 - 1][0] as i32;
        let first = out2[0][0] as i32;
        let jump = (first - last).abs();
        assert!(
            jump < 50,
            "discontinuity at boundary: last={} first={} jump={}",
            last,
            first,
            jump
        );
    }

    #[test]
    fn resample_downsample_produces_fewer_frames() {
        // 48000 → 24000: output should be ~half the input frames.
        let input = vec![100i16; 480 * 2]; // 480 stereo frames
        let mut output = [[0i16, 0]; 300];
        let mut r = LinearResampler::new(48000, 24000, 2);
        let n = r.process(&input, &mut output);
        // 480 input frames → ~240 output frames (ratio 0.5).
        assert!(n > 200 && n < 260, "expected ~240 output frames, got {}", n);
    }

    #[test]
    fn resample_upsample_produces_more_frames() {
        // 22050 → 44100: output should be ~double the input frames.
        let input = vec![100i16; 240 * 2]; // 240 stereo frames
        let mut output = [[0i16, 0]; 600];
        let mut r = LinearResampler::new(22050, 44100, 2);
        let n = r.process(&input, &mut output);
        // 240 input frames → ~480 output frames (ratio 2.0).
        assert!(n > 440 && n < 520, "expected ~480 output frames, got {}", n);
    }

    #[test]
    fn fill_silence_zeroes_buffer() {
        let mut buf = [[123i16, -456]; 32];
        LinearResampler::fill_silence(&mut buf);
        for f in &buf {
            assert_eq!(f, &[0, 0]);
        }
    }

    #[test]
    fn resampler_reset_clears_state() {
        let mut r = LinearResampler::new(48000, 44100, 2);
        let input = vec![100i16; 480 * 2];
        let mut output = [[0i16, 0]; 500];
        r.process(&input, &mut output);
        r.reset();
        assert_eq!(r.carry, FRAC_ONE);
        assert_eq!(r.last_sample, [0, 0]);
    }
}
