//! PTS (pseudo-terminal) protocol types — see spec 2.
//!
//! Both `userspace/tty/` (text-VT service) and `userspace/cluuterm/`
//! (graphical terminal) speak this verb set. Shell uses one verb set
//! regardless of which it talks to.

use alloc::string::String;
use alloc::vec::Vec;
use bitflags::bitflags;
use serde::{Deserialize, Serialize};

// ----- Verb labels -----

pub const PTS_READ_LABEL: u32 = 130;
pub const PTS_WRITE_LABEL: u32 = 131;
pub const PTS_POLL_LABEL: u32 = 132;
pub const PTS_GET_TERMIOS_LABEL: u32 = 133;
pub const PTS_SET_TERMIOS_LABEL: u32 = 134;
pub const PTS_GET_WINSIZE_LABEL: u32 = 135;
pub const PTS_SET_WINSIZE_LABEL: u32 = 136;
pub const PTS_GET_PGRP_LABEL: u32 = 137;
pub const PTS_SET_PGRP_LABEL: u32 = 138;
pub const PTS_FLUSH_LABEL: u32 = 139;
pub const PTS_CLOSED_LABEL: u32 = 140;

// ----- Termios -----

pub const NCCS: usize = 20;

#[repr(C)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_cc: [u8; NCCS],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

impl Termios {
    /// Default termios for a fresh pts. Spec 2 §7 "default termios".
    pub const fn default_pts() -> Self {
        let mut c_cc = [0u8; NCCS];
        c_cc[Self::VEOF] = 0x04; // Ctrl-D
        c_cc[Self::VEOL] = 0x00;
        c_cc[Self::VERASE] = 0x7f; // DEL
        c_cc[Self::VINTR] = 0x03; // Ctrl-C
        c_cc[Self::VKILL] = 0x15; // Ctrl-U
        c_cc[Self::VMIN] = 0x01;
        c_cc[Self::VQUIT] = 0x1c; // Ctrl-\
        c_cc[Self::VSTART] = 0x11; // Ctrl-Q
        c_cc[Self::VSTOP] = 0x13; // Ctrl-S
        c_cc[Self::VSUSP] = 0x1a; // Ctrl-Z
        c_cc[Self::VTIME] = 0x00;
        c_cc[Self::VWERASE] = 0x17; // Ctrl-W
        Self {
            c_iflag: Self::ICRNL | Self::BRKINT,
            c_oflag: Self::OPOST | Self::ONLCR,
            c_cflag: Self::CREAD | Self::CLOCAL,
            c_lflag: Self::ISIG
                | Self::ICANON
                | Self::ECHO
                | Self::ECHOE
                | Self::ECHOK
                | Self::ECHOCTL
                | Self::IEXTEN,
            c_cc,
            c_ispeed: 38400,
            c_ospeed: 38400,
        }
    }

    // c_iflag bits
    pub const IGNBRK: u32 = 0x0001;
    pub const BRKINT: u32 = 0x0002;
    pub const ICRNL: u32 = 0x0004;
    pub const INLCR: u32 = 0x0008;
    pub const IXON: u32 = 0x0010;
    pub const IXOFF: u32 = 0x0020;

    // c_oflag bits
    pub const OPOST: u32 = 0x0001;
    pub const ONLCR: u32 = 0x0002;

    // c_cflag bits
    pub const CREAD: u32 = 0x0001;
    pub const HUPCL: u32 = 0x0002;
    pub const CLOCAL: u32 = 0x0004;

    // c_lflag bits
    pub const ISIG: u32 = 0x0001;
    pub const ICANON: u32 = 0x0002;
    pub const ECHO: u32 = 0x0004;
    pub const ECHOE: u32 = 0x0008;
    pub const ECHOK: u32 = 0x0010;
    pub const ECHONL: u32 = 0x0020;
    pub const NOFLSH: u32 = 0x0040;
    pub const TOSTOP: u32 = 0x0080;
    pub const ECHOCTL: u32 = 0x0100;
    pub const ECHOPRT: u32 = 0x0200;
    pub const ECHOKE: u32 = 0x0400;
    pub const IEXTEN: u32 = 0x0800;

    // c_cc[] indices
    pub const VEOF: usize = 0;
    pub const VEOL: usize = 1;
    pub const VERASE: usize = 2;
    pub const VINTR: usize = 3;
    pub const VKILL: usize = 4;
    pub const VMIN: usize = 5;
    pub const VQUIT: usize = 6;
    pub const VSTART: usize = 7;
    pub const VSTOP: usize = 8;
    pub const VSUSP: usize = 9;
    pub const VTIME: usize = 10;
    pub const VWERASE: usize = 11;
}

// ----- Winsize -----

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

// ----- Poll -----

bitflags! {
    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub struct PollEvents: u32 {
        const POLLIN  = 0x1;
        const POLLOUT = 0x2;
        const POLLHUP = 0x4;
        const POLLERR = 0x8;
    }
}

// ----- Requests/replies/events -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadRequest {
    pub max_bytes: u32,
}

pub type ReadReply = Result<Vec<u8>, PtsErr>;

