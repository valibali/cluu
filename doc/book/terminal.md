# Terminal Stack

CLUU's terminal stack is entirely in userspace. The kernel knows nothing about
keyboards, mice, terminals, or framebuffers — it just provides the IRQ dispatch
and memory mapping primitives.

## Stack overview

```text
  ┌──────┐  scancodes   ┌──────┐  events  ┌───────┐  route  ┌──────────┐
  │ kbd  │─────────────→│      │─────────→│       │────────→│ tty (VT1)│
  └──────┘              │      │          │       │         │ (cooked) │
                        │      │          │       │         └──────────┘
  ┌──────┐  packets     │ vtmgr│          │input_ │         ┌──────────┐
  │ mouse│─────────────→│      │          │routing│────────→│ console  │
  └──────┘              │      │          │       │         │ (VT2/3)  │
                        └──────┘          │       │         └──────────┘
                                          │       │         ┌──────────┐
                                          └───────┘────────→│compositor│
                                                         4) │ (VT4)    │
                                                           └────┬─────┘
                                                                │
                                                          ┌─────┴─────┐
                                                          │ cluuterm  │
                                                          │ (window)  │
                                                          └─────┬─────┘
                                                                │ pts
                                                                ▼
                                                          ┌──────────┐
                                                          │  shell   │
                                                          └──────────┘
```

## kbd — PS/2 keyboard driver

`userspace/kbd/src/main.rs`

Reads scancodes from the PS/2 controller (scancode set 2), decodes them via the
HU QWERTZ keymap, and forwards key events to `vtmgr:input`.

- **Layout**: Hungarian QWERTZ. Direct US scancodes produce wrong characters;
  the HU map is in `layout.rs`. Includes AltGr dead-key support.
- **VT switch**: Ctrl+Alt+F1..F5 sends `VTMGR_REQUEST_VT_SWITCH_LABEL`.
- **Shutdown**: Ctrl+Alt+Del triggers system shutdown.
- **Scrollback**: Shift+PageUp/Down scrolls the console buffer.
- **IPC**: Listens on `KBD_RAW_LABEL` (IRQ1). Sends `KBD_EVENT_LABEL` to
  `vtmgr:input`.

Modules: `context` (KbdContext), `layout` (keymap), `protocol` (labels),
`scancode` (set 2 decoding).

## usb-input — USB HID keyboard/mouse driver

`userspace/usb-input/src/main.rs`

Owns the full EHCI stack (PCI probe, reset, enumeration, interrupt IN polling).
Translates USB HID boot-protocol keyboard reports to PS/2 scancodes + ASCII,
then forwards key events to `inputd:input` using the same `KBD_EVENT_LABEL`
wire format as `kbd`.

- **Layout**: Hungarian QWERTZ (same tables as `kbd`, with AltGr support via
  `MOD_ALTGR` bit). HID usage → PS/2 scancode translation in `layout.rs`
  preserves compatibility with the compositor's PS/2-scancode hotkey matcher.
- **Nav keys**: HID usages 0x49–0x52 (Insert/Home/PgUp/Delete/End/PgDn/
  arrows) are mapped to extended key codes (KEY_UP..KEY_PAGE_DOWN) and
  forwarded via `words[4]` — the same encoding `kbd` uses. The compositor
  and tty decode these via `encode_extended()` into xterm CSI sequences.
- **VT switch / shutdown**: Ctrl+Alt+F1..F5 and Ctrl+Alt+Del are intercepted
  before forwarding (same as `kbd`).
- **Key repeat**: USB-HID keyboards report current key state, not
  press/release transitions. Unlike PS/2 (hardware typematic), USB-HID
  needs software repeat. `handle_kbd_report` tracks the held key and its
  press timestamp via TSC (`clock_now` / `clock_frequency` capability
  token invokes — no IPC roundtrip). After 500ms initial delay, repeats
  at 50ms intervals (20 repeats/sec). Ctrl+Alt+key shortcuts (VT switch,
  shutdown) are exempt — they never auto-repeat. Key release clears
  repeat state.
- **IPC**: Sends `KBD_EVENT_LABEL` to `inputd:input`. Mouse reports sent as
  `MOUSE_EVENT_LABEL`.

