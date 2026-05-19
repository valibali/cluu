# Terminal + PTY Unification Implementation Plan

> **For agentic workers:** Self-contained for handoff (target: deepseek v4 pro). Each step: exact paths + complete code + verification commands. TDD where applicable. Frequent commits. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Unify the legacy TTY service (`userspace/tty/`) and `userspace/cluuterm/` onto one PTS_* verb set (labels 100-110). Shared line-discipline library in `userspace/libcluu/src/tty_core/line_discipline.rs`. Full POSIX terminal signal coverage (SIGINT, SIGTSTP, SIGQUIT, SIGWINCH, SIGTTIN, SIGTTOU). Per-session `/dev/pts/` namespace overlay. POSIX `tcgetattr`/`tcsetattr`/`tcflush`/`ioctl(TIOC*)` shims. TERM env propagation.

**Architecture:** Both services link `libcluu::tty_core` and own per-pts state in-process. Each pts has its own `LineDiscipline` instance. Service input-handler: feed_byte → `LineDiscOutput` → signal/echo/bytes. Service-shared signal routing via existing `PROCMGR_PG_SIGNAL`. PTS verbs serialized via postcard (same pattern as spec 1). VFS gains per-session `/dev/pts/` overlay keyed on session_id.

**Tech Stack:** Rust 2021, postcard 1.x (added in plan 1), bitflags 2.4, existing CLUU IPC primitives. `cluu_proto` crate established by plan 1.

**Reference spec:** `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`.

**Prerequisites:** Plan 1 tasks 1-10 complete (cluu_proto exists, libcluu re-exports it, procmgr::spawn() landed). Plan 2 can land in parallel with plan 1 tasks 11-20.

---

## File Structure

### New files

- `userspace/cluu_proto/src/pts.rs` — `PTS_*` label constants + request/reply/event types + `Termios` + `Winsize` + `PtsErr` + `PollEvents` + `FlushQueue` + `When`.

### Modified files (in order of first touch)

- `userspace/cluu_proto/src/lib.rs` — add `pts` module.
- `userspace/libcluu/src/tty_core/line_discipline.rs` — expand to `LineDiscOutput` API.
- `userspace/libcluu/src/tty_core/mod.rs` — re-export new line-discipline surface; add service-shared routing helper.
- `userspace/libcluu/src/posix/termios.rs` (NEW) — `tcgetattr`/`tcsetattr`/`tcflush`/`ioctl(TIOC*)` newlib shims.
- `userspace/cluuterm/src/tty_backend.rs` — implement all 11 PTS_* verbs.
- `userspace/cluuterm/src/main.rs` — wire compositor `WIN_CONFIGURE` → recompute winsize → SIGWINCH.
- `userspace/tty/src/main.rs` — replace TTY_* dispatch with PTS_* verb set.
- `userspace/tty/src/protocol.rs` — delete legacy `TTY_*` const definitions.
- `userspace/shell/src/...` — drop `tty_endpoint != 0` branch (commit `9ac4b12`).
- `userspace/vfs/src/main.rs` + `userspace/vfs/src/pts.rs` + `userspace/vfs/src/view.rs` — per-session `/dev/pts/` overlay.
- `userspace/vfs/src/main.rs` — `VFS_REGISTER_PTS_LABEL` accepts `session_id`.

### Test files

- `userspace/libcluu/src/tty_core/line_discipline.rs` — inline `#[cfg(test)]` module with unit tests.
- New probes for spec 2 acceptance markers (see Task 13).

---

## Build / verify commands cheat sheet

- Full build: `cargo xtask build`.
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Single crate: `cargo build -p <crate>` (`-p libcluu`, `-p cluu_proto`, `-p cluuterm`, `-p tty`).
- Host-side line-discipline tests: `cargo test -p libcluu --features host-test line_discipline`.
- Boot smoke: `bash scripts/harness_run.sh`. Expected log: `compositor: ready`.
- Marker: `HARNESS_FORCE_BUILD=1 MARKER_MODE=<m> bash scripts/harness_run.sh; grep "<m>:" serial.log`.

---

## Task 1: `cluu_proto::pts` module — types + labels

**Goal:** All PTS protocol types + label constants in `cluu_proto`. Round-trip postcard tests.

**Files:**
- Create: `userspace/cluu_proto/src/pts.rs`
- Modify: `userspace/cluu_proto/src/lib.rs`

- [ ] **Step 1: Add `pts` module to `cluu_proto::lib.rs`**

Open `userspace/cluu_proto/src/lib.rs`. After the existing `pub mod spawn; pub mod primordial;`, add:

```rust
pub mod pts;
```

- [ ] **Step 2: Write `userspace/cluu_proto/src/pts.rs`**

```rust
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

pub const PTS_READ_LABEL:           u32 = 100;
pub const PTS_WRITE_LABEL:          u32 = 101;
pub const PTS_POLL_LABEL:           u32 = 102;
pub const PTS_GET_TERMIOS_LABEL:    u32 = 103;
pub const PTS_SET_TERMIOS_LABEL:    u32 = 104;
pub const PTS_GET_WINSIZE_LABEL:    u32 = 105;
pub const PTS_SET_WINSIZE_LABEL:    u32 = 106;
pub const PTS_GET_PGRP_LABEL:       u32 = 107;
pub const PTS_SET_PGRP_LABEL:       u32 = 108;
pub const PTS_FLUSH_LABEL:          u32 = 109;
pub const PTS_CLOSED_LABEL:         u32 = 110;

// ----- Termios -----

pub const NCCS: usize = 20;

#[repr(C)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_cc:    [u8; NCCS],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

impl Termios {
    /// Default termios for a fresh pts. Spec 2 §7 "default termios".
    pub const fn default_pts() -> Self {
        let mut c_cc = [0u8; NCCS];
        c_cc[Self::VEOF]    = 0x04;  // Ctrl-D
        c_cc[Self::VEOL]    = 0x00;
        c_cc[Self::VERASE]  = 0x7f;  // DEL
        c_cc[Self::VINTR]   = 0x03;  // Ctrl-C
        c_cc[Self::VKILL]   = 0x15;  // Ctrl-U
        c_cc[Self::VMIN]    = 0x01;
        c_cc[Self::VQUIT]   = 0x1c;  // Ctrl-\
        c_cc[Self::VSTART]  = 0x11;  // Ctrl-Q
        c_cc[Self::VSTOP]   = 0x13;  // Ctrl-S
        c_cc[Self::VSUSP]   = 0x1a;  // Ctrl-Z
        c_cc[Self::VTIME]   = 0x00;
        c_cc[Self::VWERASE] = 0x17;  // Ctrl-W
        Self {
            c_iflag: Self::ICRNL | Self::BRKINT,
            c_oflag: Self::OPOST | Self::ONLCR,
            c_cflag: Self::CREAD | Self::CLOCAL,
            c_lflag: Self::ISIG | Self::ICANON | Self::ECHO
                   | Self::ECHOE | Self::ECHOK | Self::ECHOCTL | Self::IEXTEN,
            c_cc,
            c_ispeed: 38400,
            c_ospeed: 38400,
        }
    }

    // c_iflag bits
    pub const IGNBRK: u32 = 0x0001;
    pub const BRKINT: u32 = 0x0002;
    pub const ICRNL:  u32 = 0x0004;
    pub const INLCR:  u32 = 0x0008;
    pub const IXON:   u32 = 0x0010;
    pub const IXOFF:  u32 = 0x0020;

    // c_oflag bits
    pub const OPOST:  u32 = 0x0001;
    pub const ONLCR:  u32 = 0x0002;

    // c_cflag bits
    pub const CREAD:  u32 = 0x0001;
    pub const HUPCL:  u32 = 0x0002;
    pub const CLOCAL: u32 = 0x0004;

    // c_lflag bits
    pub const ISIG:    u32 = 0x0001;
    pub const ICANON:  u32 = 0x0002;
    pub const ECHO:    u32 = 0x0004;
    pub const ECHOE:   u32 = 0x0008;
    pub const ECHOK:   u32 = 0x0010;
    pub const ECHONL:  u32 = 0x0020;
    pub const NOFLSH:  u32 = 0x0040;
    pub const TOSTOP:  u32 = 0x0080;
    pub const ECHOCTL: u32 = 0x0100;
    pub const ECHOPRT: u32 = 0x0200;
    pub const ECHOKE:  u32 = 0x0400;
    pub const IEXTEN:  u32 = 0x0800;

    // c_cc[] indices
    pub const VEOF:    usize = 0;
    pub const VEOL:    usize = 1;
    pub const VERASE:  usize = 2;
    pub const VINTR:   usize = 3;
    pub const VKILL:   usize = 4;
    pub const VMIN:    usize = 5;
    pub const VQUIT:   usize = 6;
    pub const VSTART:  usize = 7;
    pub const VSTOP:   usize = 8;
    pub const VSUSP:   usize = 9;
    pub const VTIME:   usize = 10;
    pub const VWERASE: usize = 11;
}

// ----- Winsize -----

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Winsize {
    pub rows:    u16,
    pub cols:    u16,
    pub xpixel:  u16,
    pub ypixel:  u16,
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
pub struct ReadRequest { pub max_bytes: u32 }
pub type ReadReply = Result<Vec<u8>, PtsErr>;

pub type WriteRequest = Vec<u8>;
pub type WriteReply = Result<u32, PtsErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PollRequest { pub events: PollEvents }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PollReply { pub ready: PollEvents }

pub type GetTermiosReply = Termios;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum When { Now, Drain, Flush }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetTermiosRequest { pub when: When, pub termios: Termios }
pub type SetTermiosReply = Result<(), PtsErr>;

pub type GetWinsizeReply = Winsize;
pub type SetWinsizeReply = Result<(), PtsErr>;

pub type GetPgrpReply = i32;
pub type SetPgrpRequest = i32;
pub type SetPgrpReply = Result<(), PtsErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FlushQueue { Input, Output, Both }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlushRequest { pub queue: FlushQueue }
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
```

