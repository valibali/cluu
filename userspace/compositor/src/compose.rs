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
const PAD_TOP: u16 = 0;
const PAD_BOTTOM: u16 = 1;
const PAD_LEFT: u16 = 1;
const PAD_RIGHT: u16 = 1;

/// Default desktop background cell: codepoint 0x20 (space), fg 0, bg 0.
pub const BG_CELL: u64 = pack_cell(b' ' as u32, 0, 0, 0);

/// Sentinel for pixel-region cells. Distinct from BG_CELL so that
/// `flush_grid_to_backbuf` detects the transition when a pixel region
/// moves away and clears the old backbuf area.
pub const PIXEL_CELL: u64 = pack_cell(0x10FFFF, 0, 0, 0);

/// Pack `(codepoint:21, fg:8, bg:8, attrs:4)` into a single u64.
pub const fn pack_cell(cp: u32, fg: u8, bg: u8, attrs: u8) -> u64 {
    (cp as u64 & 0x1F_FFFF)
        | ((fg as u64 & 0xFF) << 21)
        | ((bg as u64 & 0xFF) << 29)
        | ((attrs as u64 & 0x0F) << 37)
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
        comp.prev_cell_grid[idx] = u64::MAX;
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

        if let Some(ref pr) = win.pixel_region {
            if pr.contains_cell(local_x, local_y) {
                return PIXEL_CELL;
            }
        }

        let suppress_chrome = win.fullscreen || win.no_chrome || win.modal || fullscreen_mode;
        let in_chrome = if suppress_chrome {
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
        let pad_l = if win.modal { 0 } else { PAD_LEFT };
        let pad_r = if win.modal { 0 } else { PAD_RIGHT };
        let pad_t = if win.modal { 0 } else { PAD_TOP };
        let pad_b = if win.modal { 0 } else { PAD_BOTTOM };
        let in_padding = if suppress_chrome {
            false
        } else {
            local_x < CHROME_LEFT + pad_l
                || local_x >= win.w.saturating_sub(CHROME_RIGHT + pad_r)
                || local_y < CHROME_TOP + pad_t
                || local_y >= win.h.saturating_sub(CHROME_BOTTOM + pad_b)
        };
        if in_padding {
            return BG_CELL;
        }
        let (ix, iy) = if suppress_chrome {
            (local_x, local_y)
        } else {
            (local_x - CHROME_LEFT - pad_l, local_y - CHROME_TOP - pad_t)
        };
        let focused = comp.focused == Some(win.id);
        return read_shm_cell(win, ix, iy, focused);
    }
    BG_CELL
}

fn chrome_glyph(win: &Window, lx: u16, ly: u16, focused: bool) -> u64 {
    // Light corners + dashed edges — unfocused windows.
    const TL_S: u32 = 0x250C; // ┌
    const TR_S: u32 = 0x2510; // ┐
    const BL_S: u32 = 0x2514; // └
    const BR_S: u32 = 0x2518; // ┘
    const H_S:  u32 = 0x254C; // ╌
    const V_S:  u32 = 0x2506; // ┆

    // Double-line box drawing — focused window.
    const TL_D: u32 = 0x2554;
    const TR_D: u32 = 0x2557;
    const BL_D: u32 = 0x255A;
    const BR_D: u32 = 0x255D;
    const H_D:  u32 = 0x2550;
    const V_D:  u32 = 0x2551;

    let (tl, tr, bl, br, h_bar, v_bar) = if focused {
        (TL_D, TR_D, BL_D, BR_D, H_D, V_D)
    } else {
        (TL_S, TR_S, BL_S, BR_S, H_S, V_S)
    };

    let w = win.w;
    let h = win.h;

    // Corners.
    let corner = match (lx, ly) {
        (0, 0) => Some(tl),
        (x, 0) if x == w - 1 => Some(tr),
        (0, y) if y == h - 1 => Some(bl),
        (x, y) if x == w - 1 && y == h - 1 => Some(br),
        _ => None,
    };
    if let Some(cp) = corner {
        return pack_chrome_cell(cp, focused);
    }

    // Top horizontal edge: centered title overlay, otherwise h_bar.
    if ly == 0 {
        if let Some(cp) = title_overlay_at(win, lx, focused) {
            return cp;
        }
        return pack_chrome_cell(h_bar, focused);
    }

    // Bottom horizontal edge.
    if ly == h - 1 {
        return pack_chrome_cell(h_bar, focused);
    }

    // Left + right vertical edges.
    if lx == 0 || lx == w - 1 {
        return pack_chrome_cell(v_bar, focused);
    }

    // Unreachable: caller checks in_chrome.
    pack_chrome_cell(h_bar, focused)
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

fn read_shm_cell(win: &Window, ix: u16, iy: u16, focused: bool) -> u64 {
    let cell = win.mapping.read_cell(ix, iy).unwrap_or(BG_CELL);
    // Gate cursor visibility. The cursor (an inverted fg/bg cell written by
    // cluuterm at `(cursor_x, cursor_y)` in SHM) is shown only when BOTH:
    //   (a) the client has marked it visible in the SHM header
    //       (`cursor_visible == 1`, the blink-on phase), AND
    //   (b) this window is the focused one.
    // Inactive windows never show the text cursor — focus moves away and
    // the cursor cell re-renders as a normal cell on the next dirty pass
    // (focus changes dirty both windows' cells — see
    // [[cluu-compositor-focus-chrome-stale]]).
    let hdr = win.mapping.header();
    let cursor_visible = unsafe {
        core::ptr::read_volatile(&hdr.cursor_visible as *const u32)
    };
    let cursor_hidden = cursor_visible == 0 || !focused;
    if cursor_hidden {
        let cx = hdr.cursor_x as u16;
        let cy = hdr.cursor_y as u16;
        // Sentinel: clients that don't have a cursor never write cursor_x/y,
        // so SHM stays zero-init at (0, 0). We can't distinguish "cursor at
        // (0,0)" from "no cursor" via coords alone, but cursor_visible == 0
        // already means "hidden" — the un-invert only fires for clients that
        // previously set cursor_visible to 1 (cluuterm) and then toggled it
        // to 0 for blink-off. Those clients always have valid cursor coords.
        if (cx != 0 || cy != 0) && ix == cx && iy == cy {
            // The cursor was rendered as an inverted cell (fg/bg swapped) by
            // cluuterm. Re-invert to restore the normal appearance.
            let cp    = cell & 0x1F_FFFF;
            let fg    = (cell >> 21) & 0xFF;
            let bg    = (cell >> 29) & 0xFF;
            let attrs = (cell >> 37) & 0x0F;
            return cp | (bg << 21) | (fg << 29) | (attrs << 37);
        }
    }
    cell
}
