//! PixelArea — framed area for compositor-backed direct pixel display.
//!
//! A bordered box with a title that reserves space in the TUI layout for
//! real framebuffer pixels. The widget renders only the frame (border +
//! title) in the cell grid; the app is responsible for registering a
//! compositor pixel region (via `libcluu::pixel_region::PixelRegion` and
//! `COMP_WIN_SET_PIXEL_REGION_LABEL`) covering the widget's inner area.
//!
//! Pixel resolution: `inner_width * GLYPH_W` × `inner_height * GLYPH_H`
//! where GLYPH_W=8, GLYPH_H=16 (the compositor's font cell size).

extern crate alloc;

use alloc::string::String;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Border, Drawable, Rect};
use crate::View;

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;

pub struct PixelArea {
    title: String,
    border: Border,
    border_fg: u8,
    bg: u8,
    show_border: bool,
}

impl PixelArea {
    pub fn new() -> Self {
        PixelArea {
            title: String::new(),
            border: Border::single(),
            border_fg: 238,
            bg: COLOR_DEFAULT,
            show_border: true,
        }
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = String::from(title);
        self
    }

    pub fn border(mut self, border: Border) -> Self {
        self.border = border;
        self
    }

    pub fn border_fg(mut self, fg: u8) -> Self {
        self.border_fg = fg;
        self
    }

    pub fn bg(mut self, bg: u8) -> Self {
        self.bg = bg;
        self
    }

    pub fn show_border(mut self, show: bool) -> Self {
        self.show_border = show;
        self
    }

    pub fn inner(&self, area: Rect) -> Rect {
        if !self.show_border {
            return area;
        }
        let border_pad = crate::layout::Padding::all(1);
        area.inner(border_pad)
    }

    /// Pixel width of the interior area (`inner_width * GLYPH_W`).
    pub fn pixel_width(&self, area: Rect) -> usize {
        self.inner(area).width * GLYPH_W
    }

    /// Pixel height of the interior area (`inner_height * GLYPH_H`).
    pub fn pixel_height(&self, area: Rect) -> usize {
        self.inner(area).height * GLYPH_H
    }
}

impl Default for PixelArea {
    fn default() -> Self {
        PixelArea::new()
    }
}

impl Drawable for PixelArea {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width < 2 || area.height < 2 {
            return;
        }

        if self.bg != COLOR_DEFAULT {
            buf.fill_rect(area.y, area.x, area.width, area.height, Cell::new(' ').bg(self.bg));
        }

        if !self.show_border {
            return;
        }

        let last_row = area.y + area.height - 1;
        let last_col = area.x + area.width - 1;

        buf.set(area.y, area.x, Cell::new(self.border.top_left).fg(self.border_fg));
        buf.set(area.y, last_col, Cell::new(self.border.top_right).fg(self.border_fg));
        buf.set(last_row, area.x, Cell::new(self.border.bottom_left).fg(self.border_fg));
        buf.set(last_row, last_col, Cell::new(self.border.bottom_right).fg(self.border_fg));

        for x in (area.x + 1)..last_col {
            buf.set(area.y, x, Cell::new(self.border.top).fg(self.border_fg));
            buf.set(last_row, x, Cell::new(self.border.bottom).fg(self.border_fg));
        }
        for y in (area.y + 1)..last_row {
            buf.set(y, area.x, Cell::new(self.border.left).fg(self.border_fg));
            buf.set(y, last_col, Cell::new(self.border.right).fg(self.border_fg));
        }

        if !self.title.is_empty() && area.width > 4 {
            let title_start = area.x + 2;
            for (i, ch) in self.title.chars().enumerate() {
                if title_start + i >= last_col - 1 { break; }
                buf.set(area.y, title_start + i, Cell::new(ch).fg(self.border_fg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Drawable;

    #[test]
    fn pixel_area_draw_border() {
        let pa = PixelArea::new().title("Image");
        let mut buf = View::new(20, 5);
        pa.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(0, 19).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(4, 0).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(4, 19).map(|c| c.ch), Some('┘'));
    }

    #[test]
    fn pixel_area_title_rendered() {
        let pa = PixelArea::new().title("PREVIEW");
        let mut buf = View::new(20, 5);
        pa.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('P'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('V'));
    }

    #[test]
    fn pixel_area_inner_area() {
        let pa = PixelArea::new();
        let area = Rect::new(0, 0, 10, 5);
        let inner = pa.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 8, 3));
    }

    #[test]
    fn pixel_area_pixel_dims() {
        let pa = PixelArea::new();
        let area = Rect::new(0, 0, 10, 5);
        assert_eq!(pa.pixel_width(area), 8 * GLYPH_W);
        assert_eq!(pa.pixel_height(area), 3 * GLYPH_H);
    }

    #[test]
    fn pixel_area_no_border_inner_is_same() {
        let pa = PixelArea::new().show_border(false);
        let area = Rect::new(0, 0, 10, 5);
        let inner = pa.inner(area);
        assert_eq!(inner, area);
    }

    #[test]
    fn pixel_area_no_border_pixel_dims() {
        let pa = PixelArea::new().show_border(false);
        let area = Rect::new(0, 0, 10, 5);
        assert_eq!(pa.pixel_width(area), 10 * GLYPH_W);
        assert_eq!(pa.pixel_height(area), 5 * GLYPH_H);
    }

    #[test]
    fn pixel_area_bg_fill() {
        let pa = PixelArea::new().bg(8);
        let mut buf = View::new(10, 5);
        pa.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(2, 5).map(|c| c.bg), Some(8));
    }

    #[test]
    fn pixel_area_custom_border() {
        let pa = PixelArea::new().border(Border::double());
        let mut buf = View::new(10, 5);
        pa.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('╔'));
    }

    #[test]
    fn pixel_area_too_small_noop() {
        let pa = PixelArea::new();
        let mut buf = View::new(1, 1);
        pa.draw(Rect::new(0, 0, 1, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }
}
