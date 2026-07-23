//! Modal file/directory browser — overlay component for TUI apps.
//!
//! Pure-state UI: holds current directory path, entries, selection, filter,
//! and multi-select marks. Performs NO I/O. Caller lists the directory and
//! feeds entries via `set_entries`; browser reports navigation actions
//! (`EnterDir`, `Confirm`, `Cancel`) back to the caller through
//! `BrowserAction`.
//!
//! Rendering overlays into a caller-provided `View` at a given rect —
//! caller is responsible for dimming/clearing the area underneath if
//! desired. The browser draws its own frame and title bar.
//!
//! Keys:
//! - `Up`/`Down`: move selection
//! - `PageUp`/`PageDown`: scroll by page
//! - `Home`/`End`: jump to first/last
//! - `Enter`: on directory → `EnterDir(path)`; on file → in single-select,
//!   `Confirm([path])`; in multi-select, toggle mark (or confirm if none
//!   marked → confirm just the selected file)
//! - `Space`: toggle mark (multi-select only)
//! - `Backspace`: navigate to parent directory (`EnterDir(parent)`)
//! - `a`: toggle hidden files visibility
//! - `/`: start filter mode (caller may interpret; this impl just clears)
//! - `Esc`: `Cancel`
//! - `q`: `Cancel` (alias for Esc)

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell as CoreCell;

use crate::input::{Direction, KeyEvent};
use crate::{
    Cell, View, ATTR_BOLD, COLOR_BLUE, COLOR_CYAN, COLOR_DEFAULT, COLOR_GREEN, COLOR_YELLOW,
};

/// File system entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    FilesAndDirs,
    FilesOnly,
    DirsOnly,
}

/// A single directory entry. Caller fills this from `VfsClient::readdir`
/// or any other source. UI-only — no I/O happens here.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
}

impl DirEntry {
    pub fn file(name: &str, size: u64) -> Self {
        Self {
            name: String::from(name),
            kind: EntryKind::File,
            size,
        }
    }

    pub fn dir(name: &str) -> Self {
        Self {
            name: String::from(name),
            kind: EntryKind::Directory,
            size: 0,
        }
    }
}

/// Action returned by `handle_key` for the caller to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    /// No actionable input; caller should re-render.
    None,
    /// User navigated into a directory. Caller should list it and call
    /// `set_entries`. The path is the new absolute directory path.
    EnterDir(String),
    /// User confirmed selection. Contains absolute paths of selected
    /// entries. In single-select mode, always exactly one. In multi-select
    /// mode, all marked entries (or just the highlighted one if none marked).
    Confirm(Vec<String>),
    /// User cancelled (Esc or q).
    Cancel,
}

/// Rendering style for a file browser overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserRenderOptions {
    framed: bool,
    background: u8,
    show_header: bool,
}

impl BrowserRenderOptions {
    pub const fn borderless(background: u8) -> Self {
        Self {
            framed: false,
            background,
            show_header: true,
        }
    }

    pub const fn borderless_no_header(background: u8) -> Self {
        Self {
            framed: false,
            background,
            show_header: false,
        }
    }

    const fn framed() -> Self {
        Self {
            framed: true,
            background: COLOR_DEFAULT,
            show_header: true,
        }
    }
}

/// Modal file browser state.
pub struct FileBrowser {
    cwd: String,
    entries: Vec<DirEntry>,
    filtered: Vec<usize>,
    selected: usize,
    page_size: usize,
    multi_select: bool,
    marked: Vec<bool>,
    filter: String,
    show_hidden: bool,
    title: String,
    mode: BrowserMode,
    viewport_start: CoreCell<usize>,
}

impl FileBrowser {
    /// Create a new browser. Caller should immediately list `cwd` and
    /// call `set_entries`.
    pub fn new(cwd: &str, page_size: usize, multi_select: bool) -> Self {
        Self {
            cwd: String::from(cwd),
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            page_size,
            multi_select,
            marked: Vec::new(),
            filter: String::new(),
            show_hidden: false,
            title: String::from(" Browse "),
            mode: BrowserMode::FilesAndDirs,
            viewport_start: CoreCell::new(0),
        }
    }

    /// Replace the entry list. Called after navigating into a new directory.
    /// Resets selection, filter, and marks.
    pub fn set_entries(&mut self, entries: Vec<DirEntry>) {
        self.entries = entries;
        self.marked = alloc::vec![false; self.entries.len()];
        self.selected = 0;
        self.viewport_start.set(0);
        self.filter.clear();
        self.apply_filter();
    }

