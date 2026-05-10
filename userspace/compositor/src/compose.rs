//! Cell-grid composer.
//!
//! For each dirty cell, walk windows top→bottom (last in vec = top of
//! z-order). The first window whose total rect contains the cell decides
//! the output. Inside the chrome strip (top 2 rows, bottom 2 rows, left
//! 2 cols, right 2 cols of the window) we currently emit BG_CELL — chrome
//! rendering lands in T14. Inside the interior we read the cell from the
//! window's SHM region, generation-acquire-loaded.

use crate::state::{Compositor, Window, WindowShm, WIN_SHM_MAGIC};

const CHROME_TOP: u16 = 2;
const CHROME_BOTTOM: u16 = 2;
const CHROME_LEFT: u16 = 2;
const CHROME_RIGHT: u16 = 2;

/// Default desktop background cell: codepoint 0x20 (space), fg 0, bg 0.
pub const BG_CELL: u64 = pack_cell(b' ' as u32, 0, 0, 0);

/// Pack `(codepoint:21, fg:8, bg:8, attrs:3)` into a single u64.
pub const fn pack_cell(cp: u32, fg: u8, bg: u8, attrs: u8) -> u64 {
    (cp as u64 & 0x1F_FFFF)
        | ((fg as u64 & 0xFF) << 21)
        | ((bg as u64 & 0xFF) << 29)
        | ((attrs as u64 & 0x07) << 37)
}

/// Walk the compositor's dirty cell list and refresh `cell_grid` accordingly.
pub fn recompute_dirty(comp: &mut Compositor) {
    let dirty = core::mem::take(&mut comp.cell_dirty);
    for (cx, cy) in dirty {
        if cx >= comp.cols || cy >= comp.rows {
            continue;
        }
        let out = compose_cell(comp, cx, cy);
        let idx = cy as usize * comp.cols as usize + cx as usize;
        comp.cell_grid[idx] = out;
    }
}

fn compose_cell(comp: &Compositor, cx: u16, cy: u16) -> u64 {
    // Walk top→bottom (last is top).
    for win in comp.windows.iter().rev() {
        if cx < win.x || cx >= win.x.saturating_add(win.w) {
            continue;
        }
        if cy < win.y || cy >= win.y.saturating_add(win.h) {
            continue;
        }
        let local_x = cx - win.x;
        let local_y = cy - win.y;
        let in_chrome = local_x < CHROME_LEFT
            || local_x >= win.w.saturating_sub(CHROME_RIGHT)
            || local_y < CHROME_TOP
            || local_y >= win.h.saturating_sub(CHROME_BOTTOM);
        if in_chrome {
            let focused = comp.focused == Some(win.id);
            return chrome_glyph(win, local_x, local_y, focused);
        }
        let ix = local_x - CHROME_LEFT;
        let iy = local_y - CHROME_TOP;
        return read_shm_cell(win, ix, iy);
    }
    BG_CELL
}

fn chrome_glyph(win: &Window, lx: u16, ly: u16, focused: bool) -> u64 {
    let w = win.w;
    let h = win.h;
    // PUA codepoints for Tier-3 corners (mapped to CP437 0xF0..0xFF
    // via unicode_to_cp437 in flush_grid_to_backbuf).
    const TL_NW: u32 = 0xE000; const TL_NE: u32 = 0xE001;
    const TL_SW: u32 = 0xE002; const TL_SE: u32 = 0xE003;
    const TR_NW: u32 = 0xE004; const TR_NE: u32 = 0xE005;
    const TR_SW: u32 = 0xE006; const TR_SE: u32 = 0xE007;
    const BL_NW: u32 = 0xE008; const BL_NE: u32 = 0xE009;
    const BL_SW: u32 = 0xE00A; const BL_SE: u32 = 0xE00B;
    const BR_NW: u32 = 0xE00C; const BR_NE: u32 = 0xE00D;
    const BR_SW: u32 = 0xE00E; const BR_SE: u32 = 0xE00F;
    const H_BAR: u32 = 0x2500; // ─
    const V_BAR: u32 = 0x2502; // │

    let cp = match (lx, ly) {
        (0, 0) => TL_NW,
        (1, 0) => TL_NE,
        (0, 1) => TL_SW,
        (1, 1) => TL_SE,
        (x, 0) if x == w.saturating_sub(2) => TR_NW,
        (x, 0) if x == w.saturating_sub(1) => TR_NE,
        (x, 1) if x == w.saturating_sub(2) => TR_SW,
        (x, 1) if x == w.saturating_sub(1) => TR_SE,
        (0, y) if y == h.saturating_sub(2) => BL_NW,
        (1, y) if y == h.saturating_sub(2) => BL_NE,
        (0, y) if y == h.saturating_sub(1) => BL_SW,
        (1, y) if y == h.saturating_sub(1) => BL_SE,
        (x, y) if x == w.saturating_sub(2) && y == h.saturating_sub(2) => BR_NW,
        (x, y) if x == w.saturating_sub(1) && y == h.saturating_sub(2) => BR_NE,
        (x, y) if x == w.saturating_sub(2) && y == h.saturating_sub(1) => BR_SW,
        (x, y) if x == w.saturating_sub(1) && y == h.saturating_sub(1) => BR_SE,
        (_, 0) => H_BAR,
        (_, 1) => return title_cell(win, lx, focused),
        (_, y) if y == h.saturating_sub(1) => H_BAR,
        (0, _) => V_BAR,
        (x, _) if x == w.saturating_sub(1) => V_BAR,
        (_, y) if y == h.saturating_sub(2) => H_BAR,
        _ => H_BAR,
    };
    let attrs = if focused { 0b001 } else { 0 };
    let fg = if focused { 15 } else { 7 };
    pack_cell(cp, fg, 0, attrs)
}

fn title_cell(win: &Window, lx: u16, focused: bool) -> u64 {
    let title_start: u16 = 3;
    let title_end = win.w.saturating_sub(3);
    if lx < title_start || lx >= title_end {
        let fg = if focused { 15 } else { 7 };
        return pack_cell(b' ' as u32, fg, 0, 0);
    }
    let pos = (lx - title_start) as usize;
    let bytes = win.title.as_bytes();
    let cp = if pos < bytes.len() { bytes[pos] as u32 } else { b' ' as u32 };
    let fg = if focused { 15 } else { 7 };
    let attrs = if focused { 0b001 } else { 0 };
    pack_cell(cp, fg, 0, attrs)
}

fn read_shm_cell(win: &Window, ix: u16, iy: u16) -> u64 {
    if win.shm_va.is_null() {
        return BG_CELL;
    }
    unsafe {
        let hdr = win.shm_va as *const WindowShm;
        if (*hdr).magic != WIN_SHM_MAGIC {
            return BG_CELL;
        }
        let inner_w = (*hdr).width as u16;
        if ix >= inner_w {
            return BG_CELL;
        }
        let header_bytes = core::mem::size_of::<WindowShm>();
        let cells_ptr = (win.shm_va as usize + header_bytes) as *const u64;
        let off = iy as usize * inner_w as usize + ix as usize;
        core::ptr::read_volatile(cells_ptr.add(off))
    }
}
