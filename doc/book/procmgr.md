# Process Management

CLUU's process management is split across two binaries, `root-procmgr`
(system-scope) and `session-procmgr` (per-session), plus a shared library
`procmgr-common`. This is the hierarchical, Genode-`init`-style model described
in [Session Encapsulation](../sessions/index.html).

## root-procmgr

**System-scope, primordial.** Spawned by init as a boot-critical service. Owns
every session in the system, mints session-scoped capability tokens, and runs
the system-wide IPC dispatch loop. SYSTEM cap-set, the broadest authority.

### What it does

- **Boot autostart**: spawns kbd, console, vtmgr, tty, and other autostart
  services from `autostart.toml`.
- **Session management**: `SESSION_CREATE` (spawns session-procmgr +
  session-vfs per login), `SESSION_DESTROY` (cascade teardown via cap
  revocation), `SESSION_HANDOFF` (VT handoff).
- **Login**: authenticates users against `/etc/users.toml`, resolves envelopes
  from `/etc/envelopes.toml`, spawns the session shell/cluuterm.
- **Container lifecycle**: `PROCMGR_SPAWN_UNIFIED`, the unified spawn verb
  that replaces the legacy spawn/container-run paths.
- **Restart policy**: `Never`, `Always`, `OnFailure` with exponential backoff.
- **Escalation**: `sudo`/`su`, authenticates and spawns with elevated profile.
- **Process groups**: PGID table for job control.
- **Pipes**: pipe table indexed by lower 16 bits of pipe ID.
- **/proc queries**: `proc_query_all`, across all sessions (root godmode).
- **Shutdown**: `shutdown_kill_sessions` → `shutdown_kill_tier2` →
  `shutdown_flush_vfs`.

### Key modules

| Module | Role |
|--------|------|
| `cap_broker` | Cap minting/sub-minting with monotone-narrowing |
| `dispatch` | IPC dispatch table |
| `escalate` | sudo/escalate handler |
| `pg_table` | Process group table |
| `proc_query_all` | /proc queries across all sessions (root godmode) |
| `restart_root` | Restart policy for root-level services |
| `services` | Service spawn helpers |
| `session_directory` | Session creation/destruction |
| `session_table` | Session table |
| `shutdown` | System shutdown sequence |
| `spawn` | Spawn helpers |

## session-procmgr

**Per-session.** One instance per authenticated login. Owns the session's
children, exit cookies, signals, pipes, process groups, controlling terminals.
Sub-mints child-scoped caps from the session cap.

### What it does

