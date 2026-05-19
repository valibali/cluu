# Terminal + PTY unification — design

**Date:** 2026-05-18
**Status:** spec — pre-implementation
**Predecessor inventory:** `docs/superpowers/specs/2026-05-18-spawn-window-pty-inventory.md`
**Companion spec:** `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md` (spec 1)
**Position in decomposition:** spec 2 of inventory §12.

## 1. Why

Today two terminal protocols coexist without convergence (inventory
§4). Legacy TTY service (`userspace/tty/`, one per text-VT) speaks
`TTY_REGISTER_LABEL`, `TTY_CTL_LABEL`, `TTY_SET_FG_LABEL`,
`TTY_READ_REQUEST_LABEL`, `TTY_POLL_QUERY_LABEL`. Cluuterm
(`userspace/cluuterm/`, the graphical-session terminal) speaks
`PTS_READ_LABEL`, `PTS_WRITE_LABEL`, `PTS_CLOSED_LABEL`.

Shell branches on context (commit `9ac4b12` skips legacy TTY IPC in
pts mode). Inventory §5 details the gap: cluuterm has zero of cooked
mode, line discipline, Ctrl-C → SIGINT, Ctrl-Z → SIGTSTP, Ctrl-D EOF,
`tcgetattr`/`tcsetattr`, winsize ioctl, TERM env. Users currently type
Ctrl-C dozens of times to interrupt hung commands and every keystroke
logs `cluuterm: Ctrl-C (signal dropped in v1)`.

This spec unifies the protocol — one verb set for both services, one
shared line-discipline library — and closes the full POSIX terminal
signal set. The text-VT service stays for boot/recovery; cluuterm
becomes the user-facing terminal.

## 2. Goals and non-goals

### Goals

1. One verb set (`PTS_*`) spoken by both `userspace/tty/` (text-VT
   service) and `userspace/cluuterm/` (graphical terminal). Shell uses
   one set regardless of which it talks to.
2. Shared line-discipline library at `libcluu/src/tty_core/line_discipline.rs`
   imported by both services. Per-pts state lives inside the service
   that owns the pts; no cross-service state.
3. Full POSIX terminal signal set: SIGINT (Ctrl-C), SIGTSTP (Ctrl-Z),
   SIGQUIT (Ctrl-\), SIGWINCH (resize), SIGTTIN (bg read), SIGTTOU
   (bg write with TOSTOP). c_cc keys: VEOF, VERASE, VKILL, VWERASE,
   VINTR, VQUIT, VSUSP.
4. POSIX termios surface: `tcgetattr` / `tcsetattr` / `tcflush` /
   `ioctl(TIOCGWINSZ|TIOCSWINSZ|TIOCGPGRP|TIOCSPGRP)` all functional.
5. Per-session `/dev/pts/` namespace. Cross-session pts access denied.
   `/dev/tty1..3` stays in global namespace (boot/recovery).
6. TERM env propagation from spawner. Cluuterm spawns with
   `TERM=xterm-256color`; tty service with `TERM=vt100`.
7. Resize delivered as SIGWINCH to fg pgrp.

### Non-goals

- Replacing the text-VT service entirely (reachable later, not in
  spec 2; keeps boot/recovery usable).
- Mouse-mode escape sequences (DEC private modes 1000+); spec 2 stops
  at signal + termios + winsize fundamentals.
- Job control (`fg`/`bg`/`jobs` shell commands) wiring beyond what
  signal delivery enables. The shell can build job control on top of
  the signal/SET_PGRP primitives spec 2 provides; that's separate.
- POSIX `tcsetpgrp`/`tcgetpgrp` C-runtime shims — handled in
  step 6 of migration but their absence at landing time doesn't block
  spec 2 (shell uses libcluu's PTS_SET_PGRP directly until newlib
  shims land).

## 3. Architecture

