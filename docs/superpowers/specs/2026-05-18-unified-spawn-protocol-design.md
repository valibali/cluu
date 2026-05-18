# Unified spawn protocol — design

**Date:** 2026-05-18
**Status:** spec — pre-implementation
**Predecessor inventory:** `docs/superpowers/specs/2026-05-18-spawn-window-pty-inventory.md`
**Supersedes:** `docs/superpowers/specs/2026-05-14-spawn-unification-design.md` (subset; this spec is broader and replaces it before that one lands).

## 1. Why

Six distinct spawn paths exist today (init kernel primordial batch,
procmgr autostart, `PROCMGR_SESSION_LOGIN_LABEL` internal spawns,
`PROCMGR_SPAWN_LABEL`, `PROCMGR_CONTAINER_RUN_LABEL`, cluuterm
`posix_spawn` via newlib). Each has its own wire format, its own
fd-inheritance wiring, its own notify-cap derivation, and its own
identity-recording rules. The inventory document maps the duplication
in detail.

This duplication has already produced concrete bugs: the "two
cluuterms" ps display (identity recorded from spawning image, not
spawned image), the FdInherit-vs-`adddup2` dual fd path, the 2 s
`COMPOSITOR_READY` timeout that violates `feedback_no_timeouts`, and
the silent envelope-field-order drift between caller and procmgr that
forced commits `860f996` and `a597e09`.

This spec defines **one** procmgr verb, **one** serialized envelope
type, **one** procmgr-internal spawn function, and a small bootstrap
verb (`PROCMGR_PRIMORDIAL_SEED`) used exactly once at init's hand-off
to procmgr. Every other call site collapses onto these two verbs plus
the in-process function.

## 2. Goals and non-goals

### Goals

1. One IPC verb (`PROCMGR_SPAWN_UNIFIED_LABEL`) replaces
   `PROCMGR_SPAWN_LABEL` and `PROCMGR_CONTAINER_RUN_LABEL`.
2. One Rust type (`cluu_proto::SpawnEnvelope`) is the contract — caller
   constructs, procmgr deserializes, no offset bookkeeping.
3. FdInherit is the sole fd-wiring mechanism on the wire. `adddup2`
   inside cluuterm retires. POSIX `posix_spawn_file_actions_t` is
   translated to FdInherit entries in the libcluu shim, parent-side.
4. Process identity (`ProcessEntry.comm`,
   eventually `/proc/<pid>/comm`) is the basename of the spawned
   image's manifest `ENTRYPOINT`, not the spawning caller's image.
5. Restart policy is declared in the spawned image's Cluufile, not
   passed by the caller. The Cluufile is the source of truth for
   "what this process IS"; lifecycle policy is part of "what".
6. View derivation is capability-style: procmgr owns a typed
   `ViewObject` table, callers hold IPC tokens routing to their view,
   spawn derives a narrowed child view per manifest MOUNT directives.
   Monotone descent (child ≤ parent) is enforced in one place.
7. Session field is `Option<TokenHandle>`; sessionless permitted only
   for system callers (init, autostart) or manifests that declare
   `RIGHT_SESSIONLESS_SPAWN`.
8. Notify field is `Option<TokenHandle>`; procmgr derives an IPC_SEND
   cap into its own table per the `a597e09` pattern.
9. Init's role shrinks to: kernel-spawn procmgr, send
   `PROCMGR_PRIMORDIAL_SEED` with the primordial envelope list,
   monitor primordial exits.
10. No new timeouts. Every blocking surface uses cap-revocation.

### Non-goals

- Backwards compatibility on the wire. CLUU rebuilds userspace as a
  unit. All callers flip together at each migration step.
- New kernel objects. No kernel-side process struct, no kernel-side
  view object, no kernel-side session struct. Procmgr remains the
  sole owner of process lifecycle (per
  `unified-process-model-decision-2026-05-18`).
- Session lifecycle redesign. That is spec 3 of the inventory's §12
  decomposition. Spec 1 only defines the `session` envelope field and
  the spawn-time attach/refcount.
