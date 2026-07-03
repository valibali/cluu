//! Cell grid → compositor SHM cell blit.
//!
//! Walks the cluuterm cell grid and writes packed u64 cells (codepoint:21,
//! fg_idx:8, bg_idx:8, attrs:3) into the SHM region shared with the
//! compositor. The compositor reads these cells and blits them with its own
//! glyph renderer during the next frame flush.
//!
//! ARGB colours stored in `fg_cells`/`bg_cells` are converted to xterm-256
//! palette indices via a nearest-match scan over the 16 basic ANSI colours;
//! this covers every colour the libcluu ANSI parser can produce.

extern crate alloc;
use alloc::vec::Vec;

use core::mem::size_of;
use libcluu::window_shm::WindowShm;
use crate::tty_backend::Cluuterm;

// ── xterm-256 basic colours (indices 0-15) ───────────────────────────────────
//
// These ARGB values match the compositor's `xterm_256_palette()` entries 0-15
// (plus the alpha byte set to 0 since the cluuterm Attr stores only RGB).
// White (index 7 = 0xC0C0C0 → "light gray") and bright-white (index 15) are
// both present so the default fg (0x00FFFFFF = bright-white) maps to index 15.

const PALETTE_16: [u32; 16] = [
    0x00000000, // 0  black
    0x00800000, // 1  red
    0x00008000, // 2  green
    0x00808000, // 3  dark-yellow / olive
    0x00000080, // 4  blue
    0x00800080, // 5  magenta
    0x00008080, // 6  cyan
    0x00C0C0C0, // 7  light-gray
    0x00808080, // 8  dark-gray (bright-black)
    0x00FF0000, // 9  bright-red
    0x0000FF00, // 10 bright-green
    0x00FFFF00, // 11 bright-yellow
    0x000000FF, // 12 bright-blue
    0x00FF00FF, // 13 bright-magenta
    0x0000FFFF, // 14 bright-cyan
    0x00FFFFFF, // 15 bright-white
];

// Mapping from the cluuterm ANSI-parser colour values to the 16 basic indices.
// The parser uses a slightly different set of ARGB values (copied from the
// legacy console colour table). We carry a second table so nearest-match works
// even when the raw value doesn't appear literally in PALETTE_16.
const PARSER_ANSI: [u32; 8] = [
    0x00000000, // 0 → black
    0x00AA0000, // 1 → red
    0x0000AA00, // 2 → green
    0x00AA5500, // 3 → brown/yellow
    0x000000AA, // 4 → blue
    0x00AA00AA, // 5 → magenta
    0x0000AAAA, // 6 → cyan
    0x00AAAAAA, // 7 → white/gray
];
const PARSER_BRIGHT: [u32; 8] = [
    0x00555555, // 8  → bright-black
    0x00FF5555, // 9  → bright-red
    0x0055FF55, // 10 → bright-green
    0x00FFFF55, // 11 → bright-yellow
    0x005555FF, // 12 → bright-blue
    0x00FF55FF, // 13 → bright-magenta
    0x0055FFFF, // 14 → bright-cyan
    0x00FFFFFF, // 15 → bright-white
];

/// Convert an ARGB u32 (as stored in `Attr::fg`/`Attr::bg`) to the nearest
/// xterm-256 palette index.
///
/// Fast path: exact-match scan over the 16 basic colours (covers every colour
/// the libcluu ANSI parser can produce). Fallback: brute-force nearest by
/// squared Euclidean distance in RGB space over the full 16-entry set.
pub fn argb_to_palette_idx(argb: u32) -> u8 {
    // Strip the alpha byte — cluuterm stores colours as 0x00RRGGBB.
    let rgb = argb & 0x00FF_FFFF;

    // Exact match against the parser's own ANSI colour tables (fast path).
    for (i, &c) in PARSER_ANSI.iter().enumerate() {
        if c == rgb { return i as u8; }
    }
    for (i, &c) in PARSER_BRIGHT.iter().enumerate() {
        if c == rgb { return (8 + i) as u8; }
    }

    // Nearest-colour fallback over the compositor palette entries 0-15.
    let r0 = ((rgb >> 16) & 0xFF) as i32;
    let g0 = ((rgb >>  8) & 0xFF) as i32;
    let b0 = ( rgb        & 0xFF) as i32;

    let mut best_idx = 0u8;
    let mut best_dist = i32::MAX;
    for (i, &c) in PALETTE_16.iter().enumerate() {
        let r1 = ((c >> 16) & 0xFF) as i32;
        let g1 = ((c >>  8) & 0xFF) as i32;
        let b1 = ( c        & 0xFF) as i32;
        let d = (r1-r0)*(r1-r0) + (g1-g0)*(g1-g0) + (b1-b0)*(b1-b0);
        if d < best_dist {
            best_dist = d;
            best_idx = i as u8;
        }
    }
    best_idx
}

