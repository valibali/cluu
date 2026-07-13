# Session Encapsulation

A **session** is the unit of process ownership and view scoping in CLUU. Each
login gets its own session, and session boundaries are authority boundaries.

## What a session owns

Each login creates:

1. **A session-procmgr**, owns the session's children, exit cookies, signals,
   pipes, process groups, controlling terminals. It is the per-session process
   manager.
2. **A session-vfs**, owns the session's VFS view, layered on top of the
   root-VFS's ext2/initrd backends. It is the per-session filesystem namespace.

 Children spawned inside a session carry `PARAM_SESSION_VFS_EP` so
 `subscribe_output("vfs", "main")` resolves to the **session-VFS**, not the
 root-VFS. This redirection is in `libcluu::registry` and is the canonical way a
 session binary reaches its session-VFS.

 Similarly, children spawned inside a session have `CLUU_SESSION_ID` set in
 their environment. When `lookup_service("procmgr:spawn")` or
 `subscribe_output("procmgr", "spawn")` is called, the registry short-circuits
 to `session-procmgr:spawn:{sid}` (session processes) or
 `root-procmgr:spawn` (boot processes). The name `"procmgr:spawn"` is purely
 virtual — it never hits the registry. There is no fallthrough: session
 processes cannot reach root-procmgr, and boot processes cannot reach
 session-procmgr. This closes the session escape path for spawn and pipe
 operations.

## The session invariant

**A session binary must only observe and affect processes within its own
session.**

This is not a policy that could regress. It is structural:

- The session-procmgr only knows about its own children (its PID table is
  session-scoped).
- The session-VFS only shows `/proc` entries for the session's own processes
  (the procfs backend queries the session-procmgr, not the root-procmgr).
- Cross-session IPC requires a capability token that the root-procmgr mints
  explicitly.

Cross-session visibility is a **privilege**, not a default.

## Root session godmode

The **root session** is the sole exception to session encapsulation. Root's
session-procmgr may observe and affect processes across the **whole system**:
all sessions, all containers, kernel telemetry.

This is the only sanctioned escape hatch. It is:

- **Bound to the root identity.** Not to a capability that can be forwarded.
  Root logs in, gets godmode, logs out, godmode doesn't persist as a token.
