//! Winamp-faithful spectrum analyzer: 512-point Hann-windowed FFT,
//! semitone band mapping (75 bars), Hermite interpolation, gravity +
//! exponential peak dynamics. Ported from Winamp C++ `classic_vis.cpp`
//! and `draw_sa.cpp`.

use microfft::complex::cfft_512;
use microfft::Complex32;

const FFT_SIZE: usize = 512;
const NUM_BARS: usize = 75;
const MAX_LEVEL: u8 = 15;

const DEFAULT_FALLOFF: i32 = 12;
const DEFAULT_PEAK_FALLOFF: f32 = 1.1;
const PEAK_INITIAL_VEL: f32 = 3.0;
// Full-scale sine through the Hann window yields peak-bin magnitude ≈ N/4 = 128;
const SPEC_SCALE: f32 = 2.0;
const DB_FLOOR: f32 = -60.0;
const DB_REFERENCE: f32 = 128.0;
const DB_EPSILON: f32 = 1.0e-12;

pub struct SpectrumAnalyzer {
    window: [f32; FFT_SIZE],
    fft_buffer: [Complex32; FFT_SIZE],
    magnitudes: [f32; 256],
    bar_values: [u8; NUM_BARS],
    bar_state: [i32; NUM_BARS],
    peak_state: [i32; NUM_BARS],
    peak_display: [u8; NUM_BARS],
    peak_vel: [f32; NUM_BARS],
    falloff: i32,
    peak_falloff: f32,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut window = [0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let theta = 2.0 * core::f32::consts::PI * i as f32 / FFT_SIZE as f32;
            window[i] = 0.5 - 0.5 * libm::cosf(theta);
        }
        Self {
            window,
            fft_buffer: [Complex32::new(0.0, 0.0); FFT_SIZE],
            magnitudes: [0f32; 256],
            bar_values: [0u8; NUM_BARS],
            bar_state: [0i32; NUM_BARS],
            peak_state: [0i32; NUM_BARS],
            peak_display: [0u8; NUM_BARS],
            peak_vel: [PEAK_INITIAL_VEL; NUM_BARS],
            falloff: DEFAULT_FALLOFF,
            peak_falloff: DEFAULT_PEAK_FALLOFF,
        }
    }

    pub fn process_pcm(&mut self, pcm_mono: &[f32]) {
        if pcm_mono.len() < FFT_SIZE {
            return;
        }
        let mean = pcm_mono[..FFT_SIZE].iter().sum::<f32>() / FFT_SIZE as f32;
        for i in 0..FFT_SIZE {
            self.fft_buffer[i] = Complex32::new((pcm_mono[i] - mean) * self.window[i], 0.0);
        }
        let spectrum = cfft_512(&mut self.fft_buffer);
        for i in 0..256 {
            let re = spectrum[i].re;
            let im = spectrum[i].im;
            self.magnitudes[i] = libm::sqrtf(re * re + im * im) * SPEC_SCALE;
        }
        self.compute_bands();
    }

    fn compute_bands(&mut self) {
        let bla = 255.0 / libm::powf(2.0, 75.0 / 12.0);
        for x in 0..NUM_BARS {
            let bin_f = (libm::powf(2.0, x as f32 / 12.0) - 1.0) * bla + 1.0;
            let next = (libm::powf(2.0, (x + 1) as f32 / 12.0) - 1.0) * bla + 1.0;
            let val = self.hermite_sample(bin_f, next);
            self.bar_values[x] = perceptual_level(val);
        }
    }

    fn hermite_sample(&self, bin_f: f32, next: f32) -> f32 {
        let mut bin = libm::floorf(bin_f) as i32;
        let end = (libm::floorf(next) as i32).min(255);
        if bin < 0 {
            bin = 0;
        }
        let mut value = 0.0f32;
        let mut cur_f = bin_f;
        let mut first = true;
        while bin <= end {
            let mult = if bin == end {
                next - cur_f
            } else if first {
                (bin as f32 + 1.0) - bin_f
            } else {
                1.0
            };
            let m0 = self.mag_at(bin - 1);
            let m1 = self.mag_at(bin);
            let m2 = self.mag_at(bin + 1);
            let m3 = self.mag_at(bin + 2);
            let t = cur_f - bin as f32;
            let h = hermite(t, m0, m1, m2, m3);
            value += h * mult;
            bin += 1;
            cur_f = bin as f32;
            first = false;
        }
        value
    }

    fn mag_at(&self, i: i32) -> f32 {
        if i < 0 || i >= 256 {
            0.0
        } else {
            self.magnitudes[i as usize]
        }
    }

    pub fn tick(&mut self) {
        for x in 0..NUM_BARS {
            let raw = self.bar_values[x];
            let t = x & !3;
            let v = if t + 3 < NUM_BARS {
                let a = self.bar_values[t] as u32;
                let b = self.bar_values[t + 1] as u32;
                let c = self.bar_values[t + 2] as u32;
                let d = self.bar_values[t + 3] as u32;
                ((a + b + c + d) / 4) as u8
            } else {
                raw
            };
            let v = (v >> 4).min(MAX_LEVEL);
            let v16 = (v as i32) << 4;
            let new_v = if v16 < self.bar_state[x] {
                self.bar_state[x] = (self.bar_state[x] - self.falloff).max(0);
                (self.bar_state[x] >> 4) as u8
            } else {
                self.bar_state[x] = v16;
                v
            };
            let v256 = (new_v as i32) * 256;
            if self.peak_state[x] <= v256 {
                self.peak_state[x] = v256;
                self.peak_vel[x] = PEAK_INITIAL_VEL;
            }
            self.peak_display[x] = (self.peak_state[x] / 256) as u8;
            self.peak_state[x] -= self.peak_vel[x] as i32;
            self.peak_vel[x] *= self.peak_falloff;
            if self.peak_state[x] < 0 {
                self.peak_state[x] = 0;
            }
        }
    }

    pub fn bar_height(&self, x: usize) -> u8 {
        if x >= NUM_BARS {
            return 0;
        }
        (self.bar_state[x] >> 4) as u8
    }

    pub fn peak_height(&self, x: usize) -> u8 {
        if x >= NUM_BARS {
            return 0;
        }
        self.peak_display[x]
    }

    pub const fn num_bars() -> usize {
        NUM_BARS
    }
}

