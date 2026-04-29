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
}

/// (piece_index, byte_offset_within_piece) for a logical byte offset.
/// Position equal to `len()` returns (pieces.len(), 0) — the past-end
/// "insert here" sentinel. Positions beyond return None.
#[derive(Debug, Clone, Copy)]
pub struct Location {
    pub piece_idx: usize,
    pub within: usize,
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
}