- Terminal/PTY redesign. That is spec 2 of the §12 decomposition.
- Window-protocol formalization. That is spec 4.

## 3. Architecture

```
                        ┌──────────────────────────┐
                        │  cluu_proto::SpawnEnvelope│
                        │  (Rust struct, postcard)  │
                        └─────────────▲────────────┘
                                      │
              ┌───────────────────────┼───────────────────────┐
              │                       │                       │
        caller-side                in-process              wire
   libcluu::spawn(env)        procmgr::spawn(env, pid)   payload
   serialize → IPC            ←─called by either         (one
   wait reply                                            label)
              │                       │                       │
              └────────────►  procmgr::spawn(env, caller_pid)  ◄┘
                              ├ resolve view token
                              ├ resolve session token
                              ├ resolve notify token
                              ├ load manifest (cache)
                              ├ derive child view
                              ├ allocate Space + Thread
                              ├ install fd_inherit
                              ├ load ELF
                              ├ write ProcessInfo page
                              ├ insert ProcessEntry
                              └ start thread
                              return SpawnReply | SpawnError
```

Both the IPC dispatch handler and procmgr-internal callers (autostart,
SESSION_LOGIN handler) route through one function: `procmgr::spawn`.
The IPC dispatch handler is a thin adapter: deserialize, call, serialize
the reply.

## 4. Types

The new shared crate (`userspace/cluu_proto/`, or an existing libcluu
shared module) defines:

```rust
/// One IPC call's payload. Postcard-serialized into payload bytes.
pub struct SpawnEnvelope {
    pub image: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub view: ViewSource,
    pub fd_inherit: Vec<FdInherit>,
    pub session: Option<TokenHandle>,
    pub notify: Option<TokenHandle>,
}

pub enum ViewSource {
    Derive(TokenHandle),       // caller's parent-view token; narrow-only derive
    BootstrapRoot,             // valid only when caller_pid == INIT_PID
}

pub struct FdInherit {
    pub child_fd: u32,
    pub source: FdSource,
    pub rights: FdRights,
}

pub enum FdSource {
    VfsFd { vfs_client_id: u64, vfs_remote_fd: u32 },
    // extensible later (PipeCap, EndpointCap); VFS-only at landing.
}

pub struct FdRights { pub read: bool, pub write: bool }

/// Lives in cluu_proto for manifest deserialization + ProcessEntry storage.
/// NOT in the envelope — manifest is the source of truth.
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure { max: u32, window_ms: u64 },
}

pub type TokenHandle = u64;

pub struct SpawnReply {
    pub pid: u32,
    pub child_thread_token: TokenHandle,
}

pub enum SpawnError {
    ImageNotFound,
    ManifestInvalid(String),
    ViewDeriveDenied,
    FdInheritDeniedAt(u32),
    SessionRevoked,
    NotifyTokenInvalid,
    PermissionDenied,
    OutOfMemory,
    Internal(u32),
}

pub struct PrimordialSeed {
    pub primordials: Vec<SpawnEnvelope>,
}

pub struct PrimordialSeedReply {
    pub results: Vec<Result<SpawnReply, SpawnError>>,
}
```

Token handle width matches the existing libcluu/procmgr handle ABI.
The serialized envelope is bounded at 64 KiB; procmgr rejects oversize
at deserialize.

## 5. Wire format

### Labels

- `PROCMGR_SPAWN_UNIFIED_LABEL = 80` (request) — replaces `PROCMGR_SPAWN_LABEL`
  and `PROCMGR_CONTAINER_RUN_LABEL`.
- `PROCMGR_PRIMORDIAL_SEED_LABEL = 81` (request) — one-shot, accepted only
  when `caller_pid == INIT_PID`, rejected after first success.

### Request format

```
words[0] = payload_len                  // set by send_msg_with_payload
words[1] = ABI_VERSION (= 1)            // procmgr rejects on mismatch
words[2..6] = 0 (reserved)
payload  = postcard::to_slice(&SpawnEnvelope)        // or PrimordialSeed
```

