//! Table — columns/rows/headers with row selection and scroll.

extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell as CoreCell;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD, ATTR_REVERSE};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone)]
pub struct TableColumn {
    pub title: String,
    pub width: usize,
}

impl TableColumn {
    pub fn new(title: &str, width: usize) -> Self {
        TableColumn { title: String::from(title), width }
    }
}

pub struct Table {
    columns: Vec<TableColumn>,
    rows: Vec<Vec<String>>,
    selected: usize,
    offset: CoreCell<usize>,
    header_fg: u8,
    header_bg: u8,
    selected_fg: u8,
    selected_bg: u8,
    show_header: bool,
}

impl Table {
    pub fn new(columns: Vec<TableColumn>) -> Self {
        Table {
            columns,
            rows: Vec::new(),
            selected: 0,
            offset: CoreCell::new(0),
            header_fg: COLOR_DEFAULT,
            header_bg: COLOR_DEFAULT,
            selected_fg: COLOR_DEFAULT,
            selected_bg: 4,
            show_header: true,
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Vec<String>>) {
        self.rows = rows;
        self.selected = 0;
        self.offset.set(0);
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&Vec<String>> {
        self.rows.get(self.selected)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn next(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.rows.len().saturating_sub(1));
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn header_fg(mut self, fg: u8) -> Self { self.header_fg = fg; self }
    pub fn header_bg(mut self, bg: u8) -> Self { self.header_bg = bg; self }
    pub fn selected_fg(mut self, fg: u8) -> Self { self.selected_fg = fg; self }
    pub fn selected_bg(mut self, bg: u8) -> Self { self.selected_bg = bg; self }
    pub fn show_header(mut self, show: bool) -> Self { self.show_header = show; self }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn visible_height(&self) -> usize {
        if self.show_header { 1 } else { 0 }
    }
}

impl Drawable for Table {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let mut y = area.y;
        let data_top = if self.show_header { area.y + 1 } else { area.y };
        let data_height = area.height.saturating_sub(self.visible_height());

        if self.show_header && area.height >= 1 {
            let mut x = area.x;
            for col in &self.columns {
                if x >= area.x + area.width {
                    break;
                }
                let col_w = col.width.min(area.x + area.width - x);
                for (i, ch) in col.title.chars().enumerate() {
                    if i >= col_w {
                        break;
                    }
                    let mut cell = Cell::new(ch).fg(self.header_fg).bg(self.header_bg).attrs(ATTR_BOLD);
                    buf.set(y, x + i, cell);
                }
                for i in col.title.chars().count()..col_w {
                    buf.set(y, x + i, Cell::new(' ').bg(self.header_bg));
                }
                x += col.width + 1;
            }
            y += 1;
        }

        if data_height == 0 {
            return;
        }

        if self.selected < self.offset.get() {
            self.offset.set(self.selected);
        } else if self.selected >= self.offset.get() + data_height {
            self.offset.set(self.selected + 1 - data_height);
        }

        let end = (self.offset.get() + data_height).min(self.rows.len());
        for row_idx in self.offset.get()..end {
            let display_y = data_top + (row_idx - self.offset.get());
            if display_y >= area.y + area.height {
                break;
            }
            let is_selected = row_idx == self.selected;
            let mut x = area.x;
            for (col_idx, col) in self.columns.iter().enumerate() {
                if x >= area.x + area.width {
                    break;
                }
                let col_w = col.width.min(area.x + area.width - x);
                let cell_text = self.rows[row_idx].get(col_idx).map(|s| s.as_str()).unwrap_or("");
                for (i, ch) in cell_text.chars().enumerate() {
                    if i >= col_w {
                        break;
                    }
                    let mut cell = Cell::new(ch);
                    if is_selected {
                        cell = cell.fg(self.selected_fg).bg(self.selected_bg);
                    }
                    buf.set(display_y, x + i, cell);
                }
                for i in cell_text.chars().count()..col_w {
                    let mut cell = Cell::new(' ');
                    if is_selected {
                        cell = cell.bg(self.selected_bg);
                    }
                    buf.set(display_y, x + i, cell);
                }
                x += col.width + 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cols() -> Vec<TableColumn> {
        vec![
            TableColumn::new("PID", 5),
            TableColumn::new("Name", 10),
        ]
    }

    fn make_rows() -> Vec<Vec<String>> {
        vec![
            vec!["1".to_string(), "init".to_string()],
            vec!["2".to_string(), "shell".to_string()],
            vec!["3".to_string(), "vfs".to_string()],
        ]
    }

    #[test]
    fn table_new_empty() {
        let t = Table::new(make_cols());
        assert_eq!(t.row_count(), 0);
    }

    #[test]
    fn table_set_rows() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        assert_eq!(t.row_count(), 3);
    }

    #[test]
    fn table_next_prev() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        t.next();
        assert_eq!(t.selected_index(), 1);
        t.prev();
        assert_eq!(t.selected_index(), 0);
        t.prev();
        assert_eq!(t.selected_index(), 0);
    }

    #[test]
    fn table_next_at_end_noop() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        t.next(); t.next();
        assert_eq!(t.selected_index(), 2);
        t.next();
        assert_eq!(t.selected_index(), 2);
    }

    #[test]
    fn table_draw_header() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        let mut buf = View::new(30, 5);
        t.draw(Rect::new(0, 0, 30, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('P'));
        assert_eq!(buf.get(0, 6).map(|c| c.ch), Some('N'));
    }

    #[test]
    fn table_draw_rows() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        let mut buf = View::new(30, 5);
        t.draw(Rect::new(0, 0, 30, 5), &mut buf);
        assert_eq!(buf.get(1, 0).map(|c| c.ch), Some('1'));
        assert_eq!(buf.get(1, 6).map(|c| c.ch), Some('i'));
        assert_eq!(buf.get(2, 0).map(|c| c.ch), Some('2'));
    }

    #[test]
    fn table_draw_selected_highlight() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        t.next();
        let mut buf = View::new(30, 5);
        t.draw(Rect::new(0, 0, 30, 5), &mut buf);
        assert_eq!(buf.get(2, 0).map(|c| c.bg), Some(4));
        assert_eq!(buf.get(1, 0).map(|c| c.bg), Some(COLOR_DEFAULT));
    }

    #[test]
    fn table_no_header() {
        let mut t = Table::new(make_cols()).show_header(false);
        t.set_rows(make_rows());
        let mut buf = View::new(30, 5);
        t.draw(Rect::new(0, 0, 30, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('1'));
    }

    #[test]
    fn table_selected_returns_row() {
        let mut t = Table::new(make_cols());
        t.set_rows(make_rows());
        t.next();
        let row = t.selected().unwrap();
        assert_eq!(row[0], "2");
        assert_eq!(row[1], "shell");
    }
}
