//! TTY raw-mode wrappers for the editor.
//!
//! Thin facade over `libcluu::posix::tty` that pins the TTY endpoint to the
//! current process's stdin slot. The editor calls `enter_raw_mode` on
//! startup and `restore_mode` before exit so cooked-mode line discipline
//! returns to the shell.

use libcluu::boot::TOKEN_STDIN;
use libcluu::posix::tty::{enter_raw, restore, SavedTty};
use libcluu::{process_info, Error, Result};

/// Opaque guard returned by `enter_raw_mode`; pass it to `restore_mode`.
pub struct EditorTty(SavedTty);

/// Switch the editor's stdin TTY into raw mode (clears ICANON+ECHO).
pub fn enter_raw_mode() -> Result<EditorTty> {
    let info = process_info();
    let stdin = info.tokens[TOKEN_STDIN];
    if stdin == 0 {
        return Err(Error::InvalidState);
    }
    Ok(EditorTty(enter_raw(stdin)?))
}

/// Restore the line-discipline state captured by `enter_raw_mode`.
pub fn restore_mode(saved: EditorTty) -> Result<()> {
    restore(saved.0)
}
