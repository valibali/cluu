//! TTY line-discipline helpers (raw mode for TUI binaries).
//!
//! Marshalled over `TTY_CTL_LABEL` — single message, words[0]=subcmd
//! (0=get, 1=set), words[4]=lflag bits. Two recognized bits:
//!   ICANON = 0x02   (canonical / line-buffered)
//!   ECHO   = 0x08   (echo input)
//!
//! These helpers were promoted from the shell's private
//! `tty_get_lflag` / `tty_set_lflag` (see
//! `userspace/shell/src/commands.rs:1629-1641` prior to T10) so the
//! editor and any future TUI binary can reuse them.

use crate::error::{Error, Result};
use crate::ipc::{call, TTY_CTL_LABEL};
use crate::syscall;
use crate::types::{IpcFlags, Message};

pub const TTY_LFLAG_ICANON: usize = 0x02;
pub const TTY_LFLAG_ECHO: usize = 0x08;

/// TTY_CTL is request/reply; the line discipline may briefly be busy on
/// concurrent activity. Retry a bounded number of times before giving up,
/// matching the shell's original behavior.
fn tty_ctl_call_with_retry(tty_endpoint: usize, msg: &mut Message) -> Result<()> {
    const RETRIES: usize = 128;
    for _ in 0..RETRIES {
        match call(tty_endpoint, msg, IpcFlags::empty()) {
            Ok(()) => return Ok(()),
            Err(Error::WouldBlock) | Err(Error::Busy) => {
                let _ = syscall::yield_cpu();
            }
            Err(err) => return Err(err),
        }
    }
    Err(Error::Busy)
}

/// Read the current lflag from the TTY at `tty_endpoint`.
pub fn get_lflag(tty_endpoint: usize) -> Result<usize> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 1);
    msg.words[0] = 0; // getattr
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)?;
    Ok(msg.words[4])
}

/// Set the lflag on the TTY at `tty_endpoint`.
pub fn set_lflag(tty_endpoint: usize, lflag: usize) -> Result<()> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    msg.words[0] = 1; // setattr
    msg.words[4] = lflag;
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)
}

/// Saved lflag state for `enter_raw` / `restore`.
#[derive(Clone, Copy)]
pub struct SavedTty {
    pub tty_endpoint: usize,
    pub saved_lflag: usize,
}

/// Switch the TTY at `tty_endpoint` into raw mode. Clears ICANON+ECHO.
pub fn enter_raw(tty_endpoint: usize) -> Result<SavedTty> {
    let saved_lflag = get_lflag(tty_endpoint)?;
    let target = saved_lflag & !(TTY_LFLAG_ICANON | TTY_LFLAG_ECHO);
    set_lflag(tty_endpoint, target)?;
    Ok(SavedTty {
        tty_endpoint,
        saved_lflag,
    })
}

/// Restore the lflag to its saved state.
pub fn restore(saved: SavedTty) -> Result<()> {
    set_lflag(saved.tty_endpoint, saved.saved_lflag)
}
