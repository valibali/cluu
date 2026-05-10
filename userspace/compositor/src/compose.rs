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
            // Chrome rendering lands in T14; for now emit bg.
            return BG_CELL;
        }
        let ix = local_x - CHROME_LEFT;
        let iy = local_y - CHROME_TOP;
        return read_shm_cell(win, ix, iy);
    }
    BG_CELL
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
