# Inventory: spawn / window / PTY / session pipeline (2026-05-18)

**Status:** Inventory only — precursor to one or more design specs.
**Why this exists:** several incremental fixes over the past sessions
made it clear the spawn + cap delegation + FdInherit + window/PTY
+ session lifecycle pipeline is over-pathed. Before designing a
unified replacement we survey what exists today and where it
duplicates, so the decomposition into design specs is informed.

This document is intentionally *not* a redesign. It is the map.

---

## 1. Spawn paths

Six distinct entry points exist today. Each has its own wire format
for FdInherit and exit-notify wiring.

| # | Caller | Verb | File:line | Notify | FdInherit | Notes |
|---|--------|------|-----------|--------|-----------|-------|
| 1 | init | kernel spawn (no message) | `userspace/init/src/wiring.rs:204` (`launch_service`) | `exit_token` via ProcessInfo slot | none | primordial batch (registry, timeserver, procmgr, vfs, virtio-blk, tpmd) |
| 2 | procmgr (autostart.toml) | internal call | `userspace/procmgr/src/main.rs:1303` (`autostart_container`) | 0 (procmgr polls its own `exit_endpoint`) | empty | autostart: console, vtmgr, kbd, compositor (session_mode=0), login |
| 3 | login binary | `PROCMGR_SESSION_LOGIN_LABEL` | `userspace/procmgr/src/main.rs:2143` (`handle_session_login`) | one-shot `COMPOSITOR_READY_LABEL` wait at line 2587 (**2 s timeout**) | none for compositor/cluuterm | spawns user compositor (line 2516) then cluuterm (line 2617) sequentially |
| 4 | SPAWN-capped thread (libcluu posix_spawn) | `PROCMGR_SPAWN_LABEL` | `userspace/procmgr/src/main.rs:4419` (`handle_spawn_message`) | `msg.words[3]` → `resolve_notify_endpoint` → `token_derive(IPC_SEND)` | `msg.words[2]` offset | bare SPAWN — used by shell pipeline stages, probes |
| 5 | shell (bare external cmd, Ctrl+Alt+N) | `PROCMGR_CONTAINER_RUN_LABEL` | `userspace/procmgr/src/main.rs:5482` (`handle_container_run`) | `msg.words[1]` → `resolve_notify_endpoint` → `token_derive(IPC_SEND)` (this session) | `msg.words[2]` offset, after trailer strip | container model — sibling under caller session |
| 6 | cluuterm → /bin/shell | `posix_spawn` via newlib (routed back to path 4 or 5) | `userspace/cluuterm/src/main.rs:241` (`spawn_shell_with_pts`) | from newlib wrapper | `posix_spawn_file_actions_adddup2(pts_fd → 0, 1, 2)` | dup2 is *the second* fd-wiring path — see §3 |

**Sibling pipe-spawn** goes via path 4. **Most user external-command
spawn** goes via path 5. **Initial shell under cluuterm** is path 6
(wrapping path 4 or 5 underneath). **System / session boot** is paths
2+3. **Kernel boot** is path 1.

Cap-monotone enforcement is *real* today: `resolve_notify_endpoint`
derives an `IPC_SEND` cap from the caller's raw handle into procmgr's
own table (the just-landed fix `a597e09`). FdInherit injection never
re-derives caps for use outside the child slot.

The 2 s `COMPOSITOR_READY` wait at procmgr:2587 violates
`feedback_no_timeouts.md`. Documented; left untouched.

---

## 2. Compositor instance lifecycle

```
boot → autostart → system compositor (pid 5, session_mode=0)
                   live → user types creds at login →
                   login sends PROCMGR_SESSION_LOGIN
                       → procmgr `kill_system_compositor` (main.rs:2288)
                            → registry force-unregister compositor:client,
                              compositor:input, compositor:control,
                              compositor:stdin/out/err/log (8 admin
                              force-unregisters)
                            → space_destroy(compositor pid 5)
                       → procmgr spawns user compositor (pid 8,
                         session_mode=1)
                       → user compositor re-registers the same 7
                         compositor:* names
                       → procmgr spawns cluuterm (pid 9) under it
```