Token handles inside the envelope reference the caller's token table.
Procmgr's `resolve_*` helpers (already in use per `a597e09`,
`860f996`) translate each handle into a procmgr-side derived cap.

### Reply format

```
words[0] = payload_len
words[1] = ABI_VERSION (= 1)
words[2..6] = 0 (reserved)
payload  = postcard::to_slice(&Result<SpawnReply, SpawnError>)
```

For `PROCMGR_PRIMORDIAL_SEED`, the payload is
`postcard::to_slice(&PrimordialSeedReply)`.

### No grant rings

Spawn is a millisecond-scale rare operation. The inline payload tier
suffices; no use of `BulkPool` or grant rings.

## 6. Process identity

Single rule:

```
ProcessEntry.comm = basename(manifest_for(envelope.image).entrypoint)
```

The spawning caller's identity is irrelevant. The spawned image's
manifest dictates the child's comm. Procmgr also overrides `argv[0]`
to `comm` regardless of what the caller put there.

Surfaces:

- `/proc/<pid>/comm` (when /proc lands fully Unix-style — out of spec 1
  scope; comm field is already prepared).
- `ps` output via /proc.
- procmgr debug log prefix `[<pid> <comm>]`.

`ProcessEntry.parent_image` records the spawning caller's image
separately, for diagnostic-only `ps -l` style listings later.

This rule fixes the "two cluuterms" bug: when cluuterm
(image=`cluuterm`) spawns shell (envelope.image=`shell`), the child's
comm is `shell`, not `cluuterm`.

## 7. FdInherit semantics

FdInherit is the sole fd-wiring mechanism on the wire.

### Caller side

For each fd the caller wants the child to inherit:

1. Caller has its own VFS fd `parent_fd` (already open).
2. Caller resolves `parent_fd → (vfs_client_id, vfs_remote_fd)` via
   `libcluu::fd_table::vfs_addr(parent_fd)`.
3. Caller appends `FdInherit { child_fd, source: VfsFd { vfs_client_id,
   vfs_remote_fd }, rights }` to envelope.fd_inherit.
4. `rights` must be a subset of caller's rights on `parent_fd`. Procmgr
   enforces monotone-decrease.

Unlisted child fd slots remain unmapped at child entry. Closes are
implicit.

POSIX `addopen`/`addclose` semantics are handled in the libcluu
`posix_spawn` shim, parent-side: shim opens the path in caller's
address space, adds a FdInherit entry for the resulting fd, and
optionally closes the parent-side fd after spawn. The wire format
never carries open-in-child or close-in-child actions.

### Procmgr side

```rust
fn install_fd_inherit(
    child_tid: ThreadId,
    child_pi_page: &mut ProcessInfoPage,
    entries: &[FdInherit],
) -> Result<(), SpawnError> {
    for e in entries {
        match &e.source {
            FdSource::VfsFd { vfs_client_id, vfs_remote_fd } => {
                vfs_derive_child_fd(
                    *vfs_client_id, *vfs_remote_fd,
                    child_tid, e.child_fd, e.rights,
                ).map_err(|_| SpawnError::FdInheritDeniedAt(e.child_fd))?;
                child_pi_page.set_inherited_fd(e.child_fd,
                    *vfs_client_id, *vfs_remote_fd);
            }
        }
    }
    Ok(())
}
```

### Child-side pickup

`crt0.S` calls `libcluu::init_stdio()`, which reads the PI page's
inherited-fd table and registers each `(child_fd, vfs_client_id,
vfs_remote_fd)` into the child's own fd_table. The loud-fail
assertion (`feedback_path_a_stdio_assertion`) stays: if PI slot says
"VFS-backed fd 0 should be present" and it isn't, child FATALs.

### Cluuterm retirement of dup2

Cluuterm currently uses newlib `posix_spawn_file_actions_adddup2`
to wire `/dev/pts/<id>` to the shell's fd 0/1/2. After spec 1:

- Cluuterm opens `/dev/pts/<id>` to get its own parent-side `pts_fd`.
- Cluuterm builds `SpawnEnvelope.fd_inherit` with three entries
  referencing `pts_fd`'s vfs addr at `child_fd = 0/1/2`.