- [ ] **Step 3: Add round-trip tests**

Append:

```rust
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
        let ws = Winsize { rows: 24, cols: 80, xpixel: 640, ypixel: 480 };
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
```

- [ ] **Step 4: Build + test**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluu_proto
cargo test -p cluu_proto --features host-test
```

Expected: build clean; 5 new tests pass (+ tests from plan 1).

- [ ] **Step 5: Commit**

```bash
git add userspace/cluu_proto/src/lib.rs userspace/cluu_proto/src/pts.rs
git commit -m "feat(cluu_proto): pts module — verb labels + termios + winsize"
```

---

## Task 2: Expand `libcluu::tty_core::line_discipline`

**Goal:** Add full `LineDiscOutput` API + cooked-mode processing + OPOST handling. Pure-function tests in same file.

**Files:**
- Modify: `userspace/libcluu/src/tty_core/line_discipline.rs`

- [ ] **Step 1: Read existing line_discipline.rs**

```
cd /home/vlb2bp/git/cluu
wc -l userspace/libcluu/src/tty_core/line_discipline.rs
grep -n "pub fn\|pub struct\|pub enum" userspace/libcluu/src/tty_core/line_discipline.rs
```

Note existing public surface — many helpers may already exist (e.g., `feed_byte`, basic ICANON). The task expands but does not replace the existing API; it adds the `LineDiscOutput` enum + canonical-mode behavior.

- [ ] **Step 2: Add/extend types**

Open `userspace/libcluu/src/tty_core/line_discipline.rs`. Near the top (after existing `use` statements), add:

```rust
use cluu_proto::pts::Termios;

/// Output of feeding one input byte through line discipline.
/// Service consumes these and dispatches accordingly.
#[derive(Clone, Debug)]
pub enum LineDiscOutput {
    /// Cooked bytes to deliver to a PTS_READ caller.
    Bytes(alloc::vec::Vec<u8>),
    /// Service should call PROCMGR_PG_SIGNAL(fg_pgid, sig).
    Signal(SignalNum),
    /// Service should write these bytes back as echo.
    Echo(alloc::vec::Vec<u8>),
    /// Canonical EOF reached (VEOF / Ctrl-D). Flush pending line + signal EOF.
    Eof,
    /// Byte consumed; no externally-visible effect (e.g. mid-edit).
    Drop,
}

/// Signal numbers used by the line discipline → service translation.
/// Values match POSIX / newlib `<signal.h>`. Service routes via
/// existing PROCMGR_PG_SIGNAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalNum {
    SIGINT  = 2,
    SIGQUIT = 3,
    SIGTSTP = 20,
}

#[derive(Clone, Debug)]
pub struct LineDiscipline {
    pub termios: Termios,
    pending_line: alloc::vec::Vec<u8>,
    output_pending: alloc::vec::Vec<u8>,
    eof_seen: bool,
    last_was_cr: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermiosErr {
    InvalidVmin,
    InvalidCcc,
    Unsupported,
}
```

If the existing file already has a `LineDiscipline` struct, merge fields — do NOT introduce a second struct. The task is to extend, not duplicate.

- [ ] **Step 3: Implement the core feed_byte logic**

Add or replace the `impl LineDiscipline` block with:

```rust
impl LineDiscipline {
    pub fn new() -> Self {
        Self {
            termios: Termios::default_pts(),
            pending_line: alloc::vec::Vec::new(),
            output_pending: alloc::vec::Vec::new(),
            eof_seen: false,
            last_was_cr: false,
        }
    }

    pub fn termios(&self) -> &Termios {
        &self.termios
    }

    pub fn set_termios(&mut self, new: Termios) -> Result<(), TermiosErr> {
        // Basic sanity: in canonical mode, VMIN must be ≥ 1 OR raw mode rules.
        // For now accept any termios; reject only obvious nonsense.
        let _ = new.c_cc[Termios::VMIN]; // potential future check
        self.termios = new;
        Ok(())
    }

