//! termios stubs for terminal mode control.
//!
//! Implements `tcgetattr` and `tcsetattr` by sending TTY_CTL messages
//! to the TTY service endpoint.

use super::c_int;
use crate::errno::{set_errno, EBADF, EINVAL, ENOSYS};
use crate::ipc::TTY_CTL_LABEL;
use crate::posix::file::get_tty_endpoint;
use crate::types::Message;

/// C-compatible termios flag type.
type tcflag_t = u32;
type cc_t = u8;
type speed_t = u32;

const NCCS: usize = 32;

/// C-compatible termios struct (matches sys/termios.h layout).
#[repr(C)]
pub struct Termios {
    pub c_iflag: tcflag_t,
    pub c_oflag: tcflag_t,
    pub c_cflag: tcflag_t,
    pub c_lflag: tcflag_t,
    pub c_cc: [cc_t; NCCS],
    pub c_ispeed: speed_t,
    pub c_ospeed: speed_t,
}

/// Get terminal attributes.
///
/// Sends TTY_CTL subcmd=0 to the TTY endpoint and populates the termios struct.
#[no_mangle]
pub extern "C" fn tcgetattr(_fd: c_int, termios_p: *mut Termios) -> c_int {
    if termios_p.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let tty_ep = match get_tty_endpoint() {
        Some(ep) => ep,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    // subcmd 0 = getattr
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 1);
    msg.words[0] = 0; // subcmd = getattr

    let mut reply_buf = [0u8; 128];
    match crate::syscall::ipc_call(tty_ep, msg.as_bytes(), &mut reply_buf) {
        Ok(bytes) => {
            if bytes < core::mem::size_of::<Message>() {
                set_errno(ENOSYS);
                return -1;
            }
            let reply_msg =
                unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
            let lflag = reply_msg.words[4] as tcflag_t;

            let t = unsafe { &mut *termios_p };
            // Zero-initialize, then set known flags
            t.c_iflag = 0;
            t.c_oflag = 0;
            t.c_cflag = 0;
            t.c_lflag = lflag;
            t.c_cc = [0; NCCS];
            t.c_ispeed = 0;
            t.c_ospeed = 0;
            0
        }
        Err(e) => {
            set_errno(crate::errno::from_cluu_error(e));
            -1
        }
    }
}

/// Set terminal attributes.
///
/// Sends TTY_CTL subcmd=1 with the lflag bits to the TTY endpoint.
#[no_mangle]
pub extern "C" fn tcsetattr(
    _fd: c_int,
    _optional_actions: c_int,
    termios_p: *const Termios,
) -> c_int {
    if termios_p.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let tty_ep = match get_tty_endpoint() {
        Some(ep) => ep,
        None => {
            set_errno(EBADF);
            return -1;
        }
    };

    let t = unsafe { &*termios_p };

    // subcmd 1 = setattr, words[4] = lflag
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    msg.words[0] = 1; // subcmd = setattr
    msg.words[4] = t.c_lflag as usize;

    let mut reply_buf = [0u8; 128];
    match crate::syscall::ipc_call(tty_ep, msg.as_bytes(), &mut reply_buf) {
        Ok(_) => 0,
        Err(e) => {
            set_errno(crate::errno::from_cluu_error(e));
            -1
        }
    }
}
