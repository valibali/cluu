//! Buffer = piece table + file path + dirty flag + cursor.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::piece::Buffer as Pieces;

pub struct EditBuffer {
    pub pieces: Pieces,
    pub path: Option<String>,
    pub dirty: bool,
    pub cursor: usize,
}

impl EditBuffer {
    pub fn new(initial: Vec<u8>, path: Option<String>) -> Self {
        EditBuffer {
            pieces: Pieces::from_bytes(initial),
            path,
            dirty: false,
            cursor: 0,
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new(), None)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_clean() {
        let b = EditBuffer::empty();
        assert!(!b.dirty);
        assert_eq!(b.cursor, 0);
        assert!(b.path.is_none());
    }
}
