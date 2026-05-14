# Shell stdio unification on POSIX fd 0 (Path A: /dev/ttyN)

**Date:** 2026-05-14
**Status:** in progress
**Why:** Bug C from LoginCC pause notes — shell input via TOKEN_STDIN push
breaks under cluuterm (VFS-backed fd 0). Cluuterm path went POSIX via pts;
legacy VT0 still pushes via TTY_READ_LABEL on TOKEN_STDIN. Dual paths in
shell are technical debt. Unify on POSIX read(0) over /dev/ttyN.

**Scope:** legacy vt container only. Cluuterm/pts path already uses POSIX
read once shell is refactored. tty primordial stays — it owns the line
discipline and acts as a VFS backend for /dev/ttyN, mirroring how cluuterm
owns /dev/pts/N.

## Architecture target

```
kbd → vtmgr → tty service (per-VT line discipline + stdin buffer)
                                 ▲                ▲
                                 │                │ TTY_READ_REQUEST_LABEL
                                 │                │ TTY_WRITE_LABEL
shell on VT0 → read(0)/write(1) → VFS → /dev/tty0 → tty service endpoint
```

- `/dev/ttyN` already exists in VFS as `DeviceType::Tty { endpoint }`
  (userspace/vfs/src/mount.rs DeviceBackend). Endpoint is the tty service's
  main endpoint, set at boot via `set_tty_endpoint`.
- VFS read of `/dev/ttyN` fd already forwards via `TTY_READ_REQUEST_LABEL`
  to tty (vfs/main.rs:2564). Reply carries the bytes.
- tty service already handles `TTY_READ_REQUEST_LABEL` with a pending-reads
  queue and `try_satisfy_reads` (tty/main.rs:110, context.rs).
- The infrastructure is in place. The bug is that procmgr's legacy vt
  container shell spawn never opens `/dev/ttyN` — it gives the shell a
  push-style `TOKEN_STDIN` endpoint that tty pushes `TTY_READ_LABEL`
  into.

## Tasks

| T  | What | Files |
|----|------|-------|
| T1 | Procmgr legacy vt shell spawn: open `/dev/ttyN` at spawn time and FDAC into the child as fd 0, 1, 2. Drop the TOKEN_STDIN endpoint creation + tty push wiring for this path. | userspace/procmgr/src/main.rs (vt container session-login=0 path, around the spawn-with-env call) |
| T2 | Shell: drop `recv_any([stdin, registry])` loop. Replace with POSIX `read(0, buf, 256)` driving the existing `handle_line_payload`. Keep registry events drained via `wait_for_grant` calls inside builtins/lookups (current pattern). | userspace/shell/src/main.rs (run loop around line 160) |
| T3 | tty service: stop calling `wire_shell_stdin` (TTY_READ_LABEL push). Keep `TTY_READ_REQUEST_LABEL` reply handler. Audit any other TOKEN_STDIN-push call sites and remove. | userspace/tty/src/main.rs, userspace/tty/src/context.rs |
| T4 | libcluu init_stdio: when fd 0 arrives as VFS-backed pointing to `/dev/ttyN`, ensure it's marked non-seekable so `poll()` doesn't return POLLIN spuriously. Optional polish for v1; required if future code uses `poll` on stdin. | userspace/libcluu/src/fd_table.rs init_stdio |
| T5 | Harness: rerun the existing legacy-vt markers (l2_tty_login, l2_shell_smoke, etc.) and the cluuterm path (l2_cluuterm_login) to confirm both shells work via POSIX read. | scripts/harness_run.sh markers |
| T6 | Memory + plan status update. Note that TOKEN_STDIN push-path is retired; mark the design memo. | MEMORY.md and feedback/project memories |

## Out of scope

- Killing the tty primordial entirely (option B). Kept for a later refactor;
  see [[vfs-direct-token-optimization]] for the broker-vs-data-path principle.
- termios via ioctl(fd). `TTY_CTL_LABEL` keeps working; ioctl wrapping can
  follow once shell + cluuterm both depend on POSIX termios.
- Removing `PARAM_TTY_INSTANCE` from shell. It's still useful for the
  prompt / session tagging.

## Risks

- VT0 legacy login UX is currently driven by the tty service writing prompts
  to console. Once shell starts via POSIX, the login modal is the compositor
  path. Need to keep the **text-mode** login on VT0-3 working (legacy VTs
  don't have a compositor). Verify the tty service's login state machine
  still runs and that its handoff to the shell uses the new fd 0/1/2 wiring.
- Procmgr's legacy session_kind=0 flow currently grants a fresh stdin
  endpoint and gives the tty service the send side. Opening `/dev/ttyN` via
  VFS at spawn time needs to happen from procmgr's context — confirm VFS
  authenticates procmgr's open and that the resulting fd derives correctly
  for the child via FDAC (mirror cluuterm's `_open` → dup2 pattern).

## Validation

- Build cleanly: `cargo xtask build`.
- Boot to VT0 text login, log in as `root`, type `ls` and `echo hi` — shell
  must read each line and execute.
- Boot to compositor login (cluuterm path), log in, same smoke.
- No `TTY_READ_LABEL` push from tty service in serial log after login.