```
graphical session                           boot / recovery / VT1-3
┌─────────────────────────────────────┐    ┌──────────────────────┐
│  kbd ──► compositor ──► cluuterm    │    │  kbd ──► userspace/  │
│            (WIN_INPUT)   │           │    │   tty (active VT)   │
│                          ▼           │    │            │         │
│                  line_discipline    │    │            ▼         │
│                          │           │    │     line_discipline │
│                          ▼           │    │            │         │
│                  /dev/pts/<n>       │    │            ▼         │
│                          │           │    │      /dev/tty<n>    │
│                  PTS_* verbs        │    │      PTS_* verbs    │
│                          │           │    │            │         │
│                          ▼           │    │            ▼         │
│                        shell        │    │          shell      │
│                  (one verb set)     │    │     (same verb set) │
└─────────────────────────────────────┘    └──────────────────────┘
       │                                          │
       └──── PROCMGR_PG_SIGNAL ───────────────────┘
             (signal routing to fg pgrp)
```

Each service owns its pts state in-process. `libcluu::tty_core` is a
Rust module both services link. Signal delivery uses the existing
`PROCMGR_PG_SIGNAL` mechanism (kernel knows nothing of terminals;
signal subsystem in libcluu, per MEMORY.md §11).

**SOLID anchors:**

- Single-responsibility: each service owns its own pts state; line
  discipline is one library; signal delivery uses existing
  procmgr-pg-signal.
- Open/closed: adding a future terminal frontend (e.g.,
  `/dev/ttyS0` serial-line service) requires a third service speaking
  PTS_* and linking `tty_core`. No protocol changes.
- Liskov substitution: shell can't tell which service it's talking to
  from behavior. Verbs behave identically.

**What dies:**

- Legacy labels: `TTY_REGISTER_LABEL`, `TTY_CTL_LABEL`,
  `TTY_SET_FG_LABEL`, `TTY_READ_REQUEST_LABEL`, `TTY_POLL_QUERY_LABEL`.
- Shell's `tty_endpoint != 0` branch from commit `9ac4b12`.
- Cluuterm's old local `PTS_READ_LABEL` / `PTS_WRITE_LABEL` /
  `PTS_CLOSED_LABEL` constants (replaced by the unified set in
  `cluu_proto::pts`; different label numbers).

## 4. Verb set

`cluu_proto::pts` module exports:

```rust
pub const PTS_READ_LABEL:         u32 = 100;
pub const PTS_WRITE_LABEL:        u32 = 101;
pub const PTS_POLL_LABEL:         u32 = 102;
pub const PTS_GET_TERMIOS_LABEL:  u32 = 103;
pub const PTS_SET_TERMIOS_LABEL:  u32 = 104;
pub const PTS_GET_WINSIZE_LABEL:  u32 = 105;
pub const PTS_SET_WINSIZE_LABEL:  u32 = 106;
pub const PTS_GET_PGRP_LABEL:     u32 = 107;
pub const PTS_SET_PGRP_LABEL:     u32 = 108;
pub const PTS_FLUSH_LABEL:        u32 = 109;
pub const PTS_CLOSED_LABEL:       u32 = 110;
```

Semantic summary:

| Verb | Request | Reply |
|---|---|---|
| READ | `{ max_bytes }` | `Result<Vec<u8>, PtsErr>` (cooked bytes if canonical mode) |
| WRITE | `Vec<u8>` | `Result<u32, PtsErr>` (bytes_written) |
| POLL | `{ events: PollEvents }` | `{ ready: PollEvents }` (POLLIN/POLLOUT/POLLHUP/POLLERR) |
| GET_TERMIOS | `()` | `Termios` |
| SET_TERMIOS | `{ when: Now\|Drain\|Flush, termios }` | `Result<(), PtsErr>` |
| GET_WINSIZE | `()` | `Winsize { rows, cols, xpixel, ypixel }` |
| SET_WINSIZE | `Winsize` | `Result<(), PtsErr>` (also emits SIGWINCH to fg_pgid) |
| GET_PGRP | `()` | `pgid_t` |
| SET_PGRP | `pgid_t` | `Result<(), PtsErr>` (errors: Eperm if caller not in session) |
| FLUSH | `{ queue: Input\|Output\|Both }` | `Result<(), PtsErr>` |
| CLOSED | `()` (async event from service to shell) | — |