Modules: `context` (UsbInputContext, registry wiring), `layout` (HID→PS/2
scancode + HU keymap).

## mouse — PS/2 mouse driver

`userspace/mouse/src/main.rs`

Reads 3-byte PS/2 mouse packets from IRQ12, reassembles them, and forwards
mouse events to `vtmgr:input`.

- **IPC**: Listens on `KBD_RAW_LABEL` (IRQ12 shared with kbd). Sends
  `MOUSE_EVENT_LABEL` to `vtmgr:input`.

Modules: `context` (MouseContext), `packet` (3-byte reassembly), `protocol`.

## vtmgr — VT manager

`userspace/vtmgr/src/main.rs`

Manages virtual terminals. Routes input events to the active VT's service
(either `tty` for text VTs or `compositor` for VT4).

- **VTs 1–3**: text VTs, owned by `tty` + `console`.
- **VT4**: owned by `compositor`.
- **Input routing**: `input_routing.rs` — routes `KBD_EVENT_LABEL` /
  `MOUSE_EVENT_LABEL` to the active VT's service.
- **VT switch**: `VTMGR_REQUEST_VT_SWITCH_LABEL`, `VTMGR_PIN_VT_LABEL`.
- **Console spawn**: `CONSOLE_CREATE_VT_LABEL` — spawns a console instance for
  a new VT.

Modules: `context` (VtmgrContext), `input_routing`.

## console — framebuffer text renderer

`userspace/console/src/main.rs`

Renders text to the GPU framebuffer (not legacy VGA). Used by text VTs (1–3).

- **Font**: 0xProto Nerd Font v2.502 (OFL-1.1). Rasterized at build time by
  `libcluu/build.rs` using `fontdue` into three 8-bit alpha glyph banks
  (Regular, Bold, Italic) — 256 glyphs × 128 bytes each = 32 KiB per variant.
  Font size 13.5pt for optimal 0xProto stroke alignment to the 8×16 grid.
  Gamma-correct alpha compositing via `blend_alpha_row`: fg/bg are converted
  to linear light via compile-time sRGB LUTs, blended, then converted back
  to sRGB. This makes glyph edges appear sharper than linear-in-sRGB
  blending. Original VGA CP437 box-drawing/block-element glyphs (0xB0–0xDF)
  preserved in `FONT_CP437_BOXES` for consistent border rendering. Arc
  corners (╭╮╰╯) and thinned box verticals override the font for 1px stroke
  consistency. Italic is supported end-to-end: `Attr.italic` → SGR 3/23 →
  4-bit packed attrs → compositor selects italic glyph bank.
- **Glyph atlas**: pre-rendered glyphs blitted to the framebuffer via SIMD.
- **Double buffering**: front/back buffers; flip on damage.
- **Framebuffer**: via `/dev/fb0` (opened through VFS).
- **IPC**: `CONSOLE_WRITE_LABEL`, `CONSOLE_WRITE_VT_LABEL`,
  `CONSOLE_SWITCH_VT_LABEL`, `CONSOLE_ACTIVATE_LABEL`, `CONSOLE_DEACTIVATE_LABEL`,
  `CONSOLE_CREATE_VT_LABEL`, `CONSOLE_SCROLL_VT_LABEL`, `CONSOLE_FB_INFO_LABEL`.
- **Endpoints**: per-VT write (`vt:0`, `vt:1`, ...), `control`, registry.

Modules: `backend` (backend trait + double-buffering), `backend/simd` (SIMD
backend), `protocol` (wire protocol), `renderer` (text grid renderer),
`simd` (SIMD primitives).

## tty — legacy text-VT terminal service

`userspace/tty/src/main.rs`

One per text-VT. Provides cooked-mode line discipline, echoes to the console,
and delivers stdin to processes.

- **Modes**: `TtyMode::Login` (Username/Password/Authenticating) and
  `TtyMode::Terminal`.
- **Cooked mode**: `LineDiscipline` — ICANON, ECHO, Ctrl-C → SIGINT, Ctrl-Z →
  SIGTSTP, Ctrl-D → EOF.
