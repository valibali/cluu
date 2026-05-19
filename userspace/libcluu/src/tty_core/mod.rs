//! Shared tty core: line discipline, scrollback ring, and extended-key map.
//! Consumed by the legacy `userspace/tty` service and by `cluuterm`.

pub mod keymap;
pub mod line_discipline;
pub mod routing;
pub mod scrollback;
pub use line_discipline::{
    LineDiscipline, EchoAction, LineEffect, TermMode,
    // Spec-2 line-discipline output API
    LineDiscOutput, SignalNum, TermiosErr,
};
pub use routing::{ServiceAction, route_input_byte, translate_output};
pub use scrollback::{HistoryRow, Scrollback};