`PTS_DRAIN` and `PTS_SEND_BREAK` are explicitly not in spec 2.
`TCSADRAIN` semantics are subsumed by `PTS_SET_TERMIOS { when: Drain }`
(service waits for output queue drain before applying). `tcsbrk` is
rare and deferred.

The pts cap is obtained via VFS `open("/dev/pts/<n>")` returning the
service's IPC token. Open/close lives in VFS, not in the PTS verb set.

## 5. Wire format

Same encoding pattern as spec 1: postcard-serialized requests/replies
inside the IPC payload buffer.

**Per-call layout (every PTS verb):**

```
words[0] = payload_len
words[1] = ABI_VERSION (= 1)
words[2..6] = 0 (reserved)
payload  = postcard::to_slice(&Request)   // or &Reply
```

**Types (`cluu_proto::pts`):**

```rust
pub struct ReadRequest      { pub max_bytes: u32 }
pub type   ReadReply        = Result<Vec<u8>, PtsErr>;

pub type   WriteRequest     = Vec<u8>;
pub type   WriteReply       = Result<u32, PtsErr>;

pub struct PollRequest      { pub events: PollEvents }
pub struct PollReply        { pub ready: PollEvents }
bitflags! { pub struct PollEvents : u32 {
    const POLLIN  = 0x1; const POLLOUT = 0x2;
    const POLLHUP = 0x4; const POLLERR = 0x8;
}}

pub type   GetTermiosReply  = Termios;
pub struct SetTermiosRequest{ pub when: When, pub termios: Termios }
pub enum   When             { Now, Drain, Flush }
pub type   SetTermiosReply  = Result<(), PtsErr>;

pub type   GetWinsizeReply  = Winsize;
pub struct Winsize          { pub rows: u16, pub cols: u16,
                              pub xpixel: u16, pub ypixel: u16 }
pub type   SetWinsizeReply  = Result<(), PtsErr>;

pub type   GetPgrpReply     = i32;          // pgid
pub type   SetPgrpRequest   = i32;
pub type   SetPgrpReply     = Result<(), PtsErr>;

pub struct FlushRequest     { pub queue: FlushQueue }
pub enum   FlushQueue       { Input, Output, Both }
pub type   FlushReply       = Result<(), PtsErr>;

pub enum PtsErr {
    Eagain, Eintr, Eio, Eperm, EinvalTermios, Internal(u32),
}
```

**Error semantics.** Every reply is a `Result<_, PtsErr>`. No timeouts.
If service dies, kernel revokes the endpoint, shell's `ipc_call`
returns the kernel cap-revoked error, libcluu translates to
`PtsErr::Eio`, shell sees EOF / hangup cleanly. Matches
`feedback_no_timeouts`.

**Caller resolution.** Shell holds a token routing to its pts service
(from `open("/dev/pts/<n>")` returning the service endpoint). Every
PTS verb is issued against that token. The service uses caller's
thread-id to identify which pts within itself the call refers to.

## 6. Line discipline library

**Location:** `userspace/libcluu/src/tty_core/line_discipline.rs`
(existing; expanded). Imported as a Rust module by both
`userspace/tty/` and `userspace/cluuterm/`.

**State (per pts instance, owned by the service):**

```rust
pub struct LineDiscipline {
    pub termios: Termios,
    pending_line: Vec<u8>,
    output_pending: Vec<u8>,
    eof_seen: bool,
    last_was_cr: bool,
}
```

**Input path API:**

```rust
pub enum LineDiscOutput {
    Bytes(Vec<u8>),
    Signal(SignalNum),
    Echo(Vec<u8>),
    Eof,
    Drop,
}

impl LineDiscipline {
    pub fn feed_byte(&mut self, byte: u8) -> Vec<LineDiscOutput>;
    pub fn flush_line(&mut self) -> Vec<u8>;
    pub fn process_output(&mut self, bytes: &[u8]) -> Vec<u8>;
    pub fn termios(&self) -> &Termios;
    pub fn set_termios(&mut self, new: Termios) -> Result<(), TermiosErr>;
}
```