- Cluuterm calls `libcluu::spawn(envelope)` directly, bypassing
  newlib's `posix_spawn` surface.

Newlib's `posix_spawn` shim remains for non-cluuterm callers (e.g.,
MicroPython subprocess); it translates `adddup2`/`addopen`/`addclose`
into FdInherit entries parent-side and calls the unified verb.

## 8. View derive semantics

Procmgr owns a typed `ViewObject` table. Each entry carries: a
parent-pointer, a list of MOUNT entries, a refcount.

### Derive operation (procmgr-internal, called by spawn)

```rust
fn derive_child_view(
    parent_token: TokenHandle,
    caller_pid: u32,
    manifest: &Manifest,
) -> Result<ViewObjectId, SpawnError> {
    let parent_view = resolve_view_token(parent_token, caller_pid)
        .ok_or(SpawnError::ViewDeriveDenied)?;
    let mut child_mounts: Vec<MountEntry> = Vec::new();
    for parent_mount in &parent_view.mounts {
        if let Some(child_mount) = narrow_for_manifest(parent_mount, manifest) {
            child_mounts.push(child_mount);
        }
    }
    for cm in &child_mounts {
        let pm = parent_view.mounts.iter()
            .find(|p| cm.path.starts_with(&p.path))
            .ok_or(SpawnError::ViewDeriveDenied)?;
        if !cm.rights.subset_of(&pm.rights) {
            return Err(SpawnError::ViewDeriveDenied);
        }
    }
    Ok(view_table.insert(ViewObject {
        parent: Some(parent_view.id),
        mounts: child_mounts,
        refcount: 1,
    }))
}
```

The monotone-decrease invariant (child mounts are each narrower than
their parent counterpart in both path coverage and rights) sits in
this one function. There is no caller-side bypass.

`narrow_for_manifest` is the existing mount-policy filter (per
`project_mount_policy`): given a parent mount and a manifest's MOUNT
directives, returns `Some(child_mount)` if the manifest requests this
path (possibly narrowed) or `None` if it doesn't.

### BootstrapRoot

When `envelope.view == ViewSource::BootstrapRoot`:

```rust
if caller_pid != INIT_PID { return Err(SpawnError::ViewDeriveDenied); }
if !inside_primordial_seed_handler { return Err(SpawnError::ViewDeriveDenied); }
mint_root_view_for_primordial(&manifest)
```

Each primordial gets its own narrowly-scoped initial view built from
its own manifest. After `PROCMGR_PRIMORDIAL_SEED` returns,
`ViewSource::BootstrapRoot` becomes permanently rejected.

### Refcount lifecycle

- Spawn success → child ViewObject created with refcount=1.
- Child exit → procmgr drops refcount; `view_table.gc()` removes entries
  at 0.
- Parent dies before child → procmgr's cascading-kill takes child first;
  child's view drops before parent's.

### Cap revocation

The child's view token, minted into the child's table at spawn, is
revoked by procmgr when the ViewObject is destroyed. Any IPC the child
has on that token returns the kernel's revoked-token error.

### Cluufile MOUNT semantics

Existing directives are carried through `narrow_for_manifest`:

- `MOUNT /` — inherit from parent.
- `MOUNT /tmp private` — independent memfs for child.
- `MOUNT /sys/audio block` — explicitly drop even if parent had it.

No new MOUNT semantics in spec 1.

## 9. Session field semantics

`envelope.session: Option<TokenHandle>`.

### Resolution at spawn

```rust
match envelope.session {
    None => {
        if !manifest.allow_sessionless && !is_system_caller(caller_pid) {
            return Err(SpawnError::PermissionDenied);
        }
        child.session_id = None;
    }
    Some(t) => {
        let session = resolve_session_token(t, caller_pid)
            .ok_or(SpawnError::SessionRevoked)?;
        if session.is_dying { return Err(SpawnError::SessionRevoked); }
        session.refcount += 1;
        child.session_id = Some(session.id);
    }
}
```

### Sessionless-permitted callers

