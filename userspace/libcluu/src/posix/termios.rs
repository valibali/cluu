//! POSIX termios + ioctl(TIOC*) shims that translate to PTS_* verbs.
//!
//! Called from newlib's libc; takes raw C-shaped pointers and returns C-shaped
//! results.  Per-fd: looks up the fd's endpoint via `FD_TABLE`, issues the
//! corresponding PTS_* IPC via postcard payload, and translates `PtsErr` to
//! errno.
//!
//! Protocol notes:
//!   - Verb labels come from `cluu_proto::pts` (100-110), NOT the legacy
//!     `PTS_IOCTL_LABEL` (0x74) in `crate::ipc`.
//!   - Requests/replies are serialized via postcard, wrapped in a `Message`
//!     header, and sent via raw `syscall::ipc_call`.

use super::{c_int, c_ulong, c_void};
use crate::errno::{set_errno, EAGAIN, EINTR, EINVAL, EIO, EPERM};
use crate::types::Message;
use alloc::vec::Vec;

// ── C-compatible structs (mirror cluu_proto::pts but repr(C) for C ABI) ───

type tcflag_t = u32;
type cc_t = u8;

const NCCS: usize = 20;

/// C-compatible termios struct (matches sys/termios.h layout).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: tcflag_t,
    pub c_oflag: tcflag_t,
    pub c_cflag: tcflag_t,
    pub c_lflag: tcflag_t,
    pub c_cc: [cc_t; NCCS],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

/// C-compatible winsize struct (sys/ioctl.h).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WinSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

// ── ioctl constants (matching Linux/newlib <sys/ioctl.h>) ─────────────────

const TIOCGWINSZ: c_ulong = 0x5413;
const TIOCSWINSZ: c_ulong = 0x5414;
const TIOCGPGRP: c_ulong = 0x540F;
const TIOCSPGRP: c_ulong = 0x5410;

// ── Termios ↔ cluu_proto::pts::Termios conversion ─────────────────────────

fn to_proto_termios(t: &Termios) -> crate::proto::pts::Termios {
    crate::proto::pts::Termios {
        c_iflag: t.c_iflag,
        c_oflag: t.c_oflag,
        c_cflag: t.c_cflag,
        c_lflag: t.c_lflag,
        c_cc: t.c_cc,
        c_ispeed: t.c_ispeed,
        c_ospeed: t.c_ospeed,
    }
}

fn from_proto_termios(p: &crate::proto::pts::Termios) -> Termios {
    Termios {
        c_iflag: p.c_iflag,
        c_oflag: p.c_oflag,
        c_cflag: p.c_cflag,
        c_lflag: p.c_lflag,
        c_cc: p.c_cc,
        c_ispeed: p.c_ispeed,
        c_ospeed: p.c_ospeed,
    }
}

// ── PtsErr → errno ────────────────────────────────────────────────────────

fn translate_err(e: &crate::proto::pts::PtsErr) -> i32 {
    match e {
        crate::proto::pts::PtsErr::Eagain => EAGAIN,
        crate::proto::pts::PtsErr::Eintr => EINTR,
        crate::proto::pts::PtsErr::Eio => EIO,
        crate::proto::pts::PtsErr::Eperm => EPERM,
        crate::proto::pts::PtsErr::EinvalTermios => EINVAL,
        crate::proto::pts::PtsErr::Internal(_) => EIO,
    }
}

// ── IPC helpers ───────────────────────────────────────────────────────────

/// Look up the IPC endpoint for a file descriptor.
fn endpoint_for_fd(fd: c_int) -> Result<usize, i32> {
    let table = crate::fd_table::FD_TABLE.lock();
    let ep = table.get(fd).map(|e| e.endpoint).unwrap_or(0);
    if ep == 0 {
        set_errno(EIO);
        Err(EIO)
    } else {
        Ok(ep)
    }
}