- **Non-delegatable.** Root cannot hand "godmode" to another session. It can
  spawn children in its own session (which inherit root's authority), but it
  cannot elevate another session's procmgr.
- **Singular.** Do not add a second godmode path. If you need cross-session
  visibility for a non-root service, mint a specific capability token for the
  specific cross-session operation, don't widen the session boundary.

## PID encoding

PIDs pack session identity and per-session local pid into a single `i32`:

```text
 31              23 22                           0
┌──┬───────────────┬──────────────────────────────┐
│ 0│  session_id   │         local_pid            │
└──┴───────────────┴──────────────────────────────┘
  sign   8 bits             23 bits
```

- `SessionId = u8`, which session the process lives in.
- `LocalPid = u32` (23-bit effective), the process number within that session.
- `Pid = i32`, matches POSIX `pid_t`. The sign bit is never set by `encode`
  (`SID_BITS + LOCAL_BITS = 31`), so a valid PID is always non-negative.

Every procmgr IPC exchange, every `ExitNotif`, every `ProcInfo` record carries
a PID encoded this way. Both procmgr sides and the wire format share the
`procmgr_common::pid` module so they cannot drift on the layout.

Session 0 is the **system session**, root-procmgr and the boot services run
here.

## The hierarchical procmgr model

CLUU's procmgr is hierarchical and multi-instance, Genode-`init`-style:

```text
                    ┌──────────┐
                    │   init   │  primordial; monitors exits
                    └────┬─────┘
                         ▼
                    ┌──────────────┐
                    │ root-procmgr │  SYSTEM cap-set, session 0
                    └──────┬───────┘
                           │ mints session caps
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │ session- │ │ session- │ │ session- │
        │ procmgr  │ │ procmgr  │ │ procmgr  │
        │ (alice)  │ │ (bob)    │ │ (root)   │
        └────┬─────┘ └────┬─────┘ └────┬─────┘
             │             │             │
        ┌────┴────┐   ┌────┴────┐   ┌────┴────┐
        │ shell,  │   │ shell,  │   │ shell,  │
        │ edit,   │   │ edit,   │   │ top,    │  ← root sees ALL
        │ ...     │   │ ...     │   │ ps, ... │    sessions
        └─────────┘   └─────────┘   └─────────┘
```

### Root-procmgr (system-scope, primordial)

- Owns **all sessions** in the system.
- Mints **session-scoped capability tokens** for each login.
- Runs the system-wide IPC dispatch loop.
- SYSTEM cap-set, the broadest authority in the system.
- Spawned by init as a boot-critical service.

### Session-procmgr (per-session)

- One instance per authenticated login.
- Owns the session's children, exit cookies, signals, pipes, process groups,
  controlling terminals.
- **Sub-mints child-scoped caps** from the session cap. Each child cap is
  strictly narrower than the session cap (monotone-narrowing).
- Runs the per-session IPC dispatch loop with the async runtime for VFS
  `derive_child_fd` calls during spawn.

### Cap derivation chain

```text
root-procmgr (SYSTEM cap)
  └→ session cap (minter by root-procmgr per login)
       └→ child cap (minter by session-procmgr per spawn)
            └→ grandchild cap (if the child can spawn)
```

Each derivation is monotone-narrowing: ≤ rights, ≤ expiry. The token table
enforces this before installing the derived token. A compromised child cannot
escalate; a buggy child cannot widen its own authority.

## Cascade teardown

When a session-procmgr dies (user logs out, crash, kill), **cap revocation**
triggers cascade teardown:

1. Root-procmgr detects session-procmgr death (exit notification).
2. Root-procmgr revokes the session cap.
3. All child caps derived from the session cap are revoked.
4. All processes in the session lose their authority, they cannot make IPC
   calls, cannot access VFS, cannot spawn children.
5. Processes exit or are killed.

This is structural: there is no "go kill all of session N's processes" loop.
The authority disappears, and the processes cannot continue.

## Why no runtime identity check

CLUU explicitly forbids runtime identity resolution in the IPC path. The
classic anti-pattern:

```rust
// FORBIDDEN in CLUU
fn handle_request(req: Request) {
    let caller_tid = req.sender_tid;
    let caller_session = resolve_session(caller_tid);  // ← runtime ACL
    if caller_session != allowed_session {
        return Err(PermissionDenied);
    }
    ...
}
```

This creates TOCTOU windows, divergent enforcement paths, and forces "what can
X do?" audits to run code instead of read static envelopes. Instead:

```rust
// CLUU way: possession = authority
fn handle_request(req: Request, cap: TokenHandle) {
    let info = token_get_info(cap)?;  // verify the cap exists + is valid
    // if the caller has the cap, it has the authority. Period.
    ...
}
```

If something must be inaccessible, simply do not include it in the cap-set or
view. Add authority by **minting/revoking tokens and shaping views**, not by
adding ACL rules.

## Session-VFS view layering

The session-VFS layers on top of the root-VFS's backends:

```text
  session-VFS view (per-login)
  ┌─────────────────────────────────┐
  │ ro:/bin  ro:/usr  ro:/etc       │  ← from envelope
  │ rw:/home/alice  rw:/tmp         │  ← from envelope
  │ ro:/proc (→ session-procmgr)    │  ← session-scoped procfs
  └─────────────────────────────────┘
             │
             ▼
  root-VFS backends (system-wide)
  ┌─────────────────────────────────┐
  │ /     → ext2 (via virtio-blk)   │
  │ /dev  → devfs                   │
  │ /proc → procfs (→ root-procmgr) │
  │ /dev/pts → PTS registry         │
  └─────────────────────────────────┘
```

The session-VFS's `is_session` branch (in `run_vfs()`) selects the registry
name (`session-vfs` vs `vfs`) and the procmgr endpoint it subscribes to. A
session binary's `/proc` queries go to the session-procmgr, so it only sees its
own session's processes.

## Envelope-driven mount views

At login, procmgr resolves the user's envelope from `etc/envelopes.toml`:

```toml
[envelope.user.vt_text]
mounts = [
    "ro:/bin", "ro:/usr", "ro:/lib", "ro:/etc",
    "ro:/dev/tty{vt}",
    "ro:/dev/null", "ro:/dev/zero", "ro:/dev/urandom",
    "rw:/home/{user}",
    "rw:/tmp",
    "ro:/proc",
]
```

The `{user}` and `{vt}` placeholders are substituted at login. A `user` profile
gets read-only `/bin`, `/usr`, `/etc` and read-write `/home/<user>`, `/tmp`. An
`admin` profile gets `rw:/` (full access). The envelope is the user-facing
expression of the session's authority.

### User envelope design (2026-04-28)

The envelope is established at session-login and defines mount view + env +
PATH for a given profile class (admin / user / service). It flows through
procmgr → shell → spawned binaries via existing capability-derivation
machinery. Key invariants:

- **Per-profile-class** (not per-user); `/etc/envelopes.toml` is parsed once
  at procmgr boot and cached. Three ship-as-default envelopes: `admin`
  (rw everywhere, PATH includes `/sbin`), `user` (ro system paths, rw
  `/home`/`/tmp`), `service` (no `/home`, no `/tmp` — boot daemons declare
  what they need in their Cluufiles).
- **`env` vs `env_template`** are two separate tables. `env` holds static
  vars (`SHELL`, `TERM`, `PATH`, `LANG`); `env_template` holds `{user}`-
  substituted vars (`HOME`, `USER`, `LOGNAME`, `PWD`). Substitution is
  one-shot at login — no `{user}` token surfaces downstream.
- **Cluufile MOUNT is strict** (fail loudly on mismatch). Cluufile asks for
  a path + mode; if the envelope doesn't provide at least that, spawn fails
  with exit cookie 126. Unmentioned paths inherit from envelope. `private`
  is a replacement (fresh MemFs), not a narrowing — always permitted.
- **Monotone cap discipline**: `binary.caps ⊆ shell.caps ⊆ envelope.caps
  ⊆ procmgr.caps`. Every step narrows, never widens. The envelope is the
  highest cap-level any user binary can ever reach.
- **Shell env**: `set` = shell-local; `export` = inherited by children.
  Child env = envelope's resolved env with `vars ∩ exported` overlaid.
  Shell→newlib `_environ` mirror is one-way (C-side `setenv` doesn't
  escape the binary — correct POSIX). Shell sources `/etc/shellrc` then
  `~/.shellrc` at startup; missing files silently skipped.

## Users and identity

A **user** is a named entry in `/etc/users.toml` that maps to a set of
session defaults. There are no UIDs. CLUU does not have Unix-style file
permission checks. The VFS view IS the access control. If Alice's view does
not include `/home/bob`, she cannot access it, regardless of what the ext2
inode metadata says.

```toml
# /etc/users.toml

[user.alice]
home = "/home/alice"
shell = "/bin/shell"
profile = "user"           # default session profile at login
escalate = "admin"         # maximum profile via sudo (optional)

[user.bob]
home = "/home/bob"
shell = "/bin/shell"
profile = "user"
# no escalate = sudo rejected unconditionally

[user.root]
home = "/root"
shell = "/bin/shell"
profile = "admin"
escalate = "supervisor"
```

Fields: **home** (absolute path), **shell** (resolved through session
view), **profile** (the CapProfile for the login session), **escalate**
(optional ceiling profile for `sudo`; if absent, escalation is rejected).
Only procmgr (SUPERVISOR) can read this file, it is not in any USER or
ADMIN view.

## Session as top-level container

A session is a **top-level container** that binds user identity to the
container model: `Session = Container + UserIdentity + VT Attachment`. It
has a `container_id`, a profile (from the user record), a VFS view (built
from the user record plus profile defaults), and private storage. Its
entrypoint is the user's shell. All containers the user launches are
children of the session, they inherit the session's view and cascade on
logout.

The session container is spawned by procmgr after authentication. It has
`parent_container_id = 0`, top-level, not a child of the VT container.
The VT is an I/O adapter attached via IPC wiring, not a lifecycle parent.

## VT-session attachment

Sessions and VTs are **siblings, not parent-child**. They are connected by
IPC wiring, not by container lifecycle. The attachment is an IPC endpoint
pair: tty holds a send token to the session's shell stdin, and the
session's shell holds a send token to tty's output.

```text
procmgr
  ├─ vtmgr (Tier 2 autostart)
  │   ├─ VT:0 (tty:0, I/O adapter)  ──IPC──┐
  │   └─ VT:1 (tty:1, I/O adapter)  ──IPC──┤
  └─ Sessions (top-level, parent=0)         │
      ├─ Session:alice (USER) ◄─────────────┘ (attached to VT:0)
      │   ├─ shell, editor (nested), ...
      └─ Session:bob (USER) ◄─── (attached to VT:1)
```

Attachment lifecycle:

| Event | VT | Session | Attachment |
|-------|-----|---------|------------|
| Login succeeds | Running | Created (parent=0) | Established |
| tty crashes | Dies | **Survives** | Broken → vtmgr respawns VT, procmgr reattaches |
| User logs out | Running | Dies (cascades children) | Broken → tty returns to login |
| VT switch away/back | Deactivated/Activated | Running | Paused/Resumed |

This is analogous to tmux/screen: the session persists independently of the
terminal. When tty crashes, procmgr detects VT death, vtmgr spawns a new
VT, and procmgr reattaches the new tty to the surviving session. The user
loses a few seconds of display, not their work.

## Login flow

```text
1. vtmgr → container run vt (instance=1)
2. VT container starts (profile=0x05: IPC+REGISTRY), tty:1 displays login
3. tty:1 reads username + password → PROCMGR_SESSION_LOGIN(...)
4. procmgr verifies against /etc/users.toml (invalid → error)
5. procmgr builds session:
     profile = user_record.profile, view = profile default with <user>
     parent_container_id = 0, entrypoint = user_record.shell
6. procmgr spawns session container, wires shell stdin/stdout to tty:1
7. tty:1 switches to terminal mode
8. shell exits → session destroyed (cascading) → tty returns to login
```

Key properties: **tty never holds credentials**, it forwards them to
procmgr via IPC, and the VT container has no VFS access. **Views only
narrow**, procmgr (SUPERVISOR) narrows to the user's view. **VT survives
logout**, tty is separate from the session. **Session survives VT crash**,
session is top-level (`parent=0`).

### Compositor-native login redesign (2026-05-12)

The tty-based autologin is replaced by a compositor-native, modal
`/bin/login` flow. After boot the user sees a fullscreen login window on
the compositor VT; successful auth spawns a cluuterm window holding a
shell. Raw VT consoles (VT0–VT3) remain reachable via Ctrl-Alt-F1..F4 and
each shows a banner + interactive `login:` prompt with no autologin.

- **authd** — new primordial service that consumes `tpmd` for hashing and
  owns `/etc/users.toml`. Procmgr keeps session ownership but RPCs authd
  for credential checks (`AUTHD_VERIFY_LABEL`, `AUTHD_USER_LOOKUP_LABEL`).
  No PAM, no nsswitch, no pluggable backends. Read-only at runtime; no
  write API.
- **`/bin/login`** — compositor-native rewrite. `WIN_REGISTER` with
  fullscreen=true, modal=true, focus=locked (compositor refuses focus
  loss while modal is up — security: a rogue client cannot steal focus
  during login). Cell-grid SHM, same primitive cluuterm uses. Tab
  toggles field focus, Enter submits. On success: process exits; procmgr
  does the rest. On failure: re-prompt in place, retry forever.
- **`PROCMGR_SESSION_LOGIN_LABEL`** payload gains a leading
  `session_kind` byte (0 = tty, 1 = compositor). Compositor session →
  procmgr spawns cluuterm bound to new session; tty session → existing
  exec-shell-in-tty path.
- **getty** — new binary for VT0–VT3 raw console. Sessionless, holds
  `RIGHT_SESSION_CREATE`, bound to a specific `/dev/tty<n>`. Prints
  sysinfo banner, prompts login/password, sends `SESSION_LOGIN` with
  `session_kind=0`. `RESTART always` → procmgr respawns on shell exit.
- **vtmgr boot-VT fix** — `active_vt` initialized to
  `DEFAULT_COMPOSITOR_VT` (4) at construction, dropping the
  `boot_switch_pending` race. Compositor renders at boot instead of
  waiting on a pin message.
- **Compositor scope additions**: modal/fullscreen flag on
  `WIN_REGISTER`, `WIN_RESIZE` app notify, single text-cell cursor in
  focused window (software-drawn, 500 ms blink). Mouse delivery deferred
  to sub-project C; login modal works keyboard-only.

### Session lifecycle (2026-05-18)

Session lifecycle is redesigned as a procmgr-owned typed object
(`SessionObject`) addressed by IPC token — replacing the fragmented state
spread across procmgr (`SessionEntry`), VFS (`PtsEntry`), and cluuterm
(local terminal state) with no back-pointers between them.

- **One persistent compositor** for the lifetime of the seat. No
  system/user swap, no `kill_system_compositor()` / `spawn_user_compositor()`
  dance, no `session_mode` PARAM discriminator (0=system vs 1=user), no
  8 admin `force_unregister` calls on `compositor:*` services. The 2 s
  `COMPOSITOR_READY_LABEL` wait — the longest-standing open
  `feedback_no_timeouts` violation — is deleted entirely.
- **Login is a regular compositor client** (sessionless binary spawned by
  compositor at boot and on logout). Compositor subscribes to
  `SESSION_ENDED` for each session it sees; on event, closes the
  session's windows and respawns login.
- **`SessionObject`** — `{ id, user_name, profile, creator_pid,
  leader_pid, state, refcount, subscribers, created_at }`. Member
  tracking is derived on demand by walking `ProcessEntry.session_id` —
  no redundant member list (keeps procmgr stateless per
  `feedback_procmgr_stateless`). Children spawned by session-procmgr
  (not root-procmgr) are registered via `SESSION_CHILD_REGISTER` so
  root-procmgr's `pid_to_session` map stays consistent for
  `SET_LEADER`'s `check_member` predicate.
- **Session leader** is explicit via `SESSION_SET_LEADER` (set-once).
  Leader exit (clean, crash, or kill) cascades: SIGHUP to members,
  fanout `SESSION_ENDED` to subscribers, cleanup via cap revocation.
  Creator death before `SET_LEADER` also destroys the session (orphan
  protection).
- **Verb set** (labels 89–96): `SESSION_CREATE`, `SESSION_DESTROY`,
  `SESSION_QUERY`, `SESSION_SUBSCRIBE`, `SESSION_DERIVE_TOKEN`,
  `SESSION_ENDED` (async event), `SESSION_SET_LEADER`,
  `SESSION_CHILD_REGISTER`. Rights bitmask:
  `CONTROL | QUERY | SUBSCRIBE | JOIN`. `DERIVE_TOKEN` narrows; derived
  rights must be a strict subset — enforced in procmgr.
  `SESSION_CHILD_REGISTER` is called by session-procmgr after each
  successful spawn to report the child PID to root-procmgr. Root-procmgr
  inserts the PID into `pid_to_session` so `SESSION_SET_LEADER`'s
  `check_member` predicate succeeds for children spawned via
  session-procmgr (which bypasses root-procmgr's spawn path). The session
  token (RIGHT_SESSION_CONTROL) is the authority — possession proves the
  caller received it from the session creator. No runtime ACL.
- **Refcount discipline**: `DERIVE_TOKEN` bumps refcount; `procmgr::spawn`
  with `envelope.session = Some(token)` does NOT bump again (child
  membership tracked via `ProcessEntry.session_id`); subscriber
  registration bumps; decrements happen on cap revocation.
- **Multi-user**: three text-VT users and one graphical user can be
  logged in concurrently. Each session independent — separate
  `session_id`, token, `/dev/pts/` overlay, `/home` view. Compositor vs
  getty: no protocol difference — both call `SESSION_CREATE` +
  `SESSION_SET_LEADER`. Differences are environmental (graphical primary
  vs text shell; compositor subscribes via `COMPOSITOR_SESSION_HANDOFF`,
  getty does not).
- **Compositor death**: `RESTART always` respawns; in-memory
  session-window table lost; previously-running cluuterms talk to a
  now-revoked endpoint → `EBADTOKEN` → cluuterm exits → cascade.
  Effective forced-logout for all users — acceptable failure mode.

### Session-as-container + unified /dev stdio (2026-05-14)

CLUU's process model converges on **seL4-style capability propagation
underneath, POSIX-shaped surface on top**. After this design lands:

- All terminal-like devices live under `/dev` (`tty0..3`, `pts/<id>`,
  `console`, `fb0`, `null`, `zero`, `urandom`). The kernel + system
  services never expose stdio through anything other than a `/dev` entry.
- The shell and every other userspace program reads stdin with POSIX
  `read(0, buf, n)`. No `TTY_READ_LABEL` push path, no `recv_any` on
  `TOKEN_STDIN`. Pipes, redirections, ttys, and pts all converge on
  `_read`/`_write` in libcluu.
- Login establishes a **session container**. From that moment, every
  user-visible process — the user-mode compositor, cluuterm, future apps
  — spawns inside that container under the user's envelope. Logout tears
  the container down; nothing the user touched survives.
- `tty` is refactored to be exactly the same shape as `cluuterm` from
  VFS's point of view: a process owning a `/dev/...` node, replying to
  read pulls, accepting write pushes. The single difference is that
  `tty` registers its nodes statically at boot rather than on demand.
- **Three dispatch legs** converge in libcluu `_read`/`_write`:
  `is_tty()` → `TTY_READ_REQUEST_LABEL` call; `is_pipe()` →
  `PIPE_DATA_LABEL` recv; `remote_fd.is_some()` → `VFS_READ_LABEL` /
  `PTS_READ_LABEL`. No code anywhere depends on `TOKEN_STDIN` being an
  active IPC endpoint.
- **`/etc/envelopes.toml`** grows `vt_text` and `vt_graphical` profile
  shapes. Procmgr picks one based on `session_kind`, substitutes `{vt}`
  (text only, validated 0..=3) and `{user}`. After substitution, every
  mount entry is matched against procmgr's full view to confirm it
  doesn't escape — reject session if any entry escapes.
- **FDAC ↔ seL4 mapping**: token handles in `FdEntry` = capability slots
  in CSpace; `token_derive(endpoint, rights)` + `vfs_derive_child_fd` =
  `seL4_CNode_Mint`; FDAC payload on `PROCMGR_SPAWN_LABEL` = `extraCaps`
  on IPC. The POSIX `posix_spawn_file_actions_t` surface is the Unix API;
  capability-pass-at-spawn is the mechanism underneath. Two views, one
  mechanism.

## Escalation: sudo

`sudo` does not widen the current session. It creates a **new container**
with an elevated profile, derived from procmgr's SUPERVISOR authority.

```text
Alice's session (USER = 0x0F)
  $ sudo reboot
  shell → PROCMGR_ESCALATE(password, "/bin/reboot")
  procmgr:
    1. verify password against alice's record
    2. check alice.escalate = "admin" → authorized
    3. build elevated container:
         profile = ADMIN (0x8F)
         view = ADMIN default with alice's home
         parent = Alice's session (cascading)
    4. spawn container, runs command, exits
```

The elevated container's view is derived from the escalation profile's
default view template, NOT from the current session's view. This is why
`sudo cat /etc/shadow` works: ADMIN's default view includes `/etc`, even
though Alice's USER view does not. Alice's session remains USER throughout.
The elevated container is a child that cascades on logout. `sudo -s`
(elevated shell) uses the same mechanism with command = shell binary.

## Identity switch: su

`su bob` creates a **nested session** with Bob's identity, inside the
current session's container tree.

```text
Alice's session (USER, view includes /home/alice)
  $ su bob
  shell → PROCMGR_SESSION_LOGIN("bob", password)
  procmgr:
    1. verify Bob's password
    2. build session from Bob's user record:
         profile = bob.profile (USER)
         view = USER default with bob's home
         parent = Alice's session (cascading)
    3. spawn Bob's session container
```

Bob's view is derived from procmgr's SUPERVISOR authority and Bob's user
record, NOT narrowed from Alice's view. This is why `su` works even though
Alice's view does not include `/home/bob`. Procmgr is creating the
container, and procmgr has SUPERVISOR. If Alice logs out, Bob's nested
session cascades, Alice's VT session is the lifecycle root.

## Session security properties

| Operation | Credential check | Profile derivation | View derivation |
|-----------|-----------------|-------------------|-----------------|
| login | password → procmgr | `user_record.profile` | profile default + user home |
| su | target's password → procmgr | target's `user_record.profile` | target's profile default + target home |
| sudo | own password → procmgr | `user_record.escalate` | escalation profile default + own home |

All three require password verification by procmgr, create a new container
(never widen existing), derive views from procmgr's SUPERVISOR authority,
and cascade on parent session death. Users cannot escalate beyond their
`escalate` ceiling, `su` without the target's password, access paths
outside their session view, or forge a session (only procmgr creates
session containers).

## Plan lessons — sessions

Distilled implementation lessons from session-related plans. 2-5 lines
each; see the dated plan file for the long form.

### session-object-typed-token (2026-05-18-plan3-session-lifecycle)

The system/user compositor swap was replaced with one persistent
compositor. Login = sessionless client respawned on logout.
`SessionObject` becomes a procmgr-owned typed object addressed by IPC token
(cap-narrow-derive). Session leader is explicit `SESSION_SET_LEADER`
(set-once). Leader exit cascades `SIGHUP` + `SESSION_ENDED` fanout.
Multi-user concurrent VTs supported via getty on `/dev/tty1..3`. The 2 s
`COMPOSITOR_READY_LABEL` wait was deleted — it was a timeout-as-deadlock-guard
that violated the no-timeouts discipline. Verb set: labels 82-88.

### envelope-vt-text-vs-vt-graphical (2026-05-14-plan2-envelope-vt-user-substitution)

`/etc/envelopes.toml` carries per-shape mount lists; `{vt}` and `{user}`
substitutions apply at SESSION_LOGIN. Each session sees the strict subset
of `/dev` defined by its envelope and VT index. Root needs a real
`env_template` (HOME etc.) — an empty root template was the root cause of
HOME-not-propagating. `vfs_view.rs` enforces monotone narrowing;
substitution must not slip past that check.

### session-cascade-teardown (2026-05-14-plan5-logout-teardown)

When a session-root process exits (clean logout, crash, or `exit`),
procmgr walks `container_children[session_cid]` in reverse-dependency
order, sends `THREAD_KILL` to each, reaps exit cookies, drops the
session_table entry, then respawns the appropriate stand-in (system
compositor for VT4, login prompt for VT0-3). The exit-cookie handler is
the hook point; existing `poll_exit_notifications` already drains the
channel.

### compositor-swap-mode-flag (2026-05-14-plan3-compositor-swap)

At login on VT4, procmgr kills the system-mode compositor and spawns a
fresh compositor under the user's envelope inside the session container.
The *same* binary runs in both modes; what differs is the VFS view +
envelope env it inherits. Mode is a flag read from `ProcessInfo` params.
No new compositor binary — system vs user is an envelope distinction, not a
code fork.

### autologin-gate-on-build-constant (2026-05-12-autologin-rip)

`try_auto_login` becomes a no-op when `SHELL_AUTOSTART_CMD.is_empty()`. The
gate is a shared `libcluu` constant read by both procmgr and tty so both
crates see the same value. The text-mode interactive login in
`tty/src/context.rs` becomes the default entry point on every text VT.
Harness cases that depended on `CLUU_SHELL_AUTOSTART_CMD` keep working
without per-test changes.

### procmgr-cap-possession-equals-authority (2026-05-21-procmgr-cap-refactor)

All runtime identity checks in procmgr were deleted. Authority is
structural: `root-procmgr` mints session-scoped caps; each
`session-procmgr` sub-mints child-scoped caps; cap derivation is
monotone-narrowing. PIDs encode `(8-bit session_id | 23-bit local pid)`.
Cascade teardown on session-procmgr death is via cap revocation. A
`cap-purity` lint gate (`xtask check-cap-purity`) grep-rejects new identity
checks. Sessions are now addressed by capability token, not by caller
inspection.