- **IPC**: `TTY_REGISTER_LABEL`, `TTY_CTL_LABEL`, `TTY_READ_REQUEST_LABEL`,
  `TTY_POLL_QUERY_LABEL`, `PTS_READ_LABEL`, `PTS_WRITE_LABEL`,
  `PROCMGR_PG_SIGNAL_LABEL`.

Modules: `context` (TtyContext), `protocol`.

## compositor — TUI window compositor

`userspace/compositor/src/main.rs`

Owns VT4. Draws floating windows with rounded Unicode chrome, dispatches
keyboard input to the focused window, and exposes a shared-memory cell-grid
protocol for native apps.

- **Windows**: 1 app = 1 window (v1). `WIN_REGISTER`, `WIN_DAMAGE`,
  `WIN_DESTROY`.
- **SHM cell-grid**: clients share a memory region via `MAP_SHARE_PHYS`; the
  compositor reads cell grids and composites them.
- **Input**: keyboard events from vtmgr, routed to focused window.
- **Hotkeys**: focus next/prev, move, resize, close, spawn cluuterm.
- **Status bar**: clock (subscribes to `TIME_TICK_LABEL`), session info.
- **Session handoff**: `COMPOSITOR_SESSION_HANDOFF_LABEL`, `SESSION_ENDED_LABEL`.

Modules: `compose` (composition loop), `config` (palette, keybindings),
`hotkeys` (hotkey dispatch), `protocol` (wire protocol), `render` (framebuffer
blit), `shm` (SHM cell-grid), `state` (compositor state), `status` (status
bar), `window_mgr` (window management).

## cluuterm — graphical terminal emulator

`userspace/cluuterm/src/main.rs`

Runs as a compositor window. Hosts a single child process (shell by default).
Registers a per-instance `/dev/pts/<id>` node in VFS. Parses ANSI/CSI output
from the child, blits cells to its window SHM, and forwards compositor
keystrokes back to the child as xterm-style byte sequences.

- **Cooked mode + raw mode**: `tcsetattr` flips between cooked (ICANON, ECHO,
  signals) and raw (for `edit`, MicroPython REPL).
- **xterm-compatible CSI key encoding**: arrows, Home, End, Delete.
- **Boot flow**: register window → register pts → spawn shell → run.

Modules: `render` (ANSI/CSI parsing → cell grid blit), `tty_backend` (Pts
handler — PTS_READ_LABEL, PTS_WRITE_LABEL, line discipline, stdin buffering).

## edit — vi-like modal text editor

`userspace/edit/src/main.rs`

TUI app running in a compositor window. Modal: Normal, Insert, Visual,
Command-pending, Ex.

- **TTY raw mode** via `tcsetattr`.
- **Reads stdin** via `TTY_READ_LABEL`, renders via CSI escapes to stdout
  (`TTY_WRITE_LABEL`).
- **VFS** for file load/save.

Modules: `buffer`, `ex` (ex-mode), `help`, `input`, `insert`, `mode` (state
machine), `motion` (cursor motion), `normal` (normal mode), `op_pending`,
`ops` (delete/yank/paste), `piece` (piece table), `prompt`, `render`, `search`,
`settings`, `tty`, `undo`, `vfs_io`, `visual`.

## libcluu terminal support

- **`ansi/`** — ANSI/CSI escape sequence parser. `state.rs` (state machine),
  `event.rs` (CsiEvent, AnsiEvent).
- **`tty_core/`** — shared terminal core. `keymap.rs`, `line_discipline.rs`
  (cooked mode, Ctrl-C/Ctrl-Z/Ctrl-D), `routing.rs` (input routing),
  `scrollback.rs` (scrollback ring buffer).

## TUI compositor design (2026-05-10)

A userspace compositor service owns VT4 and draws floating windows with
rounded Unicode chrome, dispatches keyboard input to the focused window,
and exposes a shared-memory cell-grid protocol for native apps. Legacy
console + TTY + VT manager keep running unchanged on VT1-3. Pure
userspace — all needed kernel primitives already exist
(`MAP_DEVICE_WC`, `FrameAllocate`/`FrameFree`, `MAP_FRAME_TOKEN`,
`PROC_EXIT_LABEL`).

