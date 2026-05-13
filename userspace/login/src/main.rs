//! /bin/login — compositor client: login modal window.
//!
//! Task T2: WIN_REGISTER + static modal scaffold.
//!
//! Registers a window with the compositor, maps the SHM frame, renders a
//! centered login modal (username + password fields), sends WIN_DAMAGE, then
//! loops forever on ipc_recv_any.  No input handling (Task T3), no
//! SESSION_LOGIN submit (Task T4).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::ipc::{
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY,
};
use libcluu::syscall::MAP_FRAME_TOKEN;
use libcluu::types::{IpcFlags, Message};
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, registry, syscall};

// ─── Layout constants ─────────────────────────────────────────────────────────

/// Width of the login modal window in cells (interior + 1-cell chrome each side).
const WIN_W: u32 = 52;
/// Height of the login modal window in cells.
const WIN_H: u32 = 10;

/// Virtual address for the compositor SHM frame.
/// Distinct from cluuterm (0xD100_0000) and compdemo (0xD000_0000).
const SHM_VA: usize = 0xD200_0000;

const FLAGS_USER_RW: usize = 0x07;

// ─── Cell packing ─────────────────────────────────────────────────────────────
//
// Cell word layout (64-bit):
//   bits  0..20  — Unicode codepoint (up to U+1FFFFF)
//   bits 21..28  — foreground colour index (0-255)
//   bits 29..36  — background colour index (0-255)
//   bits 37..    — attributes (bold, underline, …)

/// Pack a cell word from a Unicode codepoint, fg/bg colour indices, and attrs.
#[inline(always)]
fn pack_cell(cp: u32, fg: u8, bg: u8, attrs: u64) -> u64 {
    ((cp as u64) & 0x1F_FFFF)
        | ((fg as u64) << 21)
        | ((bg as u64) << 29)
        | (attrs << 37)
}

// ─── Colour palette constants ──────────────────────────────────────────────────

/// Colour index 0 = black.
const BLACK:     u8 = 0;
/// Colour index 7 = light-grey (default text).
const WHITE:     u8 = 7;
/// Colour index 4 = blue (window chrome / title bar).
const BLUE:      u8 = 4;
/// Colour index 15 = bright white (title text).
const BR_WHITE:  u8 = 15;
/// Colour index 8 = dark grey (field background).
const DARK_GREY: u8 = 8;
/// Colour index 2 = green (hint text).
const GREEN:     u8 = 2;

// ─── Modal rendering ──────────────────────────────────────────────────────────

/// Write a horizontal run of `count` cells using the given (cp, fg, bg, attrs).
unsafe fn fill_run(
    cells: *mut u64,
    x: u32,
    y: u32,
    w: u32,
    count: u32,
    cp: u32,
    fg: u8,
    bg: u8,
    attrs: u64,
) {
    let cell = pack_cell(cp, fg, bg, attrs);
    for dx in 0..count {
        core::ptr::write_volatile(cells.add((y * w + x + dx) as usize), cell);
    }
}

/// Write a string slice as cells at (x, y) using the given fg/bg/attrs.
unsafe fn write_str(
    cells: *mut u64,
    x: u32,
    y: u32,
    w: u32,
    s: &[u8],
    fg: u8,
    bg: u8,
    attrs: u64,
) {
    for (i, &b) in s.iter().enumerate() {
        let cp = b as u32;
        core::ptr::write_volatile(
            cells.add((y * w + x + i as u32) as usize),
            pack_cell(cp, fg, bg, attrs),
        );
    }
}

/// Render the static login modal into the SHM cell buffer.
///
/// Layout (WIN_W=52, WIN_H=10):
///   Row 0   : top chrome bar  "╔══…══ CLUU login ══…══╗"
///   Row 1   : blank line      "║                      ║"
///   Row 2   : title line      "║      CLUU login       ║"
///   Row 3   : blank           "║                      ║"
///   Row 4   : username field  "║  username: __________  ║"
///   Row 5   : blank           "║                      ║"
///   Row 6   : password field  "║  password: __________  ║"
///   Row 7   : blank           "║                      ║"
///   Row 8   : hint line       "║  [Enter] login         ║"
///   Row 9   : bottom chrome   "╚══…══════════════════╝"
unsafe fn render_modal(cells: *mut u64, w: u32, h: u32) {
    // Background: fill whole window with blank dark cells.
    fill_run(cells, 0, 0, w, w * h, b' ' as u32, WHITE, BLACK, 0);

    // ── Row 0: top border ────────────────────────────────────────────────────
    // ╔ + (w-2) × ═ + ╗
    core::ptr::write_volatile(
        cells.add((0 * w) as usize),
        pack_cell(0x2554, BR_WHITE, BLUE, 0), // ╔
    );
    fill_run(cells, 1, 0, w, w - 2, 0x2550, BR_WHITE, BLUE, 0); // ═
    core::ptr::write_volatile(
        cells.add((0 * w + w - 1) as usize),
        pack_cell(0x2557, BR_WHITE, BLUE, 0), // ╗
    );

    // ── Row 9: bottom border ─────────────────────────────────────────────────
    let last = h - 1;
    core::ptr::write_volatile(
        cells.add((last * w) as usize),
        pack_cell(0x255A, BR_WHITE, BLUE, 0), // ╚
    );
    fill_run(cells, 1, last, w, w - 2, 0x2550, BR_WHITE, BLUE, 0); // ═
    core::ptr::write_volatile(
        cells.add((last * w + w - 1) as usize),
        pack_cell(0x255D, BR_WHITE, BLUE, 0), // ╝
    );

    // ── Interior rows 1..h-2: side borders + bg ───────────────────────────────
    for row in 1..h - 1 {
        // Left border ║
        core::ptr::write_volatile(
            cells.add((row * w) as usize),
            pack_cell(0x2551, BR_WHITE, BLUE, 0),
        );
        // Interior: blank on dark background
        fill_run(cells, 1, row, w, w - 2, b' ' as u32, WHITE, BLACK, 0);
        // Right border ║
        core::ptr::write_volatile(
            cells.add((row * w + w - 1) as usize),
            pack_cell(0x2551, BR_WHITE, BLUE, 0),
        );
    }

    // ── Row 2: centred title "CLUU login" ─────────────────────────────────────
    let title = b"CLUU login";
    let title_x = (w - title.len() as u32) / 2;
    write_str(cells, title_x, 2, w, title, BR_WHITE, BLACK, 1 /* bold */);

    // ── Row 4: username prompt + blank field ───────────────────────────────────
    let user_prompt = b"username: ";
    write_str(cells, 2, 4, w, user_prompt, WHITE, BLACK, 0);
    let field_x = 2 + user_prompt.len() as u32;
    fill_run(cells, field_x, 4, w, 10, b'_' as u32, WHITE, DARK_GREY, 0);

    // ── Row 6: password prompt + blank field ───────────────────────────────────
    let pass_prompt = b"password: ";
    write_str(cells, 2, 6, w, pass_prompt, WHITE, BLACK, 0);
    fill_run(cells, field_x, 6, w, 10, b'_' as u32, WHITE, DARK_GREY, 0);

    // ── Row 8: hint ────────────────────────────────────────────────────────────
    let hint = b"[Enter] login";
    write_str(cells, 2, 8, w, hint, GREEN, BLACK, 0);
}

