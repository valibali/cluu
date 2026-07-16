# Service Catalog

Every userspace service in CLUU, its role, and its IPC labels.

## Boot-critical services (spawned by init)

### init — Primordial Userspace Init (PID 1)

`userspace/init/src/main.rs`

First userspace process. Spawns boot-critical services, monitors primordial
exits. Exit 42 = poweroff, 43 = reboot, other = halt.

- **IPC**: `primordial_exit_recv` endpoint.
- **Modules**: `boot`, `context` (InitContext), `mappings`, `measured_boot`,
  `sealed_storage`, `attestation`, `services` (SERVICE_LIST), `wiring`
  (launch_service).

### registry — Service Registry

`userspace/registry/src/main.rs`

Maps (service, endpoint) names to producer-owned grant endpoints. Central
name→endpoint broker. Producers REGISTER outputs; subscribers SUBSCRIBE;
registry forwards grant requests to producers.

- **IPC**: `REGISTRY_REGISTER_LABEL`, `REGISTRY_UNREGISTER_LABEL`,
  `REGISTRY_LIST_LABEL`, `REGISTRY_SUBSCRIBE_LABEL`,
  `REGISTRY_SUBSCRIBE_REPLY_LABEL`, `REGISTRY_GRANT_REQUEST_LABEL`.
- **Invariants**: sender_tid==0 rejected. Admin tid bootstrapped from first
  REGISTER (procmgr). Owner check enforces producer authority; admin override
  only for replacement.

### root-procmgr — System-scope Process Manager

`userspace/root-procmgr/src/main.rs`

PID 1's child. Owns every session, mints session-scoped caps, runs the
system-wide IPC dispatch loop. SYSTEM cap-set.

See [Process Management](../procmgr/index.html) and
[Session Encapsulation](../sessions/index.html).

### vfs — Virtual Filesystem Service

`userspace/vfs/src/main.rs`

Owns the global filesystem namespace. Runs as root-VFS (system-wide) or
session-VFS (per-login). Routes every `VfsOp` through a unified `MountTable`.

See [Virtual Filesystem](../vfs/index.html).

### virtio-blk — Block Device Driver

`userspace/virtio-blk/src/main.rs`

Userspace virtio-blk driver for QEMU's `virtio-blk-pci`. Exposes
`BlockDevice` trait for ext2.

See [Storage Stack](../storage/index.html).

### virtio-snd — Audio Device Driver

`userspace/virtio-snd/src/main.rs`

Userspace virtio-snd driver for QEMU's `virtio-sound-pci`. Implements the
virtio-snd device protocol (virtio 1.2 §5.14) entirely in userspace — the
kernel provides only IRQ dispatch and PCI config syscalls.

- **PCI probe**: Scans for vendor 1af4:1059, maps capability bars, configures
  4 virtqueues (control, event, TX, RX).
- **Control queue**: `set_params` → `prepare` → `start` → `stop` → `release`
  lifecycle. Self-test runs 16-period TX roundtrip at boot.
- **TX queue (playback)**: PCM data transferred via grant-based shared memory,
  not inline IPC. Each session has 8 pre-allocated ring slots (4KB periods).
  `pcm_start` is deferred until the first `SUBMIT_PCM` to prevent initial
  buffer underrun.
- **Rate enum**: Matches the virtio-snd spec exactly (5512–384000 Hz).
  Non-spec rates (12000, 24000) removed.
