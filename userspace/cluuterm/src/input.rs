//! Compositor INPUT_FORWARD → keymap → LineDiscipline → stdin_buf / echo.
//!
//! # Wire layout (COMP_INPUT_FORWARD_LABEL)
//!
//! | word | field    | notes                                    |
//! |------|----------|------------------------------------------|
//! |  0   | window_id| the focused window's id                  |
//! |  1   | ascii    | printable/control byte (0 if none)       |
//! |  2   | mods     | modifier bitmask (unused here)           |
//! |  3   | scancode | hardware scancode (unused here)          |
//! |  4   | extended | KEY_* enum from kbd driver               |
//! |  5   | kind     | 0 = ordinary input, 99 = close-request   |
//!
//! The close-request path (kind=99) is handled by the recv loop before it
//! reaches this function; we only ever see kind=0 here.

use crate::tty_backend::Cluuterm;
use libcluu::tty_core::{keymap::encode_extended, EchoAction, LineEffect};
use libcluu::types::Message;
use libcluu::debug_print;

/// Handle a `COMP_INPUT_FORWARD_LABEL` message.
///
/// Extracts the key event, runs it through the shared keymap and the terminal's
/// `LineDiscipline`, then applies the resulting `LineEffect`:
/// - Echo bytes are fed back through the ANSI parser (cell-grid update).
/// - Completed lines and raw bytes are pushed into `term.stdin_buf`.
/// - Signals (Ctrl-C / Ctrl-Z as single-byte marker in `line_ready`) are
///   dropped in v1 — no process tracking yet.
/// - TAB completion requests are silently dropped in v1 — no VFS view here.
pub fn handle(term: &mut Cluuterm, msg: &Message, _payload: &[u8]) {
    let ascii    = msg.words[1] as u8;
    let extended = msg.words[4] as u8;

    if let Some(bytes) = encode_extended(extended) {
        // Log the CSI sequence (hex) for harness observability.
        // Arrow keys produce 3-byte sequences: ESC [ A/B/C/D.
        let mut logbuf = *b"cluuterm: input csi 00";
        let hex = b"0123456789abcdef";
        // Encode first byte of the sequence (0x1b for CSI).
        if !bytes.is_empty() {
            logbuf[20] = hex[(bytes[0] >> 4) as usize];
            logbuf[21] = hex[(bytes[0] & 0xF) as usize];
        }
        let s_str = unsafe { core::str::from_utf8_unchecked(&logbuf) };
        let _ = debug_print(s_str);
        for &b in bytes {
            push_through_discipline(term, b);
        }
    } else if ascii != 0 {
        push_through_discipline(term, ascii);
    }
}

/// Feed a single byte through the line discipline and apply the effect.
fn push_through_discipline(term: &mut Cluuterm, b: u8) {
    let effect = term.discipline.handle_byte(b);
    apply_effect(term, effect);
}

/// Map a `LineEffect` to cell-grid echo and/or stdin delivery.
fn apply_effect(term: &mut Cluuterm, effect: LineEffect) {
    // ── Echo back to the cell grid ──────────────────────────────────────────
    // Feed echo bytes through `handle_pts_write` so the ANSI parser sees them
    // and the cell grid + renderer stay consistent.
    match effect.echo {
        EchoAction::None => {}
        EchoAction::Bytes(bytes) => term.handle_pts_write(bytes),
        EchoAction::Byte(byte)   => term.handle_pts_write(&[byte]),
        EchoAction::OwnedBytes(bytes) => term.handle_pts_write(&bytes),
    }

    // ── Raw mode: deliver byte immediately to stdin ─────────────────────────
    if let Some(raw) = effect.raw_byte {
        term.stdin_buf.push_back(raw);
        term.try_flush_pending_pts_read();
    }

    // ── TAB completion: not supported in v1 ────────────────────────────────
    // cluuterm has no VFS view to enumerate paths with, so TAB requests are
    // silently dropped until a future task wires tab-completion through procmgr
    // or the shell's own endpoint.
    let _ = effect.tab_request;

    // ── Cooked mode: line ready ─────────────────────────────────────────────
    if let Some(line) = effect.line_ready {
        let is_ctrl_c = line.len() == 1 && line[0] == 0x03;
        let is_ctrl_z = line.len() == 1 && line[0] == 0x1A;

        if is_ctrl_c || is_ctrl_z {
            // v1: no shell process tracking — signals are dropped.
            // The echo path above already printed "^C\n" / "^Z\n" to the cell
            // grid when the discipline produced those echo bytes, so the user
            // sees the visual feedback even though no signal is delivered.
            let _ = libcluu::debug_print(if is_ctrl_c {
                "cluuterm: Ctrl-C (signal dropped in v1)"
            } else {
                "cluuterm: Ctrl-Z (signal dropped in v1)"
            });
            return;
        }

        // Cooked-mode line (or Ctrl-D EOT): push every byte onto stdin_buf so
        // the next PTS_READ from the shell drains them.
        for b in line {
            term.stdin_buf.push_back(b);
        }
        term.try_flush_pending_pts_read();
    }
}