- **Cell payload**: one packed `u64` — 21 bits codepoint, 8 bits fg_index,
  8 bits bg_index, 3 bits attrs (bold/underline/reverse), 24 reserved.
  Compositor holds one `[u32; 256]` palette of ARGB values (xterm-256).
- **SHM window region**: per-window, page-aligned. `WindowShm` header
  (magic, version, width, height, cursor_x/y/visible, generation) +
  `[u64]` cells. App writes cells → bumps `generation` (release-store) →
  sends `WIN_DAMAGE`. Compositor reads `generation` (acquire-load)
  before/after blit. Lockless — every cell write is atomic at hardware
  level (8-byte aligned on x86_64).
- **Three IPC endpoints**: `compositor:client` (apps register/damage/
  destroy windows), `compositor:input` (kbd routes raw key events),
  `compositor:control` (vt manager + init send VT activate/deactivate).
- **Chrome (Tier 3 — 2×2-cell rounded corners)**: each window reserves
  chrome cells (top 2 rows, bottom 2 rows, left/right 2 cols). 16
  unique custom 8×16 corner bitmaps slotted into CP437 `0xF0..0xFF`.
  Minimum window 5×5 cells (1×1 interior).
- **Hotkeys**: Alt+Tab focus next, Super+Arrow move, Super+Shift+Arrow
  resize, Super+Q close-request, Super+N spawn app.
- **Compositor death**: init monitors as per-VT primordial under
  `OnFailure` restart. All clients lose windows; init re-broadcasts
  `compositor_ready` registry event so clients re-register. No state
  persisted across restarts.

## cluuterm design (2026-05-11)

`cluuterm` is the terminal-emulator binary that runs as a compositor
window and hosts a single child process (the cluu shell by default).
One cluuterm process = one shell = one window; multiple windows = multiple
cluuterm processes. Registers a per-instance `/dev/pts/<id>` node in VFS
so its child opens it as a tty device file using the same code path as
legacy `/dev/tty<N>`.

- **Code factoring**: `libcluu::ansi` (state-machine parser, extracted
  from console) + `libcluu::tty_core` (line discipline, scrollback ring,
  keymap, extracted from tty) — both shared by console + cluuterm +
  tty.
- **Boot flow**: VT4 pinned to compositor, default active VT = VT4,
  cluuterm autostarted. User lands on cluuterm window showing the login
  prompt. VT1-3 stay legacy text VTs.
- **`/bin/login`**: new binary replaces inline login UI in legacy tty.
  Login spawns shell via `posix_spawn` with auth'd uid + session, then
  exits. cluuterm tracks pts refcount, not child PIDs. Login is
  identical whether spawned by legacy tty (VT1-3) or cluuterm (VT4).
- **Default geometry**: 80×24 cells × 8×16 px = 640×384 px. Fixed in v1;
  resize deferred.
- **xterm-compatible CSI key encoding**: arrows, Home, End, Delete.
  F-keys deferred.

### v2 uplift (2026-05-18)

After v1 landed, live-fire `htop`/`top` runs revealed: `ps` renders
correctly; `top`/`htop` print partial letters, scrolling broken, Ctrl-C
cannot interrupt (`cluuterm: Ctrl-C (signal dropped in v1)`). v2 brings
cluuterm to minimum viable terminal for ncurses-style TUI apps:

- **Signals**: Ctrl-C → SIGINT, Ctrl-Z → SIGTSTP, Ctrl-\ → SIGQUIT via
  `procmgr PTS_KILL_FG` to foreground pgrp. Raw mode passes byte
  through — `enter_raw()` in `libcluu/src/posix/tty.rs` clears ISIG
  (`TTY_LFLAG_ISIG = 0x01`) in addition to ICANON and ECHO. The line
  discipline (`libcluu/src/tty_core/line_discipline.rs:feed_byte`) checks
  ISIG **before** the canonical/raw split, so 0x03 is intercepted as
  SIGINT even in raw mode unless ISIG is explicitly cleared. TUI apps
  (edit, top) handle 0x03 as a normal key event after `enter_raw()`.
