//! /bin/login — compositor client: login modal window.
//!
//! Post-authentication flow (Task 6 Plan 3):
//!   1. SESSION_CREATE (libcluu::session::create)
//!   2. DERIVE_TOKEN  (narrowed token for compositor)
//!   3. COMPOSITOR_SESSION_HANDOFF (hand over session to compositor)
//!   4. Spawn cluuterm (libcluu::spawn::spawn)
//!   5. SESSION_SET_LEADER (set primary pid)
//!
//! Registers a window with the compositor, maps the SHM frame, renders a
//! centered login modal (username + password fields), sends WIN_DAMAGE, then
//! loops on ipc_recv_any handling COMP_INPUT_FORWARD_LABEL keystrokes.
//! On Enter-in-password: executes the five-step session lifecycle.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use cluu_wire::session::{
    CompositorSessionHandoffRequest, ProfileSpec, SessionCreateRequest,
    COMPOSITOR_SESSION_HANDOFF_LABEL, RIGHT_SESSION_QUERY, RIGHT_SESSION_SUBSCRIBE,
};
use cluu_wire::spawn::{SpawnEnvelope, ViewSource};

use libcluu::boot::{process_info, space_token, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::ipc::{
    COMP_INPUT_FORWARD_LABEL, COMP_WIN_DAMAGE_LABEL, COMP_WIN_FLAG_FULLSCREEN,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY,
};
use libcluu::syscall::MAP_FRAME_TOKEN;
use libcluu::types::{IpcFlags, Message};
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, registry, syscall};

// ─── Layout constants ─────────────────────────────────────────────────────────

/// Width of the login modal box in cells (border-inclusive).
const MODAL_W: u32 = 52;
/// Height of the login modal box in cells (border-inclusive).
const MODAL_H: u32 = 10;

/// Banner text from shell — rendered above the modal.
const BANNER: &str = include_str!("../../shell/src/banner.txt");
/// Number of banner text lines (9 content rows).
const BANNER_H: u32 = 9;
/// Gap (blank rows) between banner bottom and modal top.
const BANNER_GAP: u32 = 1;

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
/// Colour index 1 = red (error text).
const RED:       u8 = 1;
/// Colour index 11 = bright yellow (focused-field highlight).
const YELLOW:    u8 = 11;

// ─── Maximum field length ─────────────────────────────────────────────────────

/// Maximum number of characters accepted in username or password field.
const FIELD_MAX: usize = 20;

// ─── Field-focus state ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Username,
    Password,
}

/// Mutable state for the login form.
struct LoginState {
    username: alloc::vec::Vec<u8>,
    password: alloc::vec::Vec<u8>,
    focus: Focus,
    /// True while "login incorrect" error banner should be shown on row 8.
    show_error: bool,
}

impl LoginState {
    fn new() -> Self {
        Self {
            username: alloc::vec::Vec::new(),
            password: alloc::vec::Vec::new(),
            focus: Focus::Username,
            show_error: false,
        }
    }

    /// Append a printable ASCII character to the active field (capped at FIELD_MAX).
    /// Clears the error banner on first keystroke after a failed login.
    fn handle_char(&mut self, c: u8) {
        self.show_error = false;
        let field = match self.focus {
            Focus::Username => &mut self.username,
            Focus::Password => &mut self.password,
        };
        if field.len() < FIELD_MAX && c.is_ascii_graphic() {
            field.push(c);
        }
    }

    /// Delete the last character from the active field.
    /// Clears the error banner on first keystroke after a failed login.
    fn handle_backspace(&mut self) {
        self.show_error = false;
        match self.focus {
            Focus::Username => { self.username.pop(); }
            Focus::Password => { self.password.pop(); }
        }
    }

    /// Toggle focus between username and password.
    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Username => Focus::Password,
            Focus::Password => Focus::Username,
        };
    }

    /// Handle Enter: advance focus from username→password, return true if on
    /// password (submit intent).
    fn handle_enter(&mut self) -> bool {
        match self.focus {
            Focus::Username => {
                self.focus = Focus::Password;
                false
            }
            Focus::Password => true,
        }
    }
}

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

