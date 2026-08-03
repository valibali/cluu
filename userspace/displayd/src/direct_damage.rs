use alloc::vec::Vec;

use cluu_wire::display::{DamageList, OutputInfo, Rect};

pub fn parse_damage_payload(payload: &[u8]) -> Vec<Rect> {
    let mut rects = Vec::new();
    let mut offset = 0;
    while offset + 16 <= payload.len() {
        let x = u32::from_le_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        let y = u32::from_le_bytes([
            payload[offset + 4],
            payload[offset + 5],
            payload[offset + 6],
            payload[offset + 7],
        ]);
        let w = u32::from_le_bytes([
            payload[offset + 8],
            payload[offset + 9],
            payload[offset + 10],
            payload[offset + 11],
        ]);
        let h = u32::from_le_bytes([
            payload[offset + 12],
            payload[offset + 13],
            payload[offset + 14],
            payload[offset + 15],
        ]);
        if w > 0 && h > 0 {
            rects.push(Rect { x, y, w, h });
        }
        offset += 16;
    }
    rects
}

pub fn fullscreen_damage(
    payload: &[u8],
    rects: &[Rect],
    output: OutputInfo,
) -> DamageList {
    if payload.is_empty() {
        DamageList::from_rects(&[Rect {
            x: 0,
            y: 0,
            w: output.width,
            h: output.height,
        }])
    } else {
        DamageList::from_rects(rects)
    }
}

#[cfg(test)]
mod tests {
    use cluu_wire::display::PixelFormat;

    use super::*;

    const OUTPUT: OutputInfo = OutputInfo {
        width: 1920,
        height: 1018,
        pitch: 1920 * 4,
        format: PixelFormat::Xrgb8888,
    };

    #[test]
    fn empty_payload_falls_back_to_full_frame_damage() {
        let damage = fullscreen_damage(&[], &[], OUTPUT);

        assert_eq!(
            damage.rects(),
            &[Rect {
                x: 0,
                y: 0,
                w: OUTPUT.width,
                h: OUTPUT.height,
            }]
        );
    }

    #[test]
    fn malformed_non_empty_payload_does_not_fall_back_to_full_frame_damage() {
        let payload = [1u8];
        let rects = parse_damage_payload(&payload);
        let damage = fullscreen_damage(&payload, &rects, OUTPUT);

        assert!(damage.rects().is_empty());
    }
}
