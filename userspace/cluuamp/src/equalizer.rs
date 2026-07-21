pub const BAND_CENTERS_HZ: [u32; 10] = [
    60, 170, 310, 600, 1_000, 3_000, 6_000, 12_000, 14_000, 16_000,
];

const MAX_CHANNELS: usize = 2;
const BAND_COUNT: usize = BAND_CENTERS_HZ.len();
const PEAKING_Q: f32 = 1.0;

#[derive(Clone, Copy)]
struct Biquad {
    b0: [f32; 4],
    b1: [f32; 4],
    b2: [f32; 4],
    a1: [f32; 4],
    a2: [f32; 4],
}

impl Biquad {
    const BYPASS: Self = Self {
        b0: [1.0, 1.0, 0.0, 0.0],
        b1: [0.0; 4],
        b2: [0.0; 4],
        a1: [0.0; 4],
        a2: [0.0; 4],
    };
}

#[derive(Clone, Copy)]
struct State {
    z1: [f32; 4],
    z2: [f32; 4],
}

impl State {
    const CLEAR: Self = Self {
        z1: [0.0; 4],
        z2: [0.0; 4],
    };
}

pub struct Equalizer {
    settings: [i8; 11],
    sample_rate: u32,
    channels: u8,
    preamp: f32,
    coefficients: [Biquad; BAND_COUNT],
    states: [State; BAND_COUNT],
}

impl Equalizer {
    pub const fn new() -> Self {
        Self {
            settings: [0; 11],
            sample_rate: 0,
            channels: 0,
            preamp: 1.0,
            coefficients: [Biquad::BYPASS; BAND_COUNT],
            states: [State::CLEAR; BAND_COUNT],
        }
    }

    pub fn configure(&mut self, settings: [i8; 11], sample_rate: u32, channels: u8) {
        let settings = settings.map(|gain| gain.clamp(-12, 12));
        let channels = channels.clamp(1, MAX_CHANNELS as u8);
        if self.settings == settings && self.sample_rate == sample_rate && self.channels == channels
        {
            return;
        }

        self.settings = settings;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.preamp = db_gain(settings[0]);
        for band in 0..BAND_COUNT {
            self.coefficients[band] =
                peaking_coefficients(BAND_CENTERS_HZ[band], sample_rate, settings[band + 1]);
        }
        self.states = [State::CLEAR; BAND_COUNT];
    }

    pub fn process_period(&mut self, source: &[u8], output: &mut [u8], enabled: bool) {
        if !enabled || self.settings == [0; 11] {
            copy_or_clear(source, output);
            return;
        }

        #[cfg(target_arch = "x86_64")]
        if self.channels == 2 {
            self.process_stereo_simd(source, output);
            return;
        }

        self.process_period_scalar_enabled(source, output);
    }

    #[cfg(test)]
    fn process_period_scalar(&mut self, source: &[u8], output: &mut [u8], enabled: bool) {
        if !enabled || self.settings == [0; 11] {
            copy_or_clear(source, output);
            return;
        }
        self.process_period_scalar_enabled(source, output);
    }

    fn process_period_scalar_enabled(&mut self, source: &[u8], output: &mut [u8]) {
        let byte_count = source.len().min(output.len()) & !1;
        for sample_offset in (0..byte_count).step_by(2) {
            let channel = (sample_offset / 2) % usize::from(self.channels);
            let mut sample =
                i16::from_le_bytes([source[sample_offset], source[sample_offset + 1]]) as f32;
            sample *= self.preamp;
            for band in 0..BAND_COUNT {
                let coefficients = self.coefficients[band];
                let state = &mut self.states[band];
                let filtered = coefficients.b0[channel] * sample + state.z1[channel];
                state.z1[channel] = coefficients.b1[channel] * sample
                    - coefficients.a1[channel] * filtered
                    + state.z2[channel];
                state.z2[channel] =
                    coefficients.b2[channel] * sample - coefficients.a2[channel] * filtered;
                sample = filtered;
            }
            store_s16(output, sample_offset, sample);
        }
        output[byte_count..].fill(0);
    }

    #[cfg(target_arch = "x86_64")]
    fn process_stereo_simd(&mut self, source: &[u8], output: &mut [u8]) {
        let byte_count = source.len().min(output.len()) & !1;
        let simd_byte_count = byte_count & !3;
        for sample_offset in (0..simd_byte_count).step_by(4) {
            let left =
                i16::from_le_bytes([source[sample_offset], source[sample_offset + 1]]) as f32;
            let right =
                i16::from_le_bytes([source[sample_offset + 2], source[sample_offset + 3]]) as f32;
            // SAFETY: Category 13 target-feature contract: this x86_64-only wrapper
            // calls an SSE2-enabled function, and x86_64 guarantees SSE2. Inputs are
            // scalar values; all coefficient and state arrays have four valid lanes.
            let samples = unsafe {
                stereo_cascade_sse2(
                    left,
                    right,
                    self.preamp,
                    &self.coefficients,
                    &mut self.states,
                )
            };
            store_s16(output, sample_offset, samples[0]);
            store_s16(output, sample_offset + 2, samples[1]);
        }
        self.process_period_scalar_enabled(
            &source[simd_byte_count..byte_count],
            &mut output[simd_byte_count..byte_count],
        );
        output[byte_count..].fill(0);
    }
}