- **Child spawn**: creates child processes with narrowed caps and views.
- **Child monitoring**: exit notifications, restart policy.
- **Signals**: kill, signal delivery.
- **Pipes**: pipe creation, pipe I/O handlers.
- **Process groups**: PGID table (session-scoped).
- **Controlling terminal**: ctty management.
- **/proc queries**: session-scoped (only this session's processes).

### Key modules

| Module | Role |
|--------|------|
| `cap_broker_session` | Session-scoped cap sub-minting |
| `child_monitor` | Child exit monitoring |
| `child_table` | Child process table |
| `ctty` | Controlling terminal |
| `dispatch` | IPC dispatch table |
| `elf_spawn` | ELF loading + spawn (cfg-gated) |
| `kill` | kill/signal |
| `pg_table` | Process group table (session-scoped) |
| `pipe_handlers` | Pipe IPC handlers |
| `pipe_registry` | Pipe registry |
| `proc_pid` | PID queries |
| `proc_query` | /proc queries (session-scoped) |
| `restart` | Restart policy |

### The spawn flow in session-procmgr

```text
1. Receive PROCMGR_SPAWN_UNIFIED with SpawnEnvelope
2. Deserialize envelope (cluu_wire::SpawnEnvelope)
3. Resolve manifest from cache
4. Build child view from envelope mount policy
5. Verify monotone: child view ≤ session view
6. Kernel: space_create + thread_create(START_SUSPENDED)
7. VFS (async): VFS_DERIVE_CHILD_FD, derive child's fd table
8. VFS: VFS_SET_VIEW, install child's view
9. Kernel: thread_resume, child starts
```

The async runtime is used for step 7 because `VFS_DERIVE_CHILD_FD` is an IPC
call to VFS, and session-procmgr is single-threaded. Without async, this would
deadlock if VFS needed to call back into session-procmgr.

## procmgr-common

Shared library compiled into both root-procmgr and session-procmgr.

| Module | Role |
|--------|------|
| `pid` | PID encoding (8-bit session_id \| 23-bit local pid) |
| `labels` | IPC label constants |
| `wire` | Wire format types (SpawnEnvelope, SessionEnvelope) |
| `envelopes` | Envelope types |
| `manifest_cache` | Cluufile manifest caching |
| `mount_policy` | MOUNT policy parsing + composition |
| `view_table` | VfsViewTable + monotone-narrowing check |
| `mint_guard` | RAII guard for cap minting |
| `handler` | Shared handler traits |
| `kernel_iface` | Kernel interface abstraction |

## cluu_wire

IPC wire format types, the single source of truth for IPC payload formats
shared between libcluu callers and services.

| Module | Role |
|--------|------|
| `spawn` | SpawnEnvelope (unified spawn protocol) |
| `session` | Session lifecycle verbs (SESSION_CREATE, DESTROY, HANDOFF) |
| `pts` | PTS (pseudo-terminal) wire types |
| `primordial` | Primordial seed verb (init → procmgr handoff) |

## The unified spawn protocol

CLUU collapsed six distinct spawn paths into one:

1. `PROCMGR_SPAWN_UNIFIED_LABEL`, the single IPC verb.
2. `cluu_proto::SpawnEnvelope`, the single serialized envelope type.
3. One procmgr-internal spawn function.
4. `PROCMGR_PRIMORDIAL_SEED`, bootstrap verb used exactly once at init's
   hand-off to procmgr.

Every other call site collapses onto these two verbs plus the in-process
function. This eliminated the duplicated fd-inheritance wiring, notify-cap
derivation, and identity-recording rules that produced bugs like "two
cluuterms" in `ps` output.

### Spawn-path inventory (2026-05-18)

The inventory that motivated the unification mapped six distinct spawn
entry points, each with its own wire format for FdInherit and exit-notify
wiring: (1) init kernel spawn, (2) procmgr autostart, (3)
`PROCMGR_SESSION_LOGIN_LABEL` internal spawns, (4) `PROCMGR_SPAWN_LABEL`,
(5) `PROCMGR_CONTAINER_RUN_LABEL`, (6) cluuterm `posix_spawn` via newlib.
The inventory also catalogued the parallel fd-inheritance mechanisms
(cluuterm `posix_spawn_file_actions_adddup2` vs procmgr FdInherit blob),
the legacy-TTY vs PTS protocol split, the fragmented session-state
ownership (procmgr `SessionEntry` + VFS `PtsEntry` + cluuterm local state
with no back-pointers), the "two cluuterms" ps display bug (identity
recorded from spawning image, not spawned image), and the 2 s
`COMPOSITOR_READY_LABEL` timeout violation.

### Unified spawn protocol design (2026-05-18)

The unification spec defines **one** procmgr verb
(`PROCMGR_SPAWN_UNIFIED_LABEL = 80`), **one** serialized envelope type
(`cluu_proto::SpawnEnvelope`), **one** procmgr-internal spawn function
(`procmgr::spawn(envelope, caller_pid)`), and a one-shot bootstrap verb
(`PROCMGR_PRIMORDIAL_SEED_LABEL = 81`, accepted only when
`caller_pid == INIT_PID`, rejected after first success). Key decisions:

- **`SpawnEnvelope`** carries `{ image, args, env, view, fd_inherit,
  session, notify }`. Postcard-serialized, bounded at 64 KiB; procmgr
  rejects oversize at deserialize. `view` is `ViewSource::Derive(token)`
  (caller's parent-view token; narrow-only) or `ViewSource::BootstrapRoot`
  (valid only when `caller_pid == INIT_PID` inside the SEED handler;
  permanently rejected after).
- **FdInherit is the sole fd-wiring mechanism on the wire.** POSIX
  `posix_spawn_file_actions_t` is translated to `Vec<FdInherit>` entries
  in the libcluu shim, parent-side. `adddup2` inside cluuterm retires.
  Rights must be a subset of caller's rights on the parent fd; procmgr
  enforces monotone-decrease. Unlisted child fd slots remain unmapped
  (closes are implicit).
- **Process identity** = `basename(manifest_for(envelope.image).entrypoint)`,
  not the spawning caller's image. Procmgr overrides `argv[0]` to `comm`
  regardless of what the caller put there. Fixes the "two cluuterms" bug:
  when cluuterm (image=`cluuterm`) spawns shell (envelope.image=`shell`),
  the child's comm is `shell`.
- **Restart policy is declared in the spawned image's Cluufile**
  (`RESTART always | never | on_failure max=N window=Ms`), not passed by
  the caller. The Cluufile is the source of truth for "what this process
  IS"; lifecycle policy is part of "what". Default if `RESTART` absent:
  `never`. On respawn, the original envelope is replayed verbatim;
  procmgr's derived caps survive parent death.
- **View derivation is capability-style.** Procmgr owns a typed
  `ViewObject` table (parent-pointer, MOUNT entries, refcount).
  `derive_child_view` narrows parent mounts per manifest MOUNT directives
  in one function — the monotone-decrease invariant sits here, no
  caller-side bypass. Child exit drops refcount; `view_table.gc()`
  removes entries at 0. Child's view token is revoked when the ViewObject
  is destroyed.
- **Session field** is `Option<TokenHandle>`. `None` permitted only for
  system callers (init, autostart) or manifests declaring
  `RIGHT_SESSIONLESS_SPAWN`. `Some(t)` resolves the session, rejects if
  dying, bumps refcount. Membership rule: children may join sessions for
  which the caller holds the token — token existence in the caller's
  table is the authorization, no ambient session-id checks.
- **Notify field** is `Option<TokenHandle>`. Procmgr derives an
  `IPC_SEND` cap into its own table (pattern from commit `a597e09`); on
  child exit, sends `PROC_EXIT_LABEL`. When the caller dies, the
  notify-derived cap is revoked — attempts to send on it silently drop.
- **`procmgr::spawn` is synchronous and bounded.** 10-step body:
  deserialize → load manifest (cached per-image) → resolve/derive view +
  session + notify tokens → allocate Space + Thread → install fd_inherit
  → load ELF → write ProcessInfo page → insert ProcessEntry → start
  thread → return. No step waits on the child. No step uses
  `recv_with_timeout`. Rollback table covers every side effect.
- **No new timeouts.** Every blocking surface uses cap-revocation.
  Procmgr crash → kernel revokes its endpoint → caller's `ipc_call`
  returns `EBADTOKEN` → libcluu translates to
  `SpawnError::Internal(EPROCMGR_DEAD)`.

The earlier `2026-05-14-spawn-unification-design.md` (posix_spawn under
CONTAINER_RUN) is a subset superseded by this broader spec before it
landed.

### Spawn env merge (2026-05-21)

`handle_spawn_unified` originally copied `envelope.env` verbatim into the
argv/env payload — no envelope-default merge. The chain `login → cluuterm
→ shell` lost `PATH`, `HOME`, `USER` at each hop because each caller
passed a minimal env. Shell reached `path_lookup`, walked an empty
`$PATH`, rejected bare commands like `ls`.

Fix: single source of truth for default env (`PATH`, `SHELL`, `LANG`,
`TERM`, `HOME`, `USER`, `LOGNAME`, `PWD`) lives in `/etc/envelopes.toml`.
`handle_spawn_unified` resolves the caller's session, looks up the
user's profile, calls `envelopes::resolve_env(envelope_def, &username)`
with `{user}` substituted, then merges: start from resolved envelope env,
for each `(k, v)` in `envelope.env` **overwrite** — caller wins. No merge
on no-session (boot/service path skips merge, packs `envelope.env`
as-is). No new IPC verb, no `SpawnEnvelope` wire change. Caller env wins
on key conflict so login can set `HOME=/home/balazs/work` and cluuterm
can set `TERM=xterm-256color` without envelope defaults rebasing them.

### Procmgr cap-model refactor (2026-05-21)

The original 7,618-line `main.rs` violated CLUU's cap/view philosophy with
four concrete runtime identity checks: `handle_container_run` called
`caller_profile.can_grant(requested_profile)` at IPC time; `proc_query_list`
walked the session-membership ancestor chain; VFS `/proc/N/stat` opens did
`caller_tid → caller_pid → session_match`; `resolve_caller_session` itself
was a runtime identity resolver. These created TOCTOU windows, divergent
enforcement paths, and forced "what can X do?" audits to run code instead
of read static envelopes/views.

The refactor makes the cap/view model **structural** rather than
**conventional** — possession-equals-authority enforced by the topology of
the system, not by code paths that could regress. Key decisions:

- **Kill all runtime identity checks.** No `resolve_caller_session`, no
  `pid_to_session`, no `caller_profile.can_grant`. Every handler
  dispatches on `(cap, label)`. Cap presence is the authority. A
  `cap-purity lint` (`xtask check-cap-purity`) greps for forbidden
  patterns (`pid_to_session`, `tid_to_pid` for ACL,
  `resolve_caller_session`, `caller_profile`, `can_grant`,
  `session_match`) — pre-commit hook + CI step, build fails on hits.
- **Hierarchical multi-instance, Genode-`init`-style.** root-procmgr
  (SYSTEM cap-set, system-scope) + session-procmgr (per-session,
  session-scoped cap-set). Crash domain = instance: session-procmgr crash
  kills exactly that session; root-procmgr crash = system reboot (init
  panics). Spawn graph = supervision tree: parent holds children's caps,
  cascade-teardown on parent death is automatic via cap revocation.
- **PID layout**: high 8 bits = session_id (0–255, 256 concurrent
  sessions max), low 23 bits = local pid within session (0–8,388,607).
  Globally unique by construction. Session-id derivable from any PID —
  routes exit/fault messages without lookup. Reuse: session destroy
  releases sid; paired with **generation counter** in session caps to
  invalidate stale caps from the previous incarnation.
- **SOLID module split.** Three crates: `procmgr-common` (shared library:
  envelopes, manifest_cache, mount_policy, view_table, pid, labels, wire,
  handler trait), `root-procmgr` (services, session_directory, cap_broker,
  escalate, proc_query_all, shutdown, init_monitor, restart),
  `session-procmgr` (spawn, child_table, pg_table, pipe_registry, ctty,
  proc_query_local, child_monitor, kill). Each handler module exports one
  struct implementing `MsgHandler` trait; dispatcher = static
  `label → fn pointer` table. No god-object state — tables are split,
  narrow APIs; cross-table effects live in handlers. Future async
  migration: trait becomes `async fn`, dispatcher becomes an executor poll
  loop (mechanical).
- **Spawn failure rollback** via `MintGuard` RAII struct — holds minted
  cap-ids, drops → revoke on early return. Single happy-path
  `mem::forget(guard)` after successful start. Prevents cap leak (cap
  pointing to a half-built process).
- **Testing: fresh `pm_*` suite, not legacy `l2_*`.** Coverage targets:
  C1 (statement + branch) ≥ 95% per crate; C2 (path) ≥ 90% on critical
  handlers, 100% on cap-mint paths. Mock kernel surface for unit tests
  without QEMU. Property tests for `cap_broker::sub_mint` monotone
  invariant (10 K random parent caps), `pid::encode/decode` roundtrip,
  session create/destroy uniqueness + generation monotonicity.

## The spawn labels

All process creation goes through procmgr. There are three spawn labels,
each for a different spawn context.

### Service spawn (`PROCMGR_SPAWN_SERVICE_LABEL`)

Used by system services (vtmgr, etc.) to spawn other system services from
initrd. This is the "simple" spawn path, no argv, no env, no exit tracking.
The spawned process gets a `ProcessInfo` with tokens derived per the
requested mode but no PID and no exit notification.

Policy checks: path must start with `sys/` (initrd only), param index 0-9,
token mode 0/1/2 (none, listen, grantable). The spawn endpoint is
capability-gated. After CapProfile integration, the caller must have
`CAP_SPAWN` and the requested profile must be a subset of the caller's.

### User spawn (`PROCMGR_SPAWN_LABEL`)

Used by shells and user programs. Supports argv, env, fd actions (pipes),
exit notification, and PID tracking. Sent via `ipc_call` (synchronous,
caller blocks until procmgr replies with PID). The reply carries error
code, PID, exit cookie (for `waitpid`), and child stdin send token (for
foreground I/O routing). After CapProfile integration, the payload carries
the requested CapProfile bitmask and optional VFS view overrides.

### Container run (`PROCMGR_CONTAINER_RUN_LABEL`)

Used to spawn image containers from manifests. The child gets a fresh
`container_id`, a profile from the manifest (validated as subset of
caller), and a view built by combining the launcher's view with the
container image. See the [Process Isolation Model](process_model/index.html)
chapter for the view construction rules.

## Intra-container binary spawn

A container image can bundle multiple binaries. The entrypoint is spawned
automatically when the container starts. Other binaries can be spawned by
any process within the container using the normal `PROCMGR_SPAWN_LABEL`
path.

**Enforcement rule:** only processes already inside a container
(`container_id != 0`) with `CAP_SPAWN` can use `PROCMGR_SPAWN_LABEL`. Bare
binary spawn outside a container context is rejected. This ensures every
process runs within a container boundary.

The child inherits the parent's container context: same `container_id`,
same view, same profile (or a narrowing). The parent can narrow the
profile (`spawn("/bin/worker", profile=SANDBOXED)`), the view
(`spawn("/bin/editor", view={/tmp})`), or both. If the parent specifies
nothing, the child inherits everything, the common case for shell
commands.

**View-aware binary loading:** when procmgr receives a spawn request for
`/bin/ls`, it resolves the path through the **caller's** VFS view, not
procmgr's own SUPERVISOR view. The caller's view maps `/bin` to
`/var/images/vt/bin/`, so procmgr loads `/var/images/vt/bin/ls`. This
ensures containers can only spawn binaries visible in their view, no
hardcoded paths in procmgr, and different containers can have different
`/bin` contents.

## Two-tier boot model

Services are divided into two tiers based on the bootstrap ordering
constraint: container-run requires procmgr + VFS + ext2, but ext2 requires
virtio-blk, which must be spawned first.

### Tier 1: primordial services (init-spawned from initrd)

These provide the infrastructure that containers depend on. They cannot go
through the container-run flow because the required infrastructure does not
exist yet when they start.

```text
init → registry → timeserver → procmgr → vfs → virtio-blk
```

Init spawns these directly using the root token. They still have
CapProfiles, VFS views, and private storage, they ARE containers in every
functional sense. They just are not spawned from a Cluufile.

### Tier 2: system service containers (procmgr-spawned from ext2)

Once the primordials are running and ext2 is mounted, all subsequent
services are real image containers with Cluufiles. Init sends
`BOOT_PHASE2_LABEL` to procmgr after all primordials are up. Procmgr reads
`/etc/autostart.toml` from ext2 and starts Tier 2 services:

```text
procmgr → container run kbd
procmgr → container run console (instance=0)
procmgr → container run vtmgr
procmgr → container run vt (instance=0)
```

On-demand VT creation (Ctrl+Alt+Fn) goes through the same path: vtmgr sends
`container run vt` with the appropriate `tty_instance` param.

## Three-tier wiring model

Process wiring, how a newly spawned process gets its tokens, endpoints,
params, and service connections, happens at three distinct tiers depending
on how the process was created.

### Tier 1: manifest wiring (container entrypoint)

Applies to the entrypoint binary of an image container (the process created
by `container run`). Source of truth: `manifest.toml`.

The manifest declares the capability profile, grantable endpoint mode,
device tokens, boot parameters, priority, and VFS view. Procmgr reads the
manifest, derives tokens per the profile, creates endpoints, maps device
tokens, sets params from the caller's overrides, and registers the VFS
view. The entrypoint starts with everything it needs, no runtime discovery
required for its core function.

### Tier 2: self-wiring (intra-container secondary binaries)

Applies to binaries spawned within a container via `PROCMGR_SPAWN_LABEL`
(e.g., tty spawns shell, shell spawns ls). Source of truth: inherited
context plus runtime service discovery.

The child inherits its parent's container context: profile (or narrowed),
`TOKEN_IPC` derived from profile, empty `TOKEN_EXTRA` slots, default
params, inherited VFS view, inherited `container_id`, stdio tokens wired to
parent's tty. The key capability is `TOKEN_IPC` with `CREATE` + `GRANT`
rights, letting the child self-wire at runtime: create its own endpoints,
register with the IPC registry, subscribe to services and receive grants.

Example: shell starts with no special endpoints. It creates an endpoint,
registers as "shell" with the registry, subscribes to "tty:N", and receives
tty's endpoint via a registry grant. All wiring happens at runtime through
standard IPC, no procmgr involvement after spawn.

### Tier 3: FDAC (explicit parent-to-child wiring)

Applies to any spawn where the parent needs to pre-wire specific connections
to the child. Source of truth: the parent's spawn request payload (FDAC
block).

FDAC (File Descriptor Action Context) lets the parent set up the child's
stdio or extra token slots at spawn time. This is used for shell pipe chains
(`cat file | grep pattern`, shell creates pipes and wires stdin/stdout of
each stage via FDAC) and for explicit parent-to-child channels where
registry discovery is too slow or inappropriate (e.g., sandboxed plugins
that cannot use the registry).

## Restart policies

Not all containers need the same restart behavior. The policy is determined
by container type and manifest declaration.

| Tier | Containers | Restart policy | Rationale |
|------|-----------|---------------|-----------|
| Primordial | procmgr, vfs, registry, virtio-blk, ext2 | Kernel panic | These ARE the infrastructure. Restarting would leave dangling state in every client. |
| System service | console, kbd, vtmgr, timeserver | Auto-restart | Stateless or recoverable. Clients reconnect via registry. |
| VT | `tty:N` containers | Auto-restart (vtmgr) | Session survives VT crash. vtmgr respawns, reattaches. |
| Session | User login sessions | No restart | Session death = logout. Intentional. |
| User | Containers spawned from session | No restart | Parent (session) decides whether to respawn. |

### Primordial failure

If a Tier 1 primordial dies, the system is in an inconsistent state. VFS
death means every process's file operations fail. Procmgr death means no
new processes can be spawned. Registry death means service discovery stops.
Restarting these would require every client to re-establish connections,
re-register, re-open files, effectively a full reboot but worse because
kernel state is stale. The correct response is a kernel panic with a
diagnostic message. Init detects primordial death (it spawned them, it
holds their exit notification tokens) and triggers the panic.

### System service auto-restart

Tier 2 autostart services are stateless or self-recovering. When one
crashes, procmgr detects container death, checks the restart policy, and if
`restart = "always"` or `"on-failure"`: waits for backoff delay
(exponential: 100ms, 200ms, 400ms, ..., max 10s), re-runs the container
from the same image, resets backoff on successful startup (running for
>30s). If crash count exceeds `max_restarts` (default 5 within 60s),
procmgr logs the error, stops restarting, and notifies the admin session if
any. Registry handles reconnection: when console restarts, it re-registers
its outputs. Clients that subscribed before get new GRANT events and update
their endpoints.

### Manifest declaration

```toml
[lifecycle]
restart = "always"     # "always" | "on-failure" | "never"
max_restarts = 5       # within restart_window seconds
restart_window = 60    # seconds
```

| restart value | When it restarts |
|---------------|-----------------|
| `never` | Container dies, stays dead. Default for user containers. |
| `on-failure` | Restarts if entrypoint exits with non-zero status. |
| `always` | Restarts regardless of exit status. For system services. |

VT containers have `restart = "always"` but the restart is managed by
vtmgr, not directly by procmgr. vtmgr tracks VT state and handles
reattaching sessions. When a VT container dies, procmgr notifies vtmgr,
vtmgr clears the `vt_spawned` bit, immediately respawns if the VT is
active, or lazily respawns on next switch to that VT if inactive.

## Graceful shutdown

The system shuts down in reverse boot order. Procmgr orchestrates the
sequence after receiving a shutdown request.

### Shutdown sequence

```text
1. Notify sessions: SHUTDOWN_NOTIFY with grace period (default 5s)
   after grace, procmgr forcibly kills remaining sessions
2. Kill user containers: all session containers destroyed (cascading)
3. Stop Tier 2 services (reverse autostart order):
   each gets SHUTDOWN_NOTIFY + 1s grace, then forcibly killed
4. Unmount filesystems: VFS flushes, ext2 syncs, virtio-blk flushes
5. Stop Tier 1 primordials (reverse init order):
   ext2, virtio-blk, vfs, registry, procmgr (self-stop)
6. init receives "all stopped" or timeout → kernel shutdown/reboot
```

### Shutdown signals

Processes learn about shutdown through two mechanisms. **SHUTDOWN_NOTIFY**
(IPC message) carries a grace period value. Well-behaved processes save
state and exit cleanly within the grace period. **Forcible kill** (kernel
`thread_destroy`) happens after the grace period expires, a hard kill with
no signal handler, no cleanup, just gone.

There are no Unix-style signals (SIGTERM, SIGKILL) in CLUU. The IPC message
IS the signal. If a process does not handle it (no listener on that
endpoint), the grace period elapses and it gets killed.

### Grace periods

| Container type | Grace period | Rationale |
|---------------|-------------|-----------|
| Session | 5s | User may have unsaved work. Shells can prompt. |
| User container | 2s | Inherited from session shutdown cascade. |
| Tier 2 service | 1s | Stateless services; flush buffers and exit. |
| Tier 1 primordial | 0s (immediate) | Shutdown is past the point of no return. |

### Shutdown triggers

| Trigger | Who sends it | Requirement |
|---------|-------------|-------------|
| `reboot` / `poweroff` command | ADMIN session shell | `CAP_ADMIN` |
| Hardware power button | kbd (ACPI event) | `CAP_ADMIN` |
| Ctrl+Alt+Del | kbd | `CAP_ADMIN` |

All shutdown triggers require the sender to have `CAP_ADMIN`. A USER session
cannot trigger shutdown, only ADMIN or SUPERVISOR can.

## Plan lessons — procmgr

Distilled implementation lessons from procmgr-related plans. 2-5 lines
each; see the dated plan file for the long form.

### unified-spawn-protocol (2026-05-18-plan1-unified-spawn-protocol)

Six existing spawn paths (init kernel batch, procmgr autostart,
SESSION_LOGIN internal, PROCMGR_SPAWN, PROCMGR_CONTAINER_RUN, cluuterm
posix_spawn) collapsed into one IPC verb (`PROCMGR_SPAWN_UNIFIED_LABEL =
80`) carrying a postcard-serialized `SpawnEnvelope`. A one-shot
`PROCMGR_PRIMORDIAL_SEED_LABEL = 81` handles init → procmgr handoff.
`ViewObject` becomes a procmgr-owned typed object; restart policy moves
from envelope to manifest. The lesson: every additional spawn path is one
more thing to fix later — collapse them early.

### procmgr-spawn-broker-pattern (2026-05-14-plan4-procmgr-spawn-broker)

The user-mode compositor holds zero spawn capability of its own. To open a
menu app, it sends `PROCMGR_SPAWN_SESSION_LABEL` to procmgr; procmgr
verifies the caller is the live session compositor (sender_tid lookup) and
spawns the named image as a sibling in the same session container. Pure
broker pattern — no additional capability handed to the compositor. A
separate label from `PROCMGR_SPAWN_LABEL` ensures arbitrary processes can't
trigger the broker path.

### env-merge-caller-wins (2026-05-21-spawn-env-merge)

`procmgr::handle_spawn_unified` merges `/etc/envelopes.toml` defaults
*under* the caller-supplied env: resolve caller's session → look up user
profile → resolve envelope → `resolve_env` → overlay caller's
`envelope.env` on top. Caller wins on key conflict. No merge on the
no-session (boot/service) path — boot services don't have user envelopes.
No new IPC verb, no wire change.

### session-cascade-teardown (2026-05-14-plan5-logout-teardown)

When a session-root process exits, procmgr walks
`container_children[session_cid]` in reverse-dependency order, sends
`THREAD_KILL` to each, reaps exit cookies, drops the session_table entry,
then respawns the appropriate stand-in. The exit-cookie handler is the
hook point; existing `poll_exit_notifications` already drains the channel.
Reverse-dependency order matters — killing a parent before its children
leaves orphans.

### cap-possession-equals-authority (2026-05-21-procmgr-cap-refactor)

All runtime identity checks in procmgr were deleted. The single 7,618-line
procmgr split into `procmgr-common` (lib), `root-procmgr` (system-scope
primordial), and `session-procmgr` (per-session instances). Cap derivation
is monotone-narrowing; `MintGuard` is RAII rollback for failed multi-step
mints. A `cap-purity` lint gate (`xtask check-cap-purity`) grep-rejects new
identity checks. Hierarchical multi-instance, Genode-`init`-style.

### posix-read-0-fdac-injection (2026-05-14-bug-c-shell-stdin-via-fd0)

Procmgr opens the right `/dev/...` node at shell-spawn time using its own
`VfsClient`, builds an FDAC payload, and injects it through the existing
`spawn_service_with_env` path. The shell reads stdin via blocking
`read(0)`; fd 0 is bound to `/dev/tty<N>` or `/dev/pts/<id>` via FDAC at
spawn time. No `TTY_READ_LABEL` push protocol. The lesson: when two paths
diverge (legacy VT0 push vs cluuterm POSIX), unify on the POSIX path and
delete the push. The architecture target was first sketched in
`2026-05-14-shell-stdio-posix-unify` (a short note); `2026-05-14-bug-c-shell-stdin-via-fd0`
was the execution plan that closed Bug C.