    /// Feed one input byte through line discipline. Returns zero or more
    /// `LineDiscOutput` events the service must handle.
    pub fn feed_byte(&mut self, byte: u8) -> alloc::vec::Vec<LineDiscOutput> {
        let mut out: alloc::vec::Vec<LineDiscOutput> = alloc::vec::Vec::new();
        let canonical = self.termios.c_lflag & Termios::ICANON != 0;
        let isig      = self.termios.c_lflag & Termios::ISIG   != 0;
        let echo      = self.termios.c_lflag & Termios::ECHO   != 0;
        let echoe     = self.termios.c_lflag & Termios::ECHOE  != 0;
        let echok     = self.termios.c_lflag & Termios::ECHOK  != 0;
        let echonl    = self.termios.c_lflag & Termios::ECHONL != 0;

        // ISIG translations always come first regardless of canonical mode.
        if isig {
            if byte == self.termios.c_cc[Termios::VINTR] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGINT));
                return out;
            }
            if byte == self.termios.c_cc[Termios::VQUIT] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGQUIT));
                return out;
            }
            if byte == self.termios.c_cc[Termios::VSUSP] {
                out.push(LineDiscOutput::Signal(SignalNum::SIGTSTP));
                return out;
            }
        }

        if !canonical {
            // Raw mode: emit byte immediately.
            out.push(LineDiscOutput::Bytes(alloc::vec![byte]));
            if echo {
                out.push(LineDiscOutput::Echo(alloc::vec![byte]));
            }
            return out;
        }

        // Canonical mode below.
        if byte == self.termios.c_cc[Termios::VEOF] {
            // EOF: flush pending_line, then signal Eof.
            if !self.pending_line.is_empty() {
                out.push(LineDiscOutput::Bytes(core::mem::take(&mut self.pending_line)));
            }
            out.push(LineDiscOutput::Eof);
            return out;
        }
        if byte == self.termios.c_cc[Termios::VERASE] {
            if self.pending_line.pop().is_some() && echoe {
                out.push(LineDiscOutput::Echo(alloc::vec![b'\x08', b' ', b'\x08']));
            }
            return out;
        }
        if byte == self.termios.c_cc[Termios::VKILL] {
            self.pending_line.clear();
            if echok {
                // Visual line clear; minimal impl: CR + clear-to-EOL.
                out.push(LineDiscOutput::Echo(alloc::vec![b'\r', 0x1b, b'[', b'K']));
            }
            return out;
        }
        if byte == self.termios.c_cc[Termios::VWERASE] {
            // Erase last word: pop trailing non-spaces then trailing spaces.
            let mut popped = false;
            while let Some(&b) = self.pending_line.last() {
                if b == b' ' { break; }
                self.pending_line.pop();
                popped = true;
            }
            while let Some(&b) = self.pending_line.last() {
                if b != b' ' { break; }
                self.pending_line.pop();
                popped = true;
            }
            if popped && echoe {
                // Minimal echo: emit "\r" + redrawn line. Simpler service-side
                // may just re-render.
                out.push(LineDiscOutput::Echo(alloc::vec![b'\r', 0x1b, b'[', b'K']));
                out.push(LineDiscOutput::Echo(self.pending_line.clone()));
            }
            return out;
        }
        if byte == b'\n' {
            self.pending_line.push(b'\n');
            let line = core::mem::take(&mut self.pending_line);
            out.push(LineDiscOutput::Bytes(line));
            if echo || echonl {
                out.push(LineDiscOutput::Echo(alloc::vec![b'\n']));
            }
            return out;
        }
        // ICRNL: translate \r to \n on input.
        if byte == b'\r' && self.termios.c_iflag & Termios::ICRNL != 0 {
            return self.feed_byte(b'\n');
        }
        // INLCR: translate \n to \r on input.
        if byte == b'\n' && self.termios.c_iflag & Termios::INLCR != 0 {
            return self.feed_byte(b'\r');
        }
        // Default: append, echo if requested.
        self.pending_line.push(byte);
        if echo {
            out.push(LineDiscOutput::Echo(alloc::vec![byte]));
        }
        out
    }

    /// Apply OPOST processing to outgoing bytes. Service calls this before
    /// rendering to the framebuffer / VT.
    pub fn process_output(&mut self, bytes: &[u8]) -> alloc::vec::Vec<u8> {
        let opost = self.termios.c_oflag & Termios::OPOST != 0;
        let onlcr = self.termios.c_oflag & Termios::ONLCR != 0;
        if !opost {
            return bytes.to_vec();
        }
        let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(bytes.len());
        for &b in bytes {
            if b == b'\n' && onlcr {
                out.push(b'\r');
                out.push(b'\n');
            } else {
                out.push(b);
            }
        }
        out
    }

    /// Flush the pending line buffer (used by tcflush(Input)).
    pub fn flush_input(&mut self) {
        self.pending_line.clear();
    }
}

impl Default for LineDiscipline {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Add pure-function tests**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_line_assembly() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'h');
        ld.feed_byte(b'i');
        let out = ld.feed_byte(b'\n');
        let bytes_emitted: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes_emitted, b"hi\n".to_vec());
    }

    #[test]
    fn test_vintr_signal_under_isig() {
        let mut ld = LineDiscipline::new();
        let out = ld.feed_byte(0x03); // Ctrl-C
        assert!(out.iter().any(|e| matches!(e, LineDiscOutput::Signal(SignalNum::SIGINT))));
    }

    #[test]
    fn test_no_signal_when_isig_clear() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !Termios::ISIG;
        let out = ld.feed_byte(0x03);
        assert!(!out.iter().any(|e| matches!(e, LineDiscOutput::Signal(_))));
    }

    #[test]
    fn test_veof_canonical() {
        let mut ld = LineDiscipline::new();
        let out = ld.feed_byte(0x04); // Ctrl-D
        assert!(out.iter().any(|e| matches!(e, LineDiscOutput::Eof)));
    }

    #[test]
    fn test_verase_with_echoe() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'a');
        let out = ld.feed_byte(0x7f); // DEL
        let echoed: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Echo(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(echoed, b"\x08 \x08".to_vec());
    }

    #[test]
    fn test_opost_nl_to_crnl() {
        let mut ld = LineDiscipline::new();
        let out = ld.process_output(b"hi\n");
        assert_eq!(out, b"hi\r\n".to_vec());
    }

    #[test]
    fn test_icrnl_translates_cr_to_nl() {
        let mut ld = LineDiscipline::new();
        ld.feed_byte(b'a');
        let out = ld.feed_byte(b'\r');
        let bytes_emitted: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes_emitted, b"a\n".to_vec());
    }

    #[test]
    fn test_raw_mode_passthrough() {
        let mut ld = LineDiscipline::new();
        ld.termios.c_lflag &= !Termios::ICANON;
        let out = ld.feed_byte(b'X');
        let bytes: alloc::vec::Vec<u8> = out.iter().filter_map(|e| match e {
            LineDiscOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        }).flatten().collect();
        assert_eq!(bytes, b"X".to_vec());
    }
}
```

- [ ] **Step 5: Build + test**

```
cd /home/vlb2bp/git/cluu
cargo build -p libcluu
cargo test -p libcluu --features host-test line_discipline
```

Expected: 8 line-discipline tests pass.

- [ ] **Step 6: Commit**

```bash
git add userspace/libcluu/src/tty_core/line_discipline.rs userspace/libcluu/src/tty_core/mod.rs
git commit -m "feat(libcluu): line discipline LineDiscOutput API + 8 tests"
```

---

## Task 3: Service-shared signal routing helper

**Goal:** A `route_input_byte` helper in `libcluu::tty_core` that both `tty/` and `cluuterm/` use to translate line-discipline output into service actions.

**Files:**
- Modify: `userspace/libcluu/src/tty_core/mod.rs`

- [ ] **Step 1: Add helper to `tty_core::mod.rs`**

Append (or create a new submodule `routing.rs`):

```rust
//! Service-side input-routing helper.
//!
//! Both `userspace/tty/` and `userspace/cluuterm/` call this to translate
//! a `LineDiscOutput` into a list of `ServiceAction`s the service then
//! executes.

use crate::tty_core::line_discipline::{LineDiscOutput, SignalNum};
use cluu_proto::pts::Termios;

/// Concrete action a PTS service must take after line-discipline processing.
#[derive(Clone, Debug)]
pub enum ServiceAction {
    /// Deliver cooked bytes to any blocked PTS_READ caller.
    DeliverBytes(alloc::vec::Vec<u8>),
    /// Send a signal to the foreground process group.
    SignalFgPgrp(SignalNum),
    /// Write echo bytes back to the rendering layer (cluuterm grid /
    /// tty framebuffer).
    Echo(alloc::vec::Vec<u8>),
    /// EOF reached; deliver EOF marker to readers.
    DeliverEof,
}

/// Translate one `LineDiscOutput` event into a `ServiceAction`.
pub fn translate_output(ev: LineDiscOutput) -> Option<ServiceAction> {
    match ev {
        LineDiscOutput::Bytes(b)   => Some(ServiceAction::DeliverBytes(b)),
        LineDiscOutput::Signal(s)  => Some(ServiceAction::SignalFgPgrp(s)),
        LineDiscOutput::Echo(b)    => Some(ServiceAction::Echo(b)),
        LineDiscOutput::Eof        => Some(ServiceAction::DeliverEof),
        LineDiscOutput::Drop       => None,
    }
}

