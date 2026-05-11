# cluuterm — terminal emulator design (2026-05-11)

**Status:** Draft for implementation.
**Sub-project:** B of the TUI compositor workstream
(A = compositor, already shipped — see `2026-05-10-tui-compositor-design.md`;
C = PS/2 mouse; D = `libtui` primitives extraction).

## 1. Summary

`cluuterm` is a terminal-emulator binary that runs as a compositor window and
hosts a single child process (the cluu shell by default). It registers a
per-instance `/dev/pts/<id>` node in VFS, so its child opens it as a tty
device file using the same code path as legacy `/dev/tty<N>` services. cluuterm
parses ANSI/CSI output from the child, blits cells to its window SHM, and
forwards compositor keystroke events back to the child as xterm-style byte
sequences.

The boot configuration is changed so that **VT4 is owned exclusively by the
compositor**, the active VT at boot is VT4, and the user lands on a cluuterm
window showing the login prompt. VTs 1-3 remain legacy text VTs.

Shared logic between legacy tty and cluuterm is extracted into two new
libraries: `libcluu::ansi` (state-machine parser, currently inline in
`userspace/console/src/renderer.rs`) and `libcluu::tty_core` (line discipline,
scrollback ring, keymap, currently inline in `userspace/tty/`).

A new `/bin/login` binary replaces the inline login UI currently embedded in
the legacy tty service.

## 2. Goals & non-goals

**Goals (v1):**

- Single binary `cluuterm` runs in a compositor window and hosts the cluu shell.
- One cluuterm process = one shell = one window. Multiple windows = multiple
  cluuterm processes.
- Full cooked-mode line discipline (ICANON, ECHO, ^C / ^Z / ^D, CRLF) plus
  raw-mode flip via `tcsetattr` so `edit`, `less`, MicroPython REPL work.
- xterm-compatible CSI key encoding for arrows, Home, End, Delete (extend to
  F-keys later).
- Scrollback ring reused from legacy VT (Phase L).
- Boot lands on VT4 → compositor → cluuterm → login prompt.
- `/bin/login`, on auth success, `posix_spawn`s the user's shell and exits;
  cluuterm tracks pts refcount, not child PIDs.

**Non-goals (v1):**

- Tabs / multiple shells per cluuterm window.
- Dynamic window resize.
- Graphical login screen.
- Clipboard / paste, mouse selection.
- xterm DECCKM cursor-key-mode and other mode switches beyond cooked/raw.
- F-keys (extend keymap later).

## 3. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                       compositor                            │
│  (window mgr, framebuffer blit, kbd routing per spec A)     │
└──┬───────────────────────────────────────────▲──────────────┘
   │ COMP_WIN_REGISTER, FRAME_READY            │ INPUT_FORWARD
   │ SHM cells                                 │ (key events)
