/*
 * Line Editor
 *
 * This module provides line editing functionality for the shell,
 * handling backspace, enter, and maintaining an input buffer.
 */

use crate::utils::console;
use heapless::{String, Vec};

const MAX_LINE_LENGTH: usize = 256;
const MAX_HISTORY_ENTRIES: usize = 16;

pub struct LineEditor {
    buffer: String<MAX_LINE_LENGTH>,
    history: Vec<String<MAX_LINE_LENGTH>, MAX_HISTORY_ENTRIES>,
    history_index: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            history: Vec::new(),
            history_index: 0,
        }
    }

    pub fn handle_char(&mut self, ch: char) -> Option<String<MAX_LINE_LENGTH>> {
        match ch {
            '\n' | '\r' => {
                // Enter pressed - return the line
                console::write_char('\n');
                let line = self.buffer.clone();
                
                // Add to history if not empty
                if !line.trim().is_empty() {
                    if self.history.is_full() {
                        // Remove oldest entry if history is full
                        self.history.remove(0);
                    }
                    let _ = self.history.push(line.clone());
                    self.history_index = self.history.len();
                }
                
                self.buffer.clear();
                Some(line)
            }
            '\x08' | '\x7F' => {
                // Backspace pressed
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                    console::backspace();
                }
                None
            }
            '\t' => {
                // Tab - could implement completion later
                None
            }
            ch if ch.is_ascii() && !ch.is_control() => {
                // Regular character
                if self.buffer.len() < MAX_LINE_LENGTH - 1 {
                    let _ = self.buffer.push(ch);
                    console::write_char(ch);
                }
                None
            }
            _ => {
                // Ignore other characters
                None
            }
        }
    }

    pub fn get_current_line(&self) -> &str {
        &self.buffer
    }

    pub fn clear_line(&mut self) {
        // Clear current line on screen
        for _ in 0..self.buffer.len() {
            console::backspace();
        }
        self.buffer.clear();
    }

    pub fn get_history(&self) -> &[String<MAX_LINE_LENGTH>] {
        &self.history
    }
}
