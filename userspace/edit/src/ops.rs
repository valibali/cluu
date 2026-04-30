//! Operator implementations. Each takes the current cursor + a target offset
//! (or line range) and applies delete/yank/change/indent/dedent.

extern crate alloc;
use alloc::vec::Vec;
use crate::mode::{Editor, Operator, Mode};

pub fn apply(state: &mut Editor, op: Operator, range_start: usize, range_end: usize) {
    let (lo, hi) = if range_start <= range_end { (range_start, range_end) } else { (range_end, range_start) };
    let bytes = state.buf.pieces.read_all();
    let yanked: Vec<u8> = bytes[lo..hi].to_vec();
    match op {
        Operator::Delete | Operator::Change => {
            state.register = yanked;
            if let Some(patch) = state.buf.pieces.delete(lo..hi) {
                state.undo.record(state.buf.cursor, lo, patch);
                state.buf.cursor = lo;
                state.buf.mark_dirty();
            }
            if op == Operator::Change {
                state.mode = Mode::Insert;
            }
        }
        Operator::Yank => {
            state.register = yanked;
            // No edit, no undo record.
        }
        Operator::Indent | Operator::Dedent => {
            // Indent/dedent: prepend/strip one tabstop's worth on each line in range.
            // For v1, use literal `\t` (Task 27 plumbs :set expandtab to swap to spaces).
            apply_indent_lines(state, lo, hi, op == Operator::Indent);
        }
    }
}

pub fn delete_char(state: &mut Editor, count: usize) {
    let start = state.buf.cursor;
    let end = (start + count).min(state.buf.pieces.len());
    if end > start {
        let bytes = state.buf.pieces.read_all();
        state.register = bytes[start..end].to_vec();
        if let Some(patch) = state.buf.pieces.delete(start..end) {
            state.undo.record(start, start, patch);
            state.buf.mark_dirty();
        }
    }
}

pub fn paste_after(state: &mut Editor) {
    if state.register.is_empty() { return; }
    let pos = state.buf.cursor + 1;
    let pos = pos.min(state.buf.pieces.len());
    let bytes = state.register.clone();
    if let Some(patch) = state.buf.pieces.insert(pos, &bytes) {
        state.undo.record(state.buf.cursor, pos + bytes.len() - 1, patch);
        state.buf.cursor = pos + bytes.len() - 1;
        state.buf.mark_dirty();
    }
}

fn apply_indent_lines(state: &mut Editor, lo: usize, hi: usize, indent: bool) {
    let (first_line, _) = state.buf.pieces.line_col(lo);
    let (last_line, _) = state.buf.pieces.line_col(hi.saturating_sub(1));
    let idx = state.buf.pieces.line_index().to_vec();
    // Walk lines bottom-up so earlier insertions don't shift later offsets.
    for line in (first_line..=last_line).rev() {
        let line_start = idx[line];
        if indent {
            state.buf.pieces.insert(line_start, b"\t");
        } else if let Some(b) = state.buf.pieces.read_all().get(line_start).copied() {
            if b == b'\t' || b == b' ' {
                state.buf.pieces.delete(line_start..line_start + 1);
            }
        }
    }
    state.buf.mark_dirty();
    // Note: this could be tightened to one PiecePatch in the undo log later;
    // for v1 the per-line edits accumulate as separate patches. Acceptable.
}
