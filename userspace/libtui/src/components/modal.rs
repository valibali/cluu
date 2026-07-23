//! Modal — centered overlay dialog with title, body, and buttons.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Border, Drawable, Rect, Position};
use crate::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalButton {
    Ok,
    Cancel,
    Yes,
    No,
    Custom,
}

pub struct Modal {
    title: String,
    body: String,
    buttons: Vec<(ModalButton, String)>,
    selected_button: usize,
    border_fg: u8,
    title_fg: u8,
    body_fg: u8,
    button_fg: u8,
    button_bg: u8,
    selected_fg: u8,
    selected_bg: u8,
}

impl Modal {
    pub fn new(title: &str, body: &str) -> Self {
        Modal {
            title: String::from(title),
            body: String::from(body),
            buttons: vec![(ModalButton::Ok, String::from("OK")), (ModalButton::Cancel, String::from("Cancel"))],
            selected_button: 0,
            border_fg: COLOR_DEFAULT,
            title_fg: COLOR_DEFAULT,
            body_fg: COLOR_DEFAULT,
            button_fg: COLOR_DEFAULT,
            button_bg: COLOR_DEFAULT,
            selected_fg: COLOR_DEFAULT,
            selected_bg: 4,
        }
    }

    pub fn title(mut self, t: &str) -> Self { self.title = String::from(t); self }
    pub fn body(mut self, b: &str) -> Self { self.body = String::from(b); self }
    pub fn border_fg(mut self, fg: u8) -> Self { self.border_fg = fg; self }
    pub fn title_fg(mut self, fg: u8) -> Self { self.title_fg = fg; self }
    pub fn body_fg(mut self, fg: u8) -> Self { self.body_fg = fg; self }
    pub fn selected_bg(mut self, bg: u8) -> Self { self.selected_bg = bg; self }

    pub fn add_button(mut self, btn: ModalButton, label: &str) -> Self {
        self.buttons.push((btn, String::from(label)));
        self
    }

    pub fn set_buttons(mut self, buttons: Vec<(ModalButton, &str)>) -> Self {
        self.buttons = buttons.into_iter().map(|(b, s)| (b, String::from(s))).collect();
        self.selected_button = 0;
        self
    }

    pub fn next_button(&mut self) {
        if self.selected_button + 1 < self.buttons.len() {
            self.selected_button += 1;
        }
    }

    pub fn prev_button(&mut self) {
        self.selected_button = self.selected_button.saturating_sub(1);
    }

    pub fn selected_button_kind(&self) -> Option<ModalButton> {
        self.buttons.get(self.selected_button).map(|(b, _)| *b)
    }

    pub fn preferred_size(&self) -> (usize, usize) {
        let body_lines: Vec<&str> = self.body.split('\n').collect();
        let body_w = body_lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let title_w = self.title.chars().count();
        let buttons_w: usize = self.buttons.iter().map(|(_, l)| l.chars().count() + 4).sum::<usize>() + self.buttons.len().saturating_sub(1);
        let width = body_w.max(title_w).max(buttons_w).max(20) + 4;
        let height = body_lines.len() + 4;
        (width, height)
    }
}

impl Drawable for Modal {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width < 4 || area.height < 4 {
            return;
        }

        let last_row = area.y + area.height - 1;
        let last_col = area.x + area.width - 1;

        buf.set(area.y, area.x, Cell::new('┌').fg(self.border_fg));
        buf.set(area.y, last_col, Cell::new('┐').fg(self.border_fg));
        buf.set(last_row, area.x, Cell::new('└').fg(self.border_fg));
        buf.set(last_row, last_col, Cell::new('┘').fg(self.border_fg));

        for x in (area.x + 1)..last_col {
            buf.set(area.y, x, Cell::new('─').fg(self.border_fg));
            buf.set(last_row, x, Cell::new('─').fg(self.border_fg));
        }
        for y in (area.y + 1)..last_row {
            buf.set(y, area.x, Cell::new('│').fg(self.border_fg));
            buf.set(y, last_col, Cell::new('│').fg(self.border_fg));
        }

        if area.width > 4 {
            let title_start = area.x + 2;
            for (i, ch) in self.title.chars().enumerate() {
                if title_start + i >= last_col - 1 { break; }
                buf.set(area.y, title_start + i, Cell::new(ch).fg(self.title_fg).attrs(ATTR_BOLD));
            }
        }