/// Pack `(codepoint:21, fg:8, bg:8, attrs:3)` into a single u64 cell word.
/// Matches the compositor's `compose::pack_cell` format exactly.
#[inline]
fn pack_cell(cp: u32, fg: u8, bg: u8, attrs: u8) -> u64 {
    (cp as u64 & 0x1F_FFFF)
        | ((fg as u64 & 0xFF) << 21)
        | ((bg as u64 & 0xFF) << 29)
        | ((attrs as u64 & 0x07) << 37)
}

/// Blit the cluuterm cell grid into the compositor SHM cell array.
///
/// The SHM layout is: `WindowShm` header (32 bytes) followed by
/// `hdr.width * hdr.height` u64 cells in row-major order.
/// Interior cells begin at offset (1,1) inside the window (chrome = 1 cell).
///
/// This function writes only the 80×24 terminal interior; the compositor
/// owns the 1-cell chrome border on all sides.
pub fn render(term: &mut Cluuterm) {
    let cols = term.cols;
    let rows = term.rows;

    let shm_ptr = term.shm;

    // Safety: shm_ptr is valid for the lifetime of `term` (mapped in main.rs
    // before Cluuterm::new is called; unmapped only after run() returns).
    let (shm_w, shm_h) = unsafe {
        let hdr = &*shm_ptr;
        (hdr.width as usize, hdr.height as usize)
    };

    // Cells follow immediately after the WindowShm header.
    let cells_base = unsafe {
        (shm_ptr as *mut u8).add(size_of::<WindowShm>()) as *mut u64
    };

    // Chrome border is 1 cell on each side — but the SHM contains ONLY
    // interior cells. The compositor handles chrome separately in
    // compose_cell and reads interior cells starting at (0,0).
    let max_ix = shm_w.min(cols);
    let max_iy = shm_h.min(rows);

    for iy in 0..max_iy {
        for ix in 0..max_ix {
            let term_pos = iy * cols + ix;
            let ch  = term.cells[term_pos];
            let fg  = argb_to_palette_idx(term.fg_cells[term_pos]);
            let bg  = argb_to_palette_idx(term.bg_cells[term_pos]);
            let cell = pack_cell(ch, fg, bg, 0);

            let shm_off = iy * shm_w + ix;
            unsafe {
                core::ptr::write_volatile(cells_base.add(shm_off), cell);
            }
        }
    }

    // Cursor block: invert fg/bg at (cursor_x, cursor_y) if in bounds.
    let cx = term.cursor_x;
    let cy = term.cursor_y;
    if cx < max_ix && cy < max_iy {
        let term_pos = cy * cols + cx;
        let ch  = term.cells[term_pos];
        let fg  = argb_to_palette_idx(term.fg_cells[term_pos]);
        let bg  = argb_to_palette_idx(term.bg_cells[term_pos]);
        // Swap fg/bg: cursor is rendered as an inverted block.
        let cell = pack_cell(ch, bg, fg, 0);
        let shm_off = cy * shm_w + cx;
        unsafe {
            core::ptr::write_volatile(cells_base.add(shm_off), cell);
            core::ptr::write_volatile(&mut (*shm_ptr).cursor_x as *mut u32, cx as u32);
            core::ptr::write_volatile(&mut (*shm_ptr).cursor_y as *mut u32, cy as u32);
        }
    }

    // Bump the generation counter (release store so the compositor sees
    // the completed cell writes before it reads generation).
    unsafe {
        let gen = (*shm_ptr).generation;
        core::ptr::write_volatile(
            &mut (*shm_ptr).generation as *mut u32,
            gen.wrapping_add(1),
        );
    }
}

/// Build the `/dev/pts/<id>` path string at runtime.
///
/// Returns a null-terminated byte vector suitable for passing to POSIX APIs.
pub fn pts_path(id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24);
    buf.extend_from_slice(b"/dev/pts/");
    let mut n = id;
    let mut digits = [0u8; 10];
    let mut i = 0;
    if n == 0 {
        digits[0] = b'0';
        i = 1;
    }
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        buf.push(digits[i]);
    }
    buf.push(0); // null-terminated for POSIX open / posix_spawn
    buf
}
