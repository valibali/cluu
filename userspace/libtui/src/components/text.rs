//! Text — styled word-wrapped paragraph block with alignment.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_NONE, ATTR_BOLD, ATTR_UNDERLINE, ATTR_REVERSE};
use crate::layout::{Drawable, Rect, Position};
use crate::View;

pub struct Text {
    content: String,
    fg: u8,
    bg: u8,
    bold: bool,
    underline: bool,
    reverse: bool,
    wrap: bool,
    align: Position,
}

impl Text {
    pub fn new(content: &str) -> Self {
        Text {
            content: String::from(content),
            fg: COLOR_DEFAULT,
            bg: COLOR_DEFAULT,
            bold: false,
            underline: false,
            reverse: false,
            wrap: true,
            align: Position::Left,
        }
    }

    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn bg(mut self, bg: u8) -> Self { self.bg = bg; self }
    pub fn bold(mut self) -> Self { self.bold = true; self }
    pub fn underline(mut self) -> Self { self.underline = true; self }
    pub fn reverse(mut self) -> Self { self.reverse = true; self }
    pub fn wrap(mut self, wrap: bool) -> Self { self.wrap = wrap; self }
    pub fn align(mut self, a: Position) -> Self { self.align = a; self }

    pub fn set_content(&mut self, content: &str) {
        self.content = String::from(content);
    }

    fn attrs(&self) -> u8 {
        let mut a = ATTR_NONE;
        if self.bold { a |= ATTR_BOLD; }
        if self.underline { a |= ATTR_UNDERLINE; }
        if self.reverse { a |= ATTR_REVERSE; }
        a
    }

    fn wrap_lines(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in self.content.split('\n') {
            if width == 0 {
                lines.push(String::from(paragraph));
                continue;
            }
            let mut current = String::new();
            for word in paragraph.split(' ') {
                if current.is_empty() {
                    current = String::from(word);
                } else if current.len() + 1 + word.len() <= width {
                    current.push(' ');
                    current.push_str(word);
                } else {
                    lines.push(current.clone());
                    current = String::from(word);
                }
            }
            lines.push(current);
        }
        lines
    }
}

impl Drawable for Text {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        if self.bg != COLOR_DEFAULT {
            buf.fill_rect(area.y, area.x, area.width, area.height, Cell::new(' ').bg(self.bg));
        }

        let lines = if self.wrap {
            self.wrap_lines(area.width)
        } else {
            self.content.split('\n').map(String::from).collect()
        };

        let attrs = self.attrs();

        for (row, line) in lines.iter().enumerate() {
            if row >= area.height {
                break;
            }
            let chars: Vec<char> = line.chars().collect();
            let content_w = chars.len().min(area.width);
            let x_offset = match self.align {
                Position::Left => 0,
                Position::Center => (area.width - content_w) / 2,
                Position::Right => area.width - content_w,
            };
            for (j, ch) in chars.iter().enumerate() {
                if j >= area.width {
                    break;
                }
                let col = area.x + x_offset + j;
                let row_pos = area.y + row;
                let mut cell = Cell::new(*ch).attrs(attrs);
                cell = cell.fg(self.fg);
                if self.bg != COLOR_DEFAULT {
                    cell = cell.bg(self.bg);
                }
                buf.set(row_pos, col, cell);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_basic_draw() {
        let t = Text::new("hello");
        let mut buf = View::new(10, 1);
        t.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('o'));
    }

    #[test]
    fn text_multiline() {
        let t = Text::new("ab\ncd");
        let mut buf = View::new(5, 2);
        t.draw(Rect::new(0, 0, 5, 2), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('a'));
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('c'));
    }

    #[test]
    fn text_wrap_breaks_long_line() {
        let t = Text::new("hello world foo");
        let mut buf = View::new(11, 2);
        t.draw(Rect::new(0, 0, 11, 2), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('w'));
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('f'));
    }

    #[test]
    fn text_no_wrap_truncates() {
        let t = Text::new("hello world").wrap(false);
        let mut buf = View::new(5, 1);
        t.draw(Rect::new(0, 0, 5, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('o'));
    }

    #[test]
    fn text_center_align() {
        let t = Text::new("hi").align(Position::Center);
        let mut buf = View::new(10, 1);
        t.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('i'));
    }

    #[test]
    fn text_right_align() {
        let t = Text::new("hi").align(Position::Right);
        let mut buf = View::new(10, 1);
        t.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 8).map(|c| c.ch), Some('h'));
        assert_eq!(buf.get(0, 9).map(|c| c.ch), Some('i'));
    }

    #[test]
    fn text_styled() {
        let t = Text::new("X").bold().underline().fg(3).bg(4);
        let mut buf = View::new(1, 1);
        t.draw(Rect::new(0, 0, 1, 1), &mut buf);
        let cell = buf.get(0, 0).unwrap();
        assert_eq!(cell.ch, 'X');
        assert_eq!(cell.fg, 3);
        assert_eq!(cell.bg, 4);
        assert!(cell.attrs & ATTR_BOLD != 0);
        assert!(cell.attrs & ATTR_UNDERLINE != 0);
    }

    #[test]
    fn text_clips_to_height() {
        let t = Text::new("a\nb\nc\nd\ne");
        let mut buf = View::new(5, 2);
        t.draw(Rect::new(0, 0, 5, 2), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('a'));
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('b'));
    }
}