**Cooked-mode (ICANON) processing:**

For each input byte:

| Condition | Action |
|---|---|
| matches c_cc[VINTR] && (c_lflag & ISIG) | emit `Signal(SIGINT)` |
| matches c_cc[VQUIT] && (c_lflag & ISIG) | emit `Signal(SIGQUIT)` |
| matches c_cc[VSUSP] && (c_lflag & ISIG) | emit `Signal(SIGTSTP)` |
| matches c_cc[VEOF] && canonical | emit `Eof` (flush pending line) |
| matches c_cc[VERASE] && canonical | erase last byte; emit `Echo("\b \b")` if ECHOE |
| matches c_cc[VKILL] && canonical | clear pending_line; emit `Echo` line-redraw if ECHOK |
| matches c_cc[VWERASE] && canonical | erase last word; emit `Echo` if ECHOE |
| `\n` && canonical | flush pending_line as `Bytes`; emit `Echo("\n")` if ECHO or ECHONL |
| other && canonical | append; emit `Echo(byte)` if ECHO |
| any && raw | emit `Bytes([byte])`; emit `Echo` only if ECHO |

**Output path (OPOST):**

If `c_oflag & OPOST` set: `\n` → `\r\n` (if ONLCR), `\r` → `""`
(if ONOCR at col 0), etc. Service calls `process_output(bytes)` before
sending to the rendering layer.

Without OPOST: pass-through verbatim.

**Termios validation.** `set_termios` rejects illegal combinations
(e.g., canonical mode requires reasonable c_cc[VEOF] etc.). Returns
`TermiosErr` mapped to `PtsErr::EinvalTermios`.

**Per-session ownership.** `LineDiscipline` lives entirely in the
service process owning the pts. Service drops it when the pts hangs
up; pending output drained or discarded per `tcflush(Output)` rules.

**Tests.** Pure-function unit tests in the same file: VINTR translation
under ISIG/no-ISIG; VEOF in canonical mode; VERASE with/without
ECHOE; OPOST NL → CRNL; ICRNL CR → NL; raw-mode pass-through.

## 7. Termios layout + signal/key mapping

**Termios struct (newlib-compatible):**

```rust
#[repr(C)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_cc:    [u8; NCCS],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}
pub const NCCS: usize = 20;
```

Field order matches newlib's `sys/termios.h` for CLUU target.
Discrepancy at landing fixes whichever file is wrong (struct-layout
lesson from kernel bugs 1-3 in MEMORY.md).

**Flag bits supported in spec 2:**

```
c_iflag: IGNBRK BRKINT ICRNL INLCR IXON IXOFF
c_oflag: OPOST ONLCR
c_cflag: CREAD HUPCL CLOCAL
c_lflag: ISIG ICANON ECHO ECHOE ECHOK ECHONL NOFLSH TOSTOP
         ECHOCTL ECHOPRT ECHOKE IEXTEN

c_cc[]:
  VEOF=0 VEOL=1 VERASE=2 VINTR=3 VKILL=4 VMIN=5 VQUIT=6
  VSTART=7 VSTOP=8 VSUSP=9 VTIME=10 VWERASE=11   (12-19 reserved)
```

**Default termios** (fresh pts):

```
c_iflag = ICRNL | BRKINT
c_oflag = OPOST | ONLCR
c_cflag = CREAD | CLOCAL
c_lflag = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | IEXTEN
c_cc[]  = VEOF=^D, VERASE=DEL, VINTR=^C, VKILL=^U, VQUIT=^\,
          VSTART=^Q, VSTOP=^S, VSUSP=^Z, VWERASE=^W, VMIN=1, VTIME=0
c_ispeed = c_ospeed = 38400
```

**Signal routing (service-side mechanism):**

```rust
match line_disc.feed_byte(byte) {
    LineDiscOutput::Signal(sig) => {
        if let Some(fg_pgid) = self.fg_pgid {
            procmgr_pg_signal(fg_pgid, sig);
        }
    }
    LineDiscOutput::Echo(bytes)  => self.queue_output(bytes),
    LineDiscOutput::Eof          => self.queue_eof_to_reader(),
    LineDiscOutput::Bytes(b)     => self.deliver_to_reader(b),
    LineDiscOutput::Drop         => {}
}
```

