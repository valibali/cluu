//! Undo / redo stack on top of piece-table edits. See spec §4.4–4.6.
//!
//! Coarse vim-style grouping: one undo entry per NORMAL command,
//! one entry per INSERT session, one entry per visual operator,
//! one entry per `:s` substitute. INSERT-session accumulation is
//! handled by `UndoBuilder`.

extern crate alloc;
use alloc::vec::Vec;
use crate::piece::{Buffer, Piece, PiecePatch};
use core::ops::Range;

#[derive(Debug)]
pub struct UndoEntry {
    pub cursor_before: usize,
    pub cursor_after: usize,
    pub patch: PiecePatch,
}

pub struct UndoStack {
    entries: Vec<UndoEntry>,
    head: usize,
    pending: Option<UndoBuilder>,
}

pub struct UndoBuilder {
    cursor_before: usize,
    /// Snapshot of the affected piece range at session start.
    initial_pieces: Vec<Piece>,
    /// Index in `pieces` where edits started.
    range_start: usize,
}

impl UndoStack {
    pub fn new() -> Self {
        UndoStack { entries: Vec::new(), head: 0, pending: None }
    }

    /// Record a single atomic edit (NORMAL command, visual op, :s).
    pub fn record(&mut self, cursor_before: usize, cursor_after: usize, patch: PiecePatch) {
        self.entries.truncate(self.head);
        self.entries.push(UndoEntry { cursor_before, cursor_after, patch });
        self.head = self.entries.len();
    }

    /// Begin an INSERT-session group. Caller must call `commit_session` on Esc.
    pub fn begin_session(&mut self, cursor_before: usize, buf: &Buffer, range_start: usize) {
        self.pending = Some(UndoBuilder {
            cursor_before,
            initial_pieces: buf.pieces[range_start..].to_vec(),
            range_start,
        });
    }

    /// Commit the in-progress INSERT session as a single undo entry.
    pub fn commit_session(&mut self, cursor_after: usize, buf: &Buffer) {
        let Some(b) = self.pending.take() else { return; };
        let final_range = b.range_start..buf.pieces.len();
        let final_pieces = buf.pieces[final_range.clone()].to_vec();
        if b.initial_pieces == final_pieces {
            return;  // no-op session, don't record
        }
        let patch = PiecePatch {
            range: b.range_start..b.range_start + b.initial_pieces.len(),
            removed: b.initial_pieces,
            inserted: final_pieces,
        };
        self.entries.truncate(self.head);
        self.entries.push(UndoEntry { cursor_before: b.cursor_before, cursor_after, patch });
        self.head = self.entries.len();
    }

    /// Reverse the most recent entry. Returns the cursor position to restore.
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        if self.head == 0 { return None; }
        self.head -= 1;
        let entry = &self.entries[self.head];
        // Reverse-apply: replace `inserted` with `removed`.
        let span = entry.patch.range.start..entry.patch.range.start + entry.patch.inserted.len();
        buf.pieces.splice(span, entry.patch.removed.iter().cloned());
        buf.edit_seq = buf.edit_seq.wrapping_add(1);
        Some(entry.cursor_before)
    }

    /// Forward-apply the next redo entry. Returns the cursor to restore.
    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        if self.head >= self.entries.len() { return None; }
        let entry = &self.entries[self.head];
        let span = entry.patch.range.start..entry.patch.range.start + entry.patch.removed.len();
        buf.pieces.splice(span, entry.patch.inserted.iter().cloned());
        buf.edit_seq = buf.edit_seq.wrapping_add(1);
        self.head += 1;
        Some(entry.cursor_after)
    }
}

/// PartialEq for Piece (needed by commit_session's no-op check).
impl PartialEq for Piece {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.offset == other.offset && self.length == other.length
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn record_then_undo_round_trips() {
        let mut buf = Buffer::from_bytes(b"hello".to_vec());
        let mut stack = UndoStack::new();
        let patch = buf.insert(5, b" world").expect("ins");
        stack.record(5, 11, patch);
        assert_eq!(buf.read_all(), b"hello world".to_vec());
        let restored = stack.undo(&mut buf).expect("undo");
        assert_eq!(restored, 5);
        assert_eq!(buf.read_all(), b"hello".to_vec());
    }

    #[test]
    fn redo_after_undo_restores() {
        let mut buf = Buffer::from_bytes(b"abc".to_vec());
        let mut stack = UndoStack::new();
        let patch = buf.insert(3, b"def").expect("ins");
        stack.record(3, 6, patch);
        stack.undo(&mut buf);
        assert_eq!(buf.read_all(), b"abc".to_vec());
        let cur = stack.redo(&mut buf).expect("redo");
        assert_eq!(cur, 6);
        assert_eq!(buf.read_all(), b"abcdef".to_vec());
    }

    #[test]
    fn new_edit_after_undo_truncates_redo() {
        let mut buf = Buffer::from_bytes(b"abc".to_vec());
        let mut stack = UndoStack::new();
        let p1 = buf.insert(3, b"def").expect("ins1");
        stack.record(3, 6, p1);
        stack.undo(&mut buf);  // back to "abc"
        let p2 = buf.insert(3, b"XYZ").expect("ins2");
        stack.record(3, 6, p2);
        // Redo should now do nothing — redo history was truncated.
        assert!(stack.redo(&mut buf).is_none());
        assert_eq!(buf.read_all(), b"abcXYZ".to_vec());
    }
}