fn perceptual_level(magnitude: f32) -> u8 {
    if magnitude <= DB_EPSILON {
        return 0;
    }
    let db = 20.0 * libm::log10f(magnitude / DB_REFERENCE);
    let normalized = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
    (normalized * 255.0) as u8
}

#[inline]
fn hermite(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    let c1 = 0.5 * (p2 - p0);
    let c2 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c3 = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    c3 * t * t * t + c2 * t * t + c1 * t + p1
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn sine_does_not_saturate_spectrum() {
        // Pre-fix bug: tick() clamped 0-255 band values to 15 instead of
        // shifting >>4, so a full-scale sine pegged a wide stripe of bars
        // at max. Post-fix: energy near the 440 Hz band (bar ~13, 4-group
        // 12..16), distant bars near zero, and only a narrow group may be
        // at high level.
        let mut sa = SpectrumAnalyzer::new();
        let freq = 440.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let excited = (12..16).map(|x| sa.bar_height(x)).max().unwrap_or(0);
        assert!(
            excited >= 2,
            "440 Hz band should be visible, got {}",
            excited
        );
        let far: u16 = (40..75).map(|x| sa.bar_height(x) as u16).sum();
        assert!(far <= 8, "distant bars should be near zero, got {}", far);
        let pegged = (0..75).filter(|&x| sa.bar_height(x) == 15).count();
        assert!(
            pegged <= 8,
            "only a narrow group may peg at 15, got {}",
            pegged
        );
    }

    #[test]
    fn num_bars_is_75() {
        assert_eq!(SpectrumAnalyzer::num_bars(), 75);
    }

    #[test]
    fn silence_produces_zero_bars() {
        let mut sa = SpectrumAnalyzer::new();
        let silence = [0.0f32; FFT_SIZE];
        sa.process_pcm(&silence);
        sa.tick();
        for x in 0..NUM_BARS {
            assert_eq!(sa.bar_height(x), 0, "bar {} should be 0 for silence", x);
        }
    }

    #[test]
    fn dc_offset_produces_zero_bars() {
        let mut sa = SpectrumAnalyzer::new();
        let dc = [0.5f32; FFT_SIZE];
        sa.process_pcm(&dc);
        sa.tick();
        for x in 0..NUM_BARS {
            assert_eq!(sa.bar_height(x), 0, "bar {} should be 0 for DC offset", x);
        }
    }

    #[test]
    fn pure_sine_produces_nonzero_bars() {
        let mut sa = SpectrumAnalyzer::new();
        let freq = 440.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let total: u16 = (0..NUM_BARS).map(|x| sa.bar_height(x) as u16).sum();
        assert!(total > 0, "at least some bars should be nonzero for a sine");
    }

    #[test]
    fn sine_amplitudes_preserve_visible_order_after_tick() {
        let mut levels = [0u8; 4];
        for (index, amplitude) in [1.0f32, 0.1, 0.01, 0.0].iter().enumerate() {
            let mut sa = SpectrumAnalyzer::new();
            let mut pcm = [0.0f32; FFT_SIZE];
            for i in 0..FFT_SIZE {
                let t = i as f32 / 44100.0;
                pcm[i] = amplitude * libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t);
            }
            sa.process_pcm(&pcm);
            sa.tick();
            levels[index] = (12..16).map(|x| sa.bar_height(x)).max().unwrap_or(0);
        }

        assert!(
            levels[0] > levels[1] && levels[1] > levels[2] && levels[2] > levels[3],
            "expected descending levels, got {:?}",
            levels
        );
    }

    #[test]
    fn high_frequency_sine_excites_upper_bars() {
        let mut sa = SpectrumAnalyzer::new();
        let freq = 8000.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let low_energy: u16 = (0..10).map(|x| sa.bar_height(x) as u16).sum();
        let high_energy: u16 = (60..75).map(|x| sa.bar_height(x) as u16).sum();
        assert!(
            high_energy >= low_energy,
            "high freq sine should excite upper bars more than lower. low={}, high={}",
            low_energy,
            high_energy
        );
    }

    #[test]
    fn unequal_tones_excite_their_regions_without_broad_saturation() {
        let mut sa = SpectrumAnalyzer::new();
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / 44100.0;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t)
                + 0.1 * libm::sinf(2.0 * core::f32::consts::PI * 8000.0 * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();

        let low = (12..16).map(|x| sa.bar_height(x)).max().unwrap_or(0);
        let high = (56..60).map(|x| sa.bar_height(x)).max().unwrap_or(0);
        let pegged = (0..NUM_BARS)
            .filter(|&x| sa.bar_height(x) == MAX_LEVEL)
            .count();
        assert!(
            low > high && high > 0,
            "expected unequal regions: low={low}, high={high}"
        );
        assert!(pegged <= 8, "unexpected broad saturation: {pegged} bars");
    }

    #[test]
    fn retains_latest_target_when_tick_has_no_new_frame() {
        let mut sa = SpectrumAnalyzer::new();
        sa.bar_values[..4].fill(160);

        sa.tick();
        sa.tick();

        assert_eq!(sa.bar_state[0], 160);
    }

    #[test]
    fn attack_reaches_target_immediately() {
        let mut sa = SpectrumAnalyzer::new();
        sa.bar_state[0] = 32;
        sa.bar_values[..4].fill(160);

        sa.tick();

        assert_eq!(sa.bar_state[0], 160);
    }

    #[test]
    fn decay_crosses_below_retained_lower_target() {
        let mut sa = SpectrumAnalyzer::new();
        sa.bar_state[0] = 160;
        sa.bar_values[..4].fill(128);

        sa.tick();
        sa.tick();
        sa.tick();

        assert_eq!(sa.bar_state[0], 124);
    }

    #[test]
    fn peak_height_is_snapped_bar_before_peak_decay() {
        let mut sa = SpectrumAnalyzer::new();
        sa.bar_values[..4].fill(160);

        sa.tick();

        assert_eq!(sa.peak_height(0), 10);
    }

    #[test]
    fn peak_velocity_uses_initial_velocity_and_multiplier() {
        let mut sa = SpectrumAnalyzer::new();
        sa.peak_state[0] = 10 * 256;
        sa.peak_vel[0] = PEAK_INITIAL_VEL;

        sa.tick();
        let first_drop = 10 * 256 - sa.peak_state[0];
        sa.tick();
        let second_drop = 10 * 256 - first_drop - sa.peak_state[0];

        assert_eq!(first_drop, 3);
        assert_eq!(second_drop, 3);
        assert!((sa.peak_vel[0] - 3.63).abs() < 0.001);
    }

    #[test]
    fn processed_silence_sets_zero_target_and_decays_bars() {
        let mut sa = SpectrumAnalyzer::new();
        let freq = 440.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let prior = sa.bar_state;
        sa.process_pcm(&[0.0; FFT_SIZE]);
        sa.tick();

        for x in 0..NUM_BARS {
            assert_eq!(sa.bar_values[x], 0);
            assert_eq!(sa.bar_state[x], (prior[x] - DEFAULT_FALLOFF).max(0));
        }
    }

    #[test]
    fn peak_decays_exponentially() {
        let mut sa = SpectrumAnalyzer::new();
        let freq = 1000.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let peak_initial: Vec<u8> = (0..NUM_BARS).map(|x| sa.peak_height(x)).collect();
        let max_peak_initial = peak_initial.iter().max().copied().unwrap_or(0);
        assert!(max_peak_initial > 0, "peak should be nonzero after input");
        sa.process_pcm(&[0.0; FFT_SIZE]);
        for _ in 0..200 {
            sa.tick();
        }
        let peak_late: Vec<u8> = (0..NUM_BARS).map(|x| sa.peak_height(x)).collect();
        let max_peak_late = peak_late.iter().max().copied().unwrap_or(0);
        assert!(
            max_peak_late < max_peak_initial,
            "peak should decay over time. initial={}, late={}",
            max_peak_initial,
            max_peak_late
        );
    }

    #[test]
    fn bar_height_out_of_bounds_returns_zero() {
        let sa = SpectrumAnalyzer::new();
        assert_eq!(sa.bar_height(75), 0);
        assert_eq!(sa.bar_height(100), 0);
    }

    #[test]
    fn peak_height_out_of_bounds_returns_zero() {
        let sa = SpectrumAnalyzer::new();
        assert_eq!(sa.peak_height(75), 0);
        assert_eq!(sa.peak_height(999), 0);
    }

    #[test]
    fn short_pcm_input_is_ignored() {
        let mut sa = SpectrumAnalyzer::new();
        let short = [0.5f32; 10];
        sa.process_pcm(&short);
        for x in 0..NUM_BARS {
            assert_eq!(sa.bar_height(x), 0, "short input should not produce bars");
        }
    }

    #[test]
    fn hermite_at_t_zero_returns_p1() {
        let result = hermite(0.0, 1.0, 5.0, 9.0, 13.0);
        assert!(
            (result - 5.0).abs() < 0.001,
            "hermite(0) should return p1=5.0, got {}",
            result
        );
    }

    #[test]
    fn hermite_at_t_one_returns_p2() {
        let result = hermite(1.0, 1.0, 5.0, 9.0, 13.0);
        assert!(
            (result - 9.0).abs() < 0.001,
            "hermite(1) should return p2=9.0, got {}",
            result
        );
    }

    #[test]
    fn hermite_is_symmetric_for_linear_data() {
        let result = hermite(0.5, 0.0, 1.0, 2.0, 3.0);
        assert!(
            (result - 1.5).abs() < 0.001,
            "hermite(0.5) on linear should be 1.5, got {}",
            result
        );
    }
}
