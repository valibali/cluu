/*
 * Line Editor
 *
 * Line editing for the shell, using the kernel heap (alloc::String / Vec).
 */

use crate::utils::console;
use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_LINE_LENGTH: usize = 256;

pub struct LineEditor {
    buffer: String,
    history: Vec<String>,
    history_limit: usize,
    history_index: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            buffer: String::with_capacity(MAX_LINE_LENGTH),
            history: Vec::new(),
            history_limit: 16,
            history_index: 0,
        }
    }

    /// Handle a single char of input.
    /// Returns Some(line) when Enter is pressed and a full line is ready.
    pub fn handle_char(&mut self, ch: char) -> Option<String> {
        match ch {
            '\n' | '\r' => {
                // Enter pressed - return the line
                console::write_char('\n');
                let line = self.buffer.clone();

                // Add to history if not empty
                if !line.trim().is_empty() {
                    if self.history.len() >= self.history_limit {
                        // Remove oldest entry if history is full
                        self.history.remove(0);
                    }
                    self.history.push(line.clone());
                    self.history_index = self.history.len();
                }

                self.buffer.clear();
                Some(line)
            }
            '\x08' | '\x7F' => {
                // Backspace
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                    console::backspace();
                }
                None
            }
            '\t' => {
                // Tab – completion later
                None
            }
            ch if ch.is_ascii() && !ch.is_control() => {
                // Regular printable ASCII
                if self.buffer.len() < MAX_LINE_LENGTH - 1 {
                    self.buffer.push(ch);
                    console::write_char(ch);
                }
                None
            }
            _ => None,
        }
    }

    pub fn get_current_line(&self) -> &str {
        &self.buffer
    }

    pub fn clear_line(&mut self) {
        // Clear current buffer from screen
        for _ in 0..self.buffer.len() {
            console::backspace();
        }
        self.buffer.clear();
    }

    pub fn get_history(&self) -> &[String] {
        &self.history
    }
}
