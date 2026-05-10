//! Status bar renderer. Lives on cell row 0; refreshed by clock tick or
//! focus change.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use crate::state::Compositor;

pub fn render_status(comp: &Compositor) -> String {
    let secs = comp.clock_seconds;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let focused_title = comp.focused
        .and_then(|id| comp.windows.iter().find(|w| w.id == id))
        .map(|w| w.title.as_str())
        .unwrap_or("(none)");
    format!(
        "[{:02}:{:02}:{:02}]  focused: {}   |   windows: {}",
        h, m, s, focused_title, comp.windows.len()
    )
}
