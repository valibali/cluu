# Session-as-container + unified /dev stdio

**Date:** 2026-05-14
**Status:** design approved, ready for plan
**Supersedes (partly):** `docs/superpowers/plans/2026-05-14-shell-stdio-posix-unify.md` — that
plan's Path A direction is correct but its scope was narrower than this spec.
This document folds the legacy-tty unification into a larger session model.

## 1. Goal

Make CLUU's process model match what the project has been informally
converging on: **seL4-style capability propagation underneath, POSIX-shaped
surface on top.** Concretely, after this design lands:

- All terminal-like devices live under `/dev` (`tty0..3`, `pts/<id>`,
  `console`, `fb0`, `null`, `zero`, `urandom`). The kernel + system services
  never expose stdio through anything other than a `/dev` entry.
- The shell and every other userspace program reads stdin with POSIX
  `read(0, buf, n)`. No `TTY_READ_LABEL` push path, no `recv_any` on
  `TOKEN_STDIN`. Pipes, redirections, ttys, and pts all converge on
  `_read`/`_write` in libcluu.
- Login establishes a **session container**. From that moment, every
  user-visible process — the user-mode compositor, cluuterm, future apps —
  spawns inside that container under the user's envelope. Logout tears the
  container down; nothing the user touched survives.
- The capability + view monotone-decrease rule is preserved end-to-end:
  every child's VFS view and token rights are a strict subset of its
  parent's. (See `feedback_vfs_view_caps_monotone.md`.)

## 2. Boundary: system primordials vs session

| Layer | Processes | Lifecycle | Privileges |
|---|---|---|---|
| **System primordials** | `console`, `vtmgr`, `kbd`, `tty` (per-VT), `pre-login compositor` | boot → forever | system view, full device access |
| **Per-VT text session** | shell (one per VT0-3, spawned at login) | login → logout | user envelope, narrowed view |
| **Per-VT graphical session** | user compositor + cluuterm(s) + future apps | login → logout | user envelope, narrowed view |

Two invariants:

- System processes never run as a user.
- Session processes never persist past logout.

The pre-login compositor is the only graphical service that runs in system
mode; it exists only to host the login modal on VT4. It exits when the user-
mode compositor takes over and respawns when the user-mode compositor exits.

## 3. /dev as the single namespace

VFS exposes a single `/dev` mount with these entries:

| Path | Owner | Backend kind | Lifecycle |
|---|---|---|---|
| `/dev/tty0..3` | `tty` service (one process, 4 instances) | VFS char backend (existing `DeviceType::Tty`) | static |
| `/dev/pts/<id>` | each cluuterm instance | VFS char backend (existing `PtsBackend`) | dynamic; cluuterm calls `PTS_REGISTER` |
| `/dev/console` | `console` service | VFS char backend | static |
| `/dev/fb0` | (VFS devfs built-in) | mmap-capable char | static |
| `/dev/null`, `/dev/zero`, `/dev/urandom` | (VFS devfs built-in) | char | static |

**Common contract:** opening any of these returns a regular VFS-backed fd.
`read(fd, …)` blocks until the owner provides data (POSIX terminal semantics).
`write(fd, …)` hands bytes to the owner. The owner serves
`TTY_READ_REQUEST_LABEL` (for ttys) or `PTS_READ_LABEL` (for pts) pulls;
there is no push-style stdin protocol.

`tty` is refactored to be exactly the same shape as `cluuterm` from VFS's
point of view: a process owning a `/dev/...` node, replying to read pulls,
accepting write pushes. The single difference is that `tty` registers its
nodes statically at boot rather than on demand.

## 4. Session container

Procmgr's existing per-session `container_id` is the unit. At login:

```
procmgr.handle_session_login(kind, vt, creds):
    auth(creds)
    envelope = resolve_envelope(user)              # /etc/envelopes.toml
    session_cid = next_container_id()
    view = build_view(envelope, vt, user)          # § 5
    if kind == 0 (text):
        spawn /bin/shell with view + envelope env
            FDAC: open /dev/tty<vt> as fd 0/1/2
    if kind == 1 (graphical):
        kill_pre_login_compositor()
        spawn user-mode compositor with view + envelope env
            FDAC fd 0: /dev/null     (compositor has no interactive stdin)
            FDAC fd 1, 2: /dev/console (panics + debug go to system log)
            compositor opens /dev/fb0 itself + mmaps it (existing pattern)
    record (session_cid, vt, user, root_pid)
```

The session compositor (or shell) is the *root* of the container.

When the user opens an app from the compositor menu:

```
compositor sends PROCMGR_SPAWN(session_cid, image_path)
procmgr:
    verify session_cid is alive + caller is its compositor
    spawn image as sibling under same container_id, same envelope view
    reply with pid
```

