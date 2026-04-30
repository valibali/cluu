//! Search & substitute. Literal patterns only (no regex). See spec §11.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::mode::{Editor, SearchDir};
use crate::ex::Range as ExRange;

/// Recompute `matches` if buffer has changed since last compute.
pub fn refresh_matches(state: &mut Editor) {
    if state.search.matches_seq == state.buf.pieces.edit_seq && !state.search.pattern.is_empty() { return; }
    state.search.matches.clear();
    if state.search.pattern.is_empty() {
        state.search.matches_seq = state.buf.pieces.edit_seq;
        return;
    }
    let bytes = state.buf.pieces.read_all();
    let pat = state.search.pattern.as_bytes();
    let ic = state.settings.ignorecase;
    let mut i = 0;
    while i + pat.len() <= bytes.len() {
        if matches_at(&bytes[i..], pat, ic) {
            state.search.matches.push(i..i + pat.len());
            i += pat.len();
        } else {
            i += 1;
        }
    }
    state.search.matches_seq = state.buf.pieces.edit_seq;
}

fn matches_at(haystack: &[u8], pat: &[u8], ic: bool) -> bool {
    if haystack.len() < pat.len() { return false; }
    for (h, p) in haystack.iter().zip(pat.iter()) {
        if ic { if to_lower(*h) != to_lower(*p) { return false; } }
        else  { if h != p                       { return false; } }
    }
    true
}

fn to_lower(b: u8) -> u8 { if b >= b'A' && b <= b'Z' { b + 32 } else { b } }

pub fn next_match(state: &mut Editor) -> Option<usize> {
    refresh_matches(state);
    if state.search.matches.is_empty() { return None; }
    match state.search.direction {
        SearchDir::Forward => {
            for r in state.search.matches.iter() {
                if r.start > state.buf.cursor { return Some(r.start); }
            }
            // Wrap.
            state.message = "search hit BOTTOM, continuing at TOP".into();
            state.search.matches.first().map(|r| r.start)
        }
        SearchDir::Backward => {
            for r in state.search.matches.iter().rev() {
                if r.start < state.buf.cursor { return Some(r.start); }
            }
            state.message = "search hit TOP, continuing at BOTTOM".into();
            state.search.matches.last().map(|r| r.start)
        }
    }
}

pub fn prev_match(state: &mut Editor) -> Option<usize> {
    let saved = state.search.direction;
    state.search.direction = match saved {
        SearchDir::Forward => SearchDir::Backward,
        SearchDir::Backward => SearchDir::Forward,
    };
    let r = next_match(state);
    state.search.direction = saved;
    r
}

pub fn set_pattern(state: &mut Editor, pat: String, dir: SearchDir) {
    state.search.pattern = pat;
    state.search.direction = dir;
    state.search.matches_seq = u64::MAX;
}

pub fn substitute(state: &mut Editor, range: ExRange, pat: &str, repl: &str, global: bool) {
    if pat.is_empty() { state.message = "E33: No previous substitute".into(); return; }
    let (lo_line, hi_line) = match range {
        ExRange::Whole       => (0usize, state.buf.pieces.line_count().saturating_sub(1)),
        ExRange::Line(n)     => (n.saturating_sub(1), n.saturating_sub(1)),
        ExRange::Lines(a, b) => (a.saturating_sub(1), b.saturating_sub(1)),
        ExRange::Current     => { let (l, _) = state.buf.pieces.line_col(state.buf.cursor); (l, l) }
    };
    let idx = state.buf.pieces.line_index().to_vec();
    let lo = idx[lo_line.min(idx.len().saturating_sub(1))];
    let hi = if hi_line + 1 < idx.len() { idx[hi_line + 1] } else { state.buf.pieces.len() };
    let bytes = state.buf.pieces.read_all();
    let pat_b = pat.as_bytes();
    let repl_b = repl.as_bytes();
    let ic = state.settings.ignorecase;

    // Walk the range, build replacements list (line by line).
    let mut count = 0;
    let mut out = alloc::vec::Vec::with_capacity(hi - lo);
    let mut i = lo;
    let mut line_start = lo;
    let mut replaced_on_line = false;
    while i < hi {
        if (!replaced_on_line || global) && i + pat_b.len() <= hi && matches_at(&bytes[i..], pat_b, ic) {
            out.extend_from_slice(repl_b);
            i += pat_b.len();
            count += 1;
            replaced_on_line = true;
            continue;
        }
        if bytes[i] == b'\n' { replaced_on_line = false; line_start = i + 1; }
        out.push(bytes[i]);
        i += 1;
    }

    if count == 0 { state.message = alloc::format!("E486: Pattern not found: {}", pat); return; }

    let cursor_before = state.buf.cursor;
    if let Some(patch) = state.buf.pieces.delete(lo..hi) {
        state.buf.pieces.insert(lo, &out);
        // Coalesce as one undo entry — re-record by composing both patches.
        // For v1 we keep the two patches; user's `u` will need two presses for
        // a `:%s` undo. Acceptable; will tighten in v2.
        let _ = patch;
    }
    state.buf.cursor = lo;
    let _ = cursor_before;
    state.buf.mark_dirty();
    state.message = alloc::format!("{} substitution{}", count, if count == 1 { "" } else { "s" });
}