- init (`caller_pid == INIT_PID`).
- procmgr itself for in-process internal calls.
- Any image whose manifest declares `RIGHT_SESSIONLESS_SPAWN` (default:
  not granted).

User-facing surfaces (cluuterm, login, shell) always carry a session
token in their envelopes.

### Membership rule

Children may join sessions for which the caller holds the token. The
token's existence in the caller's table is the authorization; no
ambient session-id checks.

### On exit

```rust
if let Some(sid) = child.session_id {
    session_table[sid].refcount -= 1;
}
```

Refcount-drop without session destroy. Spec 3 decides when sessions
are torn down and how cascade-kill propagates.

## 10. Notify field semantics

`envelope.notify: Option<TokenHandle>`.

### At spawn

```rust
let notify_derived = match envelope.notify {
    None => None,
    Some(t) => Some(
        token_derive(t, caller_pid, IPC_SEND)
            .ok_or(SpawnError::NotifyTokenInvalid)?
    ),
};
child.exit_notify = notify_derived;
```

Pattern matches commit `a597e09`: caller's raw handle is resolved
inside procmgr, an `IPC_SEND` cap is derived into procmgr's own table,
and that derived cap is what procmgr later uses to deliver
`PROC_EXIT_LABEL`.

### On child exit

```rust
if let Some(token) = child.exit_notify {
    let msg = [PROC_EXIT_COOKIE, exit_code, 0, 0, 0, 0];
    ipc_send(token, PROC_EXIT_LABEL, &msg, &[]);
}
```

### When caller dies

The notify-derived cap, held by procmgr, is revoked when the parent
process tears down. Attempts to send on it silently drop (existing
transport semantics).

## 11. Restart policy (Cluufile-driven)

Restart policy is declared in the spawned image's Cluufile. It is not
on the envelope.

### Cluufile syntax

```
ENTRYPOINT /bin/compositor
RESTART always
# alternatives:
# RESTART never
# RESTART on_failure max=5 window=60000
```

Default if `RESTART` absent: `never`.

### Procmgr at spawn

```rust
child.restart_policy   = manifest.restart_policy;
child.restart_envelope = envelope.clone();    // stored for replay
```

### On exit

```rust
match child.restart_policy {
    RestartPolicy::Never  => { /* no respawn; notify if set */ }
    RestartPolicy::Always => respawn(child.restart_envelope.clone()),
    RestartPolicy::OnFailure { max, window_ms } => {
        if exit_code != 0 {
            if recent_failures_in_window(child.image, window_ms) >= max {
                log_crash_loop(&child.image);
            } else {
                respawn(child.restart_envelope.clone());
            }
        }
    }
}
```

### Respawn semantics

- The original envelope is replayed verbatim. View/session/notify
  references stored procmgr-side via derived caps survive parent
  death (procmgr never lost the derived cap).
- If a FdInherit source has been revoked between spawn and respawn
  (e.g., the producer service died), the respawn fails with
  `FdInheritDeniedAt`, counts against `OnFailure.max`, and ultimately
  hits the crash-loop log.

### Init primordial monitoring

All primordials carry `restart_policy = Never` via their own Cluufiles.
Init's exit-monitor loop receives `PROC_EXIT_LABEL` and panics on
primordial death. Unchanged from memory entry 15.

### One-shot of a normally-respawning binary

Not supported by envelope override. Workaround: build a separate image
(`<name>-test` with `RESTART never`) that wraps the real binary, or
accept that this case is rare enough not to design for.

## 12. Error semantics and cap-revocation

`procmgr::spawn` is synchronous and bounded. The 10-step body:

1. Deserialize envelope. Fail → `SpawnError::Internal(EBADENV)`.
2. Load manifest from the procmgr-side manifest cache (per-image,
   keyed by image name, populated on first miss via a VFS read of
   `/var/images/<image>/manifest.toml`, invalidated on image reinstall
   via a procmgr-internal hook). Miss + VFS-read failure →
   `ImageNotFound`.
3. Resolve and derive view, session, notify tokens into procmgr's
   table. Any fail → roll back, `Err`.
