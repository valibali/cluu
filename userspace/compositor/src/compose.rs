//! Cell-grid composer.
//!
//! For each dirty cell, walk windows top→bottom (last in vec = top of
//! z-order). The first window whose total rect contains the cell decides
//! the output. Inside the single-cell chrome strip (top 1, bottom 1,
//! left 1, right 1) we emit arc corner or bar glyphs. Inside the interior
//! we read the cell from the window's SHM region.

use crate::config::{FOCUSED_BOLD_ATTR, FOCUSED_FG, PLAIN_FG};
use crate::state::{Compositor, Window};

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

/// Return true if the currently focused window is fullscreen.
fn focused_is_fullscreen(comp: &Compositor) -> bool {
    let Some(id) = comp.focused else { return false; };
    comp.windows.iter().any(|w| w.id == id && w.fullscreen)
}

fn compose_cell(comp: &Compositor, cx: u16, cy: u16) -> u64 {
    let fullscreen_mode = focused_is_fullscreen(comp);
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
        // Fullscreen windows have no chrome — treat every cell as interior.
        // When any window is fullscreen-focused, suppress chrome on all windows
        // (the fullscreen window's interior covers the whole screen anyway).
        let in_chrome = if win.fullscreen || fullscreen_mode {
            false
        } else {
            local_x < CHROME_LEFT
                || local_x >= win.w.saturating_sub(CHROME_RIGHT)
                || local_y < CHROME_TOP
                || local_y >= win.h.saturating_sub(CHROME_BOTTOM)
        };
        if in_chrome {
            let focused = comp.focused == Some(win.id);
            return chrome_glyph(win, local_x, local_y, focused);
        }
        // For fullscreen windows the SHM interior coordinate equals the local
        // coordinate directly (no chrome offset).
        let (ix, iy) = if win.fullscreen {
            (local_x, local_y)
        } else {
            (local_x - CHROME_LEFT, local_y - CHROME_TOP)
        };
        return read_shm_cell(win, ix, iy);
    }
    BG_CELL
}

fn chrome_glyph(win: &Window, lx: u16, ly: u16, focused: bool) -> u64 {
    const TL: u32 = 0x250C;
    const TR: u32 = 0x2510;
    const BL: u32 = 0x2514;
    const BR: u32 = 0x2518;
    const H_BAR: u32 = 0x2500;
    const V_BAR: u32 = 0x2502;

    let w = win.w;
    let h = win.h;

    // Corners.
    let corner = match (lx, ly) {
        (0, 0) => Some(TL),
        (x, 0) if x == w - 1 => Some(TR),
        (0, y) if y == h - 1 => Some(BL),
        (x, y) if x == w - 1 && y == h - 1 => Some(BR),
        _ => None,
    };
    if let Some(cp) = corner {
        return pack_chrome_cell(cp, focused);
    }

    // Top horizontal edge: centered title overlay, otherwise H_BAR.
    if ly == 0 {
        if let Some(cp) = title_overlay_at(win, lx, focused) {
            return cp;
        }
        return pack_chrome_cell(H_BAR, focused);
    }

    // Bottom horizontal edge.
    if ly == h - 1 {
        return pack_chrome_cell(H_BAR, focused);
    }

    // Left + right vertical edges.
    if lx == 0 || lx == w - 1 {
        return pack_chrome_cell(V_BAR, focused);
    }

    // Unreachable: caller checks in_chrome.
    pack_chrome_cell(H_BAR, focused)
}

/// If `lx` (in row 0, exclusive of corners) maps to a title slot, return the
/// packed cell with the title glyph + one-cell space padding on each side.
/// Returns None when this column should show a plain `─`.
fn title_overlay_at(win: &Window, lx: u16, focused: bool) -> Option<u64> {
    let w = win.w;
    let title_bytes = win.title.as_bytes();
    if title_bytes.is_empty() {
        return None;
    }
    // Available interior space between the corners: columns 1..w-1.
    let interior_w = w.saturating_sub(2) as usize;
    // Truncate title if it would overflow even with 2 padding cells.
    let max_title = interior_w.saturating_sub(2);
    let title_bytes = if title_bytes.len() > max_title {
        &title_bytes[..max_title]
    } else {
        title_bytes
    };
    if title_bytes.is_empty() {
        return None;
    }
    // padded: ' ' + title + ' '
    let display_len = title_bytes.len() + 2;
    // Center inside interior (columns 1..w-1).
    let start = 1 + (interior_w.saturating_sub(display_len)) / 2;
    let end = start + display_len;
    let lxu = lx as usize;
    if lxu < start || lxu >= end {
        return None;
    }
    let offset = lxu - start;
    let cp = if offset == 0 || offset == display_len - 1 {
        b' ' as u32
    } else {
        let ti = offset - 1;
        if ti < title_bytes.len() {
            title_bytes[ti] as u32
        } else {
            b' ' as u32
        }
    };
    Some(pack_chrome_cell(cp, focused))
}

fn pack_chrome_cell(cp: u32, focused: bool) -> u64 {
    let attrs = if focused { FOCUSED_BOLD_ATTR } else { 0 };
    let fg = if focused { FOCUSED_FG } else { PLAIN_FG };
    pack_cell(cp, fg, 0, attrs)
}

/// Lay the status bar string into cell row 0 of the compositor's cell_grid.
/// Called after recompute_dirty so it overwrites any chrome/interior that
/// landed on row 0.
/// Skipped entirely when a fullscreen window is focused — the fullscreen
/// window's content (row 0 of its SHM buffer) is already in cell_grid[row 0].
pub fn render_status_row(comp: &mut Compositor) {
    if focused_is_fullscreen(comp) {
        return;
    }
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
    win.mapping.read_cell(ix, iy).unwrap_or(BG_CELL)
}