fn copy_or_clear(source: &[u8], output: &mut [u8]) {
    let byte_count = source.len().min(output.len()) & !1;
    output[..byte_count].copy_from_slice(&source[..byte_count]);
    output[byte_count..].fill(0);
}

fn store_s16(output: &mut [u8], sample_offset: usize, sample: f32) {
    let sample = sample.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    output[sample_offset..sample_offset + 2].copy_from_slice(&sample.to_le_bytes());
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn stereo_cascade_sse2(
    left: f32,
    right: f32,
    preamp: f32,
    coefficients: &[Biquad; BAND_COUNT],
    states: &mut [State; BAND_COUNT],
) -> [f32; 4] {
    use core::arch::x86_64::{
        _mm_add_ps, _mm_loadu_ps, _mm_mul_ps, _mm_set1_ps, _mm_set_ps, _mm_storeu_ps, _mm_sub_ps,
    };

    let mut sample = _mm_mul_ps(_mm_set_ps(0.0, 0.0, right, left), _mm_set1_ps(preamp));
    for band in 0..BAND_COUNT {
        let coefficients = &coefficients[band];
        let state = &mut states[band];
        let z1 = _mm_loadu_ps(state.z1.as_ptr());
        let z2 = _mm_loadu_ps(state.z2.as_ptr());
        let filtered = _mm_add_ps(
            _mm_mul_ps(_mm_loadu_ps(coefficients.b0.as_ptr()), sample),
            z1,
        );
        let next_z1 = _mm_add_ps(
            _mm_sub_ps(
                _mm_mul_ps(_mm_loadu_ps(coefficients.b1.as_ptr()), sample),
                _mm_mul_ps(_mm_loadu_ps(coefficients.a1.as_ptr()), filtered),
            ),
            z2,
        );
        let next_z2 = _mm_sub_ps(
            _mm_mul_ps(_mm_loadu_ps(coefficients.b2.as_ptr()), sample),
            _mm_mul_ps(_mm_loadu_ps(coefficients.a2.as_ptr()), filtered),
        );
        _mm_storeu_ps(state.z1.as_mut_ptr(), next_z1);
        _mm_storeu_ps(state.z2.as_mut_ptr(), next_z2);
        sample = filtered;
    }
    let mut samples = [0.0; 4];
    _mm_storeu_ps(samples.as_mut_ptr(), sample);
    samples
}

impl Default for Equalizer {
    fn default() -> Self {
        Self::new()
    }
}

fn db_gain(gain_db: i8) -> f32 {
    libm::powf(10.0, gain_db as f32 / 20.0)
}

fn peaking_coefficients(center_hz: u32, sample_rate: u32, gain_db: i8) -> Biquad {
    if gain_db == 0 || sample_rate == 0 || center_hz.saturating_mul(2) >= sample_rate {
        return Biquad::BYPASS;
    }

    let omega = core::f32::consts::TAU * center_hz as f32 / sample_rate as f32;
    let amplitude = libm::powf(10.0, gain_db as f32 / 40.0);
    let alpha = libm::sinf(omega) / (2.0 * PEAKING_Q);
    let cosine = libm::cosf(omega);
    let a0 = 1.0 + alpha / amplitude;
    Biquad {
        b0: [
            (1.0 + alpha * amplitude) / a0,
            (1.0 + alpha * amplitude) / a0,
            0.0,
            0.0,
        ],
        b1: [-2.0 * cosine / a0, -2.0 * cosine / a0, 0.0, 0.0],
        b2: [
            (1.0 - alpha * amplitude) / a0,
            (1.0 - alpha * amplitude) / a0,
            0.0,
            0.0,
        ],
        a1: [-2.0 * cosine / a0, -2.0 * cosine / a0, 0.0, 0.0],
        a2: [
            (1.0 - alpha / amplitude) / a0,
            (1.0 - alpha / amplitude) / a0,
            0.0,
            0.0,
        ],
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::Equalizer;

    const SETTINGS_FLAT: [i8; 11] = [0; 11];

    fn sine(freq_hz: f32, sample_rate: u32, frames: usize, channels: u8) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(frames * usize::from(channels) * 2);
        for frame in 0..frames {
            let sample =
                (libm::sinf(core::f32::consts::TAU * freq_hz * frame as f32 / sample_rate as f32)
                    * 1_000.0) as i16;
            for _ in 0..channels {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
        }
        bytes
    }

    fn rms(bytes: &[u8]) -> f32 {
        let mut sum = 0.0;
        for pair in bytes.chunks_exact(2) {
            let sample = i16::from_le_bytes([pair[0], pair[1]]) as f32;
            sum += sample * sample;
        }
        libm::sqrtf(sum / (bytes.len() / 2) as f32)
    }

    fn process(settings: [i8; 11], source: &[u8], channels: u8) -> Vec<u8> {
        let mut equalizer = Equalizer::new();
        let mut output = alloc::vec![0; source.len()];
        equalizer.configure(settings, 44_100, channels);
        equalizer.process_period(source, &mut output, true);
        output
    }

    #[test]
    fn disabled_or_flat_settings_preserve_s16_bytes() {
        let source = [1, 0, 255, 127, 0, 128, 4];
        let mut output = [99; 9];
        let mut equalizer = Equalizer::new();
        equalizer.configure(SETTINGS_FLAT, 44_100, 2);

        equalizer.process_period(&source, &mut output, false);

        assert_eq!(&output[..6], &source[..6]);
        assert_eq!(&output[6..], &[0, 0, 0]);

        output.fill(99);
        equalizer.process_period(&source, &mut output, true);

        assert_eq!(&output[..6], &source[..6]);
        assert_eq!(&output[6..], &[0, 0, 0]);
    }

    #[test]
    fn positive_twelve_db_preamp_increases_low_amplitude_sine_rms() {
        let source = sine(1_000.0, 44_100, 512, 1);
        let mut settings = SETTINGS_FLAT;
        settings[0] = 12;

        let output = process(settings, &source, 1);

        assert!(rms(&output) > rms(&source) * 3.5);
    }

    #[test]
    fn boosted_one_khz_raises_matching_tone_more_than_distant_tone() {
        let mut settings = SETTINGS_FLAT;
        settings[5] = 12;
        let matching = sine(1_000.0, 44_100, 4_096, 1);
        let distant = sine(6_000.0, 44_100, 4_096, 1);

        let matching_output = process(settings, &matching, 1);
        let distant_output = process(settings, &distant, 1);

        assert!(
            rms(&matching_output[4_096..]) / rms(&matching[4_096..])
                > rms(&distant_output[4_096..]) / rms(&distant[4_096..])
        );
    }

    #[test]
    fn cut_decreases_matching_tone() {
        let mut settings = SETTINGS_FLAT;
        settings[5] = -12;
        let source = sine(1_000.0, 44_100, 4_096, 1);

        let output = process(settings, &source, 1);

        assert!(rms(&output[4_096..]) < rms(&source[4_096..]) * 0.5);
    }

    #[test]
    fn stereo_channels_keep_filter_state_independent() {
        let left = sine(1_000.0, 44_100, 1_024, 1);
        let mut source = Vec::with_capacity(left.len() * 2);
        for sample in left.chunks_exact(2) {
            source.extend_from_slice(sample);
            source.extend_from_slice(&[0, 0]);
        }
        let mut settings = SETTINGS_FLAT;
        settings[5] = 12;

        let output = process(settings, &source, 2);

        assert!(output
            .chunks_exact(4)
            .all(|frame| frame[2] == 0 && frame[3] == 0));
    }

    #[test]
    fn processing_does_not_mutate_source() {
        let source = sine(1_000.0, 44_100, 128, 1);
        let source_before = source.clone();
        let mut settings = SETTINGS_FLAT;
        settings[0] = 6;

        let _ = process(settings, &source, 1);

        assert_eq!(source, source_before);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stereo_simd_matches_scalar_across_periods() {
        let settings = [6, -4, 3, 0, 8, -12, 5, -7, 2, 9, -3];
        let mut source = Vec::new();
        for frame in 0..257_i16 {
            source.extend_from_slice(&(frame.wrapping_mul(251).wrapping_sub(30_000)).to_le_bytes());
            source
                .extend_from_slice(&(frame.wrapping_mul(-173).wrapping_add(24_000)).to_le_bytes());
        }
        let mut simd = Equalizer::new();
        let mut scalar = Equalizer::new();
        simd.configure(settings, 44_100, 2);
        scalar.configure(settings, 44_100, 2);
        let mut simd_output = alloc::vec![0; source.len()];
        let mut scalar_output = alloc::vec![0; source.len()];

        let mut offset = 0;
        for bytes in [6, 20, 4, 60, 8, 24, 16, 44, 32, 96] {
            simd.process_period(
                &source[offset..offset + bytes],
                &mut simd_output[offset..offset + bytes],
                true,
            );
            scalar.process_period_scalar(
                &source[offset..offset + bytes],
                &mut scalar_output[offset..offset + bytes],
                true,
            );
            offset += bytes;
        }
        simd.process_period(&source[offset..], &mut simd_output[offset..], true);
        scalar.process_period_scalar(&source[offset..], &mut scalar_output[offset..], true);

        assert_eq!(simd_output, scalar_output);
    }
}