/// Convenience: feed one byte, return a (possibly empty) action list.
pub fn route_input_byte(
    ld: &mut crate::tty_core::line_discipline::LineDiscipline,
    byte: u8,
) -> alloc::vec::Vec<ServiceAction> {
    ld.feed_byte(byte).into_iter().filter_map(translate_output).collect()
}
```

In `userspace/libcluu/src/tty_core/mod.rs`, ensure `pub mod line_discipline;` is present and add `pub mod routing;` if you put the helper in its own file, else inline.

- [ ] **Step 2: Build + commit**

```
cd /home/vlb2bp/git/cluu
cargo build -p libcluu
```

```bash
git add userspace/libcluu/src/tty_core/mod.rs userspace/libcluu/src/tty_core/routing.rs
git commit -m "feat(libcluu): tty_core routing helper for service-side dispatch"
```

---

## Task 4: Cluuterm speaks unified PTS_* verbs

**Goal:** Cluuterm's pts backend implements all 11 PTS_* verbs from `cluu_proto::pts`. Old local `PTS_*` consts (different numbers) deleted.

**Files:**
- Modify: `userspace/cluuterm/src/tty_backend.rs`
- Modify: `userspace/cluuterm/src/main.rs`

- [ ] **Step 1: Locate cluuterm's existing PTS dispatch**

```
cd /home/vlb2bp/git/cluu
grep -n "PTS_READ_LABEL\|PTS_WRITE_LABEL\|PTS_CLOSED_LABEL\|fn handle_pts\|dispatch" userspace/cluuterm/src/tty_backend.rs | head -20
```

Note where the current dispatch lives. It's likely a `match msg.tag.label { ... }` block.

- [ ] **Step 2: Replace old label imports**

At the top of `tty_backend.rs`, find the legacy `use libcluu::ipc::{PTS_READ_LABEL, PTS_WRITE_LABEL, PTS_CLOSED_LABEL};` import. Replace with:

```rust
use cluu_proto::pts::{
    PTS_READ_LABEL, PTS_WRITE_LABEL, PTS_POLL_LABEL,
    PTS_GET_TERMIOS_LABEL, PTS_SET_TERMIOS_LABEL,
    PTS_GET_WINSIZE_LABEL, PTS_SET_WINSIZE_LABEL,
    PTS_GET_PGRP_LABEL, PTS_SET_PGRP_LABEL,
    PTS_FLUSH_LABEL, PTS_CLOSED_LABEL,
    ReadRequest, ReadReply, WriteRequest, WriteReply,
    PollRequest, PollReply, PollEvents,
    GetTermiosReply, SetTermiosRequest, SetTermiosReply, When,
    GetWinsizeReply, Winsize, SetWinsizeReply,
    GetPgrpReply, SetPgrpRequest, SetPgrpReply,
    FlushRequest, FlushReply, FlushQueue,
    Termios, PtsErr,
};
use libcluu::tty_core::line_discipline::{LineDiscipline, SignalNum};
use libcluu::tty_core::routing::{ServiceAction, route_input_byte};
```

- [ ] **Step 3: Add per-pts state**

In the cluuterm pts struct (likely named `Pts` or `Cluuterm` in `tty_backend.rs`), ensure these fields:

```rust
pub struct Pts {
    pub id: u32,
    pub line_discipline: LineDiscipline,
    pub fg_pgid: Option<i32>,
    pub winsize: Winsize,
    pub pending_readers: alloc::collections::VecDeque<PendingRead>,
    pub blocked_writers: alloc::collections::VecDeque<u8>, // queued output bytes
    pub closed: bool,
    // ... existing fields ...
}

pub struct PendingRead {
    pub reply_id: u64,
    pub caller_pid: u32,
    pub caller_pgid: i32,
    pub max_bytes: u32,
}
```

Initialize `line_discipline: LineDiscipline::new()` and `winsize: Winsize { rows: 24, cols: 80, ... }` on creation.

- [ ] **Step 4: Implement each verb handler**

Inside `impl Pts` (or whatever holds the dispatch), add:

```rust
fn handle_pts_read(&mut self, req: ReadRequest, msg: &Message, caller_pid: u32, caller_pgid: i32) -> ReplyResult {
    // SIGTTIN: if caller is not in fg pgrp, signal them, return EINTR.
    if self.fg_pgid.is_some() && self.fg_pgid != Some(caller_pgid) {
        libcluu::ipc::procmgr_pg_signal(caller_pgid, SignalNum::SIGINT as u32 /* SIGTTIN */);
        return reply_err::<ReadReply>(msg.tag.reply_id, PTS_READ_LABEL, PtsErr::Eintr);
    }
    if self.closed {
        return reply_ok::<ReadReply>(msg.tag.reply_id, PTS_READ_LABEL, Err(PtsErr::Eio));
    }
    // If there are pending cooked bytes in the line discipline's last completed line,
    // serve them now. Otherwise queue the read.
    // (Exact buffering is engineer's call; this snippet assumes a `ready_bytes`
    // VecDeque on the Pts; if absent, add one.)
    if let Some(bytes) = self.try_take_cooked_bytes(req.max_bytes) {
        return reply_ok::<ReadReply>(msg.tag.reply_id, PTS_READ_LABEL, Ok(bytes));
    }
    self.pending_readers.push_back(PendingRead {
        reply_id: msg.tag.reply_id, caller_pid, caller_pgid,
        max_bytes: req.max_bytes,
    });
    ReplyResult::Pending  // service holds the reply; fires when bytes arrive
}

fn handle_pts_write(&mut self, req: WriteRequest, msg: &Message, caller_pid: u32, caller_pgid: i32) -> ReplyResult {
    let lflag = self.line_discipline.termios().c_lflag;
    if lflag & Termios::TOSTOP != 0
        && self.fg_pgid.is_some()
        && self.fg_pgid != Some(caller_pgid)
    {
        libcluu::ipc::procmgr_pg_signal(caller_pgid, SignalNum::SIGINT as u32 /* SIGTTOU */);
        return reply_err::<WriteReply>(msg.tag.reply_id, PTS_WRITE_LABEL, PtsErr::Eintr);
    }
    let cooked = self.line_discipline.process_output(&req);
    self.write_to_render(&cooked);
    reply_ok::<WriteReply>(msg.tag.reply_id, PTS_WRITE_LABEL, Ok(req.len() as u32))
}

fn handle_pts_get_termios(&mut self, msg: &Message) -> ReplyResult {
    let t = *self.line_discipline.termios();
    reply_ok::<GetTermiosReply>(msg.tag.reply_id, PTS_GET_TERMIOS_LABEL, t)
}

fn handle_pts_set_termios(&mut self, req: SetTermiosRequest, msg: &Message) -> ReplyResult {
    match self.line_discipline.set_termios(req.termios) {
        Ok(()) => reply_ok::<SetTermiosReply>(msg.tag.reply_id, PTS_SET_TERMIOS_LABEL, Ok(())),
        Err(_) => reply_ok::<SetTermiosReply>(msg.tag.reply_id, PTS_SET_TERMIOS_LABEL, Err(PtsErr::EinvalTermios)),
    }
}

fn handle_pts_get_winsize(&mut self, msg: &Message) -> ReplyResult {
    reply_ok::<GetWinsizeReply>(msg.tag.reply_id, PTS_GET_WINSIZE_LABEL, self.winsize)
}

fn handle_pts_set_winsize(&mut self, req: Winsize, msg: &Message) -> ReplyResult {
    self.winsize = req;
    if let Some(pgid) = self.fg_pgid {
        const SIGWINCH: u32 = 28;
        libcluu::ipc::procmgr_pg_signal(pgid, SIGWINCH);
    }
    reply_ok::<SetWinsizeReply>(msg.tag.reply_id, PTS_SET_WINSIZE_LABEL, Ok(()))
}

fn handle_pts_get_pgrp(&mut self, msg: &Message) -> ReplyResult {
    reply_ok::<GetPgrpReply>(msg.tag.reply_id, PTS_GET_PGRP_LABEL, self.fg_pgid.unwrap_or(0))
}

fn handle_pts_set_pgrp(&mut self, req: SetPgrpRequest, msg: &Message) -> ReplyResult {
    self.fg_pgid = Some(req);
    reply_ok::<SetPgrpReply>(msg.tag.reply_id, PTS_SET_PGRP_LABEL, Ok(()))
}

fn handle_pts_flush(&mut self, req: FlushRequest, msg: &Message) -> ReplyResult {
    match req.queue {
        FlushQueue::Input  | FlushQueue::Both => self.line_discipline.flush_input(),
        _ => {}
    }
    match req.queue {
        FlushQueue::Output | FlushQueue::Both => self.blocked_writers.clear(),
        _ => {}
    }
    reply_ok::<FlushReply>(msg.tag.reply_id, PTS_FLUSH_LABEL, Ok(()))
}

