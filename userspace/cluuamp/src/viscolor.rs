//! Winamp classic viscolor palette mapped to xterm-256-color indices.
//!
//! 16 bar levels (0-15) + peak color. Gradient: green (low) -> yellow -> red (high).
//! Peak rendered in white.

/// 16-entry palette for bar levels 0-15. Values are xterm 256-color indices.
pub const BAR_COLORS: [u8; 16] = [
    232, // 0  — near black
    22,  // 1  — dark green
    28,  // 2  — green
    34,  // 3  — medium green
    40,  // 4  — bright green
    46,  // 5  — lime
    70,  // 6  — yellow-green
    76,  // 7  — yellow
    190, // 8  — light yellow
    220, // 9  — gold
    214, // 10 — orange
    208, // 11 — dark orange
    202, // 12 — red-orange
    196, // 13 — red
    160, // 14 — bright red
    124, // 15 — dark red
];

/// Peak marker color (white).
pub const PEAK_COLOR: u8 = 255;

/// Oscilloscope trace colors — 6 shades from center.
pub const SCOPE_COLORS: [u8; 6] = [46, 76, 190, 220, 208, 196];

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
    fn peak_color_is_white() {
        assert_eq!(PEAK_COLOR, 255);
    }

    #[test]
    fn scope_colors_has_6_entries() {
        assert_eq!(SCOPE_COLORS.len(), 6);
    }

    #[test]
    fn gradient_progresses_green_to_red() {
        let green = bar_color(1);
        let mid = bar_color(8);
        let red = bar_color(14);
        assert_ne!(green, mid, "low and mid colors should differ");
        assert_ne!(mid, red, "mid and high colors should differ");
        assert_ne!(green, red, "low and high colors should differ");
    }

    #[test]
    fn all_palette_indices_are_valid_256_colors() {
        for &c in BAR_COLORS.iter() {
            assert!(c <= 255, "color {} > 255", c);
        }
        assert!(PEAK_COLOR <= 255);
        for &c in SCOPE_COLORS.iter() {
            assert!(c <= 255, "scope color {} > 255", c);
        }
    }
}