- **TERM env**: cluuterm publishes `TERM=xterm-256color`; tty service
  `TERM=vt100`. Propagated via `SpawnEnvelope.env`.
- **TIOCGWINSZ + SIGWINCH**: `ioctl(TIOCGWINSZ)` on pts fds returns
  current SHM cell grid dims. `PTS_SET_WINSIZE` emits SIGWINCH to fg
  pgrp.
- **ANSI parser extensions**: `CursorVisible(bool)` (`CSI ? 25 l/h`),
  `AltScreen(bool)` (`CSI ? 1049 l/h`), `SetScrollRegion(t,b)` (`CSI r`),
  `CursorSave/Restore`, 256-color + truecolor SGR, `OSC 0;<title>BEL`
  set-title, `AppCursorKeys(bool)` (DECCKM), `AutoWrap(bool)` (DECAWM),
  `Reset` (RIS), `InsertLine/DeleteLine`, `DeleteChar/InsertChar`.
- **Alt-screen buffer**: two independent cell grids + pointer-switch on
  `AltScreen` event. Scrollback wired only to main grid.
- **Scrollback**: `libcluu::tty_core::Scrollback` ring (default 1024
  lines × 80 cols). Shift+PgUp/PgDn handled in cluuterm `input.rs`
  before keymap encoding — never reaches child.

## Compositor menus + app registry (2026-05-12)

Replaces the compositor's static status bar with a Mac-style menu bar.
The compositor row 0 stops being a clock+window-count strip and becomes
the OS's primary discoverability surface.

- **Leftmost**: a system menu (labeled `CLUU`) that opens on F1.
  Hardcoded entries: About, Lock (v1 no-op), Apps submenu, Reboot,
  Poweroff.
- **Rest of row 0**: menu items declared by the currently focused
  window via `WIN_MENU_SET` IPC (sent right after `WIN_REGISTER`
  reply). Max tree depth 3, max 64 total entries, max payload 4 KiB.
  Rejected after first call in v1.
- **Apps submenu**: populated at compositor startup from
  `/var/manifests/apps.toml`, build-time-generated from each binary's
  Cluufile via a new `APP <category-path> "<label>"` directive. xtask
  walks `containers/*/Cluufile`, parses `APP`, sorts by
  `(category, label)`, emits the manifest. No `APP` = binary is not
  user-facing (default for infrastructure containers). Build = install.
- **Action dispatch**: `SpawnApp` → compositor IPCs procmgr
  `PROCMGR_CONTAINER_RUN_LABEL`; `WindowCommand { cmd_id }` →
  `INPUT_FORWARD kind=menu` to focused window; `System(...)` →
  compositor handles internally.
- **Keyboard navigation only** (mouse deferred to sub-project C). F1
  toggles system menu, F10 opens focused-window's first menu, Esc
  closes, arrows navigate, Enter activates. When a menu is open, no
  key event reaches windows.
- **About dialog**: 40×8 modal window owned by compositor (no client,
  drawn directly). Shows build hash + kernel date. Modal eats Esc to
  dismiss.

## Terminal + PTY unification (2026-05-18)

Two terminal protocols coexisted without convergence: legacy TTY service
(`TTY_REGISTER_LABEL`, `TTY_CTL_LABEL`, `TTY_READ_REQUEST_LABEL`, etc.)
and cluuterm (`PTS_READ_LABEL`, `PTS_WRITE_LABEL`, `PTS_CLOSED_LABEL`).
Shell branched on context. Cluuterm had zero of cooked mode, line
discipline, Ctrl-C → SIGINT, Ctrl-Z, Ctrl-D, `tcgetattr`/`tcsetattr`,
winsize ioctl, TERM env. Users typed Ctrl-C dozens of times to interrupt
hung commands and every keystroke logged `cluuterm: Ctrl-C (signal
dropped in v1)`.

The unification spec defines **one verb set** (`PTS_*`, labels 100-110)
spoken by both `userspace/tty/` and `userspace/cluuterm/`. Shell uses
one set regardless of which it talks to. Key decisions:

- **Shared line-discipline library** at
  `libcluu/src/tty_core/line_discipline.rs` imported by both services.
  Per-pts state lives inside the service that owns the pts; no
  cross-service state. `LineDiscOutput` enum: `Bytes`, `Signal`,
  `Echo`, `Eof`, `Drop`. Service-side `route_input_byte` translates to
  `ServiceAction`.
- **Full POSIX terminal signal set**: SIGINT (Ctrl-C), SIGTSTP (Ctrl-Z),
  SIGQUIT (Ctrl-\), SIGWINCH (resize), SIGTTIN (bg read), SIGTTOU (bg
  write with TOSTOP). c_cc keys: VEOF, VERASE, VKILL, VWERASE, VINTR,
  VQUIT, VSUSP. Signal delivery uses existing `PROCMGR_PG_SIGNAL`
  mechanism — kernel knows nothing of terminals.
- **POSIX termios surface**: `tcgetattr`/`tcsetattr`/`tcflush` +
  `ioctl(TIOCGWINSZ|TIOCSWINSZ|TIOCGPGRP|TIOCSPGRP)` all functional.
  Default termios: `ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL |
  IEXTEN`, OPOST+ONLCR, ICRNL+BRKINT.
- **Per-session `/dev/pts/` namespace**. Cross-session pts access
  denied (ENOENT). `/dev/tty1..3` stays in global namespace
  (boot/recovery). VFS view derive substitutes `/dev/pts/` with a
  session-private MemFs overlay when `envelope.session = Some(token)`.
- **TERM env propagation**: cluuterm spawns with `TERM=xterm-256color`;
  tty service with `TERM=vt100`.
- **Resize → SIGWINCH** to fg pgrp. Cluuterm's `WIN_CONFIGURE` handler
  recomputes cols/rows; if changed, calls internal `pts.set_winsize`
  which emits SIGWINCH.
- **What dies**: legacy labels `TTY_REGISTER_LABEL`, `TTY_CTL_LABEL`,
  `TTY_SET_FG_LABEL`, `TTY_READ_REQUEST_LABEL`, `TTY_POLL_QUERY_LABEL`;
  shell's `tty_endpoint != 0` branch (commit `9ac4b12`); cluuterm's
  old local `PTS_*` constants (replaced by unified set in
  `cluu_proto::pts`).

## Window protocol formalization (2026-05-18)

Today's compositor protocol has working primitives but informal
semantics: `broadcast_frame_ready` fans out a "render next frame" signal
indiscriminately to every subscriber every tick; buffer ownership is
implicit; clients guess when the compositor is done with a buffer; input
arrives as informal scancodes clients reproduce keymap logic ad hoc;
session-to-surface association is nowhere formalized.

Spec 4 lifts the de-facto Wayland-shape into a formal protocol: 16 labels
(210-226) covering create/destroy/buffer attach-commit-release/
frame-callback/configure/input/focus/closed. Key decisions:

- **Client-owned shared-memory buffers** (typed frames per the frame-typing
  redesign). Zero-copy. Compositor maps via `MAP_SHARE_PHYS`, refcount
  inc/dec on attach/detach.
- **Per-frame request frame-callback** (Wayland-strict).
  `WIN_REQUEST_FRAME_CALLBACK` registers a one-shot callback; compositor
  emits `WIN_FRAME_READY { surface_id, timestamp_ms }` once after the
  next render tick that includes this surface; then forgets. Idle
  clients receive no events. `broadcast_frame_ready` retired.
- **Explicit buffer-release event**. Client may reuse a buffer only
  after `WIN_BUFFER_RELEASED` fires. Buffer state machine per surface:
  `Detached → Attached → Pending → Scanout → ReleasedLocked →
  Attached`. No tearing.
- **Surface-local damage rects** in `WIN_COMMIT`. Empty list = damage-all.
  Damage does NOT accumulate between commits.
- **Pre-translated input events**. Compositor reads keymap; emits
  `KeyEvent { key, modifiers, state, char }`. Pointer + wheel similarly
  pre-shaped. Clients don't reproduce keymap logic.
- **Surface-to-session integration**. Every surface carries
  `session_id: Option<u32>`. `SESSION_ENDED { session_id }` from spec 3
  → compositor `WIN_CLOSED`s matching surfaces. Sessionless surfaces
  (login, status bar) persist across session lifecycles.