Two compositors back-to-back, registering identical `compositor:*`
names with admin force-unregister between. Pattern only works because
the registry happily replaces; any client cached a stale cap during
the swap window must re-subscribe.

Per `feedback_subagent_models` / pro-system parallels: in Wayland /
QNX the compositor is one process for the lifetime of the seat. Login
is a window inside it, not a separate compositor.

---

## 3. fd inheritance — two parallel mechanisms

Shell under cluuterm has its stdin/out/err handles wired *twice*:

1. **cluuterm posix_spawn dup2** (`cluuterm/src/main.rs:292-304`):
   opens `/dev/pts/<id>`, then `posix_spawn_file_actions_adddup2(pts_fd,
   newfd)` for newfd ∈ {0, 1, 2}.
2. **procmgr FdInherit blob** (`procmgr/src/main.rs:4878-5025`): reads
   the FdInherit entries from the message payload, calls
   `vfs_derive_child_fd(parent_cid, parent_fd, child_tid, child_fd)`
   to install a VFS-derived RECV/SEND token on the child slot, and
   writes the (vfs_client_id, vfs_remote_fd) trailer into the child's
   ProcessInfo page so `libcluu::fd_table::init_stdio` picks them up.

Either alone is sufficient. Both run on the same spawn. The dup2
chain came from POSIX-compat. The FdInherit blob came from CLUU
cap-clean redesign. Today the two are co-resident — confusing, and
the source of "I changed FdInherit and posix_spawn still works" bugs.

`/etc/envelopes.toml` lines 40, 114 carry `vt_graphical` mounts; the
envelope mechanism is the cap-clean side of the same story.

---

## 4. PTY vs legacy TTY

Two terminal protocols, no convergence:

| Protocol | Speaker | Labels | Where |
|----------|---------|--------|-------|
| Legacy TTY | `userspace/tty/` service (one per VT, text-VT only) | `TTY_REGISTER_LABEL`, `TTY_CTL_LABEL` (lflag get/set), `TTY_SET_FG_LABEL`, `TTY_READ_REQUEST_LABEL`, `TTY_POLL_QUERY_LABEL` | `userspace/tty/src/main.rs:110, 168, 214` |
| PTS | cluuterm-internal | `PTS_READ_LABEL`, `PTS_WRITE_LABEL`, `PTS_CLOSED_LABEL` | `userspace/cluuterm/src/tty_backend.rs:497, 514` |

Shell calls TTY_CTL / TTY_REGISTER unconditionally in legacy text-VT
mode (`shell/src/commands/exec.rs:310`). Until this session it also
tried those against the cluuterm PTS endpoint (it hung — fixed by
guarding on `tty_endpoint != 0` in `9ac4b12`). But the cluuterm PTS
has **no equivalents at all**:

- no `lflag` get/set → no cooked / canonical / echo control
- no `fg pgrp` set → no Ctrl-C → SIGINT routing (`feedback v1 dropped`)
- no winsize ioctl → htop / top see 0×0 terminal
- no TERM env propagation

`libcluu/src/tty_core/line_discipline.rs` exists and is *used by
legacy TTY only*. cluuterm imports neither line_discipline nor the
ioctl machinery.

---

## 5. Line discipline & signals

| Feature | Legacy text-VT | cluuterm path |
|---------|----------------|---------------|
| Cooked / raw mode | LineDiscipline + TTY_CTL | none |
| Echo | LineDiscipline | cluuterm always echoes via local input.rs |
| Ctrl-C → SIGINT | TTY ISIG flag + PROCMGR_PG_SIGNAL on fg pgid | dropped at `cluuterm/src/input.rs` `// signal dropped in v1` |
| Ctrl-Z / Ctrl-\ | TTY ISIG | dropped |
| Ctrl-D → EOF | LineDiscipline | none |
| termios `tcsetattr` | TTY_CTL → set_lflag | unsupported |