fn handle_pts_poll(&mut self, req: PollRequest, msg: &Message) -> ReplyResult {
    let mut ready = PollEvents::empty();
    if self.has_cooked_bytes() { ready |= PollEvents::POLLIN; }
    if !self.closed             { ready |= PollEvents::POLLOUT; }
    if self.closed              { ready |= PollEvents::POLLHUP; }
    reply_ok::<PollReply>(msg.tag.reply_id, PTS_POLL_LABEL, PollReply { ready })
}
```

Helpers `reply_ok` / `reply_err` are local wrappers around postcard-serialize + `send_reply`. Implement once:

```rust
fn reply_ok<R: serde::Serialize>(reply_id: u64, label: u32, value: R) -> ReplyResult {
    let bytes = postcard::to_allocvec(&value).expect("ser");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    libcluu::ipc::reply(reply_id, label, words, &bytes)
}
fn reply_err<R: serde::Serialize>(reply_id: u64, label: u32, err: cluu_proto::pts::PtsErr) -> ReplyResult
    where R: Default,
{
    // For Result-typed replies, wrap as Err(err); for non-Result replies, use reply_ok.
    let value: Result<R, PtsErr> = Err(err);
    let bytes = postcard::to_allocvec(&value).expect("ser");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    libcluu::ipc::reply(reply_id, label, words, &bytes)
}
```

Adjust to match `libcluu::ipc::reply`'s actual signature.

- [ ] **Step 5: Wire input from compositor → line_discipline → service actions**

Locate the existing `WIN_INPUT` handler (cluuterm receives input events from compositor). It feeds raw input bytes to the pts. Replace with the routing helper:

```rust
fn on_input_byte(&mut self, byte: u8) {
    for action in route_input_byte(&mut self.line_discipline, byte) {
        match action {
            ServiceAction::DeliverBytes(bytes) => {
                self.ready_bytes.extend_from_slice(&bytes);
                self.try_wake_pending_readers();
            }
            ServiceAction::SignalFgPgrp(sig) => {
                if let Some(pgid) = self.fg_pgid {
                    libcluu::ipc::procmgr_pg_signal(pgid, sig as u32);
                }
            }
            ServiceAction::Echo(bytes) => {
                let cooked = self.line_discipline.process_output(&bytes);
                self.write_to_render(&cooked);
            }
            ServiceAction::DeliverEof => {
                self.eof_pending = true;
                self.try_wake_pending_readers();
            }
        }
    }
}
```

- [ ] **Step 6: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluuterm
```

Expected: clean build. Compile errors are normal early; the engineer fixes type mismatches case by case.

- [ ] **Step 7: Boot smoke**

```
bash scripts/harness_run.sh
```

Expected: `compositor: ready`; login flow reaches shell; typing visible.

- [ ] **Step 8: Commit**

```bash
git add userspace/cluuterm/src/tty_backend.rs userspace/cluuterm/src/main.rs
git commit -m "feat(cluuterm): speak unified PTS_* verbs (labels 100-110)"
```

---

## Task 5: TTY service speaks unified PTS_*

**Goal:** `userspace/tty/src/main.rs` replaces every `TTY_*_LABEL` dispatch with the corresponding `PTS_*_LABEL` from `cluu_proto::pts`. Same line discipline library, same routing helpers. `/dev/tty1..3` registrations stay unchanged.

**Files:**
- Modify: `userspace/tty/src/main.rs`
- Modify: `userspace/tty/src/context.rs`
- Modify: `userspace/tty/src/protocol.rs`

- [ ] **Step 1: Read the existing dispatch**

```
cd /home/vlb2bp/git/cluu
grep -n "TTY_REGISTER_LABEL\|TTY_CTL_LABEL\|TTY_SET_FG_LABEL\|TTY_READ_REQUEST_LABEL\|TTY_POLL_QUERY_LABEL\|fn handle_" userspace/tty/src/main.rs | head -20
```

Map each existing handler to its PTS_* equivalent:
- `TTY_REGISTER_LABEL` → no equivalent (registration is VFS-side via `VFS_REGISTER_PTS_LABEL` in Task 9; TTY service registers its own `/dev/tty<n>` at startup, separate from per-call PTS_*).
- `TTY_CTL_LABEL` (lflag get/set) → `PTS_GET_TERMIOS_LABEL` / `PTS_SET_TERMIOS_LABEL`.
- `TTY_SET_FG_LABEL` → `PTS_SET_PGRP_LABEL`.
- `TTY_READ_REQUEST_LABEL` → `PTS_READ_LABEL`.
- `TTY_POLL_QUERY_LABEL` → `PTS_POLL_LABEL`.

- [ ] **Step 2: Replace handlers**

Mirror Task 4 — TTY service's dispatch is the same shape as cluuterm's. The handler code is essentially the same; the rendering layer differs (TTY service writes to framebuffer text mode; cluuterm writes to its grid).

For each `PTS_*` handler, the body is identical to cluuterm's Task 4 handler. Engineer copies the code (or extracts a shared `pts_service_impl!` macro / generic — left to the engineer's judgment, but copy-paste is acceptable for a first landing).

The TTY service holds an instance of `LineDiscipline` per `/dev/tty<n>` it serves (rather than per pts since text-VT has fixed VT numbers).

- [ ] **Step 3: Delete legacy TTY_* label dispatch**

In `userspace/tty/src/main.rs` and `protocol.rs`, find and delete:
- `match msg.tag.label { TTY_REGISTER_LABEL => ... }` arms.
- Const definitions of `TTY_REGISTER_LABEL` etc. in `protocol.rs`.

The TTY service no longer carries the legacy label constants; it speaks `cluu_proto::pts::*` only.

- [ ] **Step 4: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p tty
bash scripts/harness_run.sh
```

Expected: boots; text-VT (`Ctrl+Alt+F2`) works the same — shows getty prompt or shell.

- [ ] **Step 5: Commit**

```bash
git add userspace/tty/src/main.rs userspace/tty/src/context.rs userspace/tty/src/protocol.rs
git commit -m "feat(tty): speak unified PTS_* verbs; retire TTY_* labels"
```

---

## Task 6: libcluu POSIX termios shims

**Goal:** Newlib `tcgetattr`/`tcsetattr`/`tcflush`/`ioctl(TIOC*)` translate to PTS_* verbs.

**Files:**
- Create: `userspace/libcluu/src/posix/termios.rs`
- Modify: `userspace/libcluu/src/posix/mod.rs`

- [ ] **Step 1: Check existing posix module**

```
cd /home/vlb2bp/git/cluu
ls userspace/libcluu/src/posix/
grep -n "tcgetattr\|tcsetattr\|tcflush\|ioctl" userspace/libcluu/src/posix/*.rs 2>/dev/null
```

If there's a partial implementation, extend it. Otherwise create the file.

- [ ] **Step 2: Write `userspace/libcluu/src/posix/termios.rs`**

```rust
//! POSIX termios + ioctl(TIOC*) shims that translate to PTS_* verbs.
//!
//! Called from newlib's libc; takes raw C-shaped pointers and returns C-shaped
//! results. Per-fd: looks up the fd's endpoint via the VFS fd table; issues
//! the corresponding PTS_* IPC; translates `PtsErr` to errno.

use cluu_proto::pts::{
    PTS_GET_TERMIOS_LABEL, PTS_SET_TERMIOS_LABEL, PTS_FLUSH_LABEL,
    PTS_GET_WINSIZE_LABEL, PTS_SET_WINSIZE_LABEL,
    PTS_GET_PGRP_LABEL, PTS_SET_PGRP_LABEL,
    Termios, Winsize, When, FlushQueue, FlushRequest,
    SetTermiosRequest, PtsErr,
};

const EINVAL: i32 = 22;
const EIO:    i32 = 5;
const EINTR:  i32 = 4;
const EAGAIN: i32 = 11;
const EPERM:  i32 = 1;

fn translate_err(e: PtsErr) -> i32 {
    match e {
        PtsErr::Eagain         => EAGAIN,
        PtsErr::Eintr          => EINTR,
        PtsErr::Eio            => EIO,
        PtsErr::Eperm          => EPERM,
        PtsErr::EinvalTermios  => EINVAL,
        PtsErr::Internal(_)    => EIO,
    }
}

/// Issue a generic PTS_* call against `fd`'s underlying endpoint.
/// Returns the decoded reply payload or sets errno and returns Err.
fn pts_call<Req: serde::Serialize, Rep: for<'de> serde::Deserialize<'de>>(
    fd: i32, label: u32, request: Req,
) -> Result<Rep, i32> {
    use cluu_proto::ABI_VERSION;
    let endpoint = crate::fd_table::endpoint_for_fd(fd as u32).ok_or(EIO)?;
    let payload = postcard::to_allocvec(&request).map_err(|_| EINVAL)?;
    let mut words = [0u64; 6];
    words[0] = payload.len() as u64;
    words[1] = ABI_VERSION as u64;
    let reply = crate::ipc::call(endpoint, label, words, &payload).map_err(|_| EIO)?;
    let result: Rep = postcard::from_bytes(&reply.payload).map_err(|_| EIO)?;
    Ok(result)
}

#[no_mangle]
pub extern "C" fn tcgetattr(fd: i32, out: *mut Termios) -> i32 {
    if out.is_null() { return -1; /* errno = EINVAL via newlib glue */ }
    match pts_call::<(), Termios>(fd, PTS_GET_TERMIOS_LABEL, ()) {
        Ok(t)  => { unsafe { *out = t; } 0 }
        Err(_) => -1,
    }
}