    pub fn set_mode(&mut self, mode: BrowserMode) {
        self.mode = mode;
        self.apply_filter();
    }

    /// Current working directory path.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn entries(&self) -> &[DirEntry] {
        &self.entries
    }

    /// Set the cwd (after caller fulfilled an `EnterDir` request).
    pub fn set_cwd(&mut self, path: &str) {
        self.cwd = String::from(path);
    }

    /// Browser window title (shown in the frame header).
    pub fn set_title(&mut self, title: &str) {
        self.title = String::from(title);
    }

    /// Number of entries currently matching the filter.
    pub fn filtered_count(&self) -> usize {
        self.filtered.len()
    }

    /// Currently selected entry, if any.
    pub fn selected_entry(&self) -> Option<&DirEntry> {
        self.filtered.get(self.selected).map(|&i| &self.entries[i])
    }

    /// Indices (into the original entries vec) of marked entries.
    pub fn marked_indices(&self) -> Vec<usize> {
        self.marked
            .iter()
            .enumerate()
            .filter(|(_, &m)| m)
            .map(|(i, _)| i)
            .collect()
    }

    /// Whether the entry at the given original index is marked.
    pub fn is_marked(&self, idx: usize) -> bool {
        self.marked.get(idx).copied().unwrap_or(false)
    }

    /// Toggle mark on the currently selected entry (multi-select only).
    pub fn toggle_mark_selected(&mut self) {
        if !self.multi_select {
            return;
        }
        if let Some(&i) = self.filtered.get(self.selected) {
            self.marked[i] = !self.marked[i];
        }
    }

    /// Move selection up by one.
    pub fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move selection down by one.
    pub fn next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        self.selected = (self.selected + 1).min(max);
    }

    /// Move selection up by one page.
    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.page_size);
    }

    /// Move selection down by one page.
    pub fn page_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        self.selected = (self.selected + self.page_size).min(max);
    }

    /// Jump to first entry.
    pub fn home(&mut self) {
        self.selected = 0;
        self.viewport_start.set(0);
    }

    /// Jump to last entry.
    pub fn end(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self.filtered.len() - 1;
    }

    /// Toggle visibility of hidden entries (those starting with `.`).
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.apply_filter();
    }

    /// Set the filter string. An entry matches if its name contains the
    /// filter (case-insensitive).
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = String::from(filter);
        self.apply_filter();
    }

    /// Clear the filter.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.apply_filter();
    }

    /// Current filter string.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Visible-page range within the filtered list, based on selection.
    pub fn visible_range(&self) -> (usize, usize) {
        if self.filtered.is_empty() || self.page_size == 0 {
            return (0, 0);
        }
        let start = (self.selected / self.page_size) * self.page_size;
        let end = (start + self.page_size).min(self.filtered.len());
        (start, end)
    }

    /// Handle a key event. Returns the action the caller should perform.
    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key {
            KeyEvent::Esc | KeyEvent::Char('q') | KeyEvent::Ctrl('c') => BrowserAction::Cancel,
            KeyEvent::Arrow(Direction::Up) => {
                self.prev();
                BrowserAction::None
            }
            KeyEvent::Arrow(Direction::Down) => {
                self.next();
                BrowserAction::None
            }
            KeyEvent::PageUp => {
                self.page_up();
                BrowserAction::None
            }
            KeyEvent::PageDown => {
                self.page_down();
                BrowserAction::None
            }
            KeyEvent::Home => {
                self.home();
                BrowserAction::None
            }
            KeyEvent::End => {
                self.end();
                BrowserAction::None
            }
            KeyEvent::Backspace => {
                let parent = parent_dir(&self.cwd);
                if parent != self.cwd {
                    BrowserAction::EnterDir(parent)
                } else {
                    BrowserAction::None
                }
            }
            KeyEvent::Char('a') => {
                self.toggle_hidden();
                BrowserAction::None
            }
            KeyEvent::Char(' ') if self.multi_select => {
                self.toggle_mark_selected();
                BrowserAction::None
            }
            KeyEvent::Enter => self.handle_enter(),
            _ => BrowserAction::None,
        }
    }

    fn handle_enter(&mut self) -> BrowserAction {
        let entry = match self.selected_entry() {
            Some(e) => e.clone(),
            None => return BrowserAction::None,
        };
        match entry.kind {
            EntryKind::Directory => {
                if entry.name == "./" {
                    return BrowserAction::EnterDir(self.cwd.clone());
                }
                if entry.name == "../" || entry.name == ".." {
                    let parent = parent_dir(&self.cwd);
                    if parent != self.cwd {
                        return BrowserAction::EnterDir(parent);
                    }
                    return BrowserAction::None;
                }
                let path = join_path(&self.cwd, &entry.name);
                BrowserAction::EnterDir(path)
            }
            EntryKind::File => {
                let path = join_path(&self.cwd, &entry.name);
                if self.multi_select {
                    let marked = self.marked_indices();
                    if marked.is_empty() {
                        BrowserAction::Confirm(alloc::vec![path])
                    } else {
                        let paths: Vec<String> = marked
                            .into_iter()
                            .filter_map(|i| {
                                let e = &self.entries[i];
                                if e.kind == EntryKind::File {
                                    Some(join_path(&self.cwd, &e.name))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        if paths.is_empty() {
                            BrowserAction::Confirm(alloc::vec![path])
                        } else {
                            BrowserAction::Confirm(paths)
                        }
                    }
                } else {
                    BrowserAction::Confirm(alloc::vec![path])
                }
            }
        }
    }

    /// Render the browser overlay into `view` at `(row, col)` with the
    /// given `width` and `height`. Draws a frame, title, current path,
    /// entry list, scrollbar, and a footer hint line.
    pub fn render(&self, row: usize, col: usize, width: usize, height: usize, view: &mut View) {
        self.render_with_options(
            row,
            col,
            width,
            height,
            view,
            BrowserRenderOptions::framed(),
        );
    }

    /// Render with an opt-in modal style while preserving framed defaults.
    pub fn render_with_options(
        &self,
        row: usize,
        col: usize,
        width: usize,
        height: usize,
        view: &mut View,
        options: BrowserRenderOptions,
    ) {
        if !options.framed {
            self.draw_borderless(row, col, width, height, view, options);
            return;
        }
        if width < 4 || height < 4 {
            return;
        }
        let inner_col = col + 1;
        let inner_width = width.saturating_sub(2);
        let inner_height = height.saturating_sub(2);
        let list_top = row + 2;
        let list_height = inner_height.saturating_sub(1);

        self.draw_frame(row, col, width, height, view);
        self.draw_path_bar(row + 1, inner_col, inner_width, view);
        self.draw_entry_list(list_top, inner_col, inner_width, list_height, view);
        self.draw_scrollbar(list_top, col + width - 1, list_height, view);
        self.draw_footer_hint(row + height - 1, inner_col, inner_width, view);
    }

    fn draw_borderless(
        &self,
        row: usize,
        col: usize,
        width: usize,
        height: usize,
        view: &mut View,
        options: BrowserRenderOptions,
    ) {
        let background = options.background;
        if width == 0 || height < 2 {
            return;
        }
        let end_row = (row + height).min(view.height);
        let end_col = (col + width).min(view.width);
        for r in row..end_row {
            for c in col..end_col {
                view.set(r, c, Cell::new(' ').bg(background));
            }
        }
        if options.show_header {
            for (offset, ch) in self.title.chars().enumerate() {
                if col + offset >= end_col {
                    break;
                }
                view.set(
                    row,
                    col + offset,
                    Cell::new(ch)
                        .fg(COLOR_YELLOW)
                        .bg(background)
                        .attrs(ATTR_BOLD),
                );
            }
            for (offset, ch) in self.cwd.chars().enumerate() {
                if col + offset >= end_col {
                    break;
                }
                view.set(
                    row + 1,
                    col + offset,
                    Cell::new(ch).fg(COLOR_CYAN).bg(background),
                );
            }
        }

        let list_row = if options.show_header { row + 2 } else { row };
        let list_height = if options.show_header {
            height.saturating_sub(2)
        } else {
            height
        };
        let has_overflow = self.filtered.len() > list_height;
        let entry_width = width.saturating_sub(usize::from(has_overflow));
        let mut start = self.viewport_start.get();
        if self.selected < start {
            start = self.selected;
        } else if self.selected >= start + list_height {
            start = self.selected + 1 - list_height;
        }
        let max_start = self.filtered.len().saturating_sub(list_height);
        start = start.min(max_start);
        self.viewport_start.set(start);
        let end = (start + list_height).min(self.filtered.len());
        for index in start..end {
            let display_row = list_row + index - start;
            if display_row >= end_row {
                break;
            }
            let entry_idx = self.filtered[index];
            let entry = &self.entries[entry_idx];
            let selected = index == self.selected;
            let cursor_bg = if selected { COLOR_YELLOW } else { background };
            let cursor_fg = if selected { 0 } else { COLOR_DEFAULT };
            let mut entry_col = col;

            if selected {
                for fill_col in col..(col + entry_width).min(end_col) {
                    view.set(display_row, fill_col, Cell::new(' ').bg(cursor_bg));
                }
            }

            let icon = match entry.kind {
                EntryKind::Directory => '/',
                EntryKind::File => ' ',
            };
            if entry_col < col + entry_width && entry_col < end_col {
                let fg = if selected { cursor_fg } else if entry.kind == EntryKind::Directory { COLOR_CYAN } else { COLOR_DEFAULT };
                view.set(
                    display_row,
                    entry_col,
                    Cell::new(icon).fg(fg).bg(cursor_bg),
                );
                entry_col += 1;
            }
            for ch in entry.name.chars() {
                if entry_col >= col + entry_width || entry_col >= end_col {
                    break;
                }
                let fg = if selected { cursor_fg } else if entry.kind == EntryKind::Directory { COLOR_CYAN } else { COLOR_DEFAULT };
                view.set(display_row, entry_col, Cell::new(ch).fg(fg).bg(cursor_bg));
                entry_col += 1;
            }
        }
        if has_overflow && list_height > 0 && col + width > col {
            let scrollbar_col = col + width - 1;
            if scrollbar_col < end_col {
                let max_start = self.filtered.len() - list_height;
                let thumb_len = ((list_height * list_height) / self.filtered.len())
                    .max(1)
                    .min(list_height);
                let thumb_travel = list_height - thumb_len;
                let thumb_start = (start * thumb_travel) / max_start;
                for offset in 0..list_height {
                    let scrollbar_row = list_row + offset;
                    if scrollbar_row >= end_row {
                        break;
                    }
                    let ch = if offset >= thumb_start && offset < thumb_start + thumb_len {
                        '█'
                    } else {
                        '│'
                    };
                    view.set(
                        scrollbar_row,
                        scrollbar_col,
                        Cell::new(ch).fg(238).bg(background),
                    );
                }
            }
        }
    }

    fn draw_frame(&self, row: usize, col: usize, width: usize, height: usize, view: &mut View) {
        let tl = '\u{2554}';
        let tr = '\u{2557}';
        let bl = '\u{255A}';
        let br = '\u{255D}';
        let h = '\u{2550}';
        let v = '\u{2551}';
        view.set(row, col, Cell::new(tl).fg(238));
        view.set(row, col + width - 1, Cell::new(tr).fg(238));
        view.set(row + height - 1, col, Cell::new(bl).fg(238));
        view.set(row + height - 1, col + width - 1, Cell::new(br).fg(238));
        for i in 1..width - 1 {
            view.set(row, col + i, Cell::new(h).fg(238));
            view.set(row + height - 1, col + i, Cell::new(h).fg(238));
        }
        for j in 1..height - 1 {
            view.set(row + j, col, Cell::new(v).fg(238));
            view.set(row + j, col + width - 1, Cell::new(v).fg(238));
        }
        let title = self.title.as_str();
        let mut c = col + 2;
        let max = col + width - 2;
        for ch in title.chars() {
            if c >= max {
                break;
            }
            view.set(row, c, Cell::new(ch).fg(COLOR_YELLOW).attrs(ATTR_BOLD));
            c += 1;
        }
    }

    fn draw_path_bar(&self, row: usize, col: usize, width: usize, view: &mut View) {
        let path_str = if self.cwd.len() > width {
            let cut = self.cwd.len().saturating_sub(width);
            alloc::format!("...{}", &self.cwd[cut..])
        } else {
            self.cwd.clone()
        };
        let mut c = col;
        for ch in path_str.chars() {
            if c >= col + width {
                break;
            }
            view.set(row, c, Cell::new(ch).fg(COLOR_CYAN));
            c += 1;
        }
    }

    fn draw_entry_list(
        &self,
        row: usize,
        col: usize,
        width: usize,
        height: usize,
        view: &mut View,
    ) {
        if height == 0 || width < 2 {
            return;
        }
        let (start, end) = self.visible_range();
        if self.filtered.is_empty() {
            let empty = "(empty)";
            for (i, ch) in empty.chars().enumerate() {
                if col + i >= col + width {
                    break;
                }
                view.set(row, col + i, Cell::new(ch).fg(244));
            }
            return;
        }
        let mark_col_width = if self.multi_select { 2 } else { 0 };
        let name_col = col + mark_col_width;
        let name_width = width.saturating_sub(mark_col_width);
        for i in start..end {
            let display_row = row + (i - start);
            if display_row >= row + height {
                break;
            }
            let entry_idx = match self.filtered.get(i) {
                Some(&idx) => idx,
                None => break,
            };
            let entry = &self.entries[entry_idx];
            let is_selected = i == self.selected;
            let is_marked = self.marked.get(entry_idx).copied().unwrap_or(false);

            if self.multi_select {
                let mark_ch = if is_marked { '*' } else { ' ' };
                let mut mark_cell = Cell::new(mark_ch).fg(COLOR_GREEN);
                if is_selected {
                    mark_cell = mark_cell.bg(COLOR_BLUE);
                }
                view.set(display_row, col, mark_cell);
            }

            let icon = match entry.kind {
                EntryKind::Directory => '/',
                EntryKind::File => ' ',
            };
            let mut ic_cell = Cell::new(icon).fg(COLOR_YELLOW);
            if is_selected {
                ic_cell = ic_cell.bg(COLOR_BLUE);
            }
            view.set(display_row, name_col, ic_cell);

            let mut c = name_col + 1;
            let max_c = name_col + name_width;
            for ch in entry.name.chars() {
                if c >= max_c || c >= view.width {
                    break;
                }
                let mut cell = match entry.kind {
                    EntryKind::Directory => Cell::new(ch).fg(COLOR_CYAN),
                    EntryKind::File => Cell::new(ch).fg(COLOR_DEFAULT),
                };
                if is_selected {
                    cell = cell.bg(COLOR_BLUE);
                }
                view.set(display_row, c, cell);
                c += 1;
            }

            if is_selected {
                while c < max_c && c < view.width {
                    view.set(display_row, c, Cell::new(' ').bg(COLOR_BLUE));
                    c += 1;
                }
            }
        }
    }

    fn draw_scrollbar(&self, row: usize, col: usize, height: usize, view: &mut View) {
        if height == 0 || self.filtered.is_empty() {
            return;
        }
        let total = self.filtered.len();
        let visible = self.page_size.min(height);
        if total <= visible {
            return;
        }
        let max_offset = total - visible;
        let thumb_start = if max_offset == 0 {
            0
        } else {
            (self.selected.min(max_offset) * height) / (total.max(1))
        };
        let thumb_len = ((visible * height) / total.max(1)).max(1);
        for i in 0..height {
            if row + i >= view.height {
                break;
            }
            let ch = if i >= thumb_start && i < thumb_start + thumb_len {
                '\u{2588}'
            } else {
                '\u{2502}'
            };
            view.set(row + i, col, Cell::new(ch).fg(238));
        }
    }

    fn draw_footer_hint(&self, row: usize, col: usize, width: usize, view: &mut View) {
        let hint = if self.multi_select {
            "↑↓ nav  Enter open/confirm  Space mark  Backspace up  a hidden  Esc cancel"
        } else {
            "↑↓ nav  Enter open/confirm  Backspace up  a hidden  Esc cancel"
        };
        let mut c = col;
        for ch in hint.chars() {
            if c >= col + width || c >= view.width {
                break;
            }
            view.set(row, c, Cell::new(ch).fg(244));
            c += 1;
        }
    }

    fn apply_filter(&mut self) {
        let needle = ascii_lower(&self.filter);
        self.filtered = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                if !self.show_hidden && e.name.starts_with('.')
                    && e.name != ".." && e.name != "../"
                    && e.name != "./" && e.name != "."
                {
                    return false;
                }
                match self.mode {
                    BrowserMode::FilesOnly if e.kind == EntryKind::Directory => return false,
                    BrowserMode::DirsOnly if e.kind == EntryKind::File => return false,
                    _ => {}
                }
                if needle.is_empty() {
                    return true;
                }
                ascii_lower(&e.name).contains(&needle)
            })
            .collect();
        self.selected = 0;
    }
}