`procmgr_pg_signal` already exists; signal subsystem from MEMORY.md
§11 delivers SIGINT/SIGTSTP/SIGQUIT to all processes in the pgrp.

**SIGTTIN / SIGTTOU:**

```rust
fn handle_pts_read(caller_pid: u32, caller_pgid: i32, req: ReadRequest)
    -> Result<ReadReply, _>
{
    if self.fg_pgid != Some(caller_pgid) {
        procmgr_pg_signal(caller_pgid, SIGTTIN);
        return Err(PtsErr::Eintr);
    }
    /* normal read */
}
```

Symmetric for SIGTTOU on `PTS_WRITE` when `c_lflag & TOSTOP`.

**SIGWINCH delivery:**

`PTS_SET_WINSIZE` (or cluuterm-internal `set_winsize` on resize):

```rust
if let Some(fg_pgid) = self.fg_pgid {
    procmgr_pg_signal(fg_pgid, SIGWINCH);
}
```

## 8. TERM env + winsize flow

**TERM env propagation** (set by spawner via `SpawnEnvelope.env` from
spec 1):

| Spawner | TERM value | Rationale |
|---|---|---|
| cluuterm | `xterm-256color` | cluuterm emulates xterm; standard terminfo |
| tty service | `vt100` | conservative; works with curses/ncurses out of the box |

Future option: ship a `cluu-256color` terminfo entry when features
deviate enough. Not in spec 2.

**Initial winsize:**

| Service | Source |
|---|---|
| cluuterm | `px_w/cell_w × px_h/cell_h` computed at window-create; updated on every compositor `WIN_CONFIGURE` |
| tty service | static framebuffer text-mode size at boot; updates only on VT mode change |

**Resize flow (cluuterm):**

```
compositor (WIN_CONFIGURE: new px size)
    ↓
cluuterm on_window_configure(new_px_w, new_px_h)
    ├ new_cols = new_px_w / cell_w
    ├ new_rows = new_px_h / cell_h
    ├ if (new_cols, new_rows) changed:
    │     pts.set_winsize(new_cols, new_rows, new_px_w, new_px_h)
    │     procmgr_pg_signal(fg_pgid, SIGWINCH)
    └ redraw
```

Internal `set_winsize` is the same code path that external
`PTS_SET_WINSIZE` IPC hits.

**Resize flow (tty service):** VT mode change → tty service updates
winsize, emits SIGWINCH. Mostly stable.

**Shell SIGWINCH handler:**

```rust
fn on_sigwinch() {
    let ws = pts_get_winsize();
    /* redraw prompt; readline-style libs handle internally */
}
```

POSIX: SIGWINCH default is ignore. Shells install handlers explicitly.
Spec 2 doesn't change signal subsystem defaults; just ensures the
signal is delivered when winsize changes.

**Edge case.** SIGWINCH dropped if `fg_pgid == None` at resize time.
Shell calls `pts_get_winsize` on first read after spawn anyway.

## 9. Per-session pts namespace + lifecycle

**Namespace shape:**

| Path | Namespace | Owner | Visibility |
|---|---|---|---|
| `/dev/pts/<n>` | per-session | cluuterm in that session | session-local only |
| `/dev/tty<n>` | global | tty service for VT n | sessionless / boot |

**Spec 1 integration (per-session overlay):**

VFS view derive (spec 1 §8) extends to substitute `/dev/pts/` with a
session-private MemFs overlay when `envelope.session = Some(token)`:

```rust
fn narrow_for_manifest(parent: &MountEntry, manifest: &Manifest,
                       session: Option<SessionId>) -> Option<MountEntry>
{
    if parent.path == "/dev/pts" {
        if let Some(sid) = session {
            return Some(session_private_pts_mount(sid));
        }
    }
    /* existing logic */
}
```

Each session's `/dev/pts/` is a small MemFs holding pts dir-entries
that the session's cluuterm(s) register. Sessionless callers see no
`/dev/pts/` mount at all.

