//! Three-window Winamp layout (spec §1/§8): MAIN (rows 0-9, fixed),
//! EQUALIZER (6 rows, toggleable), PLAYLIST (rest, toggleable),
//! footer on the last row. Positions are for width >= 76, height >= 25.

/// Focus areas cycle with Tab; hidden windows are skipped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusArea {
    Transport,
    Volume,
    Balance,
    Position,
    Eq,
    Playlist,
}

impl FocusArea {
    /// Next focus area, skipping Eq/Playlist when their window is hidden.
    pub fn next(self, show_eq: bool, show_playlist: bool) -> Self {
        let mut f = self;
        loop {
            f = match f {
                FocusArea::Transport => FocusArea::Volume,
                FocusArea::Volume => FocusArea::Balance,
                FocusArea::Balance => FocusArea::Position,
                FocusArea::Position => FocusArea::Eq,
                FocusArea::Eq => FocusArea::Playlist,
                FocusArea::Playlist => FocusArea::Transport,
            };
            let hidden =
                (f == FocusArea::Eq && !show_eq) || (f == FocusArea::Playlist && !show_playlist);
            if !hidden {
                return f;
            }
        }
    }
}

pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub show_eq: bool,
    pub show_playlist: bool,
    // MAIN window (rows 0-9, fixed)
    pub main_title_row: usize,  // 0
    pub time_top: usize,        // 1 (3 rows tall)
    pub time_col: usize,        // 3 (20 cols wide)
    pub state_glyph_row: usize, // 2
    pub state_glyph_col: usize, // 1
    pub vis_top: usize,         // 1
    pub vis_left: usize,        // 24
    pub vis_width: usize,       // 24
    pub vis_height: usize,      // 3
    pub marquee_row: usize,     // 1
    pub marquee_col: usize,     // 49
    pub marquee_width: usize,   // width - 50
    pub info_row: usize,        // 2 (kbps/khz)
    pub stereo_row: usize,      // 3 (mono/STEREO)
    pub sliders_row: usize,     // 5
    pub position_row: usize,    // 7
    pub transport_row: usize,   // 9
    // EQUALIZER window (7 rows; fields valid only when show_eq)
    pub eq_title_row: usize,
    pub eq_buttons_row: usize,
    pub eq_slider_top: usize, // 3 rows tall
    pub eq_labels_row: usize,
    pub eq_band_x: [usize; 11],
    // PLAYLIST window (fields valid only when show_playlist)
    pub pl_title_row: usize,
    pub playlist_top: usize,
    pub playlist_height: usize,
    pub pl_buttons_row: usize, // height - 2
    // always
    pub footer_row: usize,    // height - 1
    pub scrollbar_col: usize, // width - 1
}

impl Layout {
    pub fn calculate(width: usize, height: usize, show_eq: bool, show_playlist: bool) -> Self {
        let mut eq_band_x = [0usize; 11];
        for i in 0..11 {
            eq_band_x[i] = 3 + i * 7;
        }
        let mut next_row = 10; // first row after MAIN
        let (eq_title_row, eq_buttons_row, eq_slider_top, eq_labels_row) = if show_eq {
            let t = next_row;
            next_row += 7;
            (t, t + 1, t + 3, t + 6)
        } else {
            (0, 0, 0, 0)
        };
        let pl_buttons_row = height.saturating_sub(2);
        let (pl_title_row, playlist_top, playlist_height) = if show_playlist {
            let t = next_row;
            let top = t + 1;
            (t, top, pl_buttons_row.saturating_sub(top))
        } else {
            (0, 0, 0)
        };
        Layout {
            width,
            height,
            show_eq,
            show_playlist,
            main_title_row: 0,
            time_top: 1,
            time_col: 3,
            state_glyph_row: 2,
            state_glyph_col: 1,
            vis_top: 1,
            vis_left: 24,
            vis_width: 24,
            vis_height: 3,
            marquee_row: 1,
            marquee_col: 49,
            marquee_width: width.saturating_sub(50),
            info_row: 2,
            stereo_row: 3,
            sliders_row: 5,
            position_row: 7,
            transport_row: 9,
            eq_title_row,
            eq_buttons_row,
            eq_slider_top,
            eq_labels_row,
            eq_band_x,
            pl_title_row,
            playlist_top,
            playlist_height,
            pl_buttons_row,
            footer_row: height.saturating_sub(1),
            scrollbar_col: width.saturating_sub(1),
        }
    }

