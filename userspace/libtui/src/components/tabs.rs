//! Tabs — tab bar with active tab highlighting.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell as CoreCell;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD, ATTR_UNDERLINE};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct Tab {
    pub label: String,
}

impl Tab {
    pub fn new(label: &str) -> Self {
        Tab { label: String::from(label) }
    }
}

pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
    active_fg: u8,
    active_bg: u8,
    inactive_fg: u8,
    inactive_bg: u8,
    cached_widths: CoreCell<Vec<usize>>,
}

impl Tabs {
    pub fn new(tabs: Vec<Tab>) -> Self {
        let count = tabs.len();
        Tabs {
            tabs,
            active: 0,
            active_fg: COLOR_DEFAULT,
            active_bg: COLOR_DEFAULT,
            inactive_fg: 8,
            inactive_bg: COLOR_DEFAULT,
            cached_widths: CoreCell::new(alloc::vec![0; count]),
        }
    }

    pub fn active(mut self, idx: usize) -> Self {
        self.active = idx.min(self.tabs.len().saturating_sub(1));
        self
    }

    pub fn set_active(&mut self, idx: usize) {
        self.active = idx.min(self.tabs.len().saturating_sub(1));
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn next(&mut self) {
        if self.active + 1 < self.tabs.len() {
            self.active += 1;
        }
    }

    pub fn prev(&mut self) {
        self.active = self.active.saturating_sub(1);
    }

    pub fn active_label(&self) -> &str {
        if self.tabs.is_empty() { "" } else { &self.tabs[self.active].label }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_fg(mut self, fg: u8) -> Self { self.active_fg = fg; self }
    pub fn active_bg(mut self, bg: u8) -> Self { self.active_bg = bg; self }
    pub fn inactive_fg(mut self, fg: u8) -> Self { self.inactive_fg = fg; self }
    pub fn inactive_bg(mut self, bg: u8) -> Self { self.inactive_bg = bg; self }
}

impl Drawable for Tabs {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 || self.tabs.is_empty() {
            return;
        }

        let mut widths = self.cached_widths.take();
        for (i, tab) in self.tabs.iter().enumerate() {
            widths[i] = tab.label.chars().count() + 2;
        }
        self.cached_widths.set(widths);

        let mut x = area.x;
        for (i, tab) in self.tabs.iter().enumerate() {
            let widths = self.cached_widths.take();
            let w = widths[i];
            self.cached_widths.set(widths);

            if x + w > area.x + area.width {
                break;
            }

            let is_active = i == self.active;
            let fg = if is_active { self.active_fg } else { self.inactive_fg };
            let bg = if is_active { self.active_bg } else { self.inactive_bg };
            let mut attrs = 0u8;
            if is_active { attrs |= ATTR_BOLD | ATTR_UNDERLINE; }

            buf.set(area.y, x, Cell::new(' ').fg(fg).bg(bg));
            x += 1;
            for ch in tab.label.chars() {
                if x >= area.x + area.width { break; }
                buf.set(area.y, x, Cell::new(ch).fg(fg).bg(bg).attrs(attrs));
                x += 1;
            }
            buf.set(area.y, x, Cell::new(' ').fg(fg).bg(bg));
            x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tabs() -> Tabs {
        Tabs::new(vec![
            Tab::new("Inbox"),
            Tab::new("Sent"),
            Tab::new("Drafts"),
        ])
    }

    #[test]
    fn tabs_new_starts_at_zero() {
        let t = make_tabs();
        assert_eq!(t.active_index(), 0);
        assert_eq!(t.active_label(), "Inbox");
    }

    #[test]
    fn tabs_next_prev() {
        let mut t = make_tabs();
        t.next();
        assert_eq!(t.active_index(), 1);
        t.next();
        assert_eq!(t.active_index(), 2);
        t.next();
        assert_eq!(t.active_index(), 2);
        t.prev();
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn tabs_set_active_clamps() {
        let mut t = make_tabs();
        t.set_active(10);
        assert_eq!(t.active_index(), 2);
    }

    #[test]
    fn tabs_draw_labels() {
        let t = make_tabs();
        let mut buf = View::new(40, 1);
        t.draw(Rect::new(0, 0, 40, 1), &mut buf);
        assert_eq!(buf.get(0, 1).map(|c| c.ch), Some('I'));
        assert_eq!(buf.get(0, 8).map(|c| c.ch), Some('S'));
        assert_eq!(buf.get(0, 14).map(|c| c.ch), Some('D'));
    }

    #[test]
    fn tabs_active_is_bold_underline() {
        let t = make_tabs();
        let mut buf = View::new(40, 1);
        t.draw(Rect::new(0, 0, 40, 1), &mut buf);
        let cell = buf.get(0, 1).unwrap();
        assert!(cell.attrs & ATTR_BOLD != 0);
        assert!(cell.attrs & ATTR_UNDERLINE != 0);
    }

    #[test]
    fn tabs_inactive_not_bold() {
        let t = make_tabs();
        let mut buf = View::new(40, 1);
        t.draw(Rect::new(0, 0, 40, 1), &mut buf);
        let cell = buf.get(0, 8).unwrap();
        assert!(cell.attrs & ATTR_BOLD == 0);
    }

    #[test]
    fn tabs_empty_noop() {
        let t = Tabs::new(vec![]);
        let mut buf = View::new(10, 1);
        t.draw(Rect::new(0, 0, 10, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }
}
