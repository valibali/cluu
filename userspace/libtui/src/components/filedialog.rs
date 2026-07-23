//! FileDialog — modal open/save file browser with filled-line cursor.
//!
//! Wraps `FileBrowser` in a modal dialog with title, path bar, entry list,
//! optional filename input (for save mode), and action buttons. Supports
//! Open File, Save File, and Select Directory modes.
//!
//! Arrow keys move the filled-line cursor through entries. Tab cycles focus
//! between the file list, filename input (save mode only), and buttons.

extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::components::browser::{BrowserAction, BrowserMode, DirEntry, FileBrowser};
use crate::components::textinput::TextInput;
use crate::input::{KeyEvent, Direction};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    OpenFile,
    OpenMulti,
    SaveFile,
    SelectDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAction {
    None,
    EnterDir(String),
    Open(Vec<String>),
    OpenDir(String),
    Save(String),
    SelectDir(String),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogFocus {
    #[default]
    FileList,
    FilenameInput,
    Buttons,
}

pub struct FileDialog {
    browser: FileBrowser,
    mode: DialogMode,
    focus: DialogFocus,
    filename_input: TextInput,
    selected_button: usize,
    title: String,
    border_fg: u8,
    path_fg: u8,
    cursor_bg: u8,
    button_fg: u8,
    button_bg: u8,
    selected_bg: u8,
}

impl FileDialog {
    pub fn open_file(cwd: &str, page_size: usize) -> Self {
        Self::new(DialogMode::OpenFile, cwd, page_size)
    }

    pub fn open_multi(cwd: &str, page_size: usize) -> Self {
        let mut d = Self::new(DialogMode::OpenMulti, cwd, page_size);
        d.browser.set_mode(BrowserMode::FilesAndDirs);
        d
    }

    pub fn save_file(cwd: &str, page_size: usize) -> Self {
        let mut d = Self::new(DialogMode::SaveFile, cwd, page_size);
        d.browser.set_mode(BrowserMode::FilesAndDirs);
        d
    }

    pub fn select_dir(cwd: &str, page_size: usize) -> Self {
        let mut d = Self::new(DialogMode::SelectDir, cwd, page_size);
        d.browser.set_mode(BrowserMode::DirsOnly);
        d
    }

    fn new(mode: DialogMode, cwd: &str, page_size: usize) -> Self {
        let title = match mode {
            DialogMode::OpenFile => " Open File ",
            DialogMode::OpenMulti => " Add to Playlist ",
            DialogMode::SaveFile => " Save File ",
            DialogMode::SelectDir => " Select Directory ",
        };
        let multi = matches!(mode, DialogMode::OpenMulti);
        let mut browser = FileBrowser::new(cwd, page_size, multi);
        browser.set_title(title);
        match mode {
            DialogMode::OpenFile => browser.set_mode(BrowserMode::FilesAndDirs),
            DialogMode::OpenMulti => browser.set_mode(BrowserMode::FilesAndDirs),
            DialogMode::SaveFile => browser.set_mode(BrowserMode::FilesAndDirs),
            DialogMode::SelectDir => browser.set_mode(BrowserMode::DirsOnly),
        }
        FileDialog {
            browser,
            mode,
            focus: DialogFocus::FileList,
            filename_input: TextInput::with_placeholder("filename"),
            selected_button: 0,
            title: String::from(title),
            border_fg: COLOR_DEFAULT,
            path_fg: 6,
            cursor_bg: 4,
            button_fg: COLOR_DEFAULT,
            button_bg: COLOR_DEFAULT,
            selected_bg: 4,
        }
    }

    pub fn cwd(&self) -> &str {
        self.browser.cwd()
    }

    pub fn set_cwd(&mut self, cwd: &str) {
        self.browser.set_cwd(cwd);
    }

    pub fn set_entries(&mut self, entries: Vec<DirEntry>) {
        self.browser.set_entries(entries);
    }

    pub fn set_filename(&mut self, name: &str) {
        self.filename_input.clear();
        for ch in name.chars() {
            self.filename_input.insert(ch);
        }
    }

    pub fn filename(&self) -> String {
        self.filename_input.value()
    }

    /// Draw as a centered modal on a blacked-out screen.
    pub fn draw_modal(&self, screen_w: usize, screen_h: usize, buf: &mut View) {
        for r in 0..screen_h {
            for c in 0..screen_w {
                buf.set(r, c, Cell::new(' ').bg(0));
            }
        }
        let box_w = (screen_w * 4 / 5).min(80).max(40);
        let box_h = (screen_h * 4 / 5).min(24).max(10);
        let col = screen_w.saturating_sub(box_w) / 2;
        let row = screen_h.saturating_sub(box_h) / 2;
        self.draw(Rect::new(col, row, box_w, box_h), buf);
    }

    pub fn focus(&self) -> DialogFocus {
        self.focus
    }

    pub fn set_focus(&mut self, focus: DialogFocus) {
        self.focus = focus;
    }

    pub fn title(mut self, t: &str) -> Self {
        self.title = String::from(t);
        self.browser.set_title(t);
        self
    }

    pub fn border_fg(mut self, fg: u8) -> Self { self.border_fg = fg; self }
    pub fn cursor_bg(mut self, bg: u8) -> Self { self.cursor_bg = bg; self }
    pub fn selected_bg(mut self, bg: u8) -> Self { self.selected_bg = bg; self }

    fn button_labels(&self) -> Vec<&'static str> {
        match self.mode {
            DialogMode::OpenFile => vec!["Open", "Cancel"],
            DialogMode::OpenMulti => vec!["Add", "Open Dir", "Cancel"],
            DialogMode::SaveFile => vec!["Save", "Cancel"],
            DialogMode::SelectDir => vec!["Select", "Cancel"],
        }
    }

    fn next_button(&mut self) {
        let count = self.button_labels().len();
        if self.selected_button + 1 < count {
            self.selected_button += 1;
        }
    }

    fn prev_button(&mut self) {
        self.selected_button = self.selected_button.saturating_sub(1);
    }

    fn tab_next(&mut self) {
        self.focus = match (self.mode, self.focus) {
            (DialogMode::SaveFile, DialogFocus::FileList) => DialogFocus::FilenameInput,
            (DialogMode::SaveFile, DialogFocus::FilenameInput) => DialogFocus::Buttons,
            (_, DialogFocus::FileList) => DialogFocus::Buttons,
            (_, DialogFocus::Buttons) => DialogFocus::FileList,
            other => other.1,
        };
    }

    fn confirm(&self) -> DialogAction {
        let labels = self.button_labels();
        let label = labels.get(self.selected_button).copied().unwrap_or("Cancel");
        match label {
            "Cancel" => DialogAction::Cancel,
            "Open" => {
                if let Some(entry) = self.browser.selected_entry() {
                    if entry.kind == crate::components::browser::EntryKind::Directory {
                        let path = join_path(self.browser.cwd(), &entry.name);
                        DialogAction::EnterDir(path)
                    } else {
                        let path = join_path(self.browser.cwd(), &entry.name);
                        DialogAction::Open(alloc::vec![path])
                    }
                } else {
                    DialogAction::None
                }
            }
            "Save" => {
                let name = self.filename_input.value();
                if name.is_empty() {
                    if let Some(entry) = self.browser.selected_entry() {
                        let path = join_path(self.browser.cwd(), &entry.name);
                        DialogAction::Save(path)
                    } else {
                        DialogAction::None
                    }
                } else {
                    let path = join_path(self.browser.cwd(), &name);
                    DialogAction::Save(path)
                }
            }
            "Add" => {
                let marked = self.browser.marked_indices();
                if !marked.is_empty() {
                    let paths: Vec<String> = marked
                        .iter()
                        .map(|&i| join_path(self.browser.cwd(), &self.browser.entries()[i].name))
                        .collect();
                    DialogAction::Open(paths)
                } else if let Some(entry) = self.browser.selected_entry() {
                    if entry.kind == crate::components::browser::EntryKind::Directory {
                        let path = join_path(self.browser.cwd(), &entry.name);
                        DialogAction::EnterDir(path)
                    } else {
                        let path = join_path(self.browser.cwd(), &entry.name);
                        DialogAction::Open(alloc::vec![path])
                    }
                } else {
                    DialogAction::None
                }
            }
            "Open Dir" => {
                if let Some(entry) = self.browser.selected_entry() {
                    if entry.kind == crate::components::browser::EntryKind::Directory {
                        if entry.name == "./" {
                            DialogAction::OpenDir(String::from(self.browser.cwd()))
                        } else if entry.name == "../" || entry.name == ".." {
                            DialogAction::OpenDir(parent_dir(self.browser.cwd()))
                        } else {
                            let path = join_path(self.browser.cwd(), &entry.name);
                            DialogAction::OpenDir(path)
                        }
                    } else {
                        DialogAction::OpenDir(String::from(self.browser.cwd()))
                    }
                } else {
                    DialogAction::OpenDir(String::from(self.browser.cwd()))
                }
            }
            "Select" => {
                if let Some(entry) = self.browser.selected_entry() {
                    let path = if entry.kind == crate::components::browser::EntryKind::Directory {
                        join_path(self.browser.cwd(), &entry.name)
                    } else {
                        String::from(self.browser.cwd())
                    };
                    DialogAction::SelectDir(path)
                } else {
                    DialogAction::SelectDir(String::from(self.browser.cwd()))
                }
            }
            _ => DialogAction::Cancel,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogAction {
        match key {
            KeyEvent::Esc => return DialogAction::Cancel,
            KeyEvent::Ctrl('c') => return DialogAction::Cancel,

            KeyEvent::Tab => {
                self.tab_next();
                return DialogAction::None;
            }

            KeyEvent::ShiftTab => {
                self.focus = match (self.mode, self.focus) {
                    (DialogMode::SaveFile, DialogFocus::Buttons) => DialogFocus::FilenameInput,
                    (DialogMode::SaveFile, DialogFocus::FilenameInput) => DialogFocus::FileList,
                    (_, DialogFocus::Buttons) => DialogFocus::FileList,
                    (_, DialogFocus::FileList) => DialogFocus::Buttons,
                    other => other.1,
                };
                return DialogAction::None;
            }

            KeyEvent::Arrow(Direction::Left) if self.focus == DialogFocus::Buttons => {
                self.prev_button();
                return DialogAction::None;
            }
            KeyEvent::Arrow(Direction::Right) if self.focus == DialogFocus::Buttons => {
                self.next_button();
                return DialogAction::None;
            }

            _ => {}
        }

        match self.focus {
            DialogFocus::FileList => {
                match key {
                    KeyEvent::Enter => {
                        let action = self.browser.handle_key(KeyEvent::Enter);
                        return self.translate_browser_action(action);
                    }
                    _ => {
                        self.browser.handle_key(key);
                        if self.mode == DialogMode::SaveFile {
                            let name = self.browser.selected_entry()
                                .filter(|e| e.kind == crate::components::browser::EntryKind::File)
                                .map(|e| e.name.clone());
                            if let Some(n) = name {
                                self.set_filename(&n);
                            }
                        }
                        return DialogAction::None;
                    }
                }
            }
            DialogFocus::FilenameInput => {
                match key {
                    KeyEvent::Enter => return self.confirm(),
                    KeyEvent::Backspace => {
                        if self.filename_input.value().is_empty() {
                            let parent = parent_dir(self.browser.cwd());
                            if parent != self.browser.cwd() {
                                return DialogAction::EnterDir(parent);
                            }
                        }
                        self.filename_input.backspace();
                        return DialogAction::None;
                    }
                    KeyEvent::Arrow(Direction::Left) => { self.filename_input.left(); return DialogAction::None; }
                    KeyEvent::Arrow(Direction::Right) => { self.filename_input.right(); return DialogAction::None; }
                    KeyEvent::Home => { self.filename_input.home(); return DialogAction::None; }
                    KeyEvent::End => { self.filename_input.end(); return DialogAction::None; }
                    KeyEvent::Char(ch) => {
                        self.filename_input.insert(ch);
                        return DialogAction::None;
                    }
                    _ => return DialogAction::None,
                }
            }
            DialogFocus::Buttons => {
                match key {
                    KeyEvent::Enter => return self.confirm(),
                    KeyEvent::Arrow(Direction::Up) | KeyEvent::Arrow(Direction::Down) => {
                        self.focus = DialogFocus::FileList;
                        return DialogAction::None;
                    }
                    _ => return DialogAction::None,
                }
            }
        }
    }

    fn translate_browser_action(&mut self, action: BrowserAction) -> DialogAction {
        match action {
            BrowserAction::None => DialogAction::None,
            BrowserAction::Cancel => DialogAction::Cancel,
            BrowserAction::EnterDir(path) => DialogAction::EnterDir(path),
            BrowserAction::Confirm(paths) => {
                match self.mode {
                    DialogMode::OpenFile | DialogMode::OpenMulti => DialogAction::Open(paths),
                    DialogMode::SaveFile => {
                        if let Some(p) = paths.first() {
                            if let Some(name) = file_name(p) {
                                self.set_filename(name);
                            }
                            DialogAction::None
                        } else {
                            DialogAction::None
                        }
                    }
                    DialogMode::SelectDir => {
                        if let Some(p) = paths.first() {
                            DialogAction::SelectDir(p.clone())
                        } else {
                            DialogAction::None
                        }
                    }
                }
            }
        }
    }

    fn min_width(&self) -> usize {
        30
    }

    fn min_height(&self) -> usize {
        match self.mode {
            DialogMode::OpenFile => 10,
            DialogMode::OpenMulti => 10,
            DialogMode::SaveFile => 12,
            DialogMode::SelectDir => 10,
        }
    }
}

impl Drawable for FileDialog {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width < 4 || area.height < 4 {
            return;
        }

        let last_row = area.y + area.height - 1;
        let last_col = area.x + area.width - 1;

        // Border
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

        // Title
        if area.width > 6 {
            let title_start = area.x + 2;
            for (i, ch) in self.title.chars().enumerate() {
                if title_start + i >= last_col - 1 { break; }
                buf.set(area.y, title_start + i, Cell::new(ch).fg(self.border_fg).attrs(ATTR_BOLD));
            }
        }

        let inner_x = area.x + 1;
        let inner_w = area.width.saturating_sub(2);

        // Path bar
        let path_y = area.y + 1;
        if inner_w > 2 {
            let path_prefix = " Path: ";
            for (i, ch) in path_prefix.chars().enumerate() {
                if inner_x + i >= last_col { break; }
                buf.set(path_y, inner_x + i, Cell::new(ch).fg(self.path_fg));
            }
            let path_start = inner_x + path_prefix.chars().count();
            for (i, ch) in self.browser.cwd().chars().enumerate() {
                if path_start + i >= last_col { break; }
                buf.set(path_y, path_start + i, Cell::new(ch).fg(self.path_fg));
            }
        }

        let mut content_top = path_y + 1;
        let mut content_bottom = last_row;

        // Filename input (save mode only)
        if self.mode == DialogMode::SaveFile {
            let input_y = path_y + 1;
            let label = " Name: ";
            for (i, ch) in label.chars().enumerate() {
                if inner_x + i >= last_col { break; }
                buf.set(input_y, inner_x + i, Cell::new(ch).fg(self.path_fg));
            }
            let input_x = inner_x + label.chars().count();
            let input_w = inner_w.saturating_sub(label.chars().count());

            let is_focused = self.focus == DialogFocus::FilenameInput;
            let input_bg = if is_focused { self.cursor_bg } else { COLOR_DEFAULT };

            for i in 0..input_w {
                buf.set(input_y, input_x + i, Cell::new(' ').bg(input_bg));
            }
            let val = self.filename_input.value();
            for (i, ch) in val.chars().enumerate() {
                if i >= input_w { break; }
                buf.set(input_y, input_x + i, Cell::new(ch).bg(input_bg));
            }
            if is_focused {
                let cursor_col = input_x + self.filename_input.cursor_pos().min(input_w);
                if cursor_col < last_col {
                    let existing = buf.get(input_y, cursor_col).map(|c| c.ch).unwrap_or(' ');
                    buf.set(input_y, cursor_col, Cell::new(existing).bg(self.cursor_bg).attrs(ATTR_BOLD));
                }
            }

            content_top = input_y + 1;
        }

        // Help line
        let help_y = last_row - 2;
        let help_text = match self.mode {
            DialogMode::OpenMulti => " Enter: open  Space: mark  Tab: focus  Esc: cancel ",
            _ => " Enter: open  Tab: focus  Esc: cancel ",
        };
        for (i, ch) in help_text.chars().enumerate() {
            if inner_x + i >= last_col { break; }
            buf.set(help_y, inner_x + i, Cell::new(ch).fg(self.path_fg));
        }

        // Buttons
        let button_y = last_row - 1;
        content_bottom = help_y - 1;

        let labels = self.button_labels();
        let total_btn_w: usize = labels.iter().map(|l| l.chars().count() + 4).sum::<usize>() + labels.len().saturating_sub(1);
        let mut btn_x = inner_x + (inner_w.saturating_sub(total_btn_w)) / 2;
        let buttons_focused = self.focus == DialogFocus::Buttons;

        for (i, label) in labels.iter().enumerate() {
            let is_selected = i == self.selected_button && buttons_focused;
            let bg = if is_selected { self.selected_bg } else { self.button_bg };
            let fg = if is_selected { self.button_fg } else { self.button_fg };

            buf.set(button_y, btn_x, Cell::new('[').fg(fg).bg(bg).attrs(ATTR_BOLD));
            btn_x += 1;
            buf.set(button_y, btn_x, Cell::new(' ').fg(fg).bg(bg));
            btn_x += 1;
            for ch in label.chars() {
                buf.set(button_y, btn_x, Cell::new(ch).fg(fg).bg(bg).attrs(ATTR_BOLD));
                btn_x += 1;
            }
            buf.set(button_y, btn_x, Cell::new(' ').fg(fg).bg(bg));
            btn_x += 1;
            buf.set(button_y, btn_x, Cell::new(']').fg(fg).bg(bg).attrs(ATTR_BOLD));
            btn_x += 1;
            if i + 1 < labels.len() {
                btn_x += 1;
            }
        }

        // File list area
        let list_height = content_bottom.saturating_sub(content_top);
        if list_height > 0 && inner_w > 0 {
            let list_rect = Rect::new(inner_x, content_top, inner_w, list_height);
            self.draw_file_list(list_rect, buf);
        }
    }
}

impl FileDialog {
    fn draw_file_list(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let entries = &self.browser;
        let visible_range = entries.visible_range();
        let (start, end) = visible_range;
        let is_focused = self.focus == DialogFocus::FileList;

        for i in start..end {
            let display_row = area.y + (i - start);
            if display_row >= area.y + area.height {
                break;
            }
            let entry_idx = match self.browser.marked_indices().get(i - start) {
                _ => i,
            };

            let entry = match entries.selected_entry() {
                _ => {
                    // Access filtered entries through render — delegate to browser
                    break;
                }
            };

            let _ = entry_idx;
            let _ = display_row;
            let _ = is_focused;
        }

        // Delegate to browser's render which handles the filled-line cursor
        // We use render_with_options with borderless style
        use crate::components::browser::BrowserRenderOptions;
        self.browser.render_with_options(
            area.y,
            area.x,
            area.width,
            area.height,
            buf,
            BrowserRenderOptions::borderless_no_header(COLOR_DEFAULT),
        );
    }
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
    if name.starts_with('/') {
        return String::from(name);
    }
    if cwd.ends_with('/') {
        alloc::format!("{}{}", cwd, name)
    } else {
        alloc::format!("{}/{}", cwd, name)
    }
}

fn file_name(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    trimmed.rfind('/').map(|idx| &trimmed[idx + 1..]).or_else(|| {
        if trimmed.is_empty() { None } else { Some(trimmed) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::browser::{DirEntry, EntryKind};

    fn make_entries() -> Vec<DirEntry> {
        vec![
            DirEntry::dir("Documents"),
            DirEntry::dir("Downloads"),
            DirEntry::file("readme.txt", 100),
            DirEntry::file("config.toml", 500),
        ]
    }

    #[test]
    fn filedialog_open_file_mode() {
        let d = FileDialog::open_file("/home", 10);
        assert_eq!(d.mode, DialogMode::OpenFile);
        assert_eq!(d.focus, DialogFocus::FileList);
        assert_eq!(d.button_labels(), vec!["Open", "Cancel"]);
    }

    #[test]
    fn filedialog_save_file_mode() {
        let d = FileDialog::save_file("/home", 10);
        assert_eq!(d.mode, DialogMode::SaveFile);
        assert_eq!(d.button_labels(), vec!["Save", "Cancel"]);
    }

    #[test]
    fn filedialog_select_dir_mode() {
        let d = FileDialog::select_dir("/home", 10);
        assert_eq!(d.mode, DialogMode::SelectDir);
        assert_eq!(d.button_labels(), vec!["Select", "Cancel"]);
    }

    #[test]
    fn filedialog_cancel_on_esc() {
        let mut d = FileDialog::open_file("/home", 10);
        let action = d.handle_key(KeyEvent::Esc);
        assert_eq!(action, DialogAction::Cancel);
    }

    #[test]
    fn filedialog_tab_cycles_focus() {
        let mut d = FileDialog::save_file("/home", 10);
        assert_eq!(d.focus, DialogFocus::FileList);
        d.handle_key(KeyEvent::Tab);
        assert_eq!(d.focus, DialogFocus::FilenameInput);
        d.handle_key(KeyEvent::Tab);
        assert_eq!(d.focus, DialogFocus::Buttons);
        d.handle_key(KeyEvent::Tab);
        assert_eq!(d.focus, DialogFocus::FileList);
    }

    #[test]
    fn filedialog_tab_open_file_skips_input() {
        let mut d = FileDialog::open_file("/home", 10);
        assert_eq!(d.focus, DialogFocus::FileList);
        d.handle_key(KeyEvent::Tab);
        assert_eq!(d.focus, DialogFocus::Buttons);
        d.handle_key(KeyEvent::Tab);
        assert_eq!(d.focus, DialogFocus::FileList);
    }

    #[test]
    fn filedialog_arrow_down_moves_browser_cursor() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_entries(make_entries());
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        assert_eq!(d.browser.selected_entry().unwrap().name, "Downloads");
    }

    #[test]
    fn filedialog_arrow_up_moves_browser_cursor() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_entries(make_entries());
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        d.handle_key(KeyEvent::Arrow(Direction::Up));
        assert_eq!(d.browser.selected_entry().unwrap().name, "Documents");
    }

    #[test]
    fn filedialog_filename_input() {
        let mut d = FileDialog::save_file("/home", 10);
        d.set_focus(DialogFocus::FilenameInput);
        d.handle_key(KeyEvent::Char('t'));
        d.handle_key(KeyEvent::Char('x'));
        d.handle_key(KeyEvent::Char('t'));
        assert_eq!(d.filename(), "txt");
    }

    #[test]
    fn filedialog_save_confirm_with_filename() {
        let mut d = FileDialog::save_file("/home", 10);
        d.set_focus(DialogFocus::FilenameInput);
        d.handle_key(KeyEvent::Char('f'));
        d.handle_key(KeyEvent::Char('o'));
        d.handle_key(KeyEvent::Char('o'));
        d.set_focus(DialogFocus::Buttons);
        // First button is "Save"
        let action = d.handle_key(KeyEvent::Enter);
        assert_eq!(action, DialogAction::Save("/home/foo".to_string()));
    }

    #[test]
    fn filedialog_open_confirm_on_file() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_entries(make_entries());
        // Navigate to readme.txt (index 2)
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        let action = d.handle_key(KeyEvent::Enter);
        match action {
            DialogAction::Open(paths) => {
                assert_eq!(paths.len(), 1);
                assert!(paths[0].contains("readme.txt"));
            }
            _ => panic!("expected Open action, got {:?}", action),
        }
    }

    #[test]
    fn filedialog_open_enter_dir() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_entries(make_entries());
        // First entry is Documents (directory)
        let action = d.handle_key(KeyEvent::Enter);
        match action {
            DialogAction::EnterDir(path) => assert!(path.contains("Documents")),
            _ => panic!("expected EnterDir, got {:?}", action),
        }
    }

    #[test]
    fn filedialog_select_dir_confirm() {
        let mut d = FileDialog::select_dir("/home", 10);
        d.set_entries(make_entries());
        // Navigate to Downloads (index 1)
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        // Use the Select button to confirm the directory
        d.set_focus(DialogFocus::Buttons);
        let action = d.handle_key(KeyEvent::Enter);
        match action {
            DialogAction::SelectDir(path) => assert!(path.contains("Downloads")),
            _ => panic!("expected SelectDir, got {:?}", action),
        }
    }

    #[test]
    fn filedialog_button_left_right_nav() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_focus(DialogFocus::Buttons);
        assert_eq!(d.selected_button, 0);
        d.handle_key(KeyEvent::Arrow(Direction::Right));
        assert_eq!(d.selected_button, 1);
        d.handle_key(KeyEvent::Arrow(Direction::Left));
        assert_eq!(d.selected_button, 0);
    }

    #[test]
    fn filedialog_cancel_button() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_focus(DialogFocus::Buttons);
        d.handle_key(KeyEvent::Arrow(Direction::Right)); // move to Cancel
        let action = d.handle_key(KeyEvent::Enter);
        assert_eq!(action, DialogAction::Cancel);
    }

    #[test]
    fn filedialog_save_picks_filename_from_list() {
        let mut d = FileDialog::save_file("/home", 10);
        d.set_entries(make_entries());
        // Navigate to readme.txt
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        d.handle_key(KeyEvent::Arrow(Direction::Down));
        // Filename should be populated
        assert_eq!(d.filename(), "readme.txt");
    }

    #[test]
    fn filedialog_draw_border() {
        let d = FileDialog::open_file("/home", 10);
        let mut buf = View::new(40, 12);
        d.draw(Rect::new(0, 0, 40, 12), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(buf.get(0, 39).map(|c| c.ch), Some('┐'));
        assert_eq!(buf.get(11, 0).map(|c| c.ch), Some('└'));
        assert_eq!(buf.get(11, 39).map(|c| c.ch), Some('┘'));
    }

    #[test]
    fn filedialog_draw_title() {
        let d = FileDialog::open_file("/home", 10);
        let mut buf = View::new(40, 12);
        d.draw(Rect::new(0, 0, 40, 12), &mut buf);
        // Title is " Open File " starting at col 2
        assert_eq!(buf.get(0, 3).map(|c| c.ch), Some('O'));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('p'));
    }

    #[test]
    fn filedialog_draw_path_bar() {
        let d = FileDialog::open_file("/home/user", 10);
        let mut buf = View::new(40, 12);
        d.draw(Rect::new(0, 0, 40, 12), &mut buf);
        assert_eq!(buf.get(1, 2).map(|c| c.ch), Some('P'));
        assert_eq!(buf.get(1, 8).map(|c| c.ch), Some('/'));
    }

    #[test]
    fn filedialog_draw_buttons() {
        let d = FileDialog::open_file("/home", 10);
        let mut buf = View::new(40, 12);
        d.draw(Rect::new(0, 0, 40, 12), &mut buf);
        // Find "Open" button — should contain 'O' of "Open"
        let found_open = (0..40).any(|x| buf.get(10, x).map(|c| c.ch == 'O').unwrap_or(false));
        assert!(found_open);
    }

    #[test]
    fn filedialog_draw_save_filename() {
        let d = FileDialog::save_file("/home", 10);
        let mut buf = View::new(40, 14);
        d.draw(Rect::new(0, 0, 40, 14), &mut buf);
        // Should have "Name:" label
        let found_name = (0..40).any(|x| buf.get(2, x).map(|c| c.ch == 'N').unwrap_or(false));
        assert!(found_name);
    }

    #[test]
    fn filedialog_draw_file_list() {
        let mut d = FileDialog::open_file("/home", 10);
        d.set_entries(make_entries());
        let mut buf = View::new(40, 12);
        d.draw(Rect::new(0, 0, 40, 12), &mut buf);
        // Should see "Documents" somewhere in the list area
        let found = (2..10).any(|y| (1..30).any(|x| buf.get(y, x).map(|c| c.ch == 'D').unwrap_or(false)));
        assert!(found);
    }

    #[test]
    fn filedialog_set_filename() {
        let mut d = FileDialog::save_file("/home", 10);
        d.set_filename("test.txt");
        assert_eq!(d.filename(), "test.txt");
    }

    #[test]
    fn filedialog_backspace_in_empty_input_goes_up() {
        let mut d = FileDialog::save_file("/home/user/sub", 10);
        d.set_focus(DialogFocus::FilenameInput);
        let action = d.handle_key(KeyEvent::Backspace);
        assert_eq!(action, DialogAction::EnterDir("/home/user".to_string()));
    }

    #[test]
    fn filedialog_too_small_noop() {
        let d = FileDialog::open_file("/home", 10);
        let mut buf = View::new(3, 3);
        d.draw(Rect::new(0, 0, 3, 3), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some(' '));
    }
}