#[no_mangle]
pub extern "C" fn tcsetattr(fd: i32, when: i32, in_t: *const Termios) -> i32 {
    if in_t.is_null() { return -1; }
    let when_e = match when {
        0 => When::Now,
        1 => When::Drain,
        2 => When::Flush,
        _ => return -1,
    };
    let t = unsafe { *in_t };
    let req = SetTermiosRequest { when: when_e, termios: t };
    match pts_call::<SetTermiosRequest, Result<(), PtsErr>>(fd, PTS_SET_TERMIOS_LABEL, req) {
        Ok(Ok(()))  => 0,
        Ok(Err(_))  => -1,
        Err(_)      => -1,
    }
}

#[no_mangle]
pub extern "C" fn tcflush(fd: i32, queue: i32) -> i32 {
    let q = match queue {
        0 => FlushQueue::Input,
        1 => FlushQueue::Output,
        2 => FlushQueue::Both,
        _ => return -1,
    };
    let req = FlushRequest { queue: q };
    match pts_call::<FlushRequest, Result<(), PtsErr>>(fd, PTS_FLUSH_LABEL, req) {
        Ok(Ok(())) => 0,
        _ => -1,
    }
}

// ioctl numbers (matching newlib's <sys/ioctl.h>):
const TIOCGWINSZ: i32 = 0x5413;
const TIOCSWINSZ: i32 = 0x5414;
const TIOCGPGRP:  i32 = 0x540F;
const TIOCSPGRP:  i32 = 0x5410;

#[no_mangle]
pub unsafe extern "C" fn ioctl(fd: i32, request: i32, arg: *mut core::ffi::c_void) -> i32 {
    match request {
        TIOCGWINSZ => {
            let out = arg as *mut Winsize;
            if out.is_null() { return -1; }
            match pts_call::<(), Winsize>(fd, PTS_GET_WINSIZE_LABEL, ()) {
                Ok(ws) => { *out = ws; 0 }
                Err(_) => -1,
            }
        }
        TIOCSWINSZ => {
            let in_ = arg as *const Winsize;
            if in_.is_null() { return -1; }
            let ws = *in_;
            match pts_call::<Winsize, Result<(), PtsErr>>(fd, PTS_SET_WINSIZE_LABEL, ws) {
                Ok(Ok(())) => 0, _ => -1,
            }
        }
        TIOCGPGRP => {
            let out = arg as *mut i32;
            if out.is_null() { return -1; }
            match pts_call::<(), i32>(fd, PTS_GET_PGRP_LABEL, ()) {
                Ok(pgid) => { *out = pgid; 0 }
                Err(_) => -1,
            }
        }
        TIOCSPGRP => {
            let in_ = arg as *const i32;
            if in_.is_null() { return -1; }
            let pgid = *in_;
            match pts_call::<i32, Result<(), PtsErr>>(fd, PTS_SET_PGRP_LABEL, pgid) {
                Ok(Ok(())) => 0, _ => -1,
            }
        }
        _ => -1, // unknown ioctl
    }
}
```

The engineer adapts `crate::fd_table::endpoint_for_fd` to the actual helper name (likely `vfs_addr` or `endpoint_for`); the call helper `crate::ipc::call` matches the existing libcluu IPC primitive.

- [ ] **Step 3: Add module decl**

In `userspace/libcluu/src/posix/mod.rs`:

```rust
pub mod termios;
```

- [ ] **Step 4: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p libcluu --features posix
```

Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add userspace/libcluu/src/posix/termios.rs userspace/libcluu/src/posix/mod.rs
git commit -m "feat(libcluu): POSIX termios+ioctl shims translate to PTS_* verbs"
```

---

## Task 7: Shell drops `tty_endpoint != 0` branch

**Goal:** Shell unconditionally uses PTS_* verbs; the dual-protocol guard from commit `9ac4b12` deleted.

**Files:**
- Modify: `userspace/shell/src/...` (locate via grep)

- [ ] **Step 1: Locate the branch**

```
cd /home/vlb2bp/git/cluu
grep -rn "tty_endpoint != 0\|tty_endpoint == 0" userspace/shell/src/ | head -10
```

Note the file:line of each guard.

- [ ] **Step 2: Delete the guards**

For each match: remove the conditional. The code that runs in the `tty_endpoint != 0` branch was the legacy `TTY_CTL_LABEL` call; the code in the `else` was the cluuterm PTS path. After spec 2, both paths use the unified PTS_* verbs, so:

- Replace `if tty_endpoint != 0 { TTY_CTL ... } else { PTS_... }` with `PTS_...` only.

If shell has a `TtyMode` enum that distinguishes legacy vs pts, simplify to a single mode (the same code path works for both).

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p shell
bash scripts/harness_run.sh
```

Expected: login → shell prompt; typing visible.

- [ ] **Step 4: Commit**

```bash
git add userspace/shell/src/
git commit -m "refactor(shell): drop tty_endpoint dual-protocol branch"
```

---

## Task 8: VFS per-session `/dev/pts/` overlay

**Goal:** Each session sees its own `/dev/pts/` namespace. View derive consults `envelope.session`; substitutes `/dev/pts/` with a session-private MemFs overlay.

**Files:**
- Modify: `userspace/vfs/src/pts.rs`
- Modify: `userspace/vfs/src/view.rs`
- Modify: `userspace/vfs/src/main.rs`

- [ ] **Step 1: Inspect current pts handling**

```
cd /home/vlb2bp/git/cluu
grep -n "PtsEntry\|REGISTER_PTS\|/dev/pts" userspace/vfs/src/pts.rs | head -20
```

Read the existing `/dev/pts/<id>` slot allocation logic. Identify where dir-entries are inserted.

- [ ] **Step 2: Add per-session overlay**

In `userspace/vfs/src/pts.rs`, change the `PtsEntry` storage from a global map to a per-session map:

```rust
pub struct PtsEntry {
    pub id:        u32,
    pub owner_tid: TidLike,
    pub refcount:  u32,
    pub session_id: Option<u32>,   // None = global namespace (e.g., text-VT pts)
}

pub struct PtsOverlay {
    /// session_id → list of pts entries in that session's view
    by_session: alloc::collections::BTreeMap<u32, alloc::collections::BTreeMap<u32 /* pts id */, PtsEntry>>,
    /// Sessionless pts (visible globally): keyed by pts id
    global: alloc::collections::BTreeMap<u32, PtsEntry>,
    next_id: alloc::collections::BTreeMap<Option<u32>, u32>,
}
```

- [ ] **Step 3: Wire view derive**

In `userspace/vfs/src/view.rs`, find the `derive_child_view` function (or its equivalent — view narrowing). When narrowing a `/dev/pts` mount, substitute the overlay per session:

```rust
fn narrow_pts_mount(parent_mount: &MountEntry, session: Option<u32>) -> Option<MountEntry> {
    if parent_mount.path == "/dev/pts" {
        match session {
            Some(sid) => Some(MountEntry {
                path: alloc::string::String::from("/dev/pts"),
                rights: parent_mount.rights,
                backend: pts_overlay_backend_for_session(sid),
            }),
            None => None,  // sessionless callers don't see /dev/pts at all
        }
    } else {
        Some(parent_mount.clone())
    }
}
```

