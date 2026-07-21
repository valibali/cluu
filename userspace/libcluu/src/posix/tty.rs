use crate::error::{Error, Result};
use crate::ipc::{call, TTY_CTL_LABEL};
use crate::syscall;
use crate::types::{IpcFlags, Message};

pub const TTY_LFLAG_ICANON: usize = 0x02;
pub const TTY_LFLAG_ECHO: usize = 0x08;
pub const TTY_LFLAG_ISIG: usize = 0x01;

// PTS protocol c_lflag bits match cluu_wire::pts::Termios and sys/termios.h.
const PTS_LFLAG_ECHO: u32 = 0x04;
const PTS_LFLAG_ECHOE: u32 = 0x08;
const PTS_LFLAG_ICANON: u32 = 0x02;
const PTS_LFLAG_ISIG: u32 = 0x01;

const fn pts_raw_lflag(lflag: u32) -> u32 {
    lflag & !(PTS_LFLAG_ICANON | PTS_LFLAG_ECHO | PTS_LFLAG_ISIG)
}

const _: () = assert!(
    pts_raw_lflag(PTS_LFLAG_ISIG | PTS_LFLAG_ICANON | PTS_LFLAG_ECHO | PTS_LFLAG_ECHOE)
        == PTS_LFLAG_ECHOE
);

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

pub fn get_lflag(tty_endpoint: usize) -> Result<usize> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 1);
    msg.words[0] = 0;
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)?;
    Ok(msg.words[4])
}

pub fn set_lflag(tty_endpoint: usize, lflag: usize) -> Result<()> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    msg.words[0] = 1;
    msg.words[4] = lflag;
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)
}

#[derive(Clone, Copy)]
pub struct SavedTty {
    pub tty_endpoint: usize,
    pub saved_lflag: usize,
    pub pts_fallback: bool,
}

fn try_legacy_enter_raw(tty_endpoint: usize) -> Result<usize> {
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
    let (reply, _payload) = crate::ipc::parse_message(&reply_buf[..bytes])
        .ok_or(Error::InvalidState)?;
    let saved_lflag = reply.words[4];

    let mut set_msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    set_msg.words[0] = 1;
    set_msg.words[4] = saved_lflag & !(TTY_LFLAG_ICANON | TTY_LFLAG_ECHO | TTY_LFLAG_ISIG);
    let set_bytes = set_msg.as_bytes();
    let mut set_reply = [0u8; 256];
    let result = syscall::ipc_call_timeout(tty_endpoint, set_bytes, &mut set_reply, 500);
    match result {
        Ok(0) | Err(Error::Timeout) => return Err(Error::Timeout),
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    Ok(saved_lflag)
}

fn try_pts_enter_raw() -> Result<usize> {
    extern "C" {
        fn tcgetattr(fd: core::ffi::c_int, t: *mut core::ffi::c_void) -> core::ffi::c_int;
        fn tcsetattr(fd: core::ffi::c_int, act: core::ffi::c_int, t: *const core::ffi::c_void) -> core::ffi::c_int;
    }
    #[repr(C)]
    struct Termios {
        c_iflag: u32, c_oflag: u32, c_cflag: u32, c_lflag: u32,
        c_cc: [u8; 20], c_ispeed: u32, c_ospeed: u32,
    }
    let mut t = Termios {
        c_iflag: 0, c_oflag: 0, c_cflag: 0, c_lflag: 0,
        c_cc: [0; 20], c_ispeed: 0, c_ospeed: 0,
    };
    if unsafe { tcgetattr(1, &mut t as *mut _ as *mut core::ffi::c_void) } != 0 {
        return Err(Error::InvalidState);
    }
    let saved = t.c_lflag;
    t.c_lflag = pts_raw_lflag(t.c_lflag);
    if unsafe { tcsetattr(1, 0, &t as *const _ as *const core::ffi::c_void) } != 0 {
        return Err(Error::InvalidState);
    }
    Ok(saved as usize)
}

pub fn enter_raw(tty_endpoint: usize) -> Result<SavedTty> {
    match try_legacy_enter_raw(tty_endpoint) {
        Ok(saved_lflag) => {
            Ok(SavedTty { tty_endpoint, saved_lflag, pts_fallback: false })
        }
        Err(_) => {
            let saved_lflag = try_pts_enter_raw()?;
            Ok(SavedTty { tty_endpoint, saved_lflag, pts_fallback: true })
        }
    }
}

pub fn restore(saved: SavedTty) -> Result<()> {
    if saved.pts_fallback {
        extern "C" {
            fn tcgetattr(fd: core::ffi::c_int, t: *mut core::ffi::c_void) -> core::ffi::c_int;
            fn tcsetattr(fd: core::ffi::c_int, act: core::ffi::c_int, t: *const core::ffi::c_void) -> core::ffi::c_int;
        }
        #[repr(C)]
        struct Termios {
            c_iflag: u32, c_oflag: u32, c_cflag: u32, c_lflag: u32,
            c_cc: [u8; 20], c_ispeed: u32, c_ospeed: u32,
        }
        let mut t = Termios {
            c_iflag: 0, c_oflag: 0, c_cflag: 0, c_lflag: 0,
            c_cc: [0; 20], c_ispeed: 0, c_ospeed: 0,
        };
        if unsafe { tcgetattr(1, &mut t as *mut _ as *mut core::ffi::c_void) } != 0 {
            return Err(Error::InvalidState);
        }
        t.c_lflag = saved.saved_lflag as u32;
        if unsafe { tcsetattr(1, 0, &t as *const _ as *const core::ffi::c_void) } != 0 {
            return Err(Error::InvalidState);
        }
        Ok(())
    } else {
        set_lflag(saved.tty_endpoint, saved.saved_lflag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_raw_lflag_clears_protocol_echo_without_clearing_echoe() {
        // Given: PTS defaults with all raw-mode flags and ECHOE enabled.
        let lflag = PTS_LFLAG_ISIG | PTS_LFLAG_ICANON | PTS_LFLAG_ECHO | PTS_LFLAG_ECHOE;

        // When: raw mode is entered through the PTS fallback.
        let raw_lflag = pts_raw_lflag(lflag);

        // Then: protocol ECHO, ISIG, and ICANON clear while ECHOE remains.
        assert_eq!(raw_lflag, PTS_LFLAG_ECHOE);
    }
}
