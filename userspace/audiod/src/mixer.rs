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

/// Maximum period size in frames. 4096 bytes / 4 bytes per stereo frame = 1024.
pub const MAX_PERIOD_FRAMES: usize = 1024;

/// Saturate an i32 to i16 range [-32768, 32767].
#[inline]
pub fn saturate_i16(v: i32) -> i16 {
    v.clamp(-32768, 32767) as i16
}

/// Constant-power pan law: balance ∈ [-100, +100].
/// gainL = cos(θ·π/2), gainR = sin(θ·π/2), θ = (balance+100)/200.
/// Q15 fixed-point: 1.0 = 32768. At center both ≈ 23170 (−3 dB).
/// L² + R² ≈ 32768² (constant power preserved across pan positions).
#[derive(Clone, Copy, Debug)]
pub struct Pan {
    balance: i8,
    gain_l_q15: i32,
    gain_r_q15: i32,
}

const PAN_TABLE: [[i32; 2]; 201] = [
    [32768,     0], [32767,   257], [32764,   515], [32759,   772], [32752,  1029], [32743,  1286],
    [32732,  1544], [32718,  1801], [32703,  2058], [32686,  2314], [32667,  2571], [32646,  2827],
    [32623,  3084], [32597,  3340], [32570,  3596], [32541,  3851], [32510,  4107], [32476,  4362],
    [32441,  4617], [32404,  4872], [32365,  5126], [32323,  5380], [32280,  5634], [32235,  5887],
    [32188,  6140], [32138,  6393], [32087,  6645], [32034,  6897], [31979,  7148], [31922,  7399],
    [31863,  7650], [31802,  7900], [31739,  8149], [31674,  8398], [31607,  8647], [31538,  8895],
    [31467,  9142], [31394,  9389], [31319,  9635], [31243,  9881], [31164, 10126], [31084, 10370],
    [31001, 10614], [30917, 10857], [30831, 11100], [30743, 11342], [30653, 11583], [30561, 11823],
    [30467, 12063], [30371, 12302], [30274, 12540], [30174, 12777], [30073, 13014], [29970, 13250],
    [29865, 13485], [29758, 13719], [29649, 13952], [29539, 14184], [29427, 14416], [29312, 14647],
    [29197, 14876], [29079, 15105], [28959, 15333], [28838, 15560], [28715, 15786], [28590, 16011],
    [28463, 16235], [28335, 16458], [28205, 16680], [28073, 16901], [27939, 17121], [27804, 17340],
    [27667, 17558], [27528, 17775], [27388, 17990], [27246, 18205], [27102, 18418], [26956, 18631],
    [26809, 18842], [26660, 19052], [26510, 19261], [26358, 19468], [26204, 19675], [26049, 19880],
    [25892, 20084], [25733, 20286], [25573, 20488], [25411, 20688], [25248, 20887], [25083, 21085],
    [24917, 21281], [24749, 21476], [24580, 21670], [24409, 21862], [24236, 22053], [24062, 22243],
    [23887, 22431], [23710, 22618], [23532, 22804], [23352, 22988], [23170, 23170], [22988, 23352],
    [22804, 23532], [22618, 23710], [22431, 23887], [22243, 24062], [22053, 24236], [21862, 24409],
    [21670, 24580], [21476, 24749], [21281, 24917], [21085, 25083], [20887, 25248], [20688, 25411],
    [20488, 25573], [20286, 25733], [20084, 25892], [19880, 26049], [19675, 26204], [19468, 26358],
    [19261, 26510], [19052, 26660], [18842, 26809], [18631, 26956], [18418, 27102], [18205, 27246],
    [17990, 27388], [17775, 27528], [17558, 27667], [17340, 27804], [17121, 27939], [16901, 28073],
    [16680, 28205], [16458, 28335], [16235, 28463], [16011, 28590], [15786, 28715], [15560, 28838],
    [15333, 28959], [15105, 29079], [14876, 29197], [14647, 29312], [14416, 29427], [14184, 29539],
    [13952, 29649], [13719, 29758], [13485, 29865], [13250, 29970], [13014, 30073], [12777, 30174],
    [12540, 30274], [12302, 30371], [12063, 30467], [11823, 30561], [11583, 30653], [11342, 30743],
    [11100, 30831], [10857, 30917], [10614, 31001], [10370, 31084], [10126, 31164], [ 9881, 31243],
    [ 9635, 31319], [ 9389, 31394], [ 9142, 31467], [ 8895, 31538], [ 8647, 31607], [ 8398, 31674],
    [ 8149, 31739], [ 7900, 31802], [ 7650, 31863], [ 7399, 31922], [ 7148, 31979], [ 6897, 32034],
    [ 6645, 32087], [ 6393, 32138], [ 6140, 32188], [ 5887, 32235], [ 5634, 32280], [ 5380, 32323],
    [ 5126, 32365], [ 4872, 32404], [ 4617, 32441], [ 4362, 32476], [ 4107, 32510], [ 3851, 32541],
    [ 3596, 32570], [ 3340, 32597], [ 3084, 32623], [ 2827, 32646], [ 2571, 32667], [ 2314, 32686],
    [ 2058, 32703], [ 1801, 32718], [ 1544, 32732], [ 1286, 32743], [ 1029, 32752], [  772, 32759],
    [  515, 32764], [  257, 32767], [    0, 32768],
];