`pts_overlay_backend_for_session` returns a backend-handle pointing into the session's overlay slot in `PtsOverlay::by_session`.

- [ ] **Step 4: Extend `VFS_REGISTER_PTS_LABEL` request shape**

Locate the existing `VFS_REGISTER_PTS_LABEL` handler:

```
cd /home/vlb2bp/git/cluu
grep -n "VFS_REGISTER_PTS_LABEL\|fn handle_register_pts\|PTS_REGISTER_LABEL" userspace/vfs/src/main.rs | head -10
```

Add a `session_id: Option<u32>` field to the request. Compositor / cluuterm pass their session id when registering pts. Sessionless callers (text-VT tty service) pass `None`.

Wire-format request shape (postcard):

```rust
// In cluu_proto::pts:
pub const VFS_REGISTER_PTS_LABEL: u32 = 111;       // OR keep existing if defined
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VfsRegisterPtsRequest {
    pub session_id:   Option<u32>,
    pub pts_endpoint: u64,
    pub suggested_id: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VfsRegisterPtsReply {
    pub assigned_id: u32,
}
```

Add the constants + types to `cluu_proto::pts`.

- [ ] **Step 5: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p vfs -p cluu_proto
bash scripts/harness_run.sh
```

Expected: boots. PTS still works for the existing graphical login flow (which is currently sessionless until spec 3 lands; pass `session_id: None`).

- [ ] **Step 6: Commit**

```bash
git add userspace/vfs/src/pts.rs userspace/vfs/src/view.rs userspace/vfs/src/main.rs \
        userspace/cluu_proto/src/pts.rs
git commit -m "feat(vfs): per-session /dev/pts overlay; VFS_REGISTER_PTS accepts session_id"
```

---

## Task 9: Cluuterm registers pts in its session

**Goal:** Cluuterm reads its session_id (via procmgr query) at startup; calls `VFS_REGISTER_PTS` with that session_id.

**Files:**
- Modify: `userspace/cluuterm/src/main.rs`
- Modify: `userspace/cluuterm/src/tty_backend.rs`

- [ ] **Step 1: Locate the existing pts registration site**

```
cd /home/vlb2bp/git/cluu
grep -n "VFS_REGISTER_PTS\|register_pts\|PTS_REGISTER" userspace/cluuterm/src/ | head -10
```

- [ ] **Step 2: Add session_id query**

Add a helper:

```rust
fn read_own_session_id() -> Option<u32> {
    // Query procmgr for the caller's session_id (spec 3 §10 will add a
    // dedicated verb; until then, read from the ProcessInfo page which
    // procmgr writes at spawn time).
    // For spec 2 landing: pass None unconditionally — the per-session
    // overlay only kicks in once spec 3 wires sessions through spawn.
    None
}
```

(Spec 3 wires this properly; spec 2's job is to *accept* `session_id` in the wire format, not to populate it from a graphical session.)

- [ ] **Step 3: Pass the session_id to VFS_REGISTER_PTS**

In the existing register call, change the payload to include `session_id: read_own_session_id()`.

- [ ] **Step 4: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluuterm
bash scripts/harness_run.sh
```

Expected: graphical login → cluuterm renders shell prompt.

- [ ] **Step 5: Commit**

```bash
git add userspace/cluuterm/src/main.rs userspace/cluuterm/src/tty_backend.rs
git commit -m "feat(cluuterm): register pts under its session"
```

---

## Task 10: TERM env propagation

**Goal:** Cluuterm spawns shell with `TERM=xterm-256color`; tty service (when it spawns shells, e.g., via getty in spec 3) uses `TERM=vt100`.

**Files:**
- Modify: `userspace/cluuterm/src/main.rs`

Note: plan 1 task 17 (cluuterm flips to libcluu::spawn) already constructed `env` with TERM. If plan 1 task 17 hasn't landed, do this here. If it has, verify the value is `xterm-256color` and move on.

- [ ] **Step 1: Verify or add TERM env in cluuterm's spawn envelope**

```
cd /home/vlb2bp/git/cluu
grep -n "\"TERM\"" userspace/cluuterm/src/main.rs
```

If present and value is `xterm-256color`, mark this task complete.

If absent, find the `env: vec![...]` construction in `spawn_shell_with_pts` (plan 1 task 17) and ensure it includes:

```rust
(alloc::string::String::from("TERM"), alloc::string::String::from("xterm-256color")),
```

- [ ] **Step 2: For TTY service (getty path), defer to spec 3 plan**

Note: spec 3's getty will pass `TERM=vt100` when it spawns a user shell on `/dev/tty<n>`. Spec 2 doesn't need to wire it yet.

- [ ] **Step 3: Commit (if any changes)**

```bash
git add userspace/cluuterm/src/main.rs
git commit -m "feat(cluuterm): set TERM=xterm-256color in shell spawn env" || true
```

---

## Task 11: SIGWINCH wiring on cluuterm window resize

**Goal:** When compositor sends `WIN_CONFIGURE` with a new size, cluuterm recomputes (cols, rows), updates its pts winsize, and emits SIGWINCH to fg pgrp.

**Files:**
- Modify: `userspace/cluuterm/src/main.rs`

- [ ] **Step 1: Locate the WIN_CONFIGURE handler**

```
cd /home/vlb2bp/git/cluu
grep -n "WIN_CONFIGURE\|on_window_configure\|on_resize" userspace/cluuterm/src/main.rs | head -10
```

If cluuterm doesn't currently handle WIN_CONFIGURE explicitly, add a stub handler in its main recv loop.

- [ ] **Step 2: Add the resize handler**

```rust
fn on_window_configure(&mut self, new_px_w: u32, new_px_h: u32) {
    let new_cols = (new_px_w / self.cell_w) as u16;
    let new_rows = (new_px_h / self.cell_h) as u16;

    let new_ws = cluu_proto::pts::Winsize {
        rows: new_rows,
        cols: new_cols,
        xpixel: new_px_w as u16,
        ypixel: new_px_h as u16,
    };

    if new_ws != self.pts.winsize {
        self.pts.winsize = new_ws;
        if let Some(pgid) = self.pts.fg_pgid {
            const SIGWINCH: u32 = 28;
            libcluu::ipc::procmgr_pg_signal(pgid, SIGWINCH);
        }
        self.redraw_grid();
    }
}
```

If `cell_w`/`cell_h` are not present, derive from font metrics.

- [ ] **Step 3: Smoke test**

Manually resize the cluuterm window via compositor IPC (or via a test stand-in that issues WIN_CONFIGURE). In the running shell, run `stty size`. Expected: new rows/cols match.

```
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_sigwinch_delivered bash scripts/harness_run.sh
grep "l2_sigwinch_delivered:" serial.log
```

Marker probe at Task 13.

- [ ] **Step 4: Commit**

```bash
git add userspace/cluuterm/src/main.rs
git commit -m "feat(cluuterm): emit SIGWINCH on WIN_CONFIGURE resize"
```

---

## Task 12: Delete dead code (legacy TTY_*, dual-protocol guards)

**Goal:** Zero hits for legacy labels + dual-protocol guards.

**Files:**
- Multiple. Use grep + targeted deletion.

- [ ] **Step 1: Verify hit lists**

```
cd /home/vlb2bp/git/cluu
git grep -n "TTY_REGISTER_LABEL\b"       # → expect remaining hits to be in cluu_proto comments only
git grep -n "TTY_CTL_LABEL\b"
git grep -n "TTY_SET_FG_LABEL\b"
git grep -n "TTY_READ_REQUEST_LABEL\b"
git grep -n "TTY_POLL_QUERY_LABEL\b"
git grep -n "tty_endpoint != 0" userspace/shell/
```

For each match: delete the line / region.

- [ ] **Step 2: Delete legacy label constants in `userspace/libcluu/src/ipc.rs`**

