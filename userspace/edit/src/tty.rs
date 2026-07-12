//! TTY raw-mode wrappers for the editor.
//!
//! Dual-path: tries legacy `TTY_CTL_LABEL` first (VT1-3 text consoles) with
//! a short timeout, then falls back to PTS termios via `tcgetattr`/`tcsetattr`
//! on fd 1 (cluuterm/VT4). The legacy path uses a timeout because under
//! cluuterm the TOKEN_STDOUT endpoint does not speak TTY_CTL.

use libcluu::boot::TOKEN_STDOUT;
use libcluu::posix::tty::{restore, SavedTty, TTY_LFLAG_ECHO, TTY_LFLAG_ICANON};
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, process_info, Error, Result};

const TTY_CTL_LABEL: u32 = libcluu::ipc::TTY_CTL_LABEL;

pub enum EditorTty {
    Legacy(SavedTty),
    Pts {
        saved_lflag: u32,
    },
}

pub fn enter_raw_mode() -> Result<EditorTty> {
    let _ = debug_print("edit: enter_raw_mode start\n");
    let info = process_info();
    let tty = info.tokens[TOKEN_STDOUT];
    if tty == 0 {
        return Err(Error::InvalidState);
    }

    let _ = debug_print("edit: trying legacy TTY_CTL (500ms timeout)\n");
    match try_legacy_enter_raw(tty) {
        Ok(saved) => {
            let _ = debug_print("edit: legacy TTY_CTL OK\n");
            return Ok(EditorTty::Legacy(saved));
        }
        Err(e) => {
            let _ = debug_print(&alloc::format!("edit: legacy failed: {:?}\n", e));
        }
    }

    let _ = debug_print("edit: trying PTS termios via tcgetattr(fd=1)\n");
    let mut t = libcluu::posix::termios::Termios {
        c_iflag: 0, c_oflag: 0, c_cflag: 0, c_lflag: 0,
        c_cc: [0; 20], c_ispeed: 0, c_ospeed: 0,
    };
    let rc = unsafe { libcluu::posix::termios::tcgetattr(1, &mut t) };
    if rc != 0 {
        let _ = debug_print("edit: tcgetattr failed\n");
        return Err(Error::InvalidState);
    }
    let saved_lflag = t.c_lflag;
    let _ = debug_print(&alloc::format!("edit: tcgetattr OK lflag={:#x}\n", saved_lflag));

    t.c_lflag &= !(TTY_LFLAG_ICANON as u32 | TTY_LFLAG_ECHO as u32);
    let rc = unsafe { libcluu::posix::termios::tcsetattr(1, 0, &t) };
    if rc != 0 {
        let _ = debug_print("edit: tcsetattr failed\n");
        return Err(Error::InvalidState);
    }
    let _ = debug_print("edit: raw mode entered via PTS termios\n");
    Ok(EditorTty::Pts { saved_lflag })
}

pub fn restore_mode(saved: EditorTty) -> Result<()> {
    match saved {
        EditorTty::Legacy(s) => restore(s),
        EditorTty::Pts { saved_lflag } => {
            let t = libcluu::posix::termios::Termios {
                c_iflag: 0, c_oflag: 0, c_cflag: 0, c_lflag: saved_lflag,
                c_cc: [0; 20], c_ispeed: 0, c_ospeed: 0,
            };
            let rc = unsafe { libcluu::posix::termios::tcsetattr(1, 0, &t) };
            if rc != 0 {
                return Err(Error::InvalidState);
            }
            Ok(())
        }
    }
}

fn try_legacy_enter_raw(tty_endpoint: usize) -> Result<SavedTty> {
    let mut get_msg = Message::new(TTY_CTL_LABEL, [0; 6], 1);
    get_msg.words[0] = 0;
    let msg_bytes = get_msg.as_bytes();
    let mut reply_buf = [0u8; 256];
    let result = syscall::ipc_call_timeout(tty_endpoint, msg_bytes, &mut reply_buf, 500);
    let bytes = match result {
        Ok(0) => return Err(Error::Timeout),
        Ok(n) => n,
        Err(e) => return Err(e),
    };
    let (reply, _payload) = libcluu::ipc::parse_message(&reply_buf[..bytes])
        .ok_or(Error::InvalidState)?;
    let saved_lflag = reply.words[4];

    let mut set_msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    set_msg.words[0] = 1;
    set_msg.words[4] = saved_lflag & !(TTY_LFLAG_ICANON | TTY_LFLAG_ECHO);
    let set_bytes = set_msg.as_bytes();
    let mut set_reply = [0u8; 256];
    let result = syscall::ipc_call_timeout(tty_endpoint, set_bytes, &mut set_reply, 500);
    match result {
        Ok(0) | Err(Error::Timeout) => return Err(Error::Timeout),
        Ok(_) => {}
        Err(e) => return Err(e),
    }

    Ok(SavedTty {
        tty_endpoint,
        saved_lflag,
        pts_fallback: false,
    })
}