┌──▼───────────────────────────────────────────┴──────────────┐
│                    cluuterm <binary>                        │
│   - registers /dev/pts/<id> in VFS                          │
│   - allocates SHM, draws cells via libcluu::tty_core        │
│   - ANSI parses child output via libcluu::ansi              │
│   - line discipline (cooked/raw) via libcluu::tty_core      │
│   - keymap (xterm CSI) via libcluu::tty_core                │
│   - spawns /bin/login as initial child                      │
└──┬─────────────────────────────────────────▲────────────────┘
   │ open /dev/pts/<id>, dup2 → fd 0/1/2     │ read/write
   │                                          │ (vfs routes
┌──▼──────────────────────────────────────────┴──────────────┐
│   /bin/login → posix_spawn(/bin/sh) → exits                │
│                shell holds pts fds via dup2 inheritance    │
└─────────────────────────────────────────────────────────────┘
```

**Crate layout changes:**

| Crate                        | State    | Role                                         |
|------------------------------|----------|----------------------------------------------|
| `userspace/cluuterm/`        | **new**  | Terminal emulator binary                     |
| `userspace/login/`           | **new**  | `/bin/login` auth UI + spawn shell           |
| `userspace/libcluu/src/ansi/` | **new**  | ANSI/CSI parser, shared by console + cluuterm |
| `userspace/libcluu/src/tty_core/` | **new** | Line discipline, scrollback, keymap          |
| `userspace/tty/`             | modified | Thin binary, depends on `libcluu::tty_core`; login UI removed |
| `userspace/console/`         | modified | Thin binary, depends on `libcluu::ansi`      |
| `userspace/compositor/`      | unchanged | Already serves VT4; Super+N keybind retargets to cluuterm |
| `userspace/vfs/`             | modified | New pts mount + refcount-based PTS_CLOSED notify |
| `userspace/vtmgr/` or boot config | modified | Pin VT4 to compositor; default active VT = VT4 |
| `etc/autostart.toml`         | modified | Add cluuterm autostart on VT4                |

## 4. Components

### 4.1 `libcluu::ansi`

Extracted from `userspace/console/src/renderer.rs:67-1046`.

- `Parser`: state machine with `EscapeState` enum.
- `Parser::feed(&[u8]) -> impl Iterator<Item = Event>`.
- `Event` enum: `Print(char)`, `MoveCursor(row, col)`, `SetAttr(Attr)`,
  `EraseLine(Mode)`, `EraseDisplay(Mode)`, `Scroll(n)`, `SetTitle(String)`, ...
- The consumer applies events to its own cell grid — `libcluu::ansi` has no
  knowledge of the rendering target.

console refactors to consume `Parser` and applies events to its VT cell grid.
cluuterm consumes the same `Parser` and applies events to its SHM cell grid.

### 4.2 `libcluu::tty_core`

Extracted from `userspace/tty/src/line_discipline.rs`, scrollback support,
and `userspace/tty/src/main.rs:257-263` keymap.

- `LineDiscipline`: cooked / raw, ICANON, ECHO, ^C → SIGINT, ^Z → SIGTSTP,
  ^D → EOF, backspace, CRLF translation. `tcgetattr`/`tcsetattr` API.
- `Scrollback`: cell ring buffer + viewport offset. (Phase L code, promoted
  from console's VT into a generic library.)
- `keymap::encode(ascii, scancode, modifiers, extended) -> Option<&'static [u8]>`:
  xterm CSI table. Inherits legacy tty's mapping verbatim:
  `\x1b[A/B/D/C` for arrows, `\x1b[H`/`\x1b[F` for Home/End, `\x1b[3~` Delete.

### 4.3 `userspace/cluuterm/`

- `main.rs`: argv parse (none required in v1 — always spawn `/bin/login`).
  Sequence: WIN_REGISTER → SHM map → PTS_REGISTER → `posix_spawn(/bin/login)`
  with file_actions binding fd 0/1/2 to `/dev/pts/<id>`.
- `tty_backend.rs`: serves the VFS-routed IPC for the pts node.
  Handles PTS_READ (returns bytes from LineDiscipline stdin queue),
  PTS_WRITE (feeds ANSI parser, updates cell grid, emits FRAME_READY),
  PTS_IOCTL (forward `tcsetattr` / `tcgetattr` to LineDiscipline),
  PTS_POLL (data-available query).
- `input.rs`: receives COMP_INPUT_FORWARD from compositor, calls
  `keymap::encode`, pushes result through LineDiscipline.
- `render.rs`: dirty-row tracking → 8×16 glyph blit → SHM frame.
- Default geometry: 80×24 cells × 8×16 pixels = 640×384 pixel window.

### 4.4 `userspace/login/` — `/bin/login`

- Reads username + password from fd 0 (raw bytes via the tty surface; echoes
  username, mask-echoes password).
- Sends auth request to procmgr via existing `LOGIN_REQUEST` IPC.
- On success: `posix_spawn(/bin/sh, ..., auth'd uid + session)`, then `exit(0)`.
- On failure: re-prompt up to `MAX_LOGIN_ATTEMPTS` (v1: unlimited, matches
  legacy behaviour), exit non-zero only on tty close.

The login binary is identical whether spawned by legacy tty (VT1-3) or by
cluuterm (VT4). It does not know which hosted it.

### 4.5 `userspace/tty/` modifications

- Inline login handler (`handle_login_key`, `send_login_request`, login_*
  fields on `TtyContext`) removed.
- tty service now spawns `/bin/login` as its initial child for each VT,
  same way cluuterm does.
- Line discipline, scrollback, keymap moved out and consumed from
  `libcluu::tty_core`.

### 4.6 `userspace/console/` modifications

- `renderer.rs` ANSI state machine deleted; parser comes from
  `libcluu::ansi::Parser`. Cell-grid application stays.
- `backend.rs` (framebuffer blit) unchanged.

### 4.7 VFS pts namespace

- New backend (`vfs/pts.rs`) for `/dev/pts/`.
- `PTS_REGISTER(owner_tid)` → returns assigned id (small int pool, v1 cap 32).
- Per-pts node records owner_tid + refcount of open fds.
- `read`/`write` IPC routed to owner cluuterm.
- On last close (refcount → 0), VFS emits `PTS_CLOSED_LABEL` to owner.
- On owner death (procmgr notifies), VFS auto-unregisters the node.

### 4.8 VT4 / compositor boot integration

- `vtmgr` (or boot config) pins **VT4 to compositor**: compositor's vt slot
  is explicit, not order-dependent. VT1-3 remain legacy text VTs with their
  own tty service + console renderer.
- Default **active VT at boot = VT4**.
- `etc/autostart.toml` adds a `cluuterm` service after `compositor`, so the
  first window the user sees is cluuterm with the login prompt.
- Existing compositor keybind **Super+N** is retargeted from `compdemo`
  to `cluuterm` (one-line patch in compositor's hotkey table).
- Ctrl+Alt+F1..F3 still switch to legacy text VTs and back via Ctrl+Alt+F4.

## 5. Data flow

### 5.1 Startup

```
1. boot: compositor on VT4 (pinned), active VT = VT4.
2. autostart spawns cluuterm.
3. cluuterm → compositor: WIN_REGISTER(640x384, title="cluuterm")
                       ← window_id + SHM fd.
4. cluuterm: mmap SHM, init 80x24 blank grid, FRAME_READY.
5. cluuterm → vfs: PTS_REGISTER(owner_tid=self) → id N.
   vfs publishes /dev/pts/N.
6. cluuterm: posix_spawn("/bin/login", file_actions=[
       open("/dev/pts/N", O_RDWR) → fd3,
       dup2(fd3, 0), dup2(fd3, 1), dup2(fd3, 2), close(fd3) ]).
7. /bin/login: reads "login: " from fd 0, prompts, validates with procmgr,
   on success posix_spawns /bin/sh with auth'd uid + session, exits.
8. shell inherits fd 0/1/2 via dup2 chain through login's spawn.
```

### 5.2 Shell output → window

```
1. shell: write(1, "hello\n", 6).
2. newlib → SYS_WRITE → vfs(/dev/pts/N).
3. vfs forwards to cluuterm: PTS_WRITE_LABEL(bytes).
4. cluuterm tty_backend.write:
       bytes → LineDiscipline.process_output (CRLF, echo-back if cooked-mode
               input loopback applies)
            → libcluu::ansi::Parser.feed → event stream
            → apply events to cell grid + scrollback ring.
5. cluuterm render.rs: dirty rows → 8x16 blit → SHM.
6. cluuterm → compositor: FRAME_READY(window_id).
```

### 5.3 Keystroke → shell

```
1. kbd → compositor: KBD_EVENT.
2. compositor (cluuterm focused): COMP_INPUT_FORWARD(ascii, scancode,
                                                    modifiers, extended).
3. cluuterm input.rs:
       keymap::encode → byte sequence (e.g. ↑ → "\x1b[A").
       LineDiscipline.process_input:
         cooked mode: line buffer + echo;
         raw mode:    passthrough into stdin ring.
4. shell: read(0, buf, 256) → vfs → cluuterm PTS_READ
       → dequeue stdin ring → reply bytes.
```

### 5.4 Shutdown

```
1. shell exits (clean or crash) → procmgr notifies.
2. shell's pts fds close → vfs pts refcount → 0
   (login already exited earlier, after spawning shell).
3. vfs → cluuterm: PTS_CLOSED_LABEL.
4. cluuterm → vfs: PTS_UNREGISTER(N).
5. cluuterm → compositor: WIN_DESTROY(window_id).
6. cluuterm: exit(0). procmgr reaps.
```

## 6. Error handling & edge cases

- **Compositor not up at autostart:** cluuterm's WIN_REGISTER returns a
  timeout / not-found error; cluuterm exits non-zero. Procmgr restart policy
  decides whether to retry. Autostart ordering should make this unreachable.
- **/bin/login fails to spawn:** cluuterm logs and exits; window already
  registered → emits WIN_DESTROY first.
- **Login auth fails:** /bin/login re-prompts indefinitely in v1 (matches
  legacy behaviour). Hard cap deferred.
- **Shell crashes:** same teardown as clean exit (§5.4). Exit code logged.
- **`/dev/pts/<id>` allocation exhausted:** PTS_REGISTER returns error,
  cluuterm exits with diagnostic message. v1 cap = 32 slots.
- **Malformed ANSI input:** parser drops unrecognised CSI silently
  (current console behaviour, preserved).
- **`tcsetattr` raw↔cooked flip mid-stream:** LineDiscipline must flush the
  cooked-mode line buffer on switch. Verify-and-preserve during port.
- **Shell writes faster than render:** vfs PTS_WRITE buffers up to 64 KB,
  then blocks the writer. ANSI parsed per write; render coalesces (one
  FRAME_READY per IPC round, not per byte).
- **Compositor close-request:** compositor → cluuterm `COMP_CLOSE_REQUEST`
  → cluuterm calls `PTS_UNREGISTER` on its node and stops servicing
  PTS_READ / PTS_WRITE → shell's outstanding and future read/write on its
  fd 0/1/2 return `EIO` → shell exits naturally → §5.4. No explicit signal
  delivery, no shell-PID tracking required.
- **VT switch away (user presses Ctrl+Alt+F1..F3):** compositor pauses
  framebuffer blits (`compositor/window_mgr.rs:266`, existing). cluuterm
  keeps ANSI-parsing shell output and updating cell grid; suppresses
  FRAME_READY while compositor is not visible (compositor coalesces on
  return).
- **cluuterm crash mid-session:** shell becomes orphan, procmgr reparents to
  init. VFS pts watchdog (PTS_REGISTER tied to owner_tid) auto-unregisters
  the node when owner dies, so a new cluuterm can re-bind the slot.

## 7. Testing

### 7.1 Harness markers (l2_*)

| Marker                       | What it asserts                                                                    |
|------------------------------|------------------------------------------------------------------------------------|
| `l2_cluuterm_smoke`          | Boot, VT4 active, compositor up, cluuterm spawned, login prompt rendered in SHM    |
| `l2_cluuterm_login`          | SENDKEY_SEQUENCE user + password, shell prompt rendered                            |
| `l2_cluuterm_ansi`           | After login, `printf '\033[31mred\033[0m'` produces red attr cells                 |
| `l2_cluuterm_keymap`         | Arrow / Home / End / Delete reach shell as xterm CSI bytes (echoed back)            |
| `l2_cluuterm_exit`           | `exit` in shell → pts unmounted, WIN_DESTROY emitted, cluuterm process gone        |
| `l2_cluuterm_two_windows`    | Super+N twice → 2 cluuterm processes + 2 pts nodes; both render independently      |
| `l2_cluuterm_raw_mode`       | `/bin/edit` inside cluuterm: line discipline does not echo, raw bytes reach edit   |
| `l2_vt4_default`             | After boot, active VT == 4, compositor visible with cluuterm login prompt          |
| `l2_vt_legacy_preserved`     | Ctrl+Alt+F1 → legacy text login on VT1; Ctrl+Alt+F4 returns to cluuterm            |

### 7.2 Component-level Rust tests

- `libcluu::ansi::Parser` — golden byte-stream → expected event sequence tables.
- `libcluu::tty_core::keymap::encode` — input event → byte sequence assertions.
- `libcluu::tty_core::LineDiscipline` — cooked / raw flip + buffer flush invariants.

### 7.3 Perf ratchet

- `b_cluuterm_blit` — cycles-per-frame baseline (cell grid → SHM blit).
  Style and infrastructure mirror `b_compositor_blit` (`scripts/perf_ratchet.json`).

### 7.4 Regression guards

- `l2_compositor_smoke` and `l2_compositor_legacy_vt` still pass after
  cluuterm + VT4 changes land.
- `l2_tty_login` still passes after legacy tty refactor onto
  `libcluu::tty_core` and `/bin/login` adoption.

## 8. Decisions log

| # | Decision | Choice |
|---|----------|--------|
| 1 | Scope | Shell-only default. `cluuterm` always spawns the cluu shell. Generic-launcher mode is a later flag. |
| 2 | TTY model | cluuterm IS a tty service instance, registers `/dev/pts/<id>` in VFS, child opens it like legacy `/dev/tty<N>`. |
| 3 | Code factoring | F1: extract `libcluu::ansi` + `libcluu::tty_core`; two thin binaries (tty + cluuterm) share both. |
| 4 | pts node naming | `/dev/pts/<id>` with id from a small int pool (v1 cap 32). Legacy stays at `/dev/tty<N>`. |
| 5 | Termios subset | Cooked default + full `tcsetattr` flip to raw. Reuse line discipline verbatim. |
| 6 | Child exit | Shell exits → tear down window + cluuterm exits. No respawn, no -hold in v1. |
| 7 | Scrollback | Reuse legacy VT scrollback ring, promoted to `libcluu::tty_core::scrollback`. |
| 8 | Multi-shell | One cluuterm = one shell = one window. Multi-window via N cluuterm processes. |
| 9 | Key mapping | xterm CSI sequences; reuse legacy tty's mapping verbatim (already xterm-compatible). F-keys deferred. |
| 10 | Window size | Fixed 80×24 cells × 8×16 px = 640×384 px. Resize deferred. |
| 11 | VT4 / compositor boot | (a) pin compositor to VT4 + (b) boot active VT = VT4 + (c) cluuterm autostarted on VT4 with login prompt. VT1-3 stay legacy. |
| 12 | Login | New `/bin/login` binary replaces inline login UI in legacy tty. login spawns shell via `posix_spawn` with auth'd uid, then exits. cluuterm tracks pts refcount instead of child PIDs. |

## 9. Future work (out of scope for v1)

- Dynamic window resize + reflow.
- `argv` flag `cluuterm <binary> [args]` for non-shell hosts.
- Tabs / multi-shell per window.
- xterm DECCKM cursor mode + extra termios flags (NL, ONLCR, IXON, etc.) beyond
  current legacy subset.
- F-key mapping (F1..F12).
- Graphical login screen in the compositor (needs widget library — sub-project D).
- Clipboard / paste / mouse selection (needs mouse — sub-project C).
- Hard cap on `MAX_LOGIN_ATTEMPTS`.
- Replace per-process pts cap (32) with a dynamic allocator.

## 10. Related documents

- `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` — sub-project A.
- `docs/CURRENT_PHASE.md` / `docs/ROADMAP.md` — phase context.
