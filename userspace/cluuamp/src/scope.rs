//! Oscilloscope renderer — 75 points from 576 samples.
//! Ported from Winamp C++ `classic_vis.cpp` `makeOscData`.

const NUM_POINTS: usize = 75;
const SAMPLE_COUNT: usize = 576;

pub struct Oscilloscope {
    points: [i8; NUM_POINTS],
}

impl Oscilloscope {
    pub fn new() -> Self {
        Self {
            points: [0i8; NUM_POINTS],
        }
    }

    pub fn process_pcm(&mut self, pcm_s16: &[i16], channels: usize) {
        if channels == 0 || pcm_s16.len() < SAMPLE_COUNT * channels {
            return;
        }
        let dd = SAMPLE_COUNT as f32 / NUM_POINTS as f32;
        for x in 0..NUM_POINTS {
            let index = (x as f32 * dd) as usize;
            let mut val = 0i32;
            for c in 0..channels {
                let sample_idx = index * channels + c;
                if sample_idx < pcm_s16.len() {
                    let msb = (pcm_s16[sample_idx] >> 8) as i8;
                    val += msb as i32;
                }
            }
            val /= channels as i32;
            val = val.clamp(-32, 31);
            self.points[x] = val as i8;
        }
    }

    pub fn point(&self, x: usize) -> i8 {
        if x >= NUM_POINTS {
            return 0;
        }
        self.points[x]
    }

    pub const fn num_points() -> usize {
        NUM_POINTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_points_is_75() {
        assert_eq!(Oscilloscope::num_points(), 75);
    }

    #[test]
    fn silence_produces_zero_points() {
        let mut scope = Oscilloscope::new();
        let silence = [0i16; SAMPLE_COUNT * 2];
        scope.process_pcm(&silence, 2);
        for x in 0..NUM_POINTS {
            assert_eq!(scope.point(x), 0, "point {} should be 0 for silence", x);
        }
    }

    #[test]
    fn sine_wave_produces_nonzero_points() {
        let mut scope = Oscilloscope::new();
        let mut pcm = [0i16; SAMPLE_COUNT * 2];
        for i in 0..SAMPLE_COUNT {
            let t = i as f32 / 44100.0;
            let val = (libm::sinf(2.0 * core::f32::consts::PI * 440.0 * t) * 16384.0) as i16;
            pcm[i * 2] = val;
            pcm[i * 2 + 1] = val;
        }
        scope.process_pcm(&pcm, 2);
        let nonzero = (0..NUM_POINTS).filter(|&x| scope.point(x) != 0).count();
        assert!(nonzero > 0, "sine wave should produce some nonzero points");
    }

    #[test]
    fn point_out_of_bounds_returns_zero() {
        let scope = Oscilloscope::new();
        assert_eq!(scope.point(75), 0);
        assert_eq!(scope.point(999), 0);
    }

    #[test]
    fn short_pcm_is_ignored() {
        let mut scope = Oscilloscope::new();
        let short = [100i16; 10];
        scope.process_pcm(&short, 2);
        for x in 0..NUM_POINTS {
            assert_eq!(scope.point(x), 0, "short input should not produce points");
        }
    }

    #[test]
    fn mono_signal_works() {
        let mut scope = Oscilloscope::new();
        let mut pcm = [0i16; SAMPLE_COUNT];
        for i in 0..SAMPLE_COUNT {
            let t = i as f32 / 44100.0;
            pcm[i] = (libm::sinf(2.0 * core::f32::consts::PI * 1000.0 * t) * 16384.0) as i16;
        }
        scope.process_pcm(&pcm, 1);
        let nonzero = (0..NUM_POINTS).filter(|&x| scope.point(x) != 0).count();
        assert!(nonzero > 0, "mono sine should produce nonzero points");
    }

    #[test]
    fn points_clamp_to_range() {
        let mut scope = Oscilloscope::new();
        let mut pcm = [0i16; SAMPLE_COUNT * 2];
        for i in 0..SAMPLE_COUNT * 2 {
            pcm[i] = 32767;
        }
        scope.process_pcm(&pcm, 2);
        for x in 0..NUM_POINTS {
            let p = scope.point(x);
            assert!(
                p >= -32 && p <= 31,
                "point {} = {} should be in [-32, 31]",
                x,
                p
            );
        }
    }

    #[test]
    fn zero_channels_is_ignored() {
        let mut scope = Oscilloscope::new();
        let pcm = [100i16; SAMPLE_COUNT * 2];
        scope.process_pcm(&pcm, 0);
        for x in 0..NUM_POINTS {
            assert_eq!(scope.point(x), 0);
        }
    }
}
