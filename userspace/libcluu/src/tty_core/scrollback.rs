//! Scrollback ring buffer for VT history.
//!
//! Stores rows that have scrolled off the top of the visible terminal grid.
//! Uses a fixed-capacity ring buffer so memory usage is bounded.

extern crate alloc;
use alloc::vec::Vec;

/// A single row saved in the scrollback history buffer.
pub struct HistoryRow {
    pub chars: Vec<u8>,
    pub fg: Vec<u32>,
    pub bg: Vec<u32>,
}

/// Fixed-capacity ring buffer of scrolled-off terminal rows.
///
/// Once the buffer reaches capacity, pushing a new row overwrites the oldest
/// entry. `len()` never exceeds `capacity`.
pub struct Scrollback {
    history: Vec<HistoryRow>,
    start: usize,
    len: usize,
    capacity: usize,
}

impl Scrollback {
    /// Create a new empty scrollback buffer with the given maximum row count.
    pub fn new(capacity: usize) -> Self {
        Self {
            history: Vec::new(),
            start: 0,
            len: 0,
            capacity,
        }
    }

    /// Push a row into the scrollback buffer.
    ///
    /// If the buffer has not yet reached capacity the row is appended and
    /// `len` is incremented. Once at capacity the oldest entry is overwritten
    /// and `len` stays at `capacity`.
    pub fn push(&mut self, row: HistoryRow) {
        if self.history.len() < self.capacity {
            self.history.push(row);
            self.len += 1;
        } else {
            self.history[self.start] = row;
            self.start = (self.start + 1) % self.capacity;
        }
    }

    /// Number of rows currently stored (at most `capacity`).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no rows are stored.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return the row at `idx_from_oldest` (0 = oldest stored row).
    ///
    /// Returns `None` if the index is out of range.
    pub fn row(&self, idx_from_oldest: usize) -> Option<&HistoryRow> {
        if idx_from_oldest >= self.len {
            return None;
        }
        let real = (self.start + idx_from_oldest) % self.capacity.max(1);
        self.history.get(real)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec;

    fn row(c: u8) -> HistoryRow {
        HistoryRow {
            chars: vec![c],
            fg: vec![0],
            bg: vec![0],
        }
    }

    #[test]
    fn ring_overwrites_oldest_at_capacity() {
        let mut s = Scrollback::new(2);
        s.push(row(b'a'));
        s.push(row(b'b'));
        s.push(row(b'c'));
        assert_eq!(s.len(), 2);
        assert_eq!(s.row(0).unwrap().chars, vec![b'b']);
        assert_eq!(s.row(1).unwrap().chars, vec![b'c']);
    }

    #[test]
    fn empty_scrollback() {
        let s = Scrollback::new(4);
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        assert!(s.row(0).is_none());
    }

    #[test]
    fn partial_fill_below_capacity() {
        let mut s = Scrollback::new(4);
        s.push(row(b'x'));
        s.push(row(b'y'));
        assert_eq!(s.len(), 2);
        assert_eq!(s.row(0).unwrap().chars, vec![b'x']);
        assert_eq!(s.row(1).unwrap().chars, vec![b'y']);
        assert!(s.row(2).is_none());
    }

    #[test]
    fn len_caps_at_capacity_after_multiple_overwrites() {
        let mut s = Scrollback::new(3);
        for i in 0..10u8 {
            s.push(row(i));
        }
        assert_eq!(s.len(), 3);
        // Oldest should be row(7), newest row(9)
        assert_eq!(s.row(0).unwrap().chars[0], 7);
        assert_eq!(s.row(1).unwrap().chars[0], 8);
        assert_eq!(s.row(2).unwrap().chars[0], 9);
    }
}