/// Render the full-screen login window into the SHM cell buffer.
///
/// The window is `cols × rows` cells (full compositor grid). The banner (9
/// rows) plus a 1-row gap plus the login modal (MODAL_W=52 × MODAL_H=10) are
/// vertically stacked and centered together:
///   total_h = BANNER_H + BANNER_GAP + MODAL_H = 20
///   stack_y = (rows - total_h) / 2
///   banner_y_top = stack_y
///   modal_y_top  = stack_y + BANNER_H + BANNER_GAP
///
/// Modal layout (relative to modal top-left):
///   Row 0   : top border    "╔══…══╗"
///   Row 1   : blank         "║    ║"
///   Row 2   : title         "║  CLUU login  ║"
///   Row 3   : blank         "║    ║"
///   Row 4   : username      "║  username: __________  ║"
///   Row 5   : blank         "║    ║"
///   Row 6   : password      "║  password: __________  ║"
///   Row 7   : blank         "║    ║"
///   Row 8   : hint/error    "║  [Tab] focus  [Enter] login  ║"
///   Row 9   : bottom border "╚══…══╝"
///
/// All cells outside the modal box are black spaces.
unsafe fn render_modal(cells: *mut u64, w: u32, h: u32, state: &LoginState) {
    // Fill entire screen with black background cells.
    fill_run(cells, 0, 0, w, w * h, b' ' as u32, WHITE, BLACK, 0);

    // Compute stacked layout: banner + gap + modal, all centered vertically.
    let total_h = BANNER_H + BANNER_GAP + MODAL_H;
    let stack_y = h.saturating_sub(total_h) / 2;
    let banner_y_top = stack_y;
    let modal_y_top = stack_y + BANNER_H + BANNER_GAP;

    // ── Banner: render each line centered horizontally ────────────────────────
    for (i, line) in BANNER.lines().enumerate() {
        let chars: alloc::vec::Vec<char> = line.chars().collect();
        let lx = (w.saturating_sub(chars.len() as u32)) / 2;
        let ly = banner_y_top + i as u32;
        for (j, ch) in chars.iter().enumerate() {
            let cp = *ch as u32;
            let cell = pack_cell(cp, BR_WHITE, BLACK, 0);
            core::ptr::write_volatile(
                cells.add((ly * w + lx + j as u32) as usize),
                cell,
            );
        }
    }

    // Center the modal box horizontally; use modal_y_top for vertical.
    let mx = (w.saturating_sub(MODAL_W)) / 2;
    let my = modal_y_top;
    let mw = MODAL_W;
    let mh = MODAL_H;

    // ── Modal row 0 (my+0): top border ╔═…═╗ ────────────────────────────────
    core::ptr::write_volatile(
        cells.add(((my) * w + mx) as usize),
        pack_cell(0x2554, BR_WHITE, BLUE, 0), // ╔
    );
    fill_run(cells, mx + 1, my, w, mw - 2, 0x2550, BR_WHITE, BLUE, 0); // ═
    core::ptr::write_volatile(
        cells.add(((my) * w + mx + mw - 1) as usize),
        pack_cell(0x2557, BR_WHITE, BLUE, 0), // ╗
    );

    // ── Modal row (my+mh-1): bottom border ╚═…═╝ ────────────────────────────
    let last_row = my + mh - 1;
    core::ptr::write_volatile(
        cells.add((last_row * w + mx) as usize),
        pack_cell(0x255A, BR_WHITE, BLUE, 0), // ╚
    );
    fill_run(cells, mx + 1, last_row, w, mw - 2, 0x2550, BR_WHITE, BLUE, 0); // ═
    core::ptr::write_volatile(
        cells.add((last_row * w + mx + mw - 1) as usize),
        pack_cell(0x255D, BR_WHITE, BLUE, 0), // ╝
    );

    // ── Interior rows (my+1)..(my+mh-1): side borders + bg ───────────────────
    for r in 1..mh - 1 {
        let row = my + r;
        // Left border ║
        core::ptr::write_volatile(
            cells.add((row * w + mx) as usize),
            pack_cell(0x2551, BR_WHITE, BLUE, 0),
        );
        // Interior: blank on dark background
        fill_run(cells, mx + 1, row, w, mw - 2, b' ' as u32, WHITE, BLACK, 0);
        // Right border ║
        core::ptr::write_volatile(
            cells.add((row * w + mx + mw - 1) as usize),
            pack_cell(0x2551, BR_WHITE, BLUE, 0),
        );
    }

    // ── Modal row 2: centred title "CLUU login" ───────────────────────────────
    let title = b"CLUU login";
    let title_x = mx + (mw - title.len() as u32) / 2;
    write_str(cells, title_x, my + 2, w, title, BR_WHITE, BLACK, 1 /* bold */);

    // ── Modal rows 4, 6, and 8: interactive content (fields + hint/error) ─────
    render_fields(cells, w, mx, my, state);
}