- **IPC**: `AUDIO_OPEN_SESSION`, `AUDIO_SUBMIT_PCM` (fire-and-forget, metadata
  only — PCM arrives via grant page), `AUDIO_COMPLETE` (completion callback to
  caller's per-session endpoint), `AUDIO_CLOSE`, `AUDIO_TID_CLEANUP`.
- **Registry**: Published as `snddev:main`.

**QEMU note**: QEMU's virtio-snd implementation ignores `buffer_bytes`/
`period_bytes` for playback — it delegates all buffering to the host audio
backend (PulseAudio). `AUD_write` is rate-limited by actual playback speed.
The driver's `buffer_bytes=32768`/`period_bytes=4096` are stored but not used
by QEMU for TX buffering.

### devmgr — Device Manager

`userspace/devmgr/src/main.rs`

Registers device classes (block, char), brokers derived device/region
capabilities to procmgr and VFS. Sync recv loop — leaf service, no downstream
IPC.

- **IPC**: `DEVMGR_REGISTER_LABEL`, `DEVMGR_REGISTER_CHAR_LABEL`,
  `DEVMGR_GRANT_REGION_LABEL`, `DEVMGR_GRANT_DEVICE_LABEL`,
  `DEVMGR_REVOKE_LABEL`, `DEVMGR_LIST_FOR_ENVELOPE_LABEL`.
- **Modules**: `device` (DeviceClass, DeviceEntry), `dev_registry`
  (DevRegistry, VisibleDevice), `handlers`.

### ramfs — RAM filesystem (STUB, not wired)

`userspace/ramfs/src/main.rs`

The crate `cluu-ramfs` (Cargo.toml description: "CLUU RAM filesystem (initrd)
driver") is built by xtask and staged into the userdisk, implying a working RAM
filesystem service. In practice `main()` is a stub: it loops on
`recv(0, &mut msg, ...)` and discards every message. The TODO comment lists
"Parse TAR initrd, Serve file read requests, Handle directory listings" — none
implemented. VFS owns the initrd directly via `mount_initrd("/dev/initrd",
initrd)` and ext2 via `blkdev`, so ramfs is not wired into the boot sequence
and receives no messages. The crate compiles and ships but does nothing.

### tpmd — TPM 2.0 Daemon

`userspace/tpmd/src/main.rs`

TIS MMIO driver + IPC server for TPM 2.0 operations. Used by login (password
hashing), measured boot (PCR extend), sealed storage, attestation. Stub mode
if no TPM present (all commands return `REPLY_ENODEV`).

- **IPC**: `LABEL_STARTUP`, `LABEL_PCR_READ`, `LABEL_PCR_EXTEND`,
  `LABEL_GET_INFO`, `LABEL_CREATE_PRIMARY`, `LABEL_SEAL`, `LABEL_UNSEAL`,
  `LABEL_CREATE_AIK`, `LABEL_QUOTE`.
- **Invariants**: Locality 0 bracketing. Seal bound to PCR 9 + PCR 14 policy
  digest. SRK handle cached after CreatePrimary.

### timeserver — Time Service

`userspace/timeserver/src/main.rs`

Reads the kernel clock and publishes periodic tick notifications to
subscribers. Deadline-driven loop — no separate timer thread.

- **IPC**: `TIME_GETTIMEOFDAY`, `TIME_GETCLOCK`,
  `TIME_SUBSCRIBE_PERIODIC_LABEL`, `TIME_UNSUBSCRIBE_LABEL`,
  `TIME_TICK_LABEL`.
- **Module**: `subscribers` (SubscriberTable).

#### Push-mode tick API (designed 2026-05-13)

The pull RPC (`TIME_GETCLOCK`/`TIME_GETTIMEOFDAY`) forces consumers into a
recv-with-timeout polling loop. Push-mode eliminates that: subscribers grant
timeserver a **SEND-only token** to their notify endpoint and wake naturally
on every tick. The cap model stays monotone-decreasing — timeserver never
gets broader rights than the subscriber granted.

- **Granularity**: 10 ms (anything finer needs a dedicated HPET/TSC
  scheduler). `period_ms` is rounded up to the nearest 10 ms; max 60 s.
- **One subscription per `(tid, period)` tuple**; re-subscribe with a
  different period replaces the previous slot. Max 64 subscribers.
- **No cumulative drift**: deadlines are anchored from subscribe time +
  N×period, not from "last delivery time."
- **No pile-up**: if a subscriber's recv is slow, timeserver coalesces (at

  most one tick per subscriber per internal loop iteration). The
  `tick_count_since_subscribe` payload lets the subscriber detect missed
  ticks; `now_monotonic_ms` saves an extra RPC.
- **Subscriber death**: kernel revokes the SEND-only token on exit; the
  next send fails; timeserver removes the entry after 3 consecutive
  failures. No leak.
- **Loop**: `recv_any_with_timeout(next_deadline - now)`. On timeout, walk
  subscribers and fire ticks for any whose deadline ≤ now. Purely additive
  — existing pull callers untouched.

## Per-session services (spawned by root-procmgr per login)

### session-procmgr — Per-session Process Manager

`userspace/session-procmgr/src/main.rs`

One per authenticated session. Owns session children, exit cookies, signals,
pipes, process groups. Sub-mints child-scoped caps. Uses async runtime for VFS
`derive_child_fd` during spawn.

See [Process Management](../procmgr/index.html).

### shell — DIY Shell

`userspace/shell/src/main.rs`

Pest grammar parser, Rust executor. Commands: `cd`, `pwd`, `ls`, `cat`,
`echo`, `touch`, `ps`, `top`, `spawn`, `spawnbg`, `jobs`, `fg`, `bg`, `stop`,
`kill`, `sudo`, `su`, `container`, `exit`. Up/down arrow history. fd 0/1/2
arrive via FdInherit at spawn.

- **IPC**: `SHELL_COMPLETION_ANNOUNCE_LABEL` (sent to cluuterm for tab
  completion).
- **Modules**: `commands/` (dispatch, builtins, exec, redirect),
  `completion`, `io`, `path_lookup`, `pipeline`, `shellrc`.

### cluuterm — Graphical Terminal Emulator

`userspace/cluuterm/src/main.rs`

Runs as a compositor window. Hosts a single child process (shell by default).
Registers `/dev/pts/<id>` in VFS. Parses ANSI/CSI, blits cells to window SHM,
forwards compositor keystrokes as xterm-style byte sequences. Cooked + raw mode
via `tcsetattr`.

See [Terminal Stack](../terminal/index.html).

### edit — vi-like Text Editor

`userspace/edit/src/main.rs`

Modal TUI editor running in a compositor window. Normal, Insert, Visual,
Command-pending, Ex modes. TTY raw mode. VFS for file load/save.

See [Terminal Stack](../terminal/index.html).

#### Design (2026-04-29)

A vi-flavored modal editor, ~3000 LOC of `no_std + alloc` Rust. No external
crates — no crossterm, no tui-rs, no regex. All hand-rolled. Targets
day-to-day editing of source and config inside CLUU; closes Phase 2's
"write code in CLUU" loop alongside MicroPython.

- **Buffer**: piece table — two append-only byte buffers (`original` +
  `add`) + an ordered `Vec<Piece>`. Edits never touch text bytes, only the
  pieces list. `add` is never garbage-collected (undo may need it back).
  File size ceiling ~1 MB; worst-case memory ~3.5 MB.
- **Undo**: vim-style coarse grouping — one entry per NORMAL command, one
  per INSERT session (Esc commits), one per visual operator, one per
  `:s`. In-memory only; no persistent `.un~` files in v1.
- **Modes**: Normal, Insert, VisualChar, VisualLine, OperatorPending,
  ExPrompt (`:`/`/`/`?`). Esc-or-Ctrl-[ exits Insert; arrows work in
  Normal; Ctrl-S/Ctrl-Q map to `:w`/`:q`.
- **UTF-8**: byte-safe — load any bytes, navigate by codepoints, render
  non-ASCII as `?`. `h`/`l` advance one codepoint, not one byte.
- **Search**: literal only (no regex). `*`/`#` word-under-cursor,
  `:set hlsearch` overlay, `:set ic` case fold (ASCII only). History ring
  ~50 entries, session-only.
- **Substitute**: `:s/old/new/[g]` literal replace, no backrefs. One undo
  entry per whole substitute.
- **Render**: full-frame redraw every state change (~6 KB/frame, ~180
  KB/s to TTY — trivial). Horizontal scroll default, `:set wrap` toggles
  soft-wrap. Status line in reverse video; message line fades after ~2 s.
- **File I/O**: atomic save via `.edit~` temp + rename (intra-dir, always
  atomic in ext2/MemFs). `:w` against a read-only mount propagates EACCES
  to the status line; buffer stays dirty.
- **Container**: `containers/edit/Cluufile` — `PROFILE ipc vfs`, no
  `MOUNT` directives (inherits parent shell's view). Editing a file in
  `/etc` from a USER envelope fails to save — correct behavior, no
  special handling.

## Terminal stack

### kbd — PS/2 Keyboard Driver

`userspace/kbd/src/main.rs`

Scancode set 2, HU QWERTZ layout. VT switch (Ctrl+Alt+F1..F5). Shutdown
(Ctrl+Alt+Del). Scrollback (Shift+PageUp/Down).

See [Terminal Stack](../terminal/index.html).

### mouse — PS/2 Mouse Driver

`userspace/mouse/src/main.rs`

3-byte PS/2 packet reassembly. Forwards to `vtmgr:input`.

### vtmgr — VT Manager

`userspace/vtmgr/src/main.rs`

Manages VTs 1–3 (text) + VT4 (compositor). Routes input to active VT's service.

### console — Framebuffer Text Renderer

`userspace/console/src/main.rs`

Glyph atlas, SIMD blit, double-buffering. Framebuffer via `/dev/fb0`.

### tty — Legacy Text-VT Terminal

`userspace/tty/src/main.rs`

Cooked mode, line discipline, Ctrl-C/Ctrl-Z/Ctrl-D. Login mode
(Username/Password/Authenticating).

### compositor — TUI Window Compositor

`userspace/compositor/src/main.rs`

Owns VT4. Floating windows, SHM cell-grid protocol. Status bar with clock.
Session handoff/ended handling.

## Libraries

### libcluu — Userspace Runtime

`userspace/libcluu/`

IPC wrappers, POSIX shim, async runtime, ANSI parser, FS protocol, capability
helpers, ELF loading, boot params, registry client, crypto, font atlas,
framebuffer, input, PCI, process info, syscall wrappers, tar parser, time,
TOML parser, types, VFS view helpers, window SHM.

### klibcluu — Shared Kernel/Userspace Library

`klibcluu/`

Shared between kernel and userspace (compiled for both targets). Types
(Message, InvokeOp), crypto (SHA-256, HMAC-SHA256), boot ELF/TAR parsing, sync,
util.

### procmgr-common — Shared Procmgr Types

`userspace/libs/procmgr-common/`

Shared between root-procmgr and session-procmgr. PID encoding, labels, wire
types, envelopes, manifest cache, mount policy, view table, mint guard.

### cluu_wire — IPC Wire Protocol Types

`userspace/cluu_wire/`

Single source of truth for IPC payload formats. SpawnEnvelope, session
lifecycle verbs, PTS wire types, primordial seed.

### cluu_lang — Shell Scripting Language

`crates/cluu_lang/`

Pest grammar parser + AST for shell scripting. `parse_program`, `format_ast`.

## Utilities (each its own container)

125 containers in `containers/`. Each has a `Cluufile` declaring its capability
profile and mount policy. Key utilities:

| Binary | Profile | Notes |
|--------|---------|-------|
| `mkdir` | ipc vfs registry | `/tmp` inherit |
| `rm` | ipc vfs registry | `/tmp` inherit, `-r` support |
| `cp` | ipc vfs registry | |
| `mv` | ipc vfs registry | |
| `cat` | ipc vfs | |
| `grep` | ipc vfs | |
| `ls` | ipc vfs | |
| `ps` | ipc vfs registry | Reads /proc |
| `top` | ipc vfs registry | Live process monitor |
| `edit` | ipc vfs compositor | TUI editor |
| `shell` | ipc vfs registry admin | DIY shell |
| `cluuterm` | ipc vfs compositor | Terminal emulator |
| `micropython` | ipc vfs | Python interpreter |
| `mp3player` | ipc vfs registry | MP3 playback via virtio-snd |
| `hello` | ipc vfs | Demo container |

See [Container Encapsulation](../containers/index.html) for the Cluufile model.

## Documentation findings

### F-006 — ramfs service is an unimplemented stub (open)

`userspace/ramfs/src/main.rs` is a stub that loops on `recv` and discards every
message. The crate is built by xtask and staged into the userdisk but is not
wired into the boot sequence. VFS owns the initrd directly. Followup: either
implement the service or remove it from the xtask build list and the userdisk
image.
### F-007 — devmgr boot root block and device tokens are identical (open)

`userspace/devmgr/src/main.rs:54-55` assigns both `boot_root_block_token` and
`boot_root_device_token` from `info.tokens[TOKEN_EXTRA_1]`. They are then
passed to different handlers: `boot_root_block_token` to
`handle_register_block`, `boot_root_device_token` to
`handle_register_char`.
Both variables hold the same token. This may be intentional (only one extra
token is provided at boot) or a copy-paste bug where `boot_root_device_token`
should have read `TOKEN_EXTRA_2`. If block and char device registration need
distinct boot authority, sharing one token means one path is over-privileged
or the other is under-privileged. Followup: determine whether the two handlers
need distinct tokens; if yes, plumb a second token through the boot parameter
block; if no, collapse to a single variable and document why.

## Plan lessons — services

Distilled implementation lessons from service-related plans. 2-5 lines
each; see the dated plan file for the long form.

### timeserver-pushmode-zero-drift (2026-05-13-timeserver-pushmode)

Timeserver extended with periodic-tick push notifications
(`TIME_SUBSCRIBE_PERIODIC` / `TIME_UNSUBSCRIBE` / `TIME_TICK`). Subscribers
grant timeserver a SEND-only token to their notify endpoint; timeserver
pushes ticks at the requested period with zero cumulative drift (anchored
deadlines, not `sleep(period)`) and auto-revokes dead subscribers. The loop
becomes deadline-driven: `recv_with_timeout(min(subscriber_deadlines) -
now)`. On message → handlers + subscribe/unsubscribe arms. On timeout →
fire ticks for due subscribers. Purely additive; no caller changes for
non-subscribers.

### compositor-clock-pushmode (2026-05-13-compositor-clock-pushmode)

The compositor was polling timeserver every loop iteration. Fix: subscribe
once at startup with `period_ms=1000`, block on `recv` (no timeout), wake
on `TIME_TICK`. Status-bar clock ticks at true 1 Hz; per-iteration IPC
pressure to timeserver goes to zero. The lesson generalizes: polling a
service from a render loop is an anti-pattern — subscribe push-mode.

### editor-piece-table-atomic-save (2026-04-29-editor)

The editor is a vi-flavored modal TUI, ~3000 LOC `no_std + alloc`. Piece
table with append-only original/add stores; coarse vim-style undo log on
top. Six-mode state machine (NORMAL/INSERT/VISUAL_CHAR/VISUAL_LINE/
OPERATOR_PENDING/EX_PROMPT). Atomic save via `open_with(tmp, ...)` →
`write` → `close` → `rename(tmp, final)`. Harness cases that test save
target ext2-backed paths, not `/tmp` (MemFs `O_WRONLY|O_CREAT` timeout
bug, task #80).
