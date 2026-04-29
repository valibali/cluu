//! Piece table buffer.
//!
//! Two append-only byte buffers (`original` from the file, `add` for new
//! text) plus an ordered list of pieces. Edits never modify text bytes —
//! only the piece list changes. See spec §4 for the full design.

extern crate alloc;
use alloc::vec::Vec;
use core::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Original,
    Add,
}

#[derive(Clone, Copy, Debug)]
pub struct Piece {
    pub source: Source,
    pub offset: usize,
    pub length: usize,
}

pub struct Buffer {
    pub original: Vec<u8>,
    pub add: Vec<u8>,
    pub pieces: Vec<Piece>,
    pub edit_seq: u64,
    pub line_idx: Vec<usize>,
    pub line_idx_seq: u64,
}

/// (piece_index, byte_offset_within_piece) for a logical byte offset.
/// Position equal to `len()` returns (pieces.len(), 0) — the past-end
/// "insert here" sentinel. Positions beyond return None.
#[derive(Debug, Clone, Copy)]
pub struct Location {
    pub piece_idx: usize,
    pub within: usize,
}

#[derive(Debug)]
pub struct PiecePatch {
    pub range: Range<usize>,
    pub removed: Vec<Piece>,
    pub inserted: Vec<Piece>,
}

impl Buffer {
    /// Construct a fresh buffer from initial file bytes.
    pub fn from_bytes(initial: Vec<u8>) -> Self {
        let length = initial.len();
        let mut pieces = Vec::new();
        if length > 0 {
            pieces.push(Piece { source: Source::Original, offset: 0, length });
        }
        Buffer {
            original: initial,
            add: Vec::new(),
            pieces,
            edit_seq: 0,
            line_idx: Vec::new(),
            line_idx_seq: u64::MAX,
        }
    }

    /// Total logical length in bytes.
    pub fn len(&self) -> usize {
        self.pieces.iter().map(|p| p.length).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty() || self.len() == 0
    }

    pub fn locate(&self, byte_offset: usize) -> Option<Location> {
        let total = self.len();
        if byte_offset > total { return None; }
        if byte_offset == total {
            return Some(Location { piece_idx: self.pieces.len(), within: 0 });
        }
        let mut acc = 0;
        for (i, p) in self.pieces.iter().enumerate() {
            if byte_offset < acc + p.length {
                return Some(Location { piece_idx: i, within: byte_offset - acc });
            }
            acc += p.length;
        }
        None
    }

    /// Insert `text` at logical byte offset `at`. Returns the patch that
    /// can be reverse-applied to undo this insert.
    pub fn insert(&mut self, at: usize, text: &[u8]) -> Option<PiecePatch> {
        let loc = self.locate(at)?;
        let add_offset = self.add.len();
        self.add.extend_from_slice(text);
        let new_piece = Piece { source: Source::Add, offset: add_offset, length: text.len() };

        let (range, removed, inserted) = if loc.within == 0 {
            // Insert before pieces[piece_idx] (or at end).
            (loc.piece_idx..loc.piece_idx, Vec::new(), alloc::vec![new_piece])
        } else {
            // Split pieces[piece_idx] at `within`.
            let target = self.pieces[loc.piece_idx];
            let left = Piece { source: target.source, offset: target.offset, length: loc.within };
            let right = Piece {
                source: target.source,
                offset: target.offset + loc.within,
                length: target.length - loc.within,
            };
            (
                loc.piece_idx..loc.piece_idx + 1,
                alloc::vec![target],
                alloc::vec![left, new_piece, right],
            )
        };

        let patch = PiecePatch {
            range: range.clone(),
            removed,
            inserted: inserted.clone(),
        };
        self.pieces.splice(range, inserted);
        self.edit_seq = self.edit_seq.wrapping_add(1);
        Some(patch)
    }

    /// Delete the byte range `start..end`. Returns the patch.
    pub fn delete(&mut self, range: Range<usize>) -> Option<PiecePatch> {
        if range.start >= range.end { return None; }
        let start = self.locate(range.start)?;
        let end = self.locate(range.end)?;

        // Rebuild the affected pieces: keep prefix of start-piece, drop the
        // middle, keep suffix of end-piece.
        let mut new_pieces = Vec::new();
        if start.within > 0 {
            let p = self.pieces[start.piece_idx];
            new_pieces.push(Piece { source: p.source, offset: p.offset, length: start.within });
        }
        if end.piece_idx < self.pieces.len() && end.within > 0 {
            let p = self.pieces[end.piece_idx];
            new_pieces.push(Piece {
                source: p.source,
                offset: p.offset + end.within,
                length: p.length - end.within,
            });
        }

        let piece_range = start.piece_idx..(end.piece_idx + if end.within > 0 { 1 } else { 0 }).min(self.pieces.len());
        let removed: Vec<Piece> = self.pieces[piece_range.clone()].to_vec();
        let patch = PiecePatch {
            range: piece_range.clone(),
            removed,
            inserted: new_pieces.clone(),
        };
        self.pieces.splice(piece_range, new_pieces);
        self.edit_seq = self.edit_seq.wrapping_add(1);
        Some(patch)
    }

    /// Materialize the entire logical content into a Vec (for tests, save).
    pub fn read_all(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        for p in &self.pieces {
            let src = match p.source {
                Source::Original => &self.original[p.offset..p.offset + p.length],
                Source::Add      => &self.add[p.offset..p.offset + p.length],
            };
            out.extend_from_slice(src);
        }
        out
    }

