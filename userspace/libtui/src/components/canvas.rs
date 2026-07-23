//! Canvas — framed raw drawing area for custom rendering (FFT, scope, etc).
//!
//! Provides a bordered box with a title. The caller gets the inner Rect
//! and can draw arbitrary cells into the View — the Canvas handles only
//! the frame and background fill. For apps that need pixel-level control
//! (spectrum analyzers, oscilloscopes, charts) without building a full
//! component.

extern crate alloc;

use alloc::string::String;

use crate::buffer::{Cell, COLOR_DEFAULT};
use crate::layout::{Border, Drawable, Rect};
use crate::View;

pub struct Canvas {
    title: String,
    border: Border,
    border_fg: u8,
    bg: u8,
    show_border: bool,
}

impl Canvas {
    pub fn new() -> Self {
        Canvas {
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

    /// Compute the interior drawing area — where the app can draw content.
    pub fn inner(&self, area: Rect) -> Rect {
        if !self.show_border {
            return area;
        }
        let border_pad = crate::layout::Padding::all(1);
        area.inner(border_pad)
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas::new()
    }
}

impl Drawable for Canvas {
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
    fn canvas_draw_border() {
        let c = Canvas::new().title("Spectrum");
        let mut buf = View::new(20, 5);
        c.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(0, 19).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(4, 0).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(4, 19).map(|c| c.ch), Some('┘'));
    }

    #[test]
    fn canvas_title_rendered() {
        let c = Canvas::new().title("SCOPE");
        let mut buf = View::new(20, 5);
        c.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('S'));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('E'));
    }

    #[test]
    fn canvas_inner_area() {
        let c = Canvas::new();
        let area = Rect::new(0, 0, 10, 5);
        let inner = c.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 8, 3));
    }

    #[test]
    fn canvas_no_border_inner_is_same() {
        let c = Canvas::new().show_border(false);
        let area = Rect::new(0, 0, 10, 5);
        let inner = c.inner(area);
        assert_eq!(inner, area);
    }

    #[test]
    fn canvas_bg_fill() {
        let c = Canvas::new().bg(8);
        let mut buf = View::new(10, 5);
        c.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(2, 5).map(|c| c.bg), Some(8));
    }

    #[test]
    fn canvas_no_border_just_bg() {
        let c = Canvas::new().show_border(false).bg(4);
        let mut buf = View::new(5, 3);
        c.draw(Rect::new(0, 0, 5, 3), &mut buf);
        for r in 0..3 {
            for col in 0..5 {
                assert_eq!(buf.get(r, col).map(|c| c.bg), Some(4));
            }
        }
    }

    #[test]
    fn canvas_custom_border() {
        let c = Canvas::new().border(Border::double());
        let mut buf = View::new(10, 5);
        c.draw(Rect::new(0, 0, 10, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('╔'));
    }

    #[test]
    fn canvas_too_small_noop() {
        let c = Canvas::new();
        let mut buf = View::new(1, 1);
        c.draw(Rect::new(0, 0, 1, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }
}