```
cd /home/vlb2bp/git/cluu
grep -n "TTY_CTL_LABEL: u32 = 3\|TTY_REGISTER_LABEL: u32 = 4\|TTY_READ_REQUEST_LABEL: u32 = 6\|TTY_POLL_QUERY_LABEL: u32 = 7\|TTY_SET_FG_LABEL: u32 = 40" userspace/libcluu/src/ipc.rs
```

Delete those const lines.

In `userspace/libcluu/src/ipc.rs`, delete the legacy `PTS_READ_LABEL: u32 = 0x72` etc. (cluuterm-local numbers superseded by `cluu_proto::pts::PTS_*`).

- [ ] **Step 3: Build clean**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both clean.

- [ ] **Step 4: Re-verify hit lists**

```
cd /home/vlb2bp/git/cluu
git grep -c "TTY_REGISTER_LABEL\b" && echo "FAIL" || echo "PASS"
git grep -c "TTY_CTL_LABEL\b"     && echo "FAIL" || echo "PASS"
git grep -c "TTY_SET_FG_LABEL\b"  && echo "FAIL" || echo "PASS"
git grep -c "TTY_READ_REQUEST_LABEL\b" && echo "FAIL" || echo "PASS"
git grep -c "TTY_POLL_QUERY_LABEL\b" && echo "FAIL" || echo "PASS"
git grep -c "tty_endpoint != 0" userspace/shell/ && echo "FAIL" || echo "PASS"
```

All must print PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: delete legacy TTY_* labels + tty_endpoint dual-protocol branch"
```

---

## Task 13: Acceptance markers

**Goal:** Add the spec 2 §12 acceptance markers. Each is a small probe.

**Files (one probe per marker):**
- `userspace/probes/l2_sigint_delivered/`
- `userspace/probes/l2_sigtstp_delivered/`
- `userspace/probes/l2_sigwinch_delivered/`
- `userspace/probes/l2_sigttin_background_read/`
- `userspace/probes/l2_term_env_cluuterm/`
- `userspace/probes/l2_tcgetattr_default/`
- `userspace/probes/l2_pts_cross_session_isolation/`
- `userspace/probes/l2_pts_service_death_hangup/`

For each marker:

- [ ] **Step 1: Scaffold from `userspace/probes/argvprobe/`**

Copy Cargo.toml; rename `name`. Add to workspace members.

- [ ] **Step 2: Write the probe**

Template (using `l2_sigint_delivered` as example):

```rust
#![no_std]
#![no_main]
extern crate alloc;
extern crate libcluu;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // Install SIGINT handler that sets a flag.
    static mut GOT_SIGINT: bool = false;
    extern "C" fn handler(_sig: i32) {
        unsafe { GOT_SIGINT = true; }
    }
    unsafe { libcluu::signal::sigaction(2 /* SIGINT */, handler); }

    // Self-feed a Ctrl-C byte into pts (this requires a helper probe API;
    // the actual implementation may invoke a special test verb on the
    // service or use a hooked stdin feeder. The probe verifies the end-to-
    // end signal path).
    libcluu::probe::inject_input_byte(0x03);

    // Wait briefly for delivery.
    for _ in 0..10000 { core::hint::spin_loop(); }

    let ok = unsafe { GOT_SIGINT };
    libcluu::print_log(if ok {
        b"l2_sigint_delivered: PASS\n"
    } else {
        b"l2_sigint_delivered: FAIL\n"
    });
    0
}
```

`libcluu::probe::inject_input_byte` may not exist; if not, the probe issues a `PTS_WRITE` of `0x03` against the service from a fake-input perspective or uses an alternate mechanism the engineer designs. Document at landing time.

Marker `l2_term_env_cluuterm` is simpler:

```rust
let term = libcluu::posix::getenv("TERM").unwrap_or("");
libcluu::print_log(if term == "xterm-256color" {
    b"l2_term_env_cluuterm: PASS\n"
} else {
    b"l2_term_env_cluuterm: FAIL\n"
});
```

Marker `l2_tcgetattr_default`:

```rust
let mut t: cluu_proto::pts::Termios = unsafe { core::mem::zeroed() };
let rc = unsafe { libcluu::posix::termios::tcgetattr(0, &mut t as *mut _) };
let ok = rc == 0
    && (t.c_lflag & cluu_proto::pts::Termios::ICANON) != 0
    && (t.c_lflag & cluu_proto::pts::Termios::ECHO) != 0
    && (t.c_lflag & cluu_proto::pts::Termios::ISIG) != 0;
libcluu::print_log(if ok {
    b"l2_tcgetattr_default: PASS\n"
} else {
    b"l2_tcgetattr_default: FAIL\n"
});
```

- [ ] **Step 3: Run each marker**

For each marker:

```
HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=<marker> MARKER_MODE=<marker> bash scripts/harness_run.sh
grep "<marker>:" serial.log
```

Expected: `<marker>: PASS`.

- [ ] **Step 4: Commit**

```bash
git add userspace/probes/l2_* Cargo.toml
git commit -m "test: spec 2 acceptance markers"
```

---

## Final verification

- [ ] **Spec 2 §12 grep proofs:**

```
cd /home/vlb2bp/git/cluu
echo "Zero-hit:"
git grep -c "TTY_REGISTER_LABEL"
git grep -c "TTY_CTL_LABEL"
git grep -c "TTY_SET_FG_LABEL"
git grep -c "TTY_READ_REQUEST_LABEL"
git grep -c "TTY_POLL_QUERY_LABEL"
git grep -c "tty_endpoint != 0" userspace/shell/

echo "One-match:"
git grep -c "PTS_READ_LABEL.*= 100"
git grep -c "fn feed_byte" userspace/libcluu/src/tty_core/
```

All zero-hit proofs → 0. All one-match → 1.

- [ ] **Functional smoke:**

```
cd /home/vlb2bp/git/cluu
bash scripts/harness_run.sh
```

- Interactive login → shell prompt.
- Ctrl-C in shell → foreground command interrupted.
- Ctrl-Z → foreground suspended.
- Ctrl-D at fresh prompt → shell exits cleanly.
- Backspace + Ctrl-W mid-line → editing visible.
- Resize cluuterm window → `stty size` reports new dims.

- [ ] **No new timeouts:**

```
grep -rn "recv_with_timeout\|call_with_timeout" userspace/cluuterm/src/ userspace/tty/src/ | wc -l
```

Same count as before plan 2.

---

## Notes for the engineer

- **TDD:** Tasks 1, 2 have unit tests. Run before moving on. Service-side tasks (4, 5) verify via harness markers (Task 13).
- **DRY:** Cluuterm + tty service share `libcluu::tty_core` (line discipline + routing helper). Don't duplicate.
- **YAGNI:** No `tcdrain`, `tcsendbreak`, mouse-mode escape sequences, runtime keymap change. All deferred to follow-ups (spec §13).
- **POSIX-shaped C surface:** the libcluu termios shims (Task 6) take raw C `*mut Termios` etc. — match newlib's `<termios.h>` byte layout exactly.
- **Per-session overlay:** Task 8 lays the groundwork; Task 9 wires cluuterm. Spec 3's plan will populate sessions to make the overlay actually scope per-user.
- **Test the failure modes:** every `PtsErr` variant should be hit by at least one marker. If not, mark unreached variants for future markers.
- **Skipped probes:** if any marker probe can't run (e.g., needs an input-injection mechanism not in tree), document in commit message and file follow-up.

---

## Spec 2 sections covered

| Spec § | Task(s) |
|---|---|
| §3 architecture | Task 4, 5 (both services share line_discipline + routing) |
| §4 verb set | Task 1 (label consts), Task 4 + Task 5 (handlers) |
| §5 wire format | Task 1 (types) |
| §6 line discipline | Task 2 (LineDiscipline + tests) |
| §7 termios + signals | Task 2 (key mapping in feed_byte), Task 4 (SIGTTIN/SIGTTOU + SIGWINCH wiring) |
| §8 TERM + winsize | Task 10, 11 |
| §9 per-session overlay | Task 8, 9 |
| §10 error semantics | Task 4 + 5 (PtsErr in every handler), Task 6 (errno translation) |
| §11 migration | Task 1-12 |
| §12 acceptance | Task 13, final verification |
| §13 follow-ups | OUT of plan 2 scope |