/// Re-render the username (modal row 4), password (modal row 6), and
/// hint/error (modal row 8) rows.
///
/// `w` is the full screen width (stride for cell addressing).
/// `mx`/`my` are the modal top-left position on the full-screen grid.
///
/// Called both from `render_modal` (initial paint) and from the input loop
/// (per-keystroke update).  The chrome rows (borders, title) are left
/// untouched.
///
/// Field layout (field_x = mx + 2 + prompt.len() = mx + 12, field_w = 10):
///   - Filled characters at positions [0..len).
///   - Underscore placeholders at positions [len..FIELD_W).
/// The focused field uses YELLOW fg on DARK_GREY bg; the unfocused one uses
/// WHITE fg on DARK_GREY bg.
unsafe fn render_fields(cells: *mut u64, w: u32, mx: u32, my: u32, state: &LoginState) {
    const FIELD_INDENT: u32 = 12; // 2 indent + "username: ".len() = 12
    const FIELD_W: u32 = 10;      // visible field width

    let user_prompt = b"username: ";
    let pass_prompt = b"password: ";

    let row4 = my + 4;
    let row6 = my + 6;
    let row8 = my + 8;

    // ── Row 4: username ────────────────────────────────────────────────────────
    let (ufg, pfg) = if state.focus == Focus::Username {
        (YELLOW, WHITE)
    } else {
        (WHITE, YELLOW)
    };

    // Re-draw prompt in the correct colour to show focus on the label too.
    write_str(cells, mx + 2, row4, w, user_prompt, ufg, BLACK, 0);

    let field_x = mx + FIELD_INDENT;
    let ulen = state.username.len() as u32;
    // Typed characters (shown as-is for username).
    for (i, &c) in state.username.iter().enumerate() {
        core::ptr::write_volatile(
            cells.add((row4 * w + field_x + i as u32) as usize),
            pack_cell(c as u32, ufg, DARK_GREY, 0),
        );
    }
    // Underscore placeholders for remaining positions.
    if ulen < FIELD_W {
        fill_run(cells, field_x + ulen, row4, w, FIELD_W - ulen, b'_' as u32, ufg, DARK_GREY, 0);
    }

    // ── Row 6: password ────────────────────────────────────────────────────────
    write_str(cells, mx + 2, row6, w, pass_prompt, pfg, BLACK, 0);

    let plen = state.password.len() as u32;
    // Password characters are masked as '*'.
    for i in 0..plen {
        core::ptr::write_volatile(
            cells.add((row6 * w + field_x + i) as usize),
            pack_cell(b'*' as u32, pfg, DARK_GREY, 0),
        );
    }
    // Underscore placeholders.
    if plen < FIELD_W {
        fill_run(cells, field_x + plen, row6, w, FIELD_W - plen, b'_' as u32, pfg, DARK_GREY, 0);
    }

    // ── Row 8: hint or error banner ────────────────────────────────────────────
    // Clear the modal interior of row 8 (preserve side borders from render_modal).
    fill_run(cells, mx + 1, row8, w, MODAL_W - 2, b' ' as u32, WHITE, BLACK, 0);
    if state.show_error {
        let err_msg = b"login incorrect";
        let interior = MODAL_W - 2;
        let msg_len = err_msg.len() as u32;
        let err_x = mx + 1 + (interior.saturating_sub(msg_len)) / 2;
        write_str(cells, err_x, row8, w, err_msg, RED, BLACK, 0);
    } else {
        let hint = b"[Tab] focus  [Enter] login";
        write_str(cells, mx + 2, row8, w, hint, GREEN, BLACK, 0);
    }
}

// ─── View token ───────────────────────────────────────────────────────────────