/// Send a PTS_* request (serialized via postcard) and receive the reply
/// payload as raw bytes (postcard-encoded).
///
/// Returns the reply payload bytes (past the 72-byte Message header).
fn pts_call_raw(label: u32, endpoint: usize, request_payload: &[u8]) -> Result<Vec<u8>, i32> {
    let hdr_len = core::mem::size_of::<Message>();

    // Build send buffer: Message header + payload.
    let total_send = hdr_len + request_payload.len();
    let mut send_buf = Vec::with_capacity(total_send);
    send_buf.resize(hdr_len, 0u8);
    {
        let mut hdr = Message::new(label, [0; 6], 1);
        hdr.words[0] = crate::proto::ABI_VERSION as usize;
        send_buf[..hdr_len].copy_from_slice(hdr.as_bytes());
    }
    send_buf.extend_from_slice(request_payload);

    // Reply buffer: header (72 bytes) + max payload.  Postcard-encoded
    // Termios is ~80 bytes; the largest reply is a Vec<u8> ReadReply which
    // could be larger.  4096 is safe for all current PTS_* replies.
    let mut reply_buf = [0u8; 4096];
    loop {
        match crate::syscall::ipc_call(endpoint, &send_buf, &mut reply_buf) {
            Ok(reply_len) => {
                if reply_len < hdr_len {
                    set_errno(EIO);
                    return Err(EIO);
                }
                let payload_len = reply_len - hdr_len;
                return Ok(reply_buf[hdr_len..hdr_len + payload_len].to_vec());
            }
            Err(crate::Error::WouldBlock) => {
                let _ = crate::syscall::yield_cpu();
            }
            Err(e) => {
                let eno = crate::errno::from_cluu_error(e);
                set_errno(eno);
                return Err(eno);
            }
        }
    }
}

/// Encode a request, send it, and decode the reply.
///
/// Returns `Ok(decoded_reply)` on success, or sets errno and returns `Err(errno)`.
macro_rules! pts_call {
    ($fd:expr, $label:expr, $req:expr, $rep_ty:ty) => {{
        let ep = endpoint_for_fd($fd)?;
        let payload = postcard::to_allocvec(&$req).map_err(|_| {
            set_errno(EINVAL);
            EINVAL
        })?;
        let reply_bytes = pts_call_raw($label, ep, &payload)?;
        let reply: $rep_ty = postcard::from_bytes(&reply_bytes).map_err(|_| {
            set_errno(EIO);
            EIO
        })?;
        Ok(reply)
    }};
}

// ── Public shims (C ABI) ──────────────────────────────────────────────────

/// Get terminal attributes via PTS_GET_TERMIOS_LABEL.
#[no_mangle]
pub extern "C" fn tcgetattr(fd: c_int, out: *mut Termios) -> c_int {
    if out.is_null() {
        set_errno(EINVAL);
        return -1;
    }
    let req: () = ();
    let label = crate::proto::pts::PTS_GET_TERMIOS_LABEL;
    match pts_call!(fd, label, req, crate::proto::pts::Termios) {
        Ok(t) => {
            unsafe { *out = from_proto_termios(&t); }
            0
        }
        Err(_eno) => -1,
    }
}

/// Set terminal attributes via PTS_SET_TERMIOS_LABEL.
///
/// `when`: 0 = TCSANOW, 1 = TCSADRAIN, 2 = TCSAFLUSH (matches `When` enum
/// discriminants in `cluu_proto::pts`).
#[no_mangle]
pub extern "C" fn tcsetattr(fd: c_int, when: c_int, t_in: *const Termios) -> c_int {
    if t_in.is_null() {
        set_errno(EINVAL);
        return -1;
    }
    let when_e = match when {
        0 => crate::proto::pts::When::Now,
        1 => crate::proto::pts::When::Drain,
        2 => crate::proto::pts::When::Flush,
        _ => {
            set_errno(EINVAL);
            return -1;
        }
    };
    let t = unsafe { &*t_in };
    let req = crate::proto::pts::SetTermiosRequest {
        when: when_e,
        termios: to_proto_termios(t),
    };
    let label = crate::proto::pts::PTS_SET_TERMIOS_LABEL;
    match pts_call!(fd, label, req, crate::proto::pts::SetTermiosReply) {
        Ok(Ok(())) => 0,
        Ok(Err(ref e)) => {
            set_errno(translate_err(e));
            -1
        }
        Err(_eno) => -1,
    }
}