// ─── WIN_REGISTER ─────────────────────────────────────────────────────────────

/// Register a compositor window and map the SHM frame.
///
/// Returns `(win_id, comp_ep, granted_w, granted_h)` on success or an i32
/// exit code on error.
fn register_window(my_ep: usize) -> Result<(u32, usize, u32, u32), i32> {
    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("login: no compositor:client in registry");
            return Err(2);
        }
    };

    let title = b"CLUU login";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [
            title.len(),      // words[0] = payload_len
            WIN_W as usize,   // words[1] = req_w
            WIN_H as usize,   // words[2] = req_h
            my_ep,            // words[3] = app input/frame endpoint
            0,                // words[4] = reserved
            0,                // words[5] = reserved
        ],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if libcluu::ipc::call_with_payload(comp_ep, &req, title, &mut reply).is_err() {
        return Err(3);
    }
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        return Err(4);
    }
    let win_id   = reply.words[0] as u32;
    let shm_tok  = reply.words[1];
    let gw       = reply.words[2] as u32;
    let gh       = reply.words[3] as u32;
    let err      = reply.words[4];
    if err != 0 {
        return Err(5);
    }

    // Map the SHM frame token into our address space at SHM_VA.
    let cells_bytes = gw as usize * gh as usize * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let space = space_token();
    if syscall::space_map_range(
        space,
        SHM_VA,
        shm_tok,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    )
    .is_err()
    {
        return Err(6);
    }

    Ok((win_id, comp_ep, gw, gh))
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let _ = debug_print("login: start");

    // Registry init must precede any lookup_service calls.
    if registry::init("login").is_err() {
        let _ = debug_print("login: registry init failed");
        return 1;
    }

    // Allocate a long-lived endpoint (compositor pacing + future input events).
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("login: endpoint_create failed");
            return 1;
        }
    };

    // Register window with compositor and map SHM.
    let (win_id, comp_ep, gw, gh) = match register_window(my_ep) {
        Ok(v) => v,
        Err(code) => {
            let _ = debug_print("login: WIN_REGISTER failed");
            return code;
        }
    };
    let _ = debug_print("login: window registered");

    // Initialise WindowShm header at SHM_VA.
    let shm_ptr = SHM_VA as *mut WindowShm;
    unsafe {
        core::ptr::write_volatile(
            &mut (*shm_ptr).magic as *mut u32,
            libcluu::window_shm::WIN_SHM_MAGIC,
        );
        core::ptr::write_volatile(
            &mut (*shm_ptr).version as *mut u32,
            libcluu::window_shm::WIN_SHM_VERSION,
        );
        core::ptr::write_volatile(&mut (*shm_ptr).width as *mut u32, gw);
        core::ptr::write_volatile(&mut (*shm_ptr).height as *mut u32, gh);
        core::ptr::write_volatile(&mut (*shm_ptr).cursor_visible as *mut u32, 0);
        core::ptr::write_volatile(&mut (*shm_ptr).generation as *mut u32, 0);
    }

    // Render the static login modal into the cell buffer (offset 32 into SHM).
    let cells_ptr = (SHM_VA + 32) as *mut u64;
    unsafe {
        render_modal(cells_ptr, gw, gh);
    }

    // Bump generation so compositor knows cells are ready.
    unsafe {
        let g = (*shm_ptr).generation;
        core::ptr::write_volatile(&mut (*shm_ptr).generation as *mut u32, g.wrapping_add(1));
    }

    // Send WIN_DAMAGE for the full window.
    let dmg = Message::new(
        COMP_WIN_DAMAGE_LABEL,
        [win_id as usize, 0, 0, gw as usize, gh as usize, 0],
        5,
    );
    let _ = libcluu::ipc::send(comp_ep, &dmg, IpcFlags::empty());

    let _ = debug_print("login: window registered");

    // Event loop stub — Task T3 will add INPUT_FORWARD handling.
    let tokens = [my_ep];
    let mut recv_buf = [0u8; 256];
    loop {
        let _ = syscall::ipc_recv_any(&tokens, &mut recv_buf, u64::MAX);
    }
}
