//! Compositor INPUT_FORWARD → keymap → LineDiscipline. Task 16 fills this in.

use crate::tty_backend::Cluuterm;
use libcluu::types::Message;

/// Handle a `COMP_INPUT_FORWARD_LABEL` message.
///
/// Task 16 will decode the key event from `msg`/`payload`, run it through the
/// keymap and LineDiscipline, and push resulting bytes into `term.stdin_buf`.
/// Until then this is a no-op stub so the recv loop compiles.
#[allow(unused_variables)]
pub fn handle(_term: &mut Cluuterm, _msg: &Message, _payload: &[u8]) {
    // TODO(task16): decode key → keymap → LineDiscipline → term.stdin_buf.
}
