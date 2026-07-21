//! Winamp classic viscolor palette mapped to xterm-256-color indices.
//!
//! 16 bar levels (0-15). Gradient: blue (low) -> orange (mid) -> red (high).
//! Matches the Winamp default viscolor.txt red-orange-green palette but
//! with blue replacing green for the low band.

pub const BAR_COLORS: [u8; 16] = [
    17,  // 0  — dark blue
    18,  // 1  — blue
    20,  // 2  — medium blue
    21,  // 3  — bright blue
    27,  // 4  — dodger blue
    33,  // 5  — light blue
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
    fn bottom_row_is_blue() {
        let bottom = bar_color(2);
        assert!(bottom >= 17 && bottom <= 39, "bottom should be blue, got {}", bottom);
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