The user compositor itself never holds an unconstrained spawn capability —
it always goes through procmgr's `PROCMGR_SPAWN`. Procmgr is the broker;
this preserves the seL4 "kernel/PD manages caps" boundary.

**Logout:**

```
on root_pid exit_endpoint signal (compositor or shell exited cleanly,
crashed, or user invoked logout):
    walk container_children[session_cid] in reverse-dep order
    THREAD_KILL each
    reap exit cookies
    drop session_cid, view, envelope state
    if kind == 1: re-spawn pre-login compositor on VT4
    if kind == 0: re-spawn /bin/login text prompt on VT<vt>
```

## 5. Envelope mounts with {vt} / {user} substitution

`/etc/envelopes.toml` grows two new profile shapes: `vt_text` and
`vt_graphical`. Procmgr picks one based on `session_kind`, then substitutes
`{vt}` (text only) and `{user}`.

```toml
[envelope.user.vt_text]
mounts = [
    "ro:/bin", "ro:/usr", "ro:/lib", "ro:/etc",
    "ro:/dev/tty{vt}",
    "ro:/dev/null", "ro:/dev/zero", "ro:/dev/urandom",
    "rw:/home/{user}",
    "rw:/tmp/{user}",
    "ro:/proc",
]

[envelope.user.vt_graphical]
mounts = [
    "ro:/bin", "ro:/usr", "ro:/lib", "ro:/etc",
    "rw:/dev/pts",
    "rw:/dev/fb0",
    "ro:/dev/null", "ro:/dev/zero", "ro:/dev/urandom",
    "rw:/home/{user}",
    "rw:/tmp/{user}",
    "ro:/proc",
]

[envelope.user.env_template]
HOME    = "/home/{user}"
USER    = "{user}"
LOGNAME = "{user}"
PWD     = "/home/{user}"
PATH    = "/bin:/usr/bin"
SHELL   = "/bin/shell"
TERM    = "cluu"
```

`admin` follows the same shape with broader mounts (`/proc` rw, `/var`,
`/dev/console` rw, etc.) — kept short for review here.

**Substitution + monotone enforcement (procmgr side):**

- `{vt}` accepts `0..=3` only (validated). Anything else → reject login.
- `{user}` accepts characters matching `users.toml`'s key syntax only.
- After substitution, every mount entry is matched against the procmgr-side
  full view to confirm `mount.path ∈ procmgr.view`. Reject session if any
  entry escapes.

## 6. Monotone view + capability propagation

End-to-end chain for the graphical case:

```
procmgr full view   /, /dev (all), /proc, /home, /tmp, ...
        │
        ▼ SESSION_LOGIN kind=1: spawn user compositor under session_cid
user compositor     /bin /usr /lib /etc /dev/pts /dev/fb0
                    /dev/{null,zero,urandom} /home/{user} /tmp/{user} /proc
        │
        ▼ PROCMGR_SPAWN(session_cid, "cluuterm")
cluuterm            same subset (procmgr re-injects session envelope)
        │
        ▼ cluuterm posix_spawns /bin/shell with FDAC
shell               inherits cluuterm view; fd 0/1/2 = /dev/pts/<id>
                    (cluuterm may narrow further by dropping /dev/fb0 etc.)
```

**Enforcement points** (audit checklist):

- `vfs_view.rs` `set_view` asserts `new_view ⊆ parent_view`. Confirm it
  still holds after `{vt}` / `{user}` substitution.
- Cluufile `MOUNT` directives may only narrow; the loader rejects broader
  entries with a clear error.
- `token_derive` calls in procmgr's FDAC path narrow rights monotonically
  (`RECV | GRANT` for fd 0, `SEND | CALL | GRANT` for fd 1+ — already true).
- `vfs_derive_child_fd` mints a child token whose rights are a subset of
  the parent's open file rights.

## 7. POSIX `read(0)` everywhere

Shell pseudocode after this lands:

```rust
fn run() -> Result<()> {
    posix_init();                       // libcluu populates fd 0/1/2/3 from
                                        // FDAC trailer at process start
    source_shellrc();                   // reads $HOME/.shellrc via fd
    loop {
        let n = read(0, &mut buf);      // blocks; POSIX terminal semantics
        if n <= 0 { break; }
        handle_line(&buf[..n]);
    }
}
```

No `recv_any([stdin, registry])`. Registry events drain on demand through
the various `subscribe_output(...)` calls inside builtins (existing
`wait_for_grant` mechanism). Job-control reaping runs between commands.

Three dispatch legs converge here (libcluu `_read`):

| FdEntry kind | Path | Backend |
|---|---|---|
| `is_tty()` | `read_tty()` (`TTY_READ_REQUEST_LABEL` call) | `tty` service |
| `is_pipe()` | `pipe::read_pipe()` (`PIPE_DATA_LABEL` recv) | procmgr-backed pipe |
| `remote_fd.is_some()` | `read_vfs()` (`VFS_READ_LABEL` / `PTS_READ_LABEL`) | VFS-mediated owner |

