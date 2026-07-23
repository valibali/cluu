//! Flex-based Winamp layout (spec §1/§8): MAIN (fixed), EQUALIZER (7 rows,
//! toggleable), PLAYLIST (fills remaining, toggleable), footer (1 row).
//! All windows are bordered boxes; the main window's top section is a
//! 3-column flex row (BigText | FFT canvas | Info).

use alloc::vec;
use alloc::vec::Vec;
use libtui::layout::{Constraint, Flex, FlexItem, Padding, Rect};

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

    pub fn prev(self, show_eq: bool, show_playlist: bool) -> Self {
        let mut f = self;
        loop {
            f = match f {
                FocusArea::Transport => FocusArea::Playlist,
                FocusArea::Volume => FocusArea::Transport,
                FocusArea::Balance => FocusArea::Volume,
                FocusArea::Position => FocusArea::Balance,
                FocusArea::Eq => FocusArea::Position,
                FocusArea::Playlist => FocusArea::Eq,
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
    // Top-level areas
    pub main_area: Rect,
    pub eq_area: Rect,
    pub playlist_area: Rect,
    pub footer_area: Rect,
    // Main sub-areas
    pub main_title: Rect,
    pub three_col_area: Rect,
    pub bigtext_box: Rect,
    pub fft_box: Rect,
    pub info_box: Rect,
    pub bigtext_inner: Rect,
    pub fft_inner: Rect,
    pub info_inner: Rect,
    pub sliders_row: Rect,
    pub seekbar_row: Rect,
    pub transport_row: Rect,
    // EQ sub-areas
    pub eq_inner: Rect,
    pub eq_left: Rect,
    pub eq_graph: Rect,
    pub eq_right: Rect,
    pub eq_sliders: Rect,
    pub eq_labels: Rect,
    pub eq_band_x: [usize; 11],
    // Playlist sub-areas
    pub pl_inner: Rect,
    pub pl_content: Rect,
    pub pl_buttons: Rect,
    pub scrollbar: Rect,
    // For model compatibility
    pub marquee_width: usize,
    pub playlist_height: usize,
}

impl Layout {
    pub fn calculate(width: usize, height: usize, show_eq: bool, show_playlist: bool) -> Self {
        let full = Rect::new(0, 0, width, height);

        // Column: main(9), [eq(7)], Fill (playlist or spacer), footer(1) with gap(1).
        // The Fill always pushes the footer to the bottom; when the playlist is
        // hidden the Fill acts as a blank spacer we simply don't draw into.
        let mut col_items: Vec<FlexItem> = vec![FlexItem::new(Constraint::Length(MAIN_H))];
        if show_eq {
            col_items.push(FlexItem::new(Constraint::Length(EQ_H)));
        }
        col_items.push(FlexItem::new(Constraint::Fill(1)));
        col_items.push(FlexItem::new(Constraint::Length(FOOTER_H)));

        let col = Flex::column().gap(1).layout(full, &col_items);
        let mut idx = 0;
        let main_area = col.get(idx).copied().unwrap_or(Rect::zero());
        idx += 1;
        let eq_area = if show_eq {
            let r = col.get(idx).copied().unwrap_or(Rect::zero());
            idx += 1;
            r
        } else {
            Rect::zero()
        };
        // Fill item — playlist area when show_playlist, otherwise a blank spacer.
        let fill_area = col.get(idx).copied().unwrap_or(Rect::zero());
        idx += 1;
        let playlist_area = if show_playlist { fill_area } else { Rect::zero() };
        let footer_area = col.get(idx).copied().unwrap_or(Rect::zero());

        // Main internal: titlebar(1), 3-col(5), sliders(1), blank(1), seekbar(1), blank(1), transport(1).
        let main_rows = main_area.rows(&[
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]);
        let main_title = main_rows.get(0).copied().unwrap_or(Rect::zero());
        let three_col_area = main_rows.get(1).copied().unwrap_or(Rect::zero());
        let sliders_row = main_rows.get(2).copied().unwrap_or(Rect::zero());
        let seekbar_row = main_rows.get(4).copied().unwrap_or(Rect::zero());
        let transport_row = main_rows.get(6).copied().unwrap_or(Rect::zero());

        // 3-column flex: BigText(24), FFT(Fill), Info(32) with gap(0) for shared borders.
        let three_col = Flex::row().gap(0).layout(
            three_col_area,
            &[
                FlexItem::new(Constraint::Length(BIGTEXT_W)),
                FlexItem::new(Constraint::Fill(1)),
                FlexItem::new(Constraint::Length(INFO_W)),
            ],
        );
        let bigtext_box = three_col.get(0).copied().unwrap_or(Rect::zero());
        let fft_box = three_col.get(1).copied().unwrap_or(Rect::zero());
        let info_box = three_col.get(2).copied().unwrap_or(Rect::zero());

        let bigtext_inner = Rect::new(
            bigtext_box.x + 2,
            three_col_area.y + 1,
            bigtext_box.width.saturating_sub(3),
            three_col_area.height.saturating_sub(2),
        );
        let fft_inner = Rect::new(
            fft_box.x + 2,
            three_col_area.y + 1,
            fft_box.width.saturating_sub(3),
            three_col_area.height.saturating_sub(2),
        );
        let info_inner = Rect::new(
            info_box.x + 2,
            three_col_area.y + 1,
            info_box.width.saturating_sub(3),
            three_col_area.height.saturating_sub(2),
        );

        let marquee_width = info_inner.width;

        let eq_inner = eq_area.inner(Padding::trbl(1, 2, 1, 2));
        let eq_sections = eq_inner.rows(&[
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ]);
        let eq_top_area = eq_sections.get(0).copied().unwrap_or(Rect::zero());
        let eq_top = Flex::row().gap(1).layout(
            eq_top_area,
            &[
                FlexItem::new(Constraint::Length(12)),
                FlexItem::new(Constraint::Fill(1)),
                FlexItem::new(Constraint::Length(10)),
            ],
        );
        let eq_left = eq_top.get(0).copied().unwrap_or(Rect::zero());
        let eq_graph = eq_top.get(1).copied().unwrap_or(Rect::zero());
        let eq_right = eq_top.get(2).copied().unwrap_or(Rect::zero());
        let eq_sliders = eq_sections.get(2).copied().unwrap_or(Rect::zero());
        let eq_labels = eq_sections.get(3).copied().unwrap_or(Rect::zero());

        let mut eq_band_x = [0usize; 11];
        for i in 0..11 {
            let center = eq_inner.x + ((2 * i + 1) * eq_inner.width) / (2 * 11);
            eq_band_x[i] = center;
        }

        // Playlist internal: content(Fill) + buttons(1) inside the border.
        let pl_inner = playlist_area.inner(Padding::trbl(1, 2, 1, 2));
        let pl_rows = pl_inner.rows(&[Constraint::Fill(1), Constraint::Length(1)]);
        let pl_content = pl_rows.get(0).copied().unwrap_or(Rect::zero());
        let pl_buttons = pl_rows.get(1).copied().unwrap_or(Rect::zero());
        let scrollbar = Rect::new(
            pl_content.right().saturating_sub(1),
            pl_content.y,
            if pl_content.width > 0 { 1 } else { 0 },
            pl_content.height,
        );
        let playlist_height = pl_content.height;

        Layout {
            width,
            height,
            show_eq,
            show_playlist,
            main_area,
            eq_area,
            playlist_area,
            footer_area,
            main_title,
            three_col_area,
            bigtext_box,
            fft_box,
            info_box,
            bigtext_inner,
            fft_inner,
            info_inner,
            sliders_row,
            seekbar_row,
            transport_row,
            eq_inner,
            eq_left,
            eq_graph,
            eq_right,
            eq_sliders,
            eq_labels,
            eq_band_x,
            pl_inner,
            pl_content,
            pl_buttons,
            scrollbar,
            marquee_width,
            playlist_height,
        }
    }

    pub fn min_width() -> usize {
        MIN_W
    }

    pub fn min_height() -> usize {
        MIN_H
    }

    pub fn fits(&self) -> bool {
        self.width >= Self::min_width()
            && self.height >= Self::min_height()
            && (!self.show_playlist || self.playlist_height >= 1)
    }
}

const MIN_W: usize = 76;
const MIN_H: usize = 36;
const MAIN_H: usize = 11;
const EQ_H: usize = 10;
const FOOTER_H: usize = 1;
const BIGTEXT_W: usize = 24;
const INFO_W: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_visible_80x36() {
        let l = Layout::calculate(80, 36, true, true);
        assert!(l.fits());
        assert!(l.main_area.height > 0);
        assert!(l.eq_area.height > 0);
        assert!(l.playlist_area.height > 0);
        assert!(l.footer_area.height > 0);
        assert_eq!(l.main_area, Rect::new(0, 0, 80, 11));
        assert_eq!(l.eq_area, Rect::new(0, 12, 80, 10));
        assert_eq!(l.playlist_area, Rect::new(0, 23, 80, 11));
        assert_eq!(l.footer_area, Rect::new(0, 35, 80, 1));
        assert_eq!(l.main_title, Rect::new(0, 0, 80, 1));
        assert_eq!(l.three_col_area, Rect::new(0, 1, 80, 5));
        assert_eq!(l.bigtext_box, Rect::new(0, 1, 24, 5));
        assert_eq!(l.fft_box, Rect::new(24, 1, 24, 5));
        assert_eq!(l.info_box, Rect::new(48, 1, 32, 5));
        assert_eq!(l.bigtext_inner, Rect::new(2, 2, 21, 3));
        assert_eq!(l.fft_inner, Rect::new(26, 2, 21, 3));
        assert_eq!(l.info_inner, Rect::new(50, 2, 29, 3));
        assert_eq!(l.sliders_row, Rect::new(0, 6, 80, 1));
        assert_eq!(l.seekbar_row, Rect::new(0, 8, 80, 1));
        assert_eq!(l.transport_row, Rect::new(0, 10, 80, 1));
        assert_eq!(l.eq_inner, Rect::new(2, 13, 76, 8));
        assert_eq!(l.eq_left, Rect::new(2, 13, 12, 3));
        assert_eq!(l.eq_graph, Rect::new(15, 13, 52, 3));
        assert_eq!(l.eq_right, Rect::new(68, 13, 10, 3));
        assert_eq!(l.eq_sliders, Rect::new(2, 17, 76, 3));
        assert_eq!(l.eq_labels, Rect::new(2, 20, 76, 1));
        assert_eq!(l.pl_inner, Rect::new(2, 24, 76, 9));
        assert_eq!(l.pl_content, Rect::new(2, 24, 76, 8));
        assert_eq!(l.pl_buttons, Rect::new(2, 32, 76, 1));
        assert_eq!(l.scrollbar, Rect::new(77, 24, 1, 8));
        assert_eq!(l.marquee_width, 29);
        assert_eq!(l.playlist_height, 8);
    }

    #[test]
    fn eq_hidden_playlist_slides_up() {
        let l = Layout::calculate(80, 36, false, true);
        assert!(l.fits());
        assert_eq!(l.eq_area, Rect::zero());
        assert_eq!(l.main_area, Rect::new(0, 0, 80, 11));
        assert_eq!(l.playlist_area, Rect::new(0, 12, 80, 22));
        assert_eq!(l.footer_area, Rect::new(0, 35, 80, 1));
        assert_eq!(l.playlist_height, 19);
    }

    #[test]
    fn playlist_hidden() {
        let l = Layout::calculate(80, 36, true, false);
        assert!(l.fits());
        assert_eq!(l.playlist_area, Rect::zero());
        assert_eq!(l.playlist_height, 0);
        assert_eq!(l.eq_area, Rect::new(0, 12, 80, 10));
        assert_eq!(l.footer_area, Rect::new(0, 35, 80, 1));
    }

    #[test]
    fn both_hidden() {
        let l = Layout::calculate(80, 36, false, false);
        assert!(l.fits());
        assert_eq!(l.eq_area, Rect::zero());
        assert_eq!(l.playlist_area, Rect::zero());
        assert_eq!(l.main_area, Rect::new(0, 0, 80, 11));
        assert_eq!(l.footer_area, Rect::new(0, 35, 80, 1));
    }

    #[test]
    fn eq_band_positions_distribute_across_inner() {
        let l = Layout::calculate(80, 36, true, true);
        assert_eq!(l.eq_band_x[0], 5);
        assert_eq!(l.eq_band_x[1], 12);
        assert_eq!(l.eq_band_x[10], 74);
    }

    #[test]
    fn min_size_fits_76x36() {
        let l = Layout::calculate(76, 36, true, true);
        assert!(l.fits());
    }

    #[test]
    fn too_narrow_75_does_not_fit() {
        let l = Layout::calculate(75, 36, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn too_short_35_does_not_fit() {
        let l = Layout::calculate(80, 35, true, true);
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