4. Allocate Space + initial Thread. Fail → roll back,
   `OutOfMemory`.
5. Install fd_inherit. Any fail → roll back, `FdInheritDeniedAt`.
6. Load ELF into child's Space. Fail → roll back, `Err`.
7. Write ProcessInfo page.
8. Insert ProcessEntry.
9. Start the thread.
10. Return `SpawnReply { pid, child_thread_token }`.

No step waits on the child. No step uses `recv_with_timeout`.

### Rollback table

| Side effect on step N | Inverse on later failure |
|---|---|
| View derived | `view_table.dec_ref(child_view_id)` |
| Session refcount incremented | `session.refcount -= 1` |
| Notify cap derived | `token_revoke(notify_derived)` |
| Space allocated | `invoke_space_destroy(child_space)` |
| FD inherit entries 0..N installed | revoke each derived child fd token; clear PI page slots |
| ELF loaded | covered by `invoke_space_destroy` |
| ProcessEntry inserted | only inserted at step 8, after all fallible steps |
| Thread started | only at step 9; if it fails, clean up partial ProcessEntry |

### Procmgr crash mid-call

Kernel revokes procmgr's endpoint → caller's `ipc_call` returns
`EBADTOKEN` → libcluu translates to
`SpawnError::Internal(EPROCMGR_DEAD)`. Only "non-deterministic"
outcome, surfaced as a concrete error.

### Cap-revocation across the child's life

Procmgr keeps a list of caps it minted into the child's address space
in `ProcessEntry.minted_tokens`: view-token, session-token, inherited-fd
tokens, and any caller-requested notify-back caps. Child exit triggers
walk-and-revoke. Other processes blocked on these tokens wake with a
concrete kernel-level error.

## 13. Primordial bootstrap

Init's responsibilities reduce to three items:

1. Kernel-spawn procmgr (sole remaining kernel-spawn path; renamed
   `launch_procmgr`).
2. Send `PROCMGR_PRIMORDIAL_SEED` with the primordial envelope list.
3. Run primordial-exit monitor (unchanged behavior).

### `PROCMGR_PRIMORDIAL_SEED` handler

```rust
fn handle_primordial_seed(caller_pid: u32, seed: PrimordialSeed)
    -> PrimordialSeedReply
{
    if caller_pid != INIT_PID
        || primordial_seed_already_consumed
    {
        return reject_all(SpawnError::PermissionDenied, seed.primordials.len());
    }
    primordial_seed_already_consumed = true;

    let mut results = Vec::with_capacity(seed.primordials.len());
    for envelope in seed.primordials {
        results.push(procmgr::spawn(envelope, caller_pid));
    }
    PrimordialSeedReply { results }
}
```

### Properties

- Sequential: each primordial fully spawned before the next. Allows
  later primordials to FdInherit from earlier ones if desired.
- Each envelope carries `view = ViewSource::BootstrapRoot`,
  `session = None`, `notify = Some(init_exit_endpoint_send_cap)`.
- Each primordial's manifest declares `RESTART never`. Death = init
  panic.
- After the call, `BootstrapRoot` and the SEED label are both
  permanently rejected.

### Init primordial ordering (data only — may change without re-spec)

procmgr (via `launch_procmgr` kernel-spawn) → then via SEED:
registry, timeserver, vfs, virtio-blk, tpmd.

## 14. Migration plan

Each step lands fully and harness-green before the next. No
intermediate state ships in a broken form.

1. **Land `cluu_proto` crate.** Defines all types and label
   constants. No call-site changes. `cargo xtask build` clean.

2. **Procmgr-internal `procmgr::spawn(envelope, caller_pid)` function.**
   Refactor `handle_spawn_message` and `handle_container_run` to build
   a `SpawnEnvelope` and delegate. Introduce internal `ViewObject`
   table + `derive_child_view`. Both adapters keep working. Harness
   golden path green.

3. **Add `PROCMGR_SPAWN_UNIFIED_LABEL = 80` handler.** Thin adapter
   around `procmgr::spawn`. No callers yet. Smoke test via a synthetic
   binary that exercises round-trip.