- **Per-client async event endpoint** (no global `compositor:input`
  bottleneck). Minted at `WIN_CREATE`; replaces global
  `compositor:input` service.
- **Focus model**: one focused surface per compositor (single-seat).
  Click-to-focus. Synthetic `Released` KeyEvent for each held modifier
  on focus-out.

## Tab completion protocol (2026-07-01)

TAB in the cluuterm shell inserted a literal tab character instead of
completing. Root cause: `LineDiscipline::feed_byte` had no TAB branch
(fell through to default insert+echo), and the legacy
`handle_byte_canonical` that *did* produce a `tab_request` was never
called by cluuterm and explicitly dropped by `userspace/tty/`. The
retired recv-on-stdin-endpoint mechanism meant the shell only saw
complete lines and couldn't react to mid-line TAB.

The fix is a synchronous RPC from cluuterm to the shell's completion
endpoint, handled inline by the shell's async main loop. Key decisions:

- **`LineDiscOutput::TabRequest { line, cursor, consecutive_tabs }`**
  — new variant. TAB branch in `feed_byte` does NOT insert into
  `pending_line`, does NOT echo; emits `TabRequest`. `consecutive_tabs`
  resets to 0 on any non-TAB byte.
- **Async main loop** — the shell uses `libcluu::async_runtime::Runtime`
  with `ipc_recv_any` on `[completion_ep, reply_ep]`. Completion queries
  are handled inline; no separate thread. This replaced the earlier
  pthread approach which had no VFS view and couldn't call
  `vfs.readdir` directly. The async runtime gives the shell's main
  thread full VFS access for lazy cache population.
- **Completion sources** (priority order for bare word with no slash):
  builtin command names → PATH executables → (filenames only when word
  contains a slash). Word WITH slash: filename completion only.
  Directory candidates get trailing `/` so next TAB descends.
- **`SHELL_COMPLETE_QUERY_LABEL = 143`** — single label suffices;
  reply rides the reply_token back. `CompleteRequest { word,
  consecutive_tabs }`, `CompleteReply { candidates, common_prefix }`.
  Shell computes `common_prefix` (longest common prefix beyond typed
  word) so cluuterm's apply step is trivial.
- **Apply logic**: unique prefix → append + echo; single candidate →
  append rest + echo; multiple candidates + single TAB → no-op (wait
  for 2nd); multiple + double TAB → echo `\n` + candidates + `\n` +
  redraw prompt+line; zero candidates → bell (`0x07`).
- **Endpoint discovery**: shell registers `shell:completion:<sid>` at
  startup (namespaced by session id, no collision). cluuterm reads
  `CLUU_SESSION_ID` from spawn env (passed by session-procmgr), looks
  up the endpoint lazily on first TAB, caches it.
- **Lazy directory cache** — `DirCache` (`BTreeMap<String, Vec<String>>`
  + `BTreeSet<String>` pending set, behind `spin::Mutex`). At startup,
  the shell spawns async `readdir` tasks for `/bin`, `/etc`, `/dev`,
  `/tmp`, `/home`, `$HOME`, `/var`, `/var/images`. Cache is populated
  as replies arrive; queries against uncached dirs return empty (not a
  hang). `/var` probes fail silently for non-supervisor views (correct
  capability-scoped behavior). The lock is never held across an
  `.await` — contention is impossible in the single-threaded runtime.
- **Stdin reads** — the shell spawns a continuous async task that
  calls `VfsClient::read_grant_async()` on fd 0 and pushes `StdinRead`
  completions to the runtime. The main loop drains these and processes
  lines. This replaced the blocking `_read(0)` loop.
- **`BuiltinRegistry` lifetime**: built once at startup via
  `Box::leak` and shared by the main loop and completion handler.
- **Bounded timeout**: cluuterm uses `call_with_reply_buf` with a
  timeout; on timeout, bell + continue. Don't hang the terminal if the
  shell is slow.

## Plan lessons — terminal stack

Distilled implementation lessons from the terminal-stack plans. 2-5 lines
each; see the dated plan file for the long form.

