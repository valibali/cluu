//! Filterable, paginated list — holds items, selected index, filter string,
//! page size.
//!
//! Items are generic over any `T: Clone`. Filtering and rendering require
//! `T: ToString`. The `filtered` vector holds indices into the original
//! `items` vector; `selected` is an index into `filtered`.

extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::{Cell, View, COLOR_BLUE};

/// A filterable, paginated list of items.
pub struct List<T: Clone> {
    items: Vec<T>,
    filtered: Vec<usize>,
    selected: usize,
    filter: String,
    page_size: usize,
}

impl<T: Clone> List<T> {
    /// Create a new list with all items visible and selected at 0.
    pub fn new(items: Vec<T>, page_size: usize) -> Self {
        let filtered: Vec<usize> = (0..items.len()).collect();
        List {
            items,
            filtered,
            selected: 0,
            filter: String::new(),
            page_size,
        }
    }

    /// Set the filter string and recompute filtered indices.
    /// An item matches if its `to_string()` contains the filter
    /// (case-insensitive). Resets selected to 0.
    pub fn set_filter(&mut self, filter: &str)
    where
        T: ToString,
    {
        self.filter = String::from(filter);
        let needle = ascii_lower(&self.filter);
        self.filtered = (0..self.items.len())
            .filter(|&i| ascii_lower(&self.items[i].to_string()).contains(&needle))
            .collect();
        self.selected = 0;
    }

    /// Move selection forward, clamped at last filtered item.
    pub fn next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        self.selected = (self.selected + 1).min(max);
    }

    /// Move selection backward, clamped at 0.
    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move selection forward by page_size, clamped at last filtered item.
    pub fn page_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        self.selected = (self.selected + self.page_size).min(max);
    }

    /// Move selection backward by page_size, clamped at 0.
    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.page_size);
    }

    /// Return the currently selected item, or None if filtered is empty.
    pub fn selected(&self) -> Option<&T> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    /// Return the index into the original items vec of the selected item,
    /// or None if filtered is empty.
    pub fn selected_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    /// Number of items matching the current filter.
    pub fn filtered_count(&self) -> usize {
        self.filtered.len()
    }

    /// Return the `(start, end)` range of the currently visible page
    /// within the filtered list.
    pub fn visible_range(&self) -> (usize, usize) {
        if self.filtered.is_empty() || self.page_size == 0 {
            return (0, 0);
        }
        let start = (self.selected / self.page_size) * self.page_size;
        let end = (start + self.page_size).min(self.filtered.len());
        (start, end)
    }

    /// Render visible items into a `View` at `(row, col)` with the given
    /// `width` per item. The selected item is highlighted with a blue
    /// background.
    pub fn render(&self, row: usize, col: usize, width: usize, view: &mut View)
    where
        T: ToString,
    {
        let (start, end) = self.visible_range();
        let max_col = col.saturating_add(width);
        for i in start..end {
            let display_row = row + (i - start);
            if display_row >= view.height {
                break;
            }
            let item_str = self.items[self.filtered[i]].to_string();
            let is_selected = i == self.selected;
            let mut c = col;
            for ch in item_str.chars() {
                if c >= max_col || c >= view.width {
                    break;
                }
                let mut cell = Cell::new(ch);
                if is_selected {
                    cell = cell.bg(COLOR_BLUE);
                }
                view.set(display_row, c, cell);
                c += 1;
            }
        }
    }
}