        let body_start_y = area.y + 1;
        for (row, line) in self.body.split('\n').enumerate() {
            let y = body_start_y + row;
            if y >= last_row { break; }
            for (i, ch) in line.chars().enumerate() {
                if area.x + 1 + i >= last_col { break; }
                buf.set(y, area.x + 1 + i, Cell::new(ch).fg(self.body_fg));
            }
        }

        let button_y = last_row - 1;
        let total_button_w: usize = self.buttons.iter().map(|(_, l)| l.chars().count() + 4).sum::<usize>() + self.buttons.len().saturating_sub(1);
        let mut x = area.x + 1 + (area.width.saturating_sub(2).saturating_sub(total_button_w)) / 2;

        for (i, (_, label)) in self.buttons.iter().enumerate() {
            let is_selected = i == self.selected_button;
            let bg = if is_selected { self.selected_bg } else { self.button_bg };
            let fg = if is_selected { self.selected_fg } else { self.button_fg };

            buf.set(button_y, x, Cell::new('[').fg(fg).bg(bg).attrs(ATTR_BOLD));
            x += 1;
            buf.set(button_y, x, Cell::new(' ').fg(fg).bg(bg));
            x += 1;
            for ch in label.chars() {
                buf.set(button_y, x, Cell::new(ch).fg(fg).bg(bg).attrs(ATTR_BOLD));
                x += 1;
            }
            buf.set(button_y, x, Cell::new(' ').fg(fg).bg(bg));
            x += 1;
            buf.set(button_y, x, Cell::new(']').fg(fg).bg(bg).attrs(ATTR_BOLD));
            x += 1;
            if i + 1 < self.buttons.len() {
                x += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_new_has_default_buttons() {
        let m = Modal::new("Test", "Body");
        assert_eq!(m.buttons.len(), 2);
        assert_eq!(m.selected_button_kind(), Some(ModalButton::Ok));
    }

    #[test]
    fn modal_next_prev_button() {
        let mut m = Modal::new("T", "B");
        m.next_button();
        assert_eq!(m.selected_button_kind(), Some(ModalButton::Cancel));
        m.prev_button();
        assert_eq!(m.selected_button_kind(), Some(ModalButton::Ok));
    }

    #[test]
    fn modal_next_at_end_noop() {
        let mut m = Modal::new("T", "B");
        m.next_button();
        m.next_button();
        assert_eq!(m.selected_button, 1);
    }

    #[test]
    fn modal_custom_buttons() {
        let m = Modal::new("T", "B").set_buttons(vec![(ModalButton::Yes, "Yes"), (ModalButton::No, "No")]);
        assert_eq!(m.buttons.len(), 2);
        assert_eq!(m.selected_button_kind(), Some(ModalButton::Yes));
    }

    #[test]
    fn modal_draw_border() {
        let m = Modal::new("Hi", "World");
        let mut buf = View::new(20, 6);
        m.draw(Rect::new(0, 0, 20, 6), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(0, 19).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(5, 0).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(5, 19).map(|c| c.ch), Some('┘'));
    }

    #[test]
    fn modal_draw_title() {
        let m = Modal::new("Warning", "Body");
        let mut buf = View::new(20, 6);
        m.draw(Rect::new(0, 0, 20, 6), &mut buf);
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('W'));
    }

    #[test]
    fn modal_draw_body() {
        let m = Modal::new("T", "Hello");
        let mut buf = View::new(20, 6);
        m.draw(Rect::new(0, 0, 20, 6), &mut buf);
        assert_eq!(buf.get(1, 1).map(|c| c.ch), Some('H'));
    }

    #[test]
    fn modal_draw_selected_button() {
        let m = Modal::new("T", "B");
        let mut buf = View::new(20, 6);
        m.draw(Rect::new(0, 0, 20, 6), &mut buf);
        assert_eq!(buf.get(4, 5).map(|c| c.bg), Some(4));
    }

    #[test]
    fn modal_preferred_size() {
        let m = Modal::new("Test", "Hello World");
        let (w, h) = m.preferred_size();
        assert!(w >= 20);
        assert!(h >= 4);
    }

    #[test]
    fn modal_too_small_noop() {
        let m = Modal::new("T", "B");
        let mut buf = View::new(2, 2);
        m.draw(Rect::new(0, 0, 2, 2), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }
}