    /// Return the (cached) byte offsets of each line start. Rebuilds on demand
    /// after edits.
    pub fn line_index(&mut self) -> &[usize] {
        if self.line_idx_seq != self.edit_seq {
            self.rebuild_line_index();
        }
        &self.line_idx
    }

    fn rebuild_line_index(&mut self) {
        self.line_idx.clear();
        self.line_idx.push(0);
        let mut byte_offset = 0;
        for p in &self.pieces {
            let src = match p.source {
                Source::Original => &self.original[p.offset..p.offset + p.length],
                Source::Add      => &self.add[p.offset..p.offset + p.length],
            };
            for (i, b) in src.iter().enumerate() {
                if *b == b'\n' {
                    self.line_idx.push(byte_offset + i + 1);
                }
            }
            byte_offset += p.length;
        }
        self.line_idx_seq = self.edit_seq;
    }

    /// Convert byte_offset → (line, col_in_bytes). Both are 0-indexed.
    pub fn line_col(&mut self, byte_offset: usize) -> (usize, usize) {
        let idx = self.line_index();
        let line = match idx.binary_search(&byte_offset) {
            Ok(i)  => i,
            Err(i) => i.saturating_sub(1),
        };
        let col = byte_offset - idx[line];
        (line, col)
    }

    /// Number of lines in the buffer (always ≥ 1; an empty buffer has one line).
    pub fn line_count(&mut self) -> usize {
        self.line_index().len().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn empty_buffer() {
        let b = Buffer::from_bytes(Vec::new());
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
    }

    #[test]
    fn single_piece_initial() {
        let b = Buffer::from_bytes(b"hello".to_vec());
        assert_eq!(b.len(), 5);
        assert_eq!(b.pieces.len(), 1);
        assert_eq!(b.pieces[0].source, Source::Original);
        assert_eq!(b.pieces[0].offset, 0);
        assert_eq!(b.pieces[0].length, 5);
    }

    #[test]
    fn locate_in_single_piece() {
        let b = Buffer::from_bytes(b"hello world".to_vec());
        let loc = b.locate(0).expect("loc 0");
        assert_eq!(loc.piece_idx, 0);
        assert_eq!(loc.within, 0);

        let loc = b.locate(5).expect("loc 5");
        assert_eq!(loc.piece_idx, 0);
        assert_eq!(loc.within, 5);

        let loc = b.locate(11).expect("loc end");
        assert_eq!(loc.piece_idx, 1);    // past last piece
        assert_eq!(loc.within, 0);
    }

    #[test]
    fn locate_past_end_returns_none() {
        let b = Buffer::from_bytes(b"abc".to_vec());
        assert!(b.locate(100).is_none());
    }

    #[test]
    fn insert_at_start() {
        let mut b = Buffer::from_bytes(b"world".to_vec());
        let patch = b.insert(0, b"hello ").expect("insert");
        assert_eq!(b.read_all(), b"hello world".to_vec());
        // After: pieces = [Add(0,6), Original(0,5)]
        assert_eq!(b.pieces.len(), 2);
        assert_eq!(patch.range, 0..0);
        assert_eq!(patch.removed.len(), 0);
        assert_eq!(patch.inserted.len(), 1);
    }

    #[test]
    fn insert_in_middle_splits_piece() {
        let mut b = Buffer::from_bytes(b"hello world".to_vec());
        b.insert(5, b" cruel").expect("insert");
        assert_eq!(b.read_all(), b"hello cruel world".to_vec());
        // After: pieces = [Original(0,5), Add(0,6), Original(5,6)]
        assert_eq!(b.pieces.len(), 3);
    }

    #[test]
    fn insert_at_end() {
        let mut b = Buffer::from_bytes(b"abc".to_vec());
        b.insert(3, b"def").expect("insert");
        assert_eq!(b.read_all(), b"abcdef".to_vec());
    }

    #[test]
    fn delete_range() {
        let mut b = Buffer::from_bytes(b"hello world".to_vec());
        let _patch = b.delete(5..6).expect("delete space");
        assert_eq!(b.read_all(), b"helloworld".to_vec());
    }

    #[test]
    fn delete_spanning_pieces() {
        let mut b = Buffer::from_bytes(b"hello".to_vec());
        b.insert(5, b" world").expect("ins");
        // pieces = [Original(0,5), Add(0,6)]
        b.delete(3..8).expect("del across boundary");
        // "hel" + "rld" = "helrld"
        assert_eq!(b.read_all(), b"helrld".to_vec());
    }

    #[test]
    fn line_index_basic() {
        let mut b = Buffer::from_bytes(b"line1\nline2\nline3".to_vec());
        let idx = b.line_index();
        // 3 lines: starts at 0, 6, 12
        assert_eq!(idx, &[0, 6, 12]);
    }

    #[test]
    fn line_index_trailing_newline() {
        let mut b = Buffer::from_bytes(b"a\nb\n".to_vec());
        let idx = b.line_index();
        // 3 lines: "a", "b", "" — starts at 0, 2, 4
        assert_eq!(idx, &[0, 2, 4]);
    }

    #[test]
    fn line_index_invalidates_after_edit() {
        let mut b = Buffer::from_bytes(b"abc\ndef".to_vec());
        b.line_index();  // populate
        b.insert(0, b"X").expect("ins");
        let idx = b.line_index();
        // After: "Xabc\ndef" — line starts at 0, 5
        assert_eq!(idx, &[0, 5]);
    }
}
