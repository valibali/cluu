//! Winamp classic viscolor palette mapped to xterm-256-color indices.
//!
//! 16 bar levels (0-15). Gradient: green (low) -> orange (mid) -> red (high).

pub const BAR_COLORS: [u8; 16] = [
    22,  // 0  — dark green
    28,  // 1  — green
    29,  // 2  — medium green
    30,  // 3  — bright green
    34,  // 4  — light green
    41,  // 5  — yellow-green
    130, // 6  — dark orange
    166, // 7  — orange
    172, // 8  — medium orange
    208, // 9  — bright orange
    214, // 10 — light orange
    202, // 11 — red-orange
    196, // 12 — red
    203, // 13 — bright red
    160, // 14 — dark red
    52,  // 15 — very dark red
];

/// Oscilloscope trace colors — 6 shades from center.
pub const SCOPE_COLORS: [u8; 6] = [33, 75, 117, 209, 203, 196];

/// EQ response-curve gradient: red (-12 dB, bottom) → yellow-orange (0 dB,
/// middle) → green (+12 dB, top).  Maps `f` (0..=24, where 0 = -12 dB and
/// 24 = +12 dB) to an xterm-256 color index.
pub const EQ_CURVE_COLORS: [u8; 25] = [
    196, // 0  — red            (-12 dB)
    196, // 1  — red
    202, // 2  — red-orange
    202, // 3  — red-orange
    208, // 4  — dark orange
    208, // 5  — dark orange
    208, // 6  — dark orange
    214, // 7  — yellow-orange
    214, // 8  — yellow-orange
    214, // 9  — yellow-orange
    214, // 10 — yellow-orange
    214, // 11 — yellow-orange
    214, // 12 — yellow-orange   (0 dB)
    214, // 13 — yellow-orange
    220, // 14 — gold
    226, // 15 — yellow
    226, // 16 — yellow
    190, // 17 — yellow-green
    190, // 18 — yellow-green
    154, // 19 — green-yellow
    154, // 20 — green-yellow
    118, // 21 — chartreuse
    118, // 22 — chartreuse
    46,  // 23 — green
    46,  // 24 — green           (+12 dB)
];

/// EQ curve color for a given `f` (0..=24).  Clamps to range.
pub fn eq_curve_color(f: usize) -> u8 {
    EQ_CURVE_COLORS[f.min(EQ_CURVE_COLORS.len() - 1)]
}

/// Bar color for a given level (0-15). Clamps to range.
pub fn bar_color(level: u8) -> u8 {
    let idx = if level >= 16 { 15 } else { level as usize };
    BAR_COLORS[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_color_level_0_returns_first_color() {
        assert_eq!(bar_color(0), BAR_COLORS[0]);
    }

    #[test]
    fn bar_color_level_15_returns_last_color() {
        assert_eq!(bar_color(15), BAR_COLORS[15]);
    }

    #[test]
    fn bar_color_clamps_above_15() {
        assert_eq!(bar_color(16), BAR_COLORS[15]);
        assert_eq!(bar_color(255), BAR_COLORS[15]);
    }

    #[test]
    fn palette_has_16_entries() {
        assert_eq!(BAR_COLORS.len(), 16);
    }

    #[test]
    fn scope_colors_has_6_entries() {
        assert_eq!(SCOPE_COLORS.len(), 6);
    }

    #[test]
    fn eq_curve_color_at_bottom_is_red() {
        assert_eq!(eq_curve_color(0), 196);
    }

    #[test]
    fn eq_curve_color_at_zero_db_is_yellow_orange() {
        assert_eq!(eq_curve_color(12), 214);
    }

    #[test]
    fn eq_curve_color_at_top_is_green() {
        assert_eq!(eq_curve_color(24), 46);
    }

    #[test]
    fn eq_curve_color_clamps_above_24() {
        assert_eq!(eq_curve_color(25), EQ_CURVE_COLORS[24]);
        assert_eq!(eq_curve_color(255), EQ_CURVE_COLORS[24]);
    }

    #[test]
    fn eq_curve_colors_has_25_entries() {
        assert_eq!(EQ_CURVE_COLORS.len(), 25);
    }

    #[test]
    fn bottom_row_is_green() {
        let bottom = bar_color(2);
        assert!(bottom >= 22 && bottom <= 48, "bottom should be green, got {}", bottom);
    }

    #[test]
    fn middle_row_is_orange() {
        let middle = bar_color(7);
        assert!(middle >= 130 && middle <= 220, "middle should be orange, got {}", middle);
    }

    #[test]
    fn top_row_is_red() {
        let top = bar_color(12);
        assert!(top >= 52 && top <= 203, "top should be red, got {}", top);
    }

    #[test]
    fn all_palette_indices_are_valid_256_colors() {
        for &c in BAR_COLORS.iter() {
            assert!(c <= 255, "color {} > 255", c);
        }
        for &c in SCOPE_COLORS.iter() {
            assert!(c <= 255, "scope color {} > 255", c);
        }
    }
}
