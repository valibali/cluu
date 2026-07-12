//! Single-line input — holds buffer, cursor, placeholder.
//!
//! A minimal text input field supporting insert, backspace, delete,
//! and cursor movement. Cursor position is tracked in chars (not bytes).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::View;

/// A single-line text input with cursor and placeholder support.
pub struct TextInput {
    buffer: Vec<char>,
    cursor: usize,
    placeholder: String,
}

impl TextInput {
    /// Create an empty input with no placeholder.
    pub fn new() -> Self {
        TextInput {
            buffer: Vec::new(),
            cursor: 0,
            placeholder: String::new(),
        }
    }

    /// Create an empty input with a placeholder shown when buffer is empty.
    pub fn with_placeholder(placeholder: &str) -> Self {
        TextInput {
            buffer: Vec::new(),
            cursor: 0,
            placeholder: String::from(placeholder),
        }
    }

    /// Insert a char at the cursor position and advance the cursor.
    pub fn insert(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += 1;
    }

    /// Delete the char before the cursor and retreat the cursor.
    /// No-op if cursor is at position 0.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    /// Delete the char at the cursor position. Cursor does not move.
    /// No-op if cursor is at the end.
    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    /// Move cursor left, clamped at 0.
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right, clamped at buffer length.
    pub fn right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    /// Move cursor to the beginning.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to the end.
    pub fn end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// Return the current text content.
    pub fn value(&self) -> String {
        self.buffer.iter().collect()
    }

    /// Clear the buffer and reset cursor to 0.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Current cursor position (char index, not byte index).
    pub fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Write the text into a `View` at `(row, col)`. Shows the placeholder
    /// if the buffer is empty and a placeholder is set.
    pub fn render(&self, row: usize, col: usize, view: &mut View) {
        if self.buffer.is_empty() && !self.placeholder.is_empty() {
            view.write_str(row, col, &self.placeholder);
            return;
        }
        let s: String = self.buffer.iter().collect();
        view.write_str(row, col, &s);
    }
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textinput_new_is_empty() {
        let ti = TextInput::new();
        assert_eq!(ti.value(), "");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_with_placeholder() {
        let ti = TextInput::with_placeholder("type here");
        assert_eq!(ti.value(), "");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_insert_advances_cursor() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.insert('c');
        assert_eq!(ti.value(), "abc");
        assert_eq!(ti.cursor_pos(), 3);
    }

    #[test]
    fn textinput_insert_at_cursor_middle() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.insert('c');
        ti.left();
        ti.insert('X');
        assert_eq!(ti.value(), "abXc");
        assert_eq!(ti.cursor_pos(), 3);
    }

    #[test]
    fn textinput_backspace_removes_before_cursor() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.insert('c');
        ti.backspace();
        assert_eq!(ti.value(), "ab");
        assert_eq!(ti.cursor_pos(), 2);
    }

    #[test]
    fn textinput_backspace_at_start_noop() {
        let mut ti = TextInput::new();
        ti.backspace();
        assert_eq!(ti.value(), "");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_delete_removes_at_cursor() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.insert('c');
        ti.home();
        ti.delete();
        assert_eq!(ti.value(), "bc");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_delete_at_end_noop() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.delete();
        assert_eq!(ti.value(), "a");
        assert_eq!(ti.cursor_pos(), 1);
    }

    #[test]
    fn textinput_left_clamps_at_zero() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.left();
        ti.left();
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_right_clamps_at_len() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.right();
        ti.right();
        assert_eq!(ti.cursor_pos(), 1);
    }

    #[test]
    fn textinput_home_and_end() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.insert('c');
        ti.home();
        assert_eq!(ti.cursor_pos(), 0);
        ti.end();
        assert_eq!(ti.cursor_pos(), 3);
    }

    #[test]
    fn textinput_clear_resets() {
        let mut ti = TextInput::new();
        ti.insert('a');
        ti.insert('b');
        ti.clear();
        assert_eq!(ti.value(), "");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_render_text() {
        let mut ti = TextInput::new();
        ti.insert('h');
        ti.insert('i');
        let mut view = View::new(10, 1);
        ti.render(0, 0, &mut view);
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('h'));
        assert_eq!(view.get(0, 1).map(|c| c.ch), Some('i'));
    }

    #[test]
    fn textinput_render_placeholder_when_empty() {
        let ti = TextInput::with_placeholder("enter name");
        let mut view = View::new(10, 1);
        ti.render(0, 0, &mut view);
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('e'));
        assert_eq!(view.get(0, 1).map(|c| c.ch), Some('n'));
    }

    #[test]
    fn textinput_render_no_placeholder_when_text_present() {
        let mut ti = TextInput::with_placeholder("enter name");
        ti.insert('x');
        let mut view = View::new(20, 1);
        ti.render(0, 0, &mut view);
        assert_eq!(view.get(0, 0).map(|c| c.ch), Some('x'));
        // placeholder should NOT appear
        assert_eq!(view.get(0, 1).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn textinput_render_at_offset() {
        let mut ti = TextInput::new();
        ti.insert('A');
        let mut view = View::new(10, 2);
        ti.render(1, 3, &mut view);
        assert_eq!(view.get(1, 3).map(|c| c.ch), Some('A'));
        assert_eq!(view.get(0, 3).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn textinput_default_is_empty() {
        let ti = TextInput::default();
        assert_eq!(ti.value(), "");
        assert_eq!(ti.cursor_pos(), 0);
    }

    #[test]
    fn textinput_multichar_insert_and_backspace_cycle() {
        let mut ti = TextInput::new();
        for ch in "hello".chars() {
            ti.insert(ch);
        }
        assert_eq!(ti.value(), "hello");
        ti.left();
        ti.left();
        ti.backspace();
        assert_eq!(ti.value(), "helo");
        assert_eq!(ti.cursor_pos(), 2);
        ti.end();
        ti.backspace();
        assert_eq!(ti.value(), "hel");
    }
}