**Pts creation:**

```
1. cluuterm starts under SESSION_LOGIN.
2. cluuterm calls VFS_REGISTER_PTS_LABEL { session_id, pts_endpoint,
                                            suggested_id }.
3. VFS picks free <n> in the session namespace; creates dir-entry at
   /dev/pts/<n> pointing at cluuterm's endpoint.
4. VFS replies with chosen <n>.
5. cluuterm spawns shell with envelope.fd_inherit pointing at parent-
   side open("/dev/pts/<n>", O_RDWR).
```

**Pts close + cleanup:**

```
1. All openers gone (VFS PtsEntry refcount drops to 0).
2. cluuterm calls VFS_UNREGISTER_PTS { pts_id }.
3. VFS removes dir-entry; recycles <n>.
4. cluuterm tears down internal pts state.
```

If cluuterm dies with shell still open, procmgr revokes cluuterm's
endpoints, shell's pending PTS verbs return `PtsErr::Eio`, shell sees
EOF and exits cleanly. No leak.

**Open/close on shell side:**

- `open("/dev/pts/<n>", O_RDWR)` → VFS routes to service endpoint;
  fd backed by PTS service IPC token.
- libcluu's fd-table associates the fd with PTS_* verb dispatch
  (vs. regular VfsFile dispatch).
- `close(fd)` → `VFS_CLOSE` → service decrements refcount; hangup
  when refcount=0 and HUPCL set.

**Text-VT lifecycle:** static. tty service registers `/dev/tty1..3`
at boot once; global namespace; never unregisters.

**Sessionless processes:** views have no `/dev/pts/` mount; any
`open("/dev/pts/<n>")` returns `ENOENT`. Drivers that legitimately
need a controlling tty mount `/dev/tty<n>` from global per manifest.

## 10. Error semantics + cap revocation

`procmgr_pg_signal` is non-blocking and idempotent (existing
behavior). Service calls it inside the input-handler hot path; no
timeouts.

PTS verbs are synchronous. Service may queue replies internally if a
PTS_READ has no data and `c_cc[VMIN]` requires more bytes — the
service holds the request and replies when more bytes arrive. The
shell-side `ipc_call` blocks. If service dies during the block, kernel
cap-revocation surfaces `Eio`.

No `recv_with_timeout` in PTS service paths. No `recv_with_timeout`
introduced in shell-side PTS dispatch.

**Cap-revocation hand-off:**

- Service is killed (e.g., cluuterm window close handled by procmgr
  reaping cluuterm pid).
- Procmgr revokes cluuterm's IPC endpoints, including the pts service
  endpoint shell holds.
- Shell's blocked `PTS_READ` returns kernel-level `EBADTOKEN` →
  libcluu translates to `PtsErr::Eio`.
- Shell sees EOF on stdin; main loop detects; shell exits.

## 11. Migration plan

Builds on spec 1's foundation. Cannot start until spec 1 has at least
landed steps 1-4 (proto crate + `procmgr::spawn` + libcluu native
spawn). Spec 2 can land in parallel with spec 1's steps 5-12.

1. **`cluu_proto::pts` module.** Verb labels (100-110), request /
   reply types, `Termios`, `Winsize`, `PtsErr`, `PollEvents`,
   `FlushQueue`, `When`. No call-site changes. Build clean.

2. **Expand `libcluu::tty_core::line_discipline`.** Add
   `LineDiscOutput` enum, `feed_byte` / `process_output` /
   `set_termios` API. Implement full c_cc set. ICANON + ECHO* +
   OPOST + ONLCR + ICRNL + INLCR behavior. Pure-function unit tests
   in the same file.

3. **Service-shared signal-routing helpers in `libcluu::tty_core`.**
   `route_input_byte(line_disc, fg_pgid, byte) -> Vec<ServiceAction>`
   used by both services. Service-agnostic logic.

4. **Cluuterm speaks unified PTS_*.** Dispatch table swap: old local
   `PTS_*` consts replaced by `cluu_proto::pts::PTS_*`. Implement
   all eleven verbs. Wire line discipline + signal routing. Default
   termios on fresh pts.