Today user typed Ctrl-C in cluuterm dozens of times to interrupt
hung commands — every keystroke logged `cluuterm: Ctrl-C (signal
dropped in v1)`.

---

## 6. Exit notify

Three half-converging paths:

| Path | Mechanism | Where |
|------|-----------|-------|
| Procmgr → parent | `exit_notify[cookie] = derived_send_token`; on child exit `send(notify, PROC_EXIT_LABEL_msg)` | `procmgr/src/main.rs:1909` |
| cluuterm window close | manual `WIN_DESTROY` to compositor | `userspace/login/src/main.rs:635` (login); cluuterm has none on its own — relies on procmgr reaping it implicitly |
| Init primordial monitor | `primordial_exit_recv` + RECV loop | `userspace/init/src/context.rs:35-38` + monitor loop |

Cap discipline now correct on path 1 (commit `a597e09`). Paths 2 and
3 are unrelated mechanisms that all describe "this thing died".

---

## 7. Session state ownership

Three separate owners hold pieces of "what session is this":

| Owner | Holds | Where |
|-------|-------|-------|
| procmgr | `SessionEntry { user, profile, shell_cid, vt, stdin_endpoint, ... }`; `session_table` keyed by session_cid | `procmgr/src/main.rs` |
| VFS | `PtsEntry { owner_tid, refcount }` keyed by pts id | `userspace/vfs/src/main.rs:967` |
| cluuterm | local `Cluuterm` struct with cell grid, blink phase, cursor pos | `userspace/cluuterm/src/tty_backend.rs:28` |

No back-pointers. No invariant that "session N has pts X and
cluuterm process Y and shell Z". Manual coupling in three places.

---

## 8. ps display confusion (the "two cluuterms" finding)

User running `ps` inside a logged-in cluuterm sees pid 9 and pid 10
both labeled `cluuterm`. Code audit (§1, §2) confirms only **one**
cluuterm is spawned per SESSION_LOGIN: pid 9 is the cluuterm process,
pid 10 is the shell that cluuterm posix_spawned.

Hypothesis: ps reads `/proc/<pid>/comm` or whatever procmgr exposes
as the process name. Procmgr likely stores the **container image
name** of the spawning container (`"cluuterm"`) rather than the
actual binary basename (`"shell"`) when a child is spawned via the
container-run path. Result: pid 10's display name = its container,
not its exe.

Action item: the redesign should fix process identity reporting so
ps shows the *binary* the process is actually running. SOLID
(single source of truth) + matches user mental model.

---

## 9. Timeout violators (cross-ref `feedback_no_timeouts.md`)

Still open:
- `procmgr/src/main.rs:2587` — 2 s `COMPOSITOR_READY_LABEL` wait
- `libcluu/src/fs/client.rs:228` — 5 s VFS call timeout
- `userspace/edit/src/input.rs` — 25 ms ESC-vs-CSI disambiguation
  (accepted as UX, not a deadlock guard)

Closed this work cycle:
- `libcluu/src/registry.rs::wait_for_grant` — was 2 s, now infinite
  (commit `da8da75`)
- `compositor/src/main.rs::recv_any` — was saturating u64::MAX, now
  30 s bounded loop driven by TIME_TICK (commit `9fda763`, refined
  in `06fcf1f`)

---

## 10. Pro-system parallels