/// Returns the view token inherited from the parent spawn.
fn login_view_token() -> u64 {
    process_info().tokens[TOKEN_EXTRA_0] as u64
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
            title.len(),                        // words[0] = payload_len
            0,                                  // words[1] = req_w (ignored: fullscreen)
            0,                                  // words[2] = req_h (ignored: fullscreen)
            my_ep,                              // words[3] = app input/frame endpoint
            COMP_WIN_FLAG_FULLSCREEN as usize,  // words[4] = flags
            0,                                  // words[5] = reserved
        ],
        5,
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

    // Allocate a long-lived endpoint (compositor pacing + input events).
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

    // Compute modal top-left position using the stacked banner+gap+modal layout.
    // This must match the computation in render_modal so render_fields targets
    // the same modal origin.
    let total_h = BANNER_H + BANNER_GAP + MODAL_H;
    let stack_y = gh.saturating_sub(total_h) / 2;
    let modal_mx = (gw.saturating_sub(MODAL_W)) / 2;
    let modal_my = stack_y + BANNER_H + BANNER_GAP;

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

    // Initial form state (focus = Username, both fields empty).
    let mut state = LoginState::new();

    // Render the login modal (chrome + initial empty fields) into the cell buffer.
    let cells_ptr = (SHM_VA + 32) as *mut u64;
    unsafe {
        render_modal(cells_ptr, gw, gh, &state);
    }

    // Helper closure: bump generation + send WIN_DAMAGE.
    let send_damage = |shm_ptr: *mut WindowShm| {
        unsafe {
            let g = (*shm_ptr).generation;
            core::ptr::write_volatile(
                &mut (*shm_ptr).generation as *mut u32,
                g.wrapping_add(1),
            );
        }
        let dmg = Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [win_id as usize, 0, 0, gw as usize, gh as usize, 0],
            5,
        );
        let _ = libcluu::ipc::send(comp_ep, &dmg, IpcFlags::empty());
    };

    // Initial damage flush.
    send_damage(shm_ptr);

    // ── Event loop ────────────────────────────────────────────────────────────
    //
    // Wire layout for COMP_INPUT_FORWARD_LABEL (from libcluu/cluuterm/src/input.rs):
    //   words[0] = window_id
    //   words[1] = ascii      (printable/control byte; 0 if none)
    //   words[2] = mods       (modifier bitmask; unused here)
    //   words[3] = scancode   (hardware scancode; unused here)
    //   words[4] = extended   (KEY_* enum; arrow keys etc.)
    //   words[5] = kind       (0 = ordinary; 99 = close-request)
    //
    // We only care about ascii (words[1]):
    //   0x09        Tab        → toggle focus
    //   0x08/0x7F   Backspace  → pop last char from active field
    //   0x0A/0x0D   Enter      → username: advance to password; password: submit
    //   graphic     printable  → append to active field
    let tokens = [my_ep];
    let mut recv_buf = [0u8; 256];

    loop {
        let (_, len) = match syscall::ipc_recv_any(&tokens, &mut recv_buf, u64::MAX) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let Some((msg, _payload)) = libcluu::ipc::parse_message(&recv_buf[..len]) else {
            continue;
        };

        match msg.tag.label {
            COMP_INPUT_FORWARD_LABEL => {
                let ascii = msg.words[1] as u8;

                match ascii {
                    // Tab: toggle focus between username and password.
                    0x09 => {
                        state.toggle_focus();
                    }
                    // Backspace (BS=0x08, DEL=0x7F): delete last char in active field.
                    0x08 | 0x7F => {
                        state.handle_backspace();
                    }
                    // Enter (LF=0x0A, CR=0x0D).
                    0x0A | 0x0D => {
                        let submit = state.handle_enter();
                        if submit {
                            let user_name =
                                alloc::string::String::from_utf8_lossy(&state.username)
                                    .into_owned();
                            // ── 1. SESSION_CREATE ──────────────────────────────────
                            let create_reply = libcluu::session::create(SessionCreateRequest {
                                user_name: user_name.clone(),
                                profile: ProfileSpec {
                                    home: alloc::format!("/home/{}", user_name),
                                    initial_view: ViewSource::Derive(login_view_token()),
                                    env: alloc::vec![
                                        (alloc::string::String::from("HOME"),
                                         alloc::format!("/home/{}", user_name)),
                                        (alloc::string::String::from("USER"),
                                         user_name.clone()),
                                        (alloc::string::String::from("TERM"),
                                         alloc::string::String::from("xterm-256color")),
                                    ],
                                    umask: 0o022,
                                },
                            });
                            let ok = match create_reply {
                                Ok(o) => o,
                                Err(ref e) => {
                                    let msg = alloc::format!(
                                        "login: SESSION_CREATE failed: {:?}", e
                                    );
                                    let _ = debug_print(&msg);
                                    return -1;
                                }
                            };

                            // ── 2. DERIVE_TOKEN ────────────────────────────────────
                            let token_sub = match libcluu::session::derive_token(
                                ok.token, RIGHT_SESSION_SUBSCRIBE | RIGHT_SESSION_QUERY,
                            ) {
                                Ok(t) => t,
                                Err(e) => {
                                    let _ = debug_print("login: derive_token failed");
                                    let _ = e;
                                    return -1;
                                }
                            };

                            // ── 3. COMPOSITOR_SESSION_HANDOFF ──────────────────────
                            let handoff_req = CompositorSessionHandoffRequest {
                                session_id: ok.session_id,
                                token_sub,
                            };
                            let payload = postcard::to_allocvec(&handoff_req)
                                .expect("ser");
                            let compositor_control = match
                                registry::lookup_service("compositor:control")
                            {
                                Some(ep) => ep,
                                None => {
                                    let _ = debug_print(
                                        "login: compositor:control not found",
                                    );
                                    return -1;
                                }
                            };
                            let words = [
                                payload.len(),
                                cluu_wire::ABI_VERSION as usize,
                                0, 0, 0, 0,
                            ];
                            let msg = Message::new(
                                COMPOSITOR_SESSION_HANDOFF_LABEL, words, 0,
                            );
                            let mut reply_buf = [0u8; 512];
                            if libcluu::ipc::call_with_reply_buf(
                                compositor_control, &msg, &payload, &mut reply_buf,
                            )
                            .is_err()
                            {
                                let _ = debug_print("login: handoff IPC failed");
                                return -1;
                            }

                            // ── 4. Spawn cluuterm ─────────────────────────────────
                            let primary_envelope = SpawnEnvelope {
                                image: alloc::string::String::from("cluuterm"),
                                args: alloc::vec::Vec::new(),
                                env: alloc::vec![
                                    (alloc::string::String::from("HOME"),
                                     alloc::format!("/home/{}", user_name)),
                                    (alloc::string::String::from("USER"),
                                     user_name.clone()),
                                ],
                                view: ViewSource::Derive(login_view_token()),
                                fd_inherit: alloc::vec::Vec::new(),
                                session: Some(ok.token),
                                notify: None,
                            };
                            let primary_pid =
                                match libcluu::spawn::spawn(primary_envelope) {
                                    Ok(r) => r.pid,
                                    Err(e) => {
                                        let _ = debug_print(
                                            &alloc::format!(
                                                "login: primary spawn failed: {:?}",
                                                e
                                            )
                                        );
                                        return -1;
                                    }
                                };

                            // ── 5. SESSION_SET_LEADER ─────────────────────────────
                            if libcluu::session::set_leader(ok.token, primary_pid)
                                .is_err()
                            {
                                let _ = debug_print("login: set_leader failed");
                                return -1;
                            }

                            // Tell the compositor to destroy our window before we
                            // exit so the user immediately sees just the cluuterm
                            // window.
                            let destroy = Message::new(
                                libcluu::ipc::COMP_WIN_DESTROY_LABEL,
                                [win_id as usize, 0, 0, 0, 0, 0],
                                1,
                            );
                            let _ = libcluu::ipc::send(
                                comp_ep, &destroy, IpcFlags::empty(),
                            );
                            return 0;
                        }
                        // Moved focus to password — fall through to re-render.
                    }
                    // Printable ASCII: append to active field.
                    _ if ascii.is_ascii_graphic() => {
                        state.handle_char(ascii);
                    }
                    // Other control characters: ignore.
                    _ => continue,
                }

                // Re-render the field rows and send WIN_DAMAGE.
                unsafe {
                    render_fields(cells_ptr, gw, modal_mx, modal_my, &state);
                }
                send_damage(shm_ptr);
            }
            // All other labels are silently dropped.
            _ => {}
        }
    }
}