5. **TTY service speaks unified PTS_*.** Rewrite
   `userspace/tty/src/main.rs` dispatch: replace `TTY_*` labels with
   `PTS_*`. Same line discipline library, same routing helpers.
   `/dev/tty1..3` registrations unchanged.

6. **libcluu newlib shims.** `tcgetattr`, `tcsetattr`, `tcflush`,
   `ioctl(TIOCGWINSZ|TIOCSWINSZ|TIOCGPGRP|TIOCSPGRP)`. Translate
   `PtsErr` to errno. POSIX surface for C-runtime callers.

7. **Shell drops dual-protocol branches.** Remove `tty_endpoint != 0`
   guard from `9ac4b12`; shell unconditionally uses PTS_* verbs.
   Pipeline-stage signal forwarding uses existing signal subsystem.

8. **VFS per-session `/dev/pts/` overlay.** VFS gains
   `pts_overlay: HashMap<SessionId, MemFs>`. View derive substitutes
   `/dev/pts/`. `VFS_REGISTER_PTS_LABEL` takes `session_id` and routes
   registration into the session's overlay.

9. **Cluuterm registers pts in its session.** Reads `session_id` from
   `ProcessEntry` (via procmgr query); calls
   `VFS_REGISTER_PTS { session_id, pts_endpoint }`. Shell envelope's
   view token routes correctly.

10. **TERM env propagation.** Cluuterm's shell-spawn envelope adds
    `("TERM", "xterm-256color")`. TTY service's (if it spawns shells)
    adds `("TERM", "vt100")`.

11. **SIGWINCH wiring.** Cluuterm's `WIN_CONFIGURE` handler:
    recompute cols/rows; if changed, call internal `pts.set_winsize`
    which emits SIGWINCH. Marker `l2_sigwinch_delivered`.

12. **Delete dead code.** `TTY_*` consts, TTY service old dispatch
    arms, shell `tty_endpoint != 0` branch, cluuterm old local
    `PTS_*` consts (different numbers from unified).

13. **Verify.** Acceptance criteria pass.

Per-step gate: `bash scripts/harness_run.sh` reaches `compositor:
ready` and `shell: ready`; interactive login flow works.

## 12. Acceptance criteria

### Build

- `cargo xtask build` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Grep zero-hit proofs

- `git grep TTY_REGISTER_LABEL`
- `git grep TTY_CTL_LABEL`
- `git grep TTY_SET_FG_LABEL`
- `git grep TTY_READ_REQUEST_LABEL`
- `git grep TTY_POLL_QUERY_LABEL`
- `git grep "tty_endpoint != 0"` inside `userspace/shell/`

### Grep one-match proofs

- `git grep "PTS_READ_LABEL.*= 100"` → one match in `cluu_proto::pts`.
- `git grep "fn feed_byte" userspace/libcluu/src/tty_core/` → one.

### Functional smoke

- Interactive root/root login, shell prompt, type, echo, run `ls`.
- Ctrl-C while a foreground command runs → command exits with SIGINT.
- Ctrl-Z → foreground suspended; `fg` resumes.
- Ctrl-D at fresh prompt → shell exits (canonical EOF).
- Ctrl-\ → SIGQUIT delivered (verify via test program).
- Backspace and Ctrl-W mid-line → editing visible; submitted line
  correct.
- Resize cluuterm via compositor → `stty size` reports new dims.

### Line-discipline unit tests

`cargo test -p libcluu`:

- `test_canonical_line_assembly`
- `test_vintr_signal_under_isig`
- `test_veof_canonical`
- `test_verase_with_echoe`
- `test_opost_nl_to_crnl`
- `test_icrnl`
- `test_raw_mode_passthrough`

### Cap-discipline markers

- `l2_pts_cross_session_isolation`: process in session A
  `open("/dev/pts/<n>")` where `<n>` exists in session B → `ENOENT`.
- `l2_pts_sessionless_no_overlay`: sessionless driver attempts
  `open("/dev/pts/0")` → `ENOENT`.
