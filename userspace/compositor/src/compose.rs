//! Cell-grid composer.
//!
//! For each dirty cell, walk windows top→bottom (last in vec = top of
//! z-order). The first window whose total rect contains the cell decides
//! the output. Inside the single-cell chrome strip (top 1, bottom 1,
//! left 1, right 1) we emit arc corner or bar glyphs. Inside the interior
//! we read the cell from the window's SHM region.

use crate::state::{Compositor, Window, WindowShm, WIN_SHM_MAGIC};

const CHROME_TOP: u16 = 1;
const CHROME_BOTTOM: u16 = 1;
const CHROME_LEFT: u16 = 1;
const CHROME_RIGHT: u16 = 1;

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
    // CP437 sharp box-drawing corners (U+250C/U+2510/U+2514/U+2518).
    const TL: u32 = 0x250C; // ┌ top-left
    const TR: u32 = 0x2510; // ┐ top-right
    const BL: u32 = 0x2514; // └ bottom-left
    const BR: u32 = 0x2518; // ┘ bottom-right
    const H_BAR: u32 = 0x2500; // ─
    const V_BAR: u32 = 0x2502; // │

    // Title row is lx=1..w-2, ly=0 (between TL and TR corners).
    let cp = match (lx, ly) {
        (0, 0)                                        => TL,
        (x, 0) if x == w.saturating_sub(1)           => TR,
        (0, y) if y == h.saturating_sub(1)           => BL,
        (x, y) if x == w.saturating_sub(1) && y == h.saturating_sub(1) => BR,
        // Top row: title cells between corners.
        (_, 0) => return title_cell(win, lx, focused),
        // Bottom row: horizontal bar.
        (_, y) if y == h.saturating_sub(1)           => H_BAR,
        // Left/right columns: vertical bars.
        (0, _)                                        => V_BAR,
        (x, _) if x == w.saturating_sub(1)           => V_BAR,
        _                                             => H_BAR, // fallback
    };
    let attrs = if focused { 0b001 } else { 0 };
    let fg = if focused { 15 } else { 7 };
    pack_cell(cp, fg, 0, attrs)
}

fn title_cell(win: &Window, lx: u16, focused: bool) -> u64 {
    // lx=0 is TL corner, lx=w-1 is TR corner; title occupies lx=1..w-2.
    let title_start: u16 = 1;
    let title_end = win.w.saturating_sub(1);
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

/// Lay the status bar string into cell row 0 of the compositor's cell_grid.
/// Called after recompute_dirty so it overwrites any chrome/interior that
/// landed on row 0.
pub fn render_status_row(comp: &mut Compositor) {
    let s = crate::status::render_status(comp);
    let bytes = s.as_bytes();
    for cx in 0..comp.cols {
        let cp = if (cx as usize) < bytes.len() {
            bytes[cx as usize] as u32
        } else {
            b' ' as u32
        };
        let cell = pack_cell(cp, 7, 0, 0);
        let idx = cx as usize;
        comp.cell_grid[idx] = cell;
    }
}

fn read_shm_cell(win: &Window, ix: u16, iy: u16) -> u64 {
    if win.shm_va.is_null() {
        return BG_CELL;
    }
    unsafe {
        let hdr = win.shm_va as *const WindowShm;
        let magic = (*hdr).magic;
        if magic != WIN_SHM_MAGIC {
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
