//! Motion primitives. Operate on byte offsets; return new cursor position.
//! See spec §7.3 NORMAL keymap.

use crate::buffer::EditBuffer;

pub fn left(buf: &mut EditBuffer, count: usize) -> usize {
    buf.cursor.saturating_sub(count)
}

pub fn right(buf: &mut EditBuffer, count: usize) -> usize {
    let new = buf.cursor + count;
    let total = buf.pieces.len();
    new.min(total)
}

pub fn down(buf: &mut EditBuffer, count: usize) -> usize {
    let (line, col) = buf.pieces.line_col(buf.cursor);
    let total_lines = buf.pieces.line_count();
    let target_line = (line + count).min(total_lines.saturating_sub(1));
    line_col_to_offset(buf, target_line, col)
}

pub fn up(buf: &mut EditBuffer, count: usize) -> usize {
    let (line, col) = buf.pieces.line_col(buf.cursor);
    let target_line = line.saturating_sub(count);
    line_col_to_offset(buf, target_line, col)
}

pub fn line_start(buf: &mut EditBuffer) -> usize {
    let (line, _) = buf.pieces.line_col(buf.cursor);
    let idx = buf.pieces.line_index();
    idx[line]
}

pub fn line_end(buf: &mut EditBuffer) -> usize {
    let (line, _) = buf.pieces.line_col(buf.cursor);
    let idx = buf.pieces.line_index().to_vec();
    if line + 1 < idx.len() { idx[line + 1].saturating_sub(1) } else { buf.pieces.len() }
}

pub fn first_line(buf: &mut EditBuffer) -> usize { 0 }

pub fn last_line(buf: &mut EditBuffer) -> usize {
    let total = buf.pieces.line_count();
    let idx = buf.pieces.line_index().to_vec();
    if total == 0 { 0 } else { idx[total - 1] }
}

pub fn word_forward(buf: &mut EditBuffer, count: usize) -> usize {
    let bytes = buf.pieces.read_all();
    let mut pos = buf.cursor;
    for _ in 0..count {
        pos = next_word_start(&bytes, pos);
    }
    pos
}

pub fn word_backward(buf: &mut EditBuffer, count: usize) -> usize {
    let bytes = buf.pieces.read_all();
    let mut pos = buf.cursor;
    for _ in 0..count {
        pos = prev_word_start(&bytes, pos);
    }
    pos
}

pub fn match_bracket(buf: &mut EditBuffer) -> usize {
    let bytes = buf.pieces.read_all();
    let here = buf.cursor;
    if here >= bytes.len() { return here; }
    let (open, close, dir) = match bytes[here] {
        b'(' => (b'(', b')',  1i64),
        b')' => (b'(', b')', -1i64),
        b'[' => (b'[', b']',  1i64),
        b']' => (b'[', b']', -1i64),
        b'{' => (b'{', b'}',  1i64),
        b'}' => (b'{', b'}', -1i64),
        _    => return here,
    };
    let mut depth: i64 = 0;
    let mut pos: i64 = here as i64;
    loop {
        if pos < 0 || pos >= bytes.len() as i64 { return here; }
        let b = bytes[pos as usize];
        if b == open  { depth += 1; }
        if b == close { depth -= 1; }
        if depth == 0 && pos != here as i64 { return pos as usize; }
        pos += dir;
    }
}

fn line_col_to_offset(buf: &mut EditBuffer, line: usize, col: usize) -> usize {
    let idx = buf.pieces.line_index().to_vec();
    if line >= idx.len() { return buf.pieces.len(); }
    let start = idx[line];
    let end = if line + 1 < idx.len() { idx[line + 1].saturating_sub(1) } else { buf.pieces.len() };
    start + col.min(end - start)
}

fn is_word(b: u8) -> bool {
    (b >= b'a' && b <= b'z') || (b >= b'A' && b <= b'Z')
        || (b >= b'0' && b <= b'9') || b == b'_'
}

fn next_word_start(bytes: &[u8], pos: usize) -> usize {
    let mut p = pos;
    let n = bytes.len();
    // Skip current word.
    while p < n && is_word(bytes[p]) { p += 1; }
    // Skip whitespace / punctuation.
    while p < n && !is_word(bytes[p]) { p += 1; }
    p
}

fn prev_word_start(bytes: &[u8], pos: usize) -> usize {
    if pos == 0 { return 0; }
    let mut p = pos - 1;
    while p > 0 && !is_word(bytes[p]) { p -= 1; }
    while p > 0 && is_word(bytes[p - 1]) { p -= 1; }
    p
}