### tui-compositor-vt4-ownership (2026-05-10-tui-compositor)

Compositor owns VT4 — registered with the registry as `compositor`, three
IPC endpoints (`client`, `input`, `control`). Per-window SHM region carries
a `WindowShm` header + `[u64]` cells, allocated by the compositor via
`FrameAllocate` and shared via `MAP_FRAME_TOKEN`. Compose pipeline is
single-threaded: cell-level walk top→bottom of z-stack, glyph blit via the
existing atlas, pixel flush via `DoubleBufferBackend`. Legacy console + TTY
+ vt manager keep running on VT1-3 untouched.

### cluuterm-shared-libs (2026-05-11-cluuterm)

ANSI parser and tty internals extracted into `libcluu::ansi` and
`libcluu::tty_core` so console, cluuterm, and tty share *one* ANSI parser
and *one* tty core. `/bin/login` is a separate binary (not inline in
cluuterm). cluuterm hosts the shell behind `/dev/pts/<id>`; VFS gains a pts
namespace. VT4 pinned, default active VT. The lesson: when two services
implement the same parser, one of them is wrong.

### compositor-clock-pushmode (2026-05-13-compositor-clock-pushmode)

The compositor was polling timeserver every loop iteration. Fix: subscribe
once at startup with `period_ms=1000`, block on `recv` (no timeout), wake on
`TIME_TICK` to update the clock. Status-bar clock ticks at true 1 Hz;
per-iteration IPC pressure to timeserver goes to zero. Polling a service
from a render loop is an anti-pattern — subscribe push-mode instead.

### login-as-compositor-client (2026-05-13-login-as-compositor-client)

`/bin/login` becomes a compositor client (not cluuterm's child). Cluuterm
is spawned only post-login by procmgr, bound to the authenticated user
session. Identity propagates from `/bin/login` through procmgr
`SESSION_LOGIN` to cluuterm's shell. The architectural inversion where a
system service (cluuterm) owned the auth flow is removed — auth belongs to
a dedicated binary that exits on success.

### unified-pts-verb-set (2026-05-18-plan2-terminal-pty-unification)

Legacy TTY service and cluuterm unified onto one `PTS_*` verb set (labels
100-110). Shared `libcluu::tty_core::LineDiscipline` linked by both
services; each pts has its own `LineDiscipline` instance. Full POSIX
signal coverage (SIGINT/SIGTSTP/SIGQUIT/SIGWINCH/SIGTTIN/SIGTTOU). Per-session
`/dev/pts/` namespace overlay in VFS, keyed on `session_id`. POSIX
`tcgetattr`/`tcsetattr`/`tcflush`/`ioctl(TIOC*)` shims. `TERM` env
propagation. PTS verbs serialized via postcard.

### window-protocol-formalization (2026-05-18-plan4-window-protocol)

The compositor's informal Wayland-shape protocol formalized into 16 verb
labels (210-226). Client-owned typed-frame buffers (via `MAP_SHARE_PHYS` +
spec 1's frame typing). Per-frame request frame-callback retires
`broadcast_frame_ready`. Explicit `WIN_BUFFER_RELEASED` event. Pre-translated
input events (compositor reads keymap from `/etc/keymap/<layout>.toml`).
`Surface` state machine: `Created → BufferAttached → Mapped → Closing →
Destroyed`. Per-client async event endpoint replaces global
`compositor:input`. Surface `session_id` filled from `WIN_CREATE.session_token`;
`SESSION_ENDED` closes matching surfaces.

### fb-glyph-atlas-and-devfb0 (2026-05-10-fb-atlas-and-devfb0)

The framebuffer perf piece landed as two workstreams: (A) glyph atlas —
precomputed mask template per char, SIMD-friendly `(mask & fg) | (!mask &
bg)` blend, no per-cell bit-by-bit compose; (B) `/dev/fb0` —
`DeviceBackend::Fb` variant, `open` returns device file, `read` returns
geometry, `write` clamps onto front-buffer, `mmap` routes through
`MAP_DEVICE_WC`. TUI compositor scaffold was deferred to a separate plan —
don't bundle independent workstreams.
