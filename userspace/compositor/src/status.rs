//! Cached status bar cells for compositor-owned row 0.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub struct StatusState<'a> {
    pub cols: u16,
    pub clock_ready: bool,
    pub clock_seconds: u64,
    pub focused_title: &'a str,
    pub window_count: usize,
}

pub struct StatusCache {
    cells: Vec<u8>,
    cols: u16,
    clock_ready: bool,
    clock_seconds: u64,
    focused_title: String,
    window_count: usize,
}

impl StatusCache {
    pub const fn new() -> Self {
        Self {
            cells: Vec::new(),
            cols: 0,
            clock_ready: false,
            clock_seconds: 0,
            focused_title: String::new(),
            window_count: 0,
        }
    }

    pub fn update(&mut self, state: StatusState<'_>) -> bool {
        if self.cols == state.cols
            && self.clock_ready == state.clock_ready
            && self.clock_seconds == state.clock_seconds
            && self.focused_title == state.focused_title
            && self.window_count == state.window_count
        {
            return false;
        }

        self.cols = state.cols;
        self.clock_ready = state.clock_ready;
        self.clock_seconds = state.clock_seconds;
        self.focused_title.clear();
        self.focused_title.push_str(state.focused_title);
        self.window_count = state.window_count;

        let clock = if state.clock_ready {
            let secs = state.clock_seconds;
            let h = (secs / 3600) % 24;
            let m = (secs / 60) % 60;
            let s = secs % 60;
            format!("{:02}:{:02}:{:02}", h, m, s)
        } else {
            String::from("--:--:--")
        };
        self.cells = format!(
            " CLUU  [{}]  {}  \u{b7}  {} wins",
            clock, state.focused_title, state.window_count
        )
        .into_bytes();
        self.cells.resize(state.cols as usize, b' ');
        self.cells.truncate(state.cols as usize);
        true
    }

    pub fn cells(&self) -> &[u8] {
        &self.cells
    }
}

#[cfg(test)]
mod tests {
    use super::{StatusCache, StatusState};

    fn state<'a>(seconds: u64, title: &'a str, count: usize) -> StatusState<'a> {
        StatusState {
            cols: 80,
            clock_ready: true,
            clock_seconds: seconds,
            focused_title: title,
            window_count: count,
        }
    }

    #[test]
    fn update_changes_only_clock_digits_when_one_second_advances() {
        // Given: cached status cells for one focused window.
        let mut cache = StatusCache::new();
        cache.update(state(1, "shell", 1));
        let before = cache.cells().to_vec();

        // When: only clock seconds advance.
        cache.update(state(2, "shell", 1));
        let changed = before
            .iter()
            .zip(cache.cells())
            .filter(|(old, new)| old != new)
            .count();

        // Then: one status cell changes, not the full row.
        assert_eq!(changed, 1);
    }

    #[test]
    fn update_reports_unchanged_for_identical_mouse_loop_state() {
        // Given: an initialized status cache.
        let mut cache = StatusCache::new();
        cache.update(state(1, "shell", 1));

        // When: compositor state visible in the status row is unchanged.
        let changed = cache.update(state(1, "shell", 1));

        // Then: no status regeneration is requested.
        assert!(!changed);
    }

    #[test]
    fn update_refreshes_title_and_window_count() {
        // Given: status cells for one focused window.
        let mut cache = StatusCache::new();
        cache.update(state(1, "shell", 1));

        // When: focus/title and window count change.
        let changed = cache.update(state(1, "editor", 2));

        // Then: cached cells contain the new visible state.
        assert!(changed && cache.cells().windows(6).any(|part| part == b"editor"));
    }
}