Same three legs for `_write`. Same applies to `cat`, `grep`, every other
program. No code anywhere depends on `TOKEN_STDIN` being an active IPC
endpoint.

## 8. Pipelines and redirection

Same FDAC mechanism handles everything POSIX-shaped:

```c
// shell parses "a | b > out"
int fds[2]; pipe(fds);                            // procmgr-backed pipe
int out_fd = open("out", O_WRONLY|O_CREAT, 0644); // VFS-backed file

// spawn 'a': fd 1 → pipe write end
posix_spawn_file_actions_adddup2(fa_a, fds[1], 1);
posix_spawn(&pid_a, "a", fa_a, ..., argv, envp);

// spawn 'b': fd 0 → pipe read end, fd 1 → out_fd
posix_spawn_file_actions_adddup2(fa_b, fds[0], 0);
posix_spawn_file_actions_adddup2(fa_b, out_fd, 1);
posix_spawn(&pid_b, "b", fa_b, ..., argv, envp);

close(fds[0]); close(fds[1]); close(out_fd);
```

Procmgr's FDAC parser already handles the legacy (pipe) and VFS-backed
(file, /dev/...) legs — no protocol change. `TOKEN_STDIN`/`STDOUT` slots
become unused for shell-style processes and can be removed in a follow-up
cleanup. `TOKEN_STDERR` is the same. `TOKEN_STDLOG` stays as a separate
IPC slot for system-log writes (or migrates to `/dev/log` later).

## 9. FDAC ↔ seL4 mapping

| seL4 primitive | CLUU FDAC equivalent |
|---|---|
| Capability slots in CSpace | Token handles in `libcluu::fd_table::FdEntry` |
| `seL4_CNode_Mint(parent_cap, rights_mask)` | `token_derive(endpoint, rights)` + `vfs_derive_child_fd(rights)` |
| `extraCaps` on IPC | FDAC payload appended to `PROCMGR_SPAWN_LABEL` |
| Thread creation with seeded CSpace | `spawn_service_with_env` parses FDAC then installs into the child's `fd_table` before `thread_resume` |
| Rights monotone-decrease | Procmgr's FDAC `probe_rights` per-fd mask + `vfs_derive_child_fd` rights narrowing |

The POSIX `posix_spawn_file_actions_t` surface is the Unix API; the
mechanism underneath is capability-pass-at-spawn. Two views, one
mechanism.

## 10. Out of scope (future work, separate specs)

- Killing the `tty` primordial entirely and absorbing line discipline into
  VFS — see `vfs_direct_token_optimization`.
- TAB-completion across `/dev/ttyN` and `/dev/pts/<id>` (currently relied
  on `TTY_TAB_QUERY_LABEL` over the now-defunct stdin recv path).
- Direct-token optimization for high-throughput stdio paths.
- `/dev/log` migration of `TOKEN_STDLOG`.
- Multi-user simultaneous graphical sessions (single-user-at-a-time today).
- Cluufile `MOUNT` `{vt}` / `{user}` substitution validator pass.

## 11. Validation criteria

- Boot reaches the login modal on VT4 and a `login:` prompt on VT0-3.
- Login on VT0 (root): shell prompt, `ls /dev` shows `tty0` + safe nodes +
  `null/zero/urandom` (no `tty1..3`, no `fb0`). `echo hi`, `ls /home/root`,
  `cat /etc/motd` all work via POSIX read/write.
- Login on VT4 (root): user-mode compositor takes over. Cluuterm window
  opens via compositor menu, shell inside cluuterm shows `root:/home/root>`
  prompt and accepts typed commands.
- Logout VT4: compositor exits → session container reaped → pre-login
  compositor respawns within 1 s → login modal visible again.
- Cross-session reject: shell on VT0 cannot `cat /dev/tty1` (ENOENT — not
  visible in its view).
- Monotone audit: grep every `set_view`, every envelope construction, every
  `token_derive` site. Report a clean bill.
- Pipelines: `ls | head -n 3` works under both VT0 (text) and cluuterm
  (graphical). `echo hi > /tmp/root/test && cat /tmp/root/test` works
  identically in both contexts.

## 12. File registry

- This spec: `docs/superpowers/specs/2026-05-14-session-container-unified-stdio-design.md`
- Existing related plans: `docs/superpowers/plans/2026-05-14-shell-stdio-posix-unify.md`
  (Path A scope; folded into Section 7).
- Memory pointers (for implementation): `feedback_vfs_view_caps_monotone.md`,
  `project_pipes_procmgr_backed.md`, `project_proc_unix_compliance.md`,
  `project_dev_fb_unix.md`, `project_loginCC_session_2026_05_13.md`,
  `vfs_direct_token_optimization`.
