//! TTY raw-mode wrappers for the editor.
//!
//! Thin facade over `libcluu::posix::tty`. Talks to the TTY service via the
//! TOKEN_STDOUT slot — procmgr wires that slot to `tty_endpoints[0]`, which
//! is the TTY service's `main` endpoint that handles `TTY_CTL_LABEL`.
//! TOKEN_STDIN is a procmgr-managed bridge endpoint for keystroke delivery
//! and does NOT respond to TTY_CTL — sending a control message there hangs
//! forever because procmgr never replies.

use libcluu::boot::TOKEN_STDOUT;
use libcluu::posix::tty::{enter_raw, restore, SavedTty};
use libcluu::{process_info, Error, Result};

/// Opaque guard returned by `enter_raw_mode`; pass it to `restore_mode`.
pub struct EditorTty(SavedTty);

/// Switch the TTY into raw mode (clears ICANON+ECHO).
pub fn enter_raw_mode() -> Result<EditorTty> {
    let info = process_info();
    let tty = info.tokens[TOKEN_STDOUT];
    if tty == 0 {
        return Err(Error::InvalidState);
    }
    Ok(EditorTty(enter_raw(tty)?))
}

/// Restore the line-discipline state captured by `enter_raw_mode`.
pub fn restore_mode(saved: EditorTty) -> Result<()> {
    restore(saved.0)
}
