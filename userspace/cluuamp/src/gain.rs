#[derive(Clone, Copy)]
pub struct Gain {
    volume: u8,
    balance: i8,
    channels: u8,
}

impl Gain {
    pub fn new(volume: u8, balance: i8, channels: u8) -> Self {
        Self {
            volume: volume.min(100),
            balance: balance.clamp(-50, 50),
            channels,
        }
    }
}

pub fn apply_period(source: &[u8], output: &mut [u8], gain: Gain) {
    let byte_count = source.len().min(output.len()) & !1;
    for sample_offset in (0..byte_count).step_by(2) {
        let sample = i16::from_le_bytes([source[sample_offset], source[sample_offset + 1]]);
        let channel = (sample_offset / 2) % usize::from(gain.channels.max(1));
        let balance = match (gain.channels, channel, gain.balance) {
            (2, 0, 1..=50) => 50 - i32::from(gain.balance),
            (2, 1, -50..=-1) => 50 + i32::from(gain.balance),
            _ => 50,
        };
        let scaled = i32::from(sample) * i32::from(gain.volume) * balance / 5_000;
        let clamped = scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        let sample = match i16::try_from(clamped) {
            Ok(sample) => sample,
            Err(_) if clamped < 0 => i16::MIN,
            Err(_) => i16::MAX,
        };
        output[sample_offset..sample_offset + 2].copy_from_slice(&sample.to_le_bytes());
    }
    output[byte_count..].fill(0);
}

#[cfg(test)]
mod tests {
    use super::{apply_period, Gain};

    fn samples(bytes: &[u8]) -> [i16; 2] {
        [
            i16::from_le_bytes([bytes[0], bytes[1]]),
            i16::from_le_bytes([bytes[2], bytes[3]]),
        ]
    }

    #[test]
    fn halves_stereo_samples_at_fifty_percent_volume() {
        let source = [0x20, 0x4e, 0xe0, 0xb1];
        let mut output = [0; 4];

        apply_period(&source, &mut output, Gain::new(50, 0, 2));

        assert_eq!(samples(&output), [10_000, -10_000]);
    }

    #[test]
    fn hard_left_mutes_right_stereo_channel() {
        let source = [0x20, 0x4e, 0x20, 0x4e];
        let mut output = [0; 4];

        apply_period(&source, &mut output, Gain::new(100, -50, 2));

        assert_eq!(samples(&output), [20_000, 0]);
    }

    #[test]
    fn hard_right_mutes_left_stereo_channel() {
        let source = [0x20, 0x4e, 0x20, 0x4e];
        let mut output = [0; 4];

        apply_period(&source, &mut output, Gain::new(100, 50, 2));

        assert_eq!(samples(&output), [0, 20_000]);
    }

    #[test]
    fn centered_balance_preserves_stereo_samples() {
        let source = [0x20, 0x4e, 0xe0, 0xb1];
        let mut output = [0; 4];

        apply_period(&source, &mut output, Gain::new(100, 0, 2));

        assert_eq!(output, source);
    }

    #[test]
    fn mono_ignores_balance() {
        let source = [0x20, 0x4e, 0xe0, 0xb1];
        let mut output = [0; 4];

        apply_period(&source, &mut output, Gain::new(50, 50, 1));

        assert_eq!(samples(&output), [10_000, -10_000]);
    }

    #[test]
    fn queued_raw_samples_follow_latest_controls_without_mutation() {
        let source = [0x20, 0x4e, 0x20, 0x4e];
        let mut quiet = [0; 4];
        let mut right = [0; 4];

        apply_period(&source, &mut quiet, Gain::new(50, 0, 2));
        apply_period(&source, &mut right, Gain::new(100, 50, 2));

        assert_eq!(samples(&quiet), [10_000, 10_000]);
        assert_eq!(samples(&right), [0, 20_000]);
        assert_eq!(source, [0x20, 0x4e, 0x20, 0x4e]);
    }
}