4. **libcluu native `spawn(envelope)`.** Public API. Existing
   `posix_spawn` shim still uses old labels.

5. **Cluuterm flips to `libcluu::spawn`.** Drops newlib `posix_spawn`
   + `adddup2`. Builds FdInherit entries against `/dev/pts/<id>`.
   "Two cluuterms" bug resolves here.

6. **Shell pipeline + external-command spawns flip.** Shell builds
   `SpawnEnvelope` directly via libcluu. No more `PROCMGR_SPAWN_LABEL`
   or `PROCMGR_CONTAINER_RUN_LABEL` calls from shell.

7. **Newlib `posix_spawn` shim builds `SpawnEnvelope`.** Translates
   `posix_spawn_file_actions_t` into `Vec<FdInherit>` parent-side.
   Old transport unused.

8. **Procmgr autostart flips.** `autostart_container()` builds
   `SpawnEnvelope` and calls `procmgr::spawn` in-process. Drops any
   `restart_policy` column from autostart.toml — manifest is now the
   source of truth.

9. **SESSION_LOGIN internal spawns flip.** `handle_session_login`
   builds `SpawnEnvelope` for compositor and cluuterm and calls
   `procmgr::spawn`. The 2 s `COMPOSITOR_READY` wait stays untouched in
   spec 1 (spec 3 territory).

10. **Init flips.** Kernel-spawn only procmgr. Build
    `PrimordialSeed { primordials: vec![ ... ] }`. Send
    `PROCMGR_PRIMORDIAL_SEED`. Reduce `launch_service` to
    `launch_procmgr`.

11. **Delete dead code.**
    - `PROCMGR_SPAWN_LABEL` const + `handle_spawn_message` +
      `build_spawn_payload`.
    - `PROCMGR_CONTAINER_RUN_LABEL` const + `handle_container_run` +
      `build_container_run_payload*`.
    - Kernel-side batch-spawn loader in init.
    - Old offset-encoded payload builders.

12. **Verify.** Grep proofs from §15 return their expected zero/one
    matches. Harness markers from §15 pass. Each new marker
    (`l2_spawn_view_widen_denied`, `l2_spawn_fd_inherit_widen_denied`,
    `l2_spawn_session_revoked`, `l2_primordial_seed_caller_check`,
    `l2_spawn_identity_basename`, `l2_restart_manifest_always`,
    `l2_restart_manifest_never`, `l2_restart_envelope_no_override`,
    `l2_cluuterm_no_dup2`) is added as part of the step that the
    marker validates (e.g., the identity marker lands with step 5).

Per-step gate: `bash scripts/harness_run.sh` reaches `compositor: ready`
and `shell: ready`.

## 15. Acceptance criteria

### Build

- `cargo xtask build` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Grep zero-hit proofs

- `git grep PROCMGR_SPAWN_LABEL`
- `git grep PROCMGR_CONTAINER_RUN_LABEL`
- `git grep handle_spawn_message`
- `git grep handle_container_run`
- `git grep build_container_run_payload`
- `git grep build_spawn_payload`
- `git grep posix_spawn_file_actions_adddup2` inside
  `userspace/cluuterm/`

### Grep one-match proofs

- `git grep "PROCMGR_SPAWN_UNIFIED_LABEL = 80"` → one match in
  `cluu_proto`.
- `git grep "PROCMGR_PRIMORDIAL_SEED_LABEL = 81"` → one match.
- `git grep "fn spawn(.*SpawnEnvelope" userspace/procmgr/` → one match.

### Functional

- Boot smoke: `bash scripts/harness_run.sh` reaches `compositor:
  ready` and `shell: ready`.
- Interactive root/root login reaches a shell prompt; shell echoes
  typed input.
- ps (or procmgr debug log) shows cluuterm-spawned shell as `shell`,
  not `cluuterm`.
- Pipeline `echo hi | cat | grep h` runs through `procmgr::spawn`.
- External command run via `Ctrl+Alt+N` succeeds.

### Cap-discipline markers