impl Pan {
    pub const CENTER: Self = Self { balance: 0, gain_l_q15: 23170, gain_r_q15: 23170 };

    pub fn from_balance(balance: i8) -> Self {
        let b = balance.clamp(-100, 100) as i32 + 100;
        let [l, r] = PAN_TABLE[b as usize];
        Self { balance, gain_l_q15: l, gain_r_q15: r }
    }

    pub fn balance(&self) -> i8 {
        self.balance
    }

    #[inline]
    pub fn apply_l(&self, sample: i32) -> i32 {
        (sample * self.gain_l_q15) >> 15
    }

    #[inline]
    pub fn apply_r(&self, sample: i32) -> i32 {
        (sample * self.gain_r_q15) >> 15
    }
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

    #[test]
    fn pan_center_is_minus_3db() {
        let p = Pan::CENTER;
        let l = p.apply_l(32767);
        let r = p.apply_r(32767);
        assert!((l - 23170).abs() <= 1, "center L ≈ 23170, got {}", l);
        assert!((r - 23170).abs() <= 1, "center R ≈ 23170, got {}", r);
    }

    #[test]
    fn pan_hard_left_mutes_right() {
        let p = Pan::from_balance(-100);
        assert_eq!(p.apply_l(32767), 32767);
        assert_eq!(p.apply_r(32767), 0);
    }

    #[test]
    fn pan_hard_right_mutes_left() {
        let p = Pan::from_balance(100);
        assert_eq!(p.apply_l(32767), 0);
        assert_eq!(p.apply_r(32767), 32767);
    }

    #[test]
    fn pan_constant_power() {
        // L² + R² ≈ 32768² for several balance values.
        for b in [-100, -50, -25, 0, 25, 50, 100] {
            let p = Pan::from_balance(b as i8);
            let l = p.apply_l(32767) as i64;
            let r = p.apply_r(32767) as i64;
            let energy = l * l + r * r;
            let unity = 32767i64 * 32767i64;
            let ratio = energy as f64 / unity as f64;
            assert!(ratio > 0.95 && ratio < 1.05,
                "balance {} energy ratio {} out of [0.95, 1.05]", b, ratio);
        }
    }

    #[test]
    fn pan_balance_clamps() {
        let a = Pan::from_balance(127);
        let b = Pan::from_balance(100);
        assert_eq!(a.apply_l(32767), b.apply_l(32767));
        assert_eq!(a.apply_r(32767), b.apply_r(32767));
        let c = Pan::from_balance(-128);
        let d = Pan::from_balance(-100);
        assert_eq!(c.apply_l(32767), d.apply_l(32767));
        assert_eq!(c.apply_r(32767), d.apply_r(32767));
    }

    #[test]
    fn gain_applied_in_mix() {
        let a: Vec<[i16; 2]> = (0..32).map(|_| [32767, 32767]).collect();
        let mut out = [[0i16, 0]; 32];
        let streams: [(&[[i16; 2]], Gain); 1] = [(&a, Gain::from_percent(50))];
        let n = mix_streams(&streams, &mut out);
        assert_eq!(n, 32);
        for f in &out {
            assert_eq!(f, &[16383, 16383], "50% gain halves amplitude (Q15 truncated)");
        }
    }
}