pub type WriteRequest = Vec<u8>;
pub type WriteReply = Result<u32, PtsErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PollRequest {
    pub events: PollEvents,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PollReply {
    pub ready: PollEvents,
}

pub type GetTermiosReply = Termios;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum When {
    Now,
    Drain,
    Flush,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetTermiosRequest {
    pub when: When,
    pub termios: Termios,
}

pub type SetTermiosReply = Result<(), PtsErr>;

pub type GetWinsizeReply = Winsize;
pub type SetWinsizeReply = Result<(), PtsErr>;

pub type GetPgrpReply = i32;
pub type SetPgrpRequest = i32;
pub type SetPgrpReply = Result<(), PtsErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FlushQueue {
    Input,
    Output,
    Both,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlushRequest {
    pub queue: FlushQueue,
}

pub type FlushReply = Result<(), PtsErr>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PtsErr {
    Eagain,
    Eintr,
    Eio,
    Eperm,
    EinvalTermios,
    Internal(u32),
}

// ----- VFS PTS registration -----

/// Verb label for registering a pseudo-terminal slave in a session-aware manner.
/// Separate from the legacy `PTS_REGISTER_LABEL` (0x70); this label carries a
/// `session_id` field for per-session /dev/pts/ overlay.
pub const VFS_REGISTER_PTS_LABEL: u32 = 141;

/// Reverse delivery label: cluuterm → VFS.
///
/// Sent by cluuterm in response to a `PTS_READ_LABEL` drain-hint from VFS.
/// Wire layout (fire-and-forget, no reply slot):
///   `words[0]` = payload_len (overwritten by `send_msg_with_payload`)
///   `words[1]` = pts_id
///
/// Payload = raw cooked bytes to deliver to the blocked shell read().
/// VFS pops the `ParkedRead` for `pts_id` from `pending_pts_reads`, grants
/// the payload into the shell's target buffer, and replies the parked
/// `reply_token` to unblock the shell.
pub const PTS_READ_DELIVER_LABEL: u32 = 142;

/// Request: register a new `/dev/pts/<id>` under the given session.
///
/// `session_id` = `None` for sessionless callers (text-VT tty service).
/// These entries land in the global namespace and are visible to all sessions.
///
/// `session_id` = `Some(sid)` for graphical (cluuterm) sessions; the pts is
/// only visible within that session's derived /dev/pts/ overlay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VfsRegisterPtsRequest {
    pub session_id: Option<u32>,
    pub pts_endpoint: u64,
    pub suggested_id: Option<u32>,
}

/// Reply for a successful PTS registration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VfsRegisterPtsReply {
    pub assigned_id: u32,
}

// ----- Shell completion -----

/// Verb label for TAB completion queries from cluuterm to the shell.
///
/// Sent by cluuterm when the user presses TAB at the shell prompt. The
/// shell inspects `CompleteRequest::word` and `consecutive_tabs` and
/// replies with a `CompleteReply`. See spec
/// `docs/superpowers/specs/2026-07-01-tab-completion-protocol-design.md` §4.
pub const SHELL_COMPLETE_QUERY_LABEL: u32 = 143;

/// Request: cluuterm → shell, asking for completions of `word`.
///
/// `consecutive_tabs` counts consecutive TAB presses at the same cursor
/// position (1 = first TAB, 2 = second TAB that may trigger list-all
/// behavior, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub word: String,
    pub consecutive_tabs: u8,
}

/// Reply: shell → cluuterm, carrying the completion candidates.
///
/// `common_prefix` is the longest shared prefix of `candidates` (empty if
/// there is none or no candidates). cluuterm may use it to extend the
/// current word before deciding to display the full list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompleteReply {
    pub candidates: Vec<String>,
    pub common_prefix: String,
}

/// Verb label for shell→cluuterm announcement of the shell's completion
/// endpoint token.
///
/// Sent on shell startup and re-sent on every prompt redraw so cluuterm
/// always queries the currently active shell (handles nested shells).
/// Replaces the previous `shell:completion:<sid>` registry registration,
/// which collided between nested shells in the same session.
///
/// Wire format: `words[0]` = completion endpoint token. No payload.
pub const SHELL_COMPLETION_ANNOUNCE_LABEL: u32 = 144;

// ----- Tests -----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termios_default_has_icanon_echo() {
        let t = Termios::default_pts();
        assert!(t.c_lflag & Termios::ICANON != 0);
        assert!(t.c_lflag & Termios::ECHO != 0);
        assert!(t.c_lflag & Termios::ISIG != 0);
        assert_eq!(t.c_cc[Termios::VINTR], 0x03);
        assert_eq!(t.c_cc[Termios::VEOF], 0x04);
    }

    #[test]
    fn termios_roundtrip() {
        let t = Termios::default_pts();
        let bytes = postcard::to_allocvec(&t).expect("ser");
        let decoded: Termios = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.c_iflag, t.c_iflag);
        assert_eq!(decoded.c_cc, t.c_cc);
    }

    #[test]
    fn pts_err_roundtrip() {
        let err = PtsErr::EinvalTermios;
        let bytes = postcard::to_allocvec(&err).expect("ser");
        let decoded: PtsErr = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded, err);
    }

    #[test]
    fn winsize_roundtrip() {
        let ws = Winsize {
            rows: 24,
            cols: 80,
            xpixel: 640,
            ypixel: 480,
        };
        let bytes = postcard::to_allocvec(&ws).expect("ser");
        let decoded: Winsize = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded, ws);
    }

    #[test]
    fn read_request_roundtrip() {
        let r = ReadRequest { max_bytes: 4096 };
        let bytes = postcard::to_allocvec(&r).expect("ser");
        let decoded: ReadRequest = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.max_bytes, 4096);
    }
}