/// ASCII lowercase conversion — no_std compatible.
fn ascii_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn make_items() -> Vec<String> {
        vec![
            "apple".to_string(),
            "banana".to_string(),
            "cherry".to_string(),
            "apricot".to_string(),
            "blueberry".to_string(),
        ]
    }

    #[test]
    fn list_new_all_items_visible() {
        let list: List<String> = List::new(make_items(), 3);
        assert_eq!(list.filtered_count(), 5);
        assert_eq!(list.selected_index(), Some(0));
        assert_eq!(list.selected().map(|s| s.as_str()), Some("apple"));
    }

    #[test]
    fn list_filter_matching() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.set_filter("ap");
        assert_eq!(list.filtered_count(), 2);
        assert_eq!(list.selected().map(|s| s.as_str()), Some("apple"));
    }

    #[test]
    fn list_filter_case_insensitive() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.set_filter("BAN");
        assert_eq!(list.filtered_count(), 1);
        assert_eq!(list.selected().map(|s| s.as_str()), Some("banana"));
    }

    #[test]
    fn list_filter_resets_selected() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.next();
        list.next();
        assert_eq!(list.selected_index(), Some(2));
        list.set_filter("a");
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn list_filter_empty_matches_all() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.set_filter("xyz");
        assert_eq!(list.filtered_count(), 0);
        list.set_filter("");
        assert_eq!(list.filtered_count(), 5);
    }

    #[test]
    fn list_next_clamps_at_last() {
        let mut list: List<String> = List::new(make_items(), 3);
        for _ in 0..10 {
            list.next();
        }
        assert_eq!(list.selected_index(), Some(4));
    }

    #[test]
    fn list_prev_clamps_at_zero() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.prev();
        list.prev();
        assert_eq!(list.selected_index(), Some(0));
    }

    #[test]
    fn list_page_down() {
        let mut list: List<String> = List::new(make_items(), 2);
        list.page_down();
        assert_eq!(list.selected_index(), Some(2));
        list.page_down();
        assert_eq!(list.selected_index(), Some(4));
    }

    #[test]
    fn list_page_up() {
        let mut list: List<String> = List::new(make_items(), 2);
        list.page_down();
        list.page_down();
        assert_eq!(list.selected_index(), Some(4));
        list.page_up();
        assert_eq!(list.selected_index(), Some(2));
    }

    #[test]
    fn list_page_clamps() {
        let mut list: List<String> = List::new(make_items(), 2);
        list.page_up();
        assert_eq!(list.selected_index(), Some(0));
        list.page_down();
        list.page_down();
        list.page_down();
        assert_eq!(list.selected_index(), Some(4));
    }

    #[test]
    fn list_empty_filtered_selected_and_next() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.set_filter("zzz");
        assert!(list.selected().is_none());
        assert!(list.selected_index().is_none());
        list.next();
        assert!(list.selected().is_none());
    }

    #[test]
    fn list_empty() {
        let list: List<String> = List::new(Vec::new(), 3);
        assert_eq!(list.filtered_count(), 0);
        assert!(list.selected().is_none());
        assert!(list.selected_index().is_none());
    }

    #[test]
    fn list_visible_range_pages() {
        let mut list: List<String> = List::new(make_items(), 2);
        assert_eq!(list.visible_range(), (0, 2));
        list.page_down();
        assert_eq!(list.visible_range(), (2, 4));
        list.page_down();
        assert_eq!(list.visible_range(), (4, 5));
    }

    #[test]
    fn list_visible_range_empty() {
        let list: List<String> = List::new(Vec::new(), 3);
        assert_eq!(list.visible_range(), (0, 0));
    }

    #[test]
    fn list_render_writes_items() {
        let list: List<String> = List::new(make_items(), 3);
        let mut view = View::new(20, 5);
        list.render(0, 0, 20, &mut view);
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('a'));
        assert_eq!(view.get(1, 0).map(|c| c.ch), Some('b'));
        assert_eq!(view.get(2, 0).map(|c| c.ch), Some('c'));
    }

    #[test]
    fn list_render_highlights_selected() {
        let mut list: List<String> = List::new(make_items(), 3);
        list.next();
        let mut view = View::new(20, 3);
        list.render(0, 0, 20, &mut view);
        // selected is item 1 (banana), should have COLOR_BLUE bg
        let selected_cell = view.get(1, 0);
        assert!(selected_cell.is_some());
        assert_eq!(selected_cell.map(|c| c.bg), Some(COLOR_BLUE));
        // non-selected item 0 (apple) should have default bg
        let normal_cell = view.get(0, 0);
        assert_eq!(normal_cell.map(|c| c.bg), Some(crate::COLOR_DEFAULT));
    }

    #[test]
    fn list_render_clips_to_width() {
        let list: List<String> = List::new(make_items(), 3);
        let mut view = View::new(3, 1);
        list.render(0, 0, 3, &mut view);
        // "apple" clipped to "app"
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('a'));
        assert_eq!(view.get(0, 1).map(|c| c.ch), Some('p'));
        assert_eq!(view.get(0, 2).map(|c| c.ch), Some('p'));
    }

    #[test]
    fn list_render_clips_to_view_height() {
        let list: List<String> = List::new(make_items(), 2);
        let mut view = View::new(20, 1);
        list.render(0, 0, 20, &mut view);
        // only 1 row, only first item of first page visible
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('a'));
    }
}
