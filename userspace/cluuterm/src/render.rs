//! Cell grid → compositor SHM cell blit.
//!
//! Walks the cluuterm cell grid and writes packed u64 cells (codepoint:21,
//! fg_idx:8, bg_idx:8, attrs:3) into the SHM region shared with the
//! compositor. The compositor reads these cells and blits them with its own
//! glyph renderer during the next frame flush.
//!
//! ARGB colours stored in `fg_cells`/`bg_cells` are converted to xterm-256
//! palette indices via exact-match over the 16 basic colours, then a
//! nearest-match scan over the full 256-entry palette for colours outside
//! the basic set. When the ANSI parser set an explicit 256-colour index
//! (CSI 38;5;N / 48;5;N), the index is encoded in the alpha byte
//! (0xFF00_00NN) and passed through directly without RGB conversion.

extern crate alloc;
use alloc::vec::Vec;

use core::mem::size_of;
use libcluu::window_shm::WindowShm;
use crate::tty_backend::Cluuterm;

// The parser uses a slightly different set of ARGB values (copied from the
// legacy console colour table) than the compositor's palette entries 0-15.
// We carry these tables so exact-match works even when the raw value from
// the parser doesn't appear literally in PALETTE_256.
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

/// Full xterm-256 palette (0x00RRGGBB, no alpha) for nearest-match fallback.
/// Matches the compositor's `xterm_256_palette()` entries with the alpha
/// byte stripped (cluuterm stores colours as 0x00RRGGBB).
const fn build_xterm_256() -> [u32; 256] {
    let mut p = [0u32; 256];
    let basic: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000,
        0x000080, 0x800080, 0x008080, 0xC0C0C0,
        0x808080, 0xFF0000, 0x00FF00, 0xFFFF00,
        0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];
    let mut i = 0;
    while i < 16 {
        p[i] = basic[i];
        i += 1;
    }
    let mut i = 0;
    while i < 216 {
        let r = (i / 36) % 6;
        let g = (i / 6) % 6;
        let b = i % 6;
        let rf = if r == 0 { 0 } else { (r as u32) * 40 + 55 };
        let gf = if g == 0 { 0 } else { (g as u32) * 40 + 55 };
        let bf = if b == 0 { 0 } else { (b as u32) * 40 + 55 };
        p[16 + i] = (rf << 16) | (gf << 8) | bf;
        i += 1;
    }
    let mut i = 0;
    while i < 24 {
        let v = 8 + (i as u32) * 10;
        p[232 + i] = (v << 16) | (v << 8) | v;
        i += 1;
    }
    p
}

const PALETTE_256: [u32; 256] = build_xterm_256();

/// Convert an ARGB u32 (as stored in `Attr::fg`/`Attr::bg`) to the nearest
/// xterm-256 palette index.
///
/// Fast path: exact-match scan over the 16 basic colours (covers every colour
/// the libcluu ANSI parser can produce from SGR 30-37/90-97). Fallback:
/// brute-force nearest by squared Euclidean distance in RGB space over the
/// full 256-entry xterm palette (covers the 6×6×6 cube and grayscale ramp).
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

    // Nearest-colour fallback over the full xterm-256 palette.
    let r0 = ((rgb >> 16) & 0xFF) as i32;
    let g0 = ((rgb >>  8) & 0xFF) as i32;
    let b0 = ( rgb        & 0xFF) as i32;

    let mut best_idx = 0u8;
    let mut best_dist = i32::MAX;
    for (i, &c) in PALETTE_256.iter().enumerate() {
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

/// Decode a per-cell colour value to an xterm-256 palette index.
/// Alpha byte 0xFF → explicit palette index in the low byte (from
/// CSI 38;5;N / 48;5;N). Alpha byte 0x00 → ARGB, convert via nearest-match.
fn decode_palette_idx(raw: u32) -> u8 {
    if (raw >> 24) == 0xFF {
        (raw & 0xFF) as u8
    } else {
        argb_to_palette_idx(raw)
    }
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
/// This function writes only the 80×25 terminal interior; the compositor
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
            let fg  = decode_palette_idx(term.fg_cells[term_pos]);
            let bg  = decode_palette_idx(term.bg_cells[term_pos]);
            let attrs = term.attr_cells[term_pos];
            let cell = pack_cell(ch, fg, bg, attrs);

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
        let fg  = decode_palette_idx(term.fg_cells[term_pos]);
        let bg  = decode_palette_idx(term.bg_cells[term_pos]);
        let attrs = term.attr_cells[term_pos];
        // Swap fg/bg: cursor is rendered as an inverted block.
        let cell = pack_cell(ch, bg, fg, attrs);
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
