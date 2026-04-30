//! Search & substitute. Literal patterns only (no regex). See spec §11.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::mode::{Editor, SearchDir};

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