    pub fn min_width() -> usize {
        76
    }

    pub fn min_height() -> usize {
        25
    }

    pub fn fits(&self) -> bool {
        self.width >= Self::min_width()
            && self.height >= Self::min_height()
            && (!self.show_playlist || self.playlist_height >= 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_visible_80x25() {
        let l = Layout::calculate(80, 25, true, true);
        assert!(l.fits());
        assert_eq!(l.main_title_row, 0);
        assert_eq!(l.time_top, 1);
        assert_eq!(l.time_col, 3);
        assert_eq!(l.vis_top, 1);
        assert_eq!(l.vis_left, 24);
        assert_eq!(l.vis_width, 24);
        assert_eq!(l.vis_height, 3);
        assert_eq!(l.marquee_row, 1);
        assert_eq!(l.marquee_col, 49);
        assert_eq!(l.marquee_width, 30);
        assert_eq!(l.info_row, 2);
        assert_eq!(l.stereo_row, 3);
        assert_eq!(l.sliders_row, 5);
        assert_eq!(l.position_row, 7);
        assert_eq!(l.transport_row, 9);
        assert_eq!(l.eq_title_row, 10);
        assert_eq!(l.eq_buttons_row, 11);
        assert_eq!(l.eq_slider_top, 13);
        assert_eq!(l.eq_labels_row, 16);
        assert_eq!(l.pl_title_row, 17);
        assert_eq!(l.playlist_top, 18);
        assert_eq!(l.pl_buttons_row, 23);
        assert_eq!(l.playlist_height, 5);
        assert_eq!(l.footer_row, 24);
        assert_eq!(l.scrollbar_col, 79);
    }

    #[test]
    fn eq_hidden_playlist_slides_up() {
        let l = Layout::calculate(80, 25, false, true);
        assert!(l.fits());
        assert_eq!(l.pl_title_row, 10);
        assert_eq!(l.playlist_top, 11);
        assert_eq!(l.playlist_height, 12);
        assert_eq!(l.pl_buttons_row, 23);
        assert_eq!(l.footer_row, 24);
    }

    #[test]
    fn playlist_hidden() {
        let l = Layout::calculate(80, 25, true, false);
        assert!(l.fits());
        assert_eq!(l.eq_title_row, 10);
        assert_eq!(l.eq_buttons_row, 11);
        assert_eq!(l.eq_slider_top, 13);
        assert_eq!(l.eq_labels_row, 16);
        assert_eq!(l.playlist_height, 0);
        assert_eq!(l.footer_row, 24);
    }

    #[test]
    fn both_hidden() {
        let l = Layout::calculate(80, 25, false, false);
        assert!(l.fits());
        assert_eq!(l.transport_row, 9);
        assert_eq!(l.footer_row, 24);
    }

    #[test]
    fn eq_band_positions() {
        let l = Layout::calculate(80, 25, true, true);
        assert_eq!(l.eq_band_x[0], 3);
        assert_eq!(l.eq_band_x[1], 10);
        assert_eq!(l.eq_band_x[10], 73);
    }

    #[test]
    fn min_size_fits_76x25() {
        let l = Layout::calculate(76, 25, true, true);
        assert!(l.fits());
    }

    #[test]
    fn too_narrow_75_does_not_fit() {
        let l = Layout::calculate(75, 25, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn too_short_24_does_not_fit() {
        let l = Layout::calculate(80, 24, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn zero_size_does_not_panic() {
        let l = Layout::calculate(0, 0, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn focus_next_full_cycle() {
        let f = FocusArea::Transport;
        assert!(f.next(true, true) == FocusArea::Volume);
        assert!(FocusArea::Position.next(true, true) == FocusArea::Eq);
        assert!(FocusArea::Eq.next(true, true) == FocusArea::Playlist);
        assert!(FocusArea::Playlist.next(true, true) == FocusArea::Transport);
    }

    #[test]
    fn focus_next_skips_hidden_eq() {
        assert!(FocusArea::Position.next(false, true) == FocusArea::Playlist);
    }

    #[test]
    fn focus_next_skips_hidden_playlist() {
        assert!(FocusArea::Eq.next(true, false) == FocusArea::Transport);
    }

    #[test]
    fn focus_next_skips_both_hidden() {
        assert!(FocusArea::Position.next(false, false) == FocusArea::Transport);
    }
}
