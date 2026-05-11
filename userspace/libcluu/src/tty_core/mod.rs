//! Shared tty core: line discipline, scrollback ring, and extended-key map.
//! Consumed by the legacy `userspace/tty` service and by `cluuterm`.

pub mod keymap;
pub mod line_discipline;
pub use line_discipline::{LineDiscipline, EchoAction, LineEffect, TermMode};
// pub mod scrollback;       — added in Task 9