- `l2_pts_service_death_hangup`: kill cluuterm with shell open;
  shell's pending `PTS_READ` returns `Eio` → EOF → shell exits cleanly.

### Signal markers

- `l2_sigint_delivered`: foreground sleep, VINTR byte; sleep exits
  with SIGINT.
- `l2_sigtstp_delivered`: VSUSP → SIGTSTP; resume via SIGCONT.
- `l2_sigquit_delivered`: VQUIT → SIGQUIT.
- `l2_sigwinch_delivered`: resize → SIGWINCH to fg pgrp.
- `l2_sigttin_background_read`: background child reads → SIGTTIN.
- `l2_sigttou_background_write`: TOSTOP set, bg child writes →
  SIGTTOU.
- `l2_no_signal_when_isig_clear`: clear `ISIG`; VINTR byte delivered
  as data, no signal.

### Termios markers

- `l2_tcgetattr_default`: fresh pts; `tcgetattr` returns defaults.
- `l2_tcsetattr_raw`: set raw mode; subsequent bytes delivered
  immediately, no echo.
- `l2_tcflush_input`: pending line discarded; subsequent read empty.

### Winsize markers

- `l2_tiocgwinsz_initial`: `ioctl(0, TIOCGWINSZ, &ws)` returns
  non-zero rows/cols.
- `l2_resize_winsize_updated`: programmatic resize; `TIOCGWINSZ`
  returns new dims.

### TERM env markers

- `l2_term_env_cluuterm`: shell under cluuterm sees
  `getenv("TERM") == "xterm-256color"`.
- `l2_term_env_text_vt`: shell on `/dev/tty1` sees
  `getenv("TERM") == "vt100"`.

### No new timeouts proof

`grep -rn "recv_with_timeout\|call_with_timeout"
userspace/cluuterm/src/ userspace/tty/src/` returns same set as today
(no new entries).

### Performance gate

- Keystroke latency (kbd → shell echo back to screen) under 16 ms p99
  (one frame).
- `cat /var/images/coreutils/manifest.toml` throughput unchanged from
  pre-spec-2 baseline.

### Documentation

This file landed at
`docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`;
referenced from `docs/ROADMAP.md` and `docs/CURRENT_PHASE.md`.

### Spec 1 dependency

Verb labels 100-110 reserved; do not conflict with spec 1's 80-81.
`Vec<(String, String)>` env field of `SpawnEnvelope` accepts
`("TERM", value)` — already in spec 1.

## 13. Open follow-ups (out of spec 2)

- `tcdrain` / `tcsendbreak` C-runtime shims (use is rare; defer).
- Mouse-mode escape sequences (DECSET 1000+); add when a TUI program
  in the tree needs them.
- Replace `userspace/tty/` entirely with a `cluuterm-text` variant
  rendering through framebuffer text mode — only if text-VT niche
  shrinks to zero (Wayland-style endpoint).
- `cluu-256color` terminfo entry when behavior deviates from xterm
  enough to require it.

## 14. Related memory

- `[[no-timeouts]]` — cap-revocation on service death; no
  `recv_with_timeout`.
- `[[unified-process-model-decision-2026-05-18]]` — procmgr as sole
  process owner; service-side state is service's own.
- `[[vfs-view-caps-monotone]]` — per-session `/dev/pts/` overlay is a
  narrowing, not a widening.
- `[[mount-policy]]` — existing mount-policy machinery is what
  per-session overlay extends.
- `[[phase4-plan-d-todos]]` — Ctrl-Z/SIGTTIN deferred; closed by
  spec 2.

## 15. Related committed work

- `1a8c218` docs(spawn-window-pty): inventory of current pipeline.
- `9ac4b12` shell: skip TTY-service IPC in cluuterm/pts mode (the
  branch this spec deletes).
- `9b982c4` rename FDAC → FdInherit (used by spec 1; pts inherits via
  FdInherit in spec 2).
- `da8da75` libcluu/registry: drop 2 s subscribe timeout (matches the
  cap-revocation discipline spec 2 honors).
