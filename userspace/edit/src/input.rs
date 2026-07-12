//! Input types re-exported from libtui's input decoder.
//!
//! The editor's mode handlers (normal, insert, visual, ex, prompt) all
//! match on `KeyEvent` / `Direction`. libtui's `input` module defines
//! identical enum shapes, so we re-export them here to avoid touching
//! every mode handler's import paths.

pub use libtui::input::{Direction, KeyEvent};