| System | Spawn model | Terminal model | Compositor model |
|--------|-------------|----------------|------------------|
| seL4 / Genode | parent grants a fixed *session* of caps; child holds for life | dedicated component, one PTY = one component | one nitpicker for the seat |
| Wayland | shell-spawned children inherit terminal pts via POSIX | terminal owns PTY end-to-end (line discipline, ANSI, signals); no legacy TTY service | one compositor for the seat; login is a window |
| QNX Neutrino | resource managers expose paths in /dev or /proc; open → cap → RPC | proc managers own pty devices; clients open by path | windowing is its own resource manager |
| Linux + Wayland | `fork+exec` with file descriptor inheritance; cgroup-based session | shell talks to /dev/pts/N, ioctl TIOCGWINSZ etc.; SIGWINCH on resize | one wlroots-based compositor; login is a `greetd` window |

CLUU's natural alignment is seL4 (cap-session at spawn) + Wayland
(terminal owns PTY end-to-end, compositor is just surfaces).

---

## 11. Redesign anchors (preview)

To be expanded in the upcoming design specs:

1. **One spawn protocol.** Unify SPAWN, CONTAINER_RUN, autostart,
   primordial into a single verb taking a single envelope
   `{ image, args, env, view, fd_inherit, parent_session, notify,
   restart_policy }`. Procmgr calls this for autostart too. No more
   "which path are we on?" branches.
2. **FdInherit is THE inheritance mechanism.** Retire the
   posix_spawn `adddup2` dup2 path inside cluuterm. cluuterm hands
   procmgr a single FdInherit manifest; procmgr does all wiring.
3. **One PTY protocol.** Retire legacy TTY service for graphical
   sessions. cluuterm IS the terminal: speaks PTS_READ / PTS_WRITE
   / PTS_IOCTL (winsize, lflag, fgpgrp) / PTS_KILL_FG. Shell uses
   one set of verbs regardless of whether stdout is a text-VT TTY
   or a cluuterm PTS.
4. **Compositor lives.** No system→user compositor swap. The
   compositor is the seat owner; login is a window the compositor
   spawns on its own VT4 at boot, dies after auth. seL4 / Wayland
   parallel.
5. **Cap-revocation only.** Every blocking IPC uses kernel
   cap-revocation as the unblock signal. No wallclock timeouts as
   deadlock guards.
6. **Process identity = binary name.** /proc/PID/comm reports the
   actual executable's basename, not the spawning container's image.

---

## 12. Suggested decomposition

Four design specs in order:

1. **Unified spawn protocol** — single IPC verb, single envelope,
   FdInherit as sole inheritance. Includes process-identity rule.
   Pre-requisite for everything else.
2. **Terminal+PTY unification** — retire legacy TTY for graphical
   path; cluuterm wears the full terminal protocol; line discipline
   shared via `libcluu::tty_core`.
3. **Session lifecycle** — no compositor swap; login is a window;
   session is a procmgr concept driving spawn envelopes.
4. **Window protocol formalization** — formalize the Wayland-style
   frame-callback already present (`broadcast_frame_ready` in
   `compositor/main.rs:32`); define damage/present cleanly.

Each spec gets its own brainstorm → spec → plan → implementation
cycle per the project's workflow.

---

## Related memory

- [[no-timeouts]]
- [[unified-process-model-decision-2026-05-18]]
- [[frame-typing-redesign-landed-2026-05-18]]
- [[spawn-cap-composable]]
- [[procmgr-stateless]]

## Related committed work this session

- `860f996` procmgr: derive parent_stdin_send from original stdin endpoint
- `a597e09` procmgr: derive notify_endpoint into own token table
- `9ac4b12` shell: skip TTY-service IPC in cluuterm/pts mode
- `db492d2` compositor+cluuterm: fix phantom login cursor and missing blink
- `9ca96aa` compositor: bigger cascade stagger for multi-window visibility
- `06fcf1f` compositor: drop blink ownership; strip per-recv log
- `659465a` cluuterm: own cursor blink timer
- `bc6b61e` compositor: tick at 500 ms
- `5c62468` compositor: double-line chrome for focused window
- `da8da75` libcluu/registry: drop 2 s subscribe timeout
- `9b982c4` rename FDAC → FdInherit
- `72d7185` shell: FdInherit stdio passthrough for bare external commands