impl crate::layout::Drawable for FileBrowser {
    fn draw(&self, area: crate::layout::Rect, buf: &mut View) {
        self.render(area.y, area.x, area.width, area.height, buf);
    }
}

fn ascii_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

fn parent_dir(path: &str) -> String {
    if path.is_empty() {
        return String::from("/");
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::from("/");
    }
    match trimmed.rfind('/') {
        Some(0) => String::from("/"),
        Some(idx) => String::from(&trimmed[..idx]),
        None => String::from(path),
    }
}

fn join_path(cwd: &str, name: &str) -> String {
    if cwd.ends_with('/') {
        alloc::format!("{}{}", cwd, name)
    } else {
        alloc::format!("{}/{}", cwd, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    fn make_entries() -> Vec<DirEntry> {
        vec![
            DirEntry::dir("Documents"),
            DirEntry::dir("Music"),
            DirEntry::file("readme.txt", 1024),
            DirEntry::file("song.mp3", 5_000_000),
            DirEntry::dir(".hidden_dir"),
            DirEntry::file(".secret", 100),
        ]
    }

    #[test]
    fn new_browser_empty() {
        let b = FileBrowser::new("/host", 10, false);
        assert_eq!(b.cwd(), "/host");
        assert_eq!(b.filtered_count(), 0);
        assert!(b.selected_entry().is_none());
    }

    #[test]
    fn set_entries_populates() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(make_entries());
        assert_eq!(b.filtered_count(), 4);
        assert_eq!(b.selected_entry().unwrap().name, "Documents");
    }

    #[test]
    fn hidden_entries_filtered_by_default() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        assert_eq!(b.filtered_count(), 4);
        b.toggle_hidden();
        assert_eq!(b.filtered_count(), 6);
    }

    #[test]
    fn next_prev_clamp() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        b.prev();
        assert_eq!(b.selected, 0);
        b.next();
        b.next();
        b.next();
        assert_eq!(b.selected, 3);
        b.next();
        assert_eq!(b.selected, 3);
    }

    #[test]
    fn page_down_up() {
        let mut b = FileBrowser::new("/", 2, false);
        b.set_entries(make_entries());
        b.page_down();
        assert_eq!(b.selected, 2);
        b.page_down();
        assert_eq!(b.selected, 3);
        b.page_up();
        assert_eq!(b.selected, 1);
    }

    #[test]
    fn home_end() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        b.end();
        assert_eq!(b.selected, 3);
        b.home();
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn enter_directory_returns_enter_dir() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(make_entries());
        let action = b.handle_key(KeyEvent::Enter);
        assert_eq!(
            action,
            BrowserAction::EnterDir(String::from("/host/Documents"))
        );
    }

    #[test]
    fn enter_file_single_select_returns_confirm() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(make_entries());
        b.next();
        b.next();
        let action = b.handle_key(KeyEvent::Enter);
        assert_eq!(
            action,
            BrowserAction::Confirm(vec![String::from("/host/readme.txt")])
        );
    }

    #[test]
    fn enter_file_multi_select_with_no_marks_confirms_highlighted() {
        let mut b = FileBrowser::new("/host", 10, true);
        b.set_entries(make_entries());
        b.next();
        b.next();
        let action = b.handle_key(KeyEvent::Enter);
        assert_eq!(
            action,
            BrowserAction::Confirm(vec![String::from("/host/readme.txt")])
        );
    }

    #[test]
    fn space_toggles_mark_multi_select() {
        let mut b = FileBrowser::new("/host", 10, true);
        b.set_entries(make_entries());
        assert_eq!(b.is_marked(0), false);
        b.handle_key(KeyEvent::Char(' '));
        assert_eq!(b.is_marked(0), true);
        b.handle_key(KeyEvent::Char(' '));
        assert_eq!(b.is_marked(0), false);
    }

    #[test]
    fn space_does_nothing_in_single_select() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(make_entries());
        b.handle_key(KeyEvent::Char(' '));
        assert_eq!(b.is_marked(0), false);
    }

    #[test]
    fn enter_file_multi_select_with_marks_confirms_all_marked() {
        let mut b = FileBrowser::new("/host", 10, true);
        b.set_entries(make_entries());
        b.handle_key(KeyEvent::Char(' '));
        b.next();
        b.next();
        b.handle_key(KeyEvent::Char(' '));
        let action = b.handle_key(KeyEvent::Enter);
        assert_eq!(
            action,
            BrowserAction::Confirm(vec![String::from("/host/readme.txt"),])
        );
    }

    #[test]
    fn backspace_returns_parent_dir() {
        let mut b = FileBrowser::new("/host/music", 10, false);
        b.set_entries(make_entries());
        let action = b.handle_key(KeyEvent::Backspace);
        assert_eq!(action, BrowserAction::EnterDir(String::from("/host")));
    }

    #[test]
    fn backspace_at_root_stays_at_root() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        let action = b.handle_key(KeyEvent::Backspace);
        assert_eq!(action, BrowserAction::None);
    }

    #[test]
    fn esc_returns_cancel() {
        let mut b = FileBrowser::new("/", 10, false);
        assert_eq!(b.handle_key(KeyEvent::Esc), BrowserAction::Cancel);
    }

    #[test]
    fn q_returns_cancel() {
        let mut b = FileBrowser::new("/", 10, false);
        assert_eq!(b.handle_key(KeyEvent::Char('q')), BrowserAction::Cancel);
    }

    #[test]
    fn ctrl_c_returns_cancel() {
        let mut b = FileBrowser::new("/", 10, false);
        assert_eq!(b.handle_key(KeyEvent::Ctrl('c')), BrowserAction::Cancel);
    }

    #[test]
    fn toggle_hidden_key() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        assert_eq!(b.filtered_count(), 4);
        b.handle_key(KeyEvent::Char('a'));
        assert_eq!(b.filtered_count(), 6);
        b.handle_key(KeyEvent::Char('a'));
        assert_eq!(b.filtered_count(), 4);
    }

    #[test]
    fn filter_matches_case_insensitive() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        b.set_filter("MP3");
        assert_eq!(b.filtered_count(), 1);
        assert_eq!(b.selected_entry().unwrap().name, "song.mp3");
    }

    #[test]
    fn filter_resets_selected() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        b.next();
        b.next();
        assert_eq!(b.selected, 2);
        b.set_filter("mp3");
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn clear_filter_shows_all_visible() {
        let mut b = FileBrowser::new("/", 10, false);
        b.set_entries(make_entries());
        b.set_filter("xyz");
        assert_eq!(b.filtered_count(), 0);
        b.clear_filter();
        assert_eq!(b.filtered_count(), 4);
    }

    #[test]
    fn visible_range_pagination() {
        let mut b = FileBrowser::new("/", 2, false);
        b.set_entries(make_entries());
        assert_eq!(b.visible_range(), (0, 2));
        b.page_down();
        assert_eq!(b.visible_range(), (2, 4));
    }

    #[test]
    fn visible_range_empty() {
        let b = FileBrowser::new("/", 10, false);
        assert_eq!(b.visible_range(), (0, 0));
    }

    #[test]
    fn enter_with_no_entries_returns_none() {
        let mut b = FileBrowser::new("/", 10, false);
        let action = b.handle_key(KeyEvent::Enter);
        assert_eq!(action, BrowserAction::None);
    }

    #[test]
    fn marked_indices_returns_marked_only() {
        let mut b = FileBrowser::new("/", 10, true);
        b.set_entries(make_entries());
        b.handle_key(KeyEvent::Char(' '));
        b.next();
        b.next();
        b.handle_key(KeyEvent::Char(' '));
        let marked = b.marked_indices();
        assert_eq!(marked, vec![0, 2]);
    }

    #[test]
    fn parent_dir_basic() {
        assert_eq!(parent_dir("/host/music"), String::from("/host"));
        assert_eq!(parent_dir("/host/"), String::from("/"));
        assert_eq!(parent_dir("/host"), String::from("/"));
        assert_eq!(parent_dir("/"), String::from("/"));
        assert_eq!(parent_dir(""), String::from("/"));
    }

    #[test]
    fn join_path_basic() {
        assert_eq!(
            join_path("/host", "file.txt"),
            String::from("/host/file.txt")
        );
        assert_eq!(join_path("/", "file.txt"), String::from("/file.txt"));
        assert_eq!(
            join_path("/host/music", "song.mp3"),
            String::from("/host/music/song.mp3")
        );
    }

    #[test]
    fn render_draws_frame() {
        let b = FileBrowser::new("/host", 10, false);
        let mut v = View::new(40, 12);
        b.render(0, 0, 40, 12, &mut v);
        assert_eq!(v.get(0, 0).unwrap().ch, '\u{2554}');
        assert_eq!(v.get(0, 39).unwrap().ch, '\u{2557}');
        assert_eq!(v.get(11, 0).unwrap().ch, '\u{255A}');
        assert_eq!(v.get(11, 39).unwrap().ch, '\u{255D}');
    }

    #[test]
    fn render_too_small_noop() {
        let b = FileBrowser::new("/host", 10, false);
        let mut v = View::new(3, 3);
        b.render(0, 0, 3, 3, &mut v);
    }

    #[test]
    fn render_shows_entries() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(vec![DirEntry::dir("Music"), DirEntry::file("a.mp3", 100)]);
        let mut v = View::new(40, 12);
        b.render(0, 0, 40, 12, &mut v);
        let row2_ch = v.get(2, 1).unwrap().ch;
        assert_eq!(row2_ch, '/', "directory entry should show / icon");
    }

    #[test]
    fn render_shows_selected_highlight() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(vec![DirEntry::file("a.mp3", 100)]);
        let mut v = View::new(40, 12);
        b.render(0, 0, 40, 12, &mut v);
        let selected_cell = v.get(2, 1).unwrap();
        assert_eq!(selected_cell.bg, COLOR_BLUE);
    }

    #[test]
    fn render_multi_select_shows_mark_column() {
        let mut b = FileBrowser::new("/host", 10, true);
        b.set_entries(vec![DirEntry::file("a.mp3", 100)]);
        let mut v = View::new(40, 12);
        b.render(0, 0, 40, 12, &mut v);
        let mark_cell = v.get(2, 1).unwrap();
        assert_eq!(mark_cell.ch, ' ');
        b.toggle_mark_selected();
        let mut v2 = View::new(40, 12);
        b.render(0, 0, 40, 12, &mut v2);
        let mark_cell = v2.get(2, 1).unwrap();
        assert_eq!(mark_cell.ch, '*');
    }

    #[test]
    fn render_with_options_draws_borderless_cursor_after_arrow_down() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(vec![
            DirEntry::file("first.mp3", 1),
            DirEntry::file("second.mp3", 1),
        ]);
        b.handle_key(KeyEvent::Arrow(Direction::Down));
        let mut v = View::new(20, 6);
        b.render_with_options(0, 0, 20, 6, &mut v, BrowserRenderOptions::borderless(8));
        let cell = v.get(3, 0).unwrap();
        assert_eq!(cell.bg, COLOR_YELLOW);
    }

    #[test]
    fn render_with_options_uses_full_height_without_scrollbar_when_entries_fit() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(vec![
            DirEntry::file("first.mp3", 1),
            DirEntry::file("second.mp3", 1),
            DirEntry::file("last.mp3", 1),
        ]);
        let mut v = View::new(20, 5);
        b.render_with_options(0, 0, 20, 5, &mut v, BrowserRenderOptions::borderless(8));
        assert_eq!(v.get(4, 1).unwrap().ch, 'l');
        assert_ne!(v.get(2, 19).unwrap().ch, '│');
    }

    #[test]
    fn render_with_options_scrolls_viewport_and_draws_scrollbar_on_overflow() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(
            (0..5)
                .map(|index| DirEntry::file(&format!("{index}"), 1))
                .collect(),
        );
        for _ in 0..4 {
            b.handle_key(KeyEvent::Arrow(Direction::Down));
        }
        let mut v = View::new(20, 5);
        b.render_with_options(0, 0, 20, 5, &mut v, BrowserRenderOptions::borderless(8));
        assert_eq!(v.get(2, 1).unwrap().ch, '2');
        assert_eq!(v.get(2, 19).unwrap().ch, '│');
    }

    #[test]
    fn render_with_options_places_scrollbar_thumb_at_bottom() {
        let mut b = FileBrowser::new("/host", 10, false);
        b.set_entries(
            (0..5)
                .map(|index| DirEntry::file(&format!("{index}"), 1))
                .collect(),
        );
        b.handle_key(KeyEvent::End);
        let mut v = View::new(20, 5);
        b.render_with_options(0, 0, 20, 5, &mut v, BrowserRenderOptions::borderless(8));
        assert_eq!(v.get(4, 19).unwrap().ch, '█');
    }
}
