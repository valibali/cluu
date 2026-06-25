//! Status bar renderer. Lives on cell row 0; refreshed by clock tick or
//! focus change.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use crate::state::Compositor;

pub fn render_status(comp: &Compositor) -> String {
    let clock_str = if comp.clock_ready {
        let secs = comp.clock_seconds;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("--:--:--")
    };
    let focused_title = comp.focused
        .and_then(|id| comp.windows.iter().find(|w| w.id == id))
        .map(|w| w.title.as_str())
        .unwrap_or("(none)");
    format!(
        " CLUU  [{}]  {}  \u{b7}  {} wins",
        clock_str, focused_title, comp.windows.len()
    )
}