- `l2_spawn_view_widen_denied` — manifest tries to widen MOUNT;
  expect `ViewDeriveDenied`; caller does not crash.
- `l2_spawn_fd_inherit_widen_denied` — FdInherit rights wider than
  caller's; expect `FdInheritDeniedAt(fd)`.
- `l2_spawn_session_revoked` — race between session kill and spawn;
  expect success-or-`SessionRevoked`, never hang.
- `l2_primordial_seed_caller_check` — non-init sends SEED; expect
  `PermissionDenied`.

### Identity marker

- `l2_spawn_identity_basename` — cluuterm spawns shell; procmgr
  `ProcessEntry.comm` for shell's pid is `"shell"`. Cluuterm's own pid
  shows `"cluuterm"`.

### Restart-policy markers (manifest-driven)

- `l2_restart_manifest_always` — test image with `RESTART always` is
  spawned, killed externally; procmgr respawns it.
- `l2_restart_manifest_never` — test image with `RESTART never` exits;
  procmgr does not respawn.
- `l2_restart_envelope_no_override` — caller cannot influence restart
  policy; envelope has no such field.

### Cluuterm-dup2 retirement marker

- `l2_cluuterm_no_dup2` — shell spawned under cluuterm has fd 0/1/2
  populated via FdInherit only. Cluuterm's compiled binary contains no
  reference to newlib `posix_spawn`.

### No new timeouts proof

`grep -rn "recv_with_timeout\|call_with_timeout\|wait_for_grant_with_timeout"
userspace/procmgr/src/` returns the same set as before spec 1 — spec 1
introduces zero new time-bounded waits.

### Performance gate

`l2_jobchurn_heavy` and `b_spawn_warm` markers run within 20% of
pre-spec baseline. Manifest cache makes per-spawn manifest read O(1)
after first.

### Documentation

This file landed at
`docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`;
referenced from `docs/ROADMAP.md` and `docs/CURRENT_PHASE.md`.

## 16. Open follow-ups (out of spec 1, recorded for downstream specs)

- 2 s `COMPOSITOR_READY` timeout removal (spec 3 — session lifecycle).
- 5 s VFS `call_with_timeout` removal (independent; tracked in
  `feedback_no_timeouts`).
- `FdSource::PipeCap` / `EndpointCap` variants (extends FdInherit to
  non-VFS-backed sources; needed once raw cap inheritance becomes
  required, e.g., for in-procmgr pipe endpoints).
- `/proc/<pid>/comm` real file (Unix-style /proc; currently
  shim-via-IPC per `project_proc_unix_compliance`).
- Cluufile inheritance / base manifests (when multi-tool bundles like
  coreutils want shared declarations — deferred until 1:1 image:binary
  rule hurts in practice).

## 17. Related memory

- `[[no-timeouts]]` — the law spec 1 honors.
- `[[unified-process-model-decision-2026-05-18]]` — procmgr as sole
  process-lifecycle owner.
- `[[frame-typing-redesign-landed-2026-05-18]]` — typed frames are
  the kernel-side mechanism this spec relies on for child Space
  teardown.
- `[[spawn-cap-composable]]` — every binary's authority is its
  Cluufile profile, not its role.
- `[[procmgr-stateless]]` — caller-authoritative for per-spawn
  attributes; procmgr stores only what it must to make routing /
  lifecycle decisions.
- `[[vfs-view-caps-monotone]]` — view-derive is monotone-narrowing,
  enforced in one function.

## 18. Related committed work

- `1a8c218` docs(spawn-window-pty): inventory of current pipeline.
- `860f996` procmgr: derive parent_stdin_send from original stdin
  endpoint.
- `a597e09` procmgr: derive notify_endpoint into own token table.
- `9ac4b12` shell: skip TTY-service IPC in cluuterm/pts mode.
- `9b982c4` rename FDAC → FdInherit.
- `72d7185` shell: FdInherit stdio passthrough for bare external
  commands.
- `da8da75` libcluu/registry: drop 2 s subscribe timeout.

These commits already shape the surface this spec stabilizes.