/// Flush terminal queues via PTS_FLUSH_LABEL.
///
/// `queue`: 0 = TCIFLUSH, 1 = TCOFLUSH, 2 = TCIOFLUSH.
#[no_mangle]
pub extern "C" fn tcflush(fd: c_int, queue: c_int) -> c_int {
    let q = match queue {
        0 => crate::proto::pts::FlushQueue::Input,
        1 => crate::proto::pts::FlushQueue::Output,
        2 => crate::proto::pts::FlushQueue::Both,
        _ => {
            set_errno(EINVAL);
            return -1;
        }
    };
    let req = crate::proto::pts::FlushRequest { queue: q };
    let label = crate::proto::pts::PTS_FLUSH_LABEL;
    match pts_call!(fd, label, req, crate::proto::pts::FlushReply) {
        Ok(Ok(())) => 0,
        Ok(Err(ref e)) => {
            set_errno(translate_err(e));
            -1
        }
        Err(_eno) => -1,
    }
}

/// ioctl stub for terminal devices.
///
/// Supported requests:
///   - `TIOCGWINSZ`  → PTS_GET_WINSIZE_LABEL
///   - `TIOCSWINSZ`  → PTS_SET_WINSIZE_LABEL
///   - `TIOCGPGRP`   → PTS_GET_PGRP_LABEL
///   - `TIOCSPGRP`   → PTS_SET_PGRP_LABEL
///
/// Unknown requests return -1 / EINVAL (callers should not use ioctl on
/// non-terminal fds).
#[no_mangle]
pub unsafe extern "C" fn _ioctl(fd: c_int, request: c_ulong, argp: *mut c_void) -> c_int {
    match request {
        TIOCGWINSZ => {
            if argp.is_null() {
                set_errno(EINVAL);
                return -1;
            }
            let req: () = ();
            let label = crate::proto::pts::PTS_GET_WINSIZE_LABEL;
            match pts_call!(fd, label, req, crate::proto::pts::Winsize) {
                Ok(ws) => {
                    let out = &mut *(argp as *mut WinSize);
                    out.ws_row = ws.rows;
                    out.ws_col = ws.cols;
                    out.ws_xpixel = ws.xpixel;
                    out.ws_ypixel = ws.ypixel;
                    0
                }
                Err(_eno) => -1,
            }
        }
        TIOCSWINSZ => {
            if argp.is_null() {
                set_errno(EINVAL);
                return -1;
            }
            let ws_in = &*(argp as *const WinSize);
            let ws = crate::proto::pts::Winsize {
                rows: ws_in.ws_row,
                cols: ws_in.ws_col,
                xpixel: ws_in.ws_xpixel,
                ypixel: ws_in.ws_ypixel,
            };
            let label = crate::proto::pts::PTS_SET_WINSIZE_LABEL;
            match pts_call!(fd, label, ws, crate::proto::pts::SetWinsizeReply) {
                Ok(Ok(())) => 0,
                Ok(Err(ref e)) => {
                    set_errno(translate_err(e));
                    -1
                }
                Err(_eno) => -1,
            }
        }
        TIOCGPGRP => {
            if argp.is_null() {
                set_errno(EINVAL);
                return -1;
            }
            let req: () = ();
            let label = crate::proto::pts::PTS_GET_PGRP_LABEL;
            match pts_call!(fd, label, req, i32) {
                Ok(pgid) => {
                    *(argp as *mut i32) = pgid;
                    0
                }
                Err(_eno) => -1,
            }
        }
        TIOCSPGRP => {
            if argp.is_null() {
                set_errno(EINVAL);
                return -1;
            }
            let pgid = *(argp as *const i32);
            let label = crate::proto::pts::PTS_SET_PGRP_LABEL;
            match pts_call!(fd, label, pgid, crate::proto::pts::SetPgrpReply) {
                Ok(Ok(())) => 0,
                Ok(Err(ref e)) => {
                    set_errno(translate_err(e));
                    -1
                }
                Err(_eno) => -1,
            }
        }
        _ => {
            set_errno(EINVAL);
            -1
        }
    }
}

/// C-callable ioctl entry point (matches newlib <sys/ioctl.h>).
#[no_mangle]
pub extern "C" fn ioctl(fd: c_int, request: c_ulong, argp: *mut c_void) -> c_int {
    _ioctl(fd, request, argp)
}