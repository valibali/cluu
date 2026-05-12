# FDAC fd handoff for VFS-backed fds (Path A — final design)

**Date:** 2026-05-12
**Status:** Ready to implement.
**Context:** cluuterm Task 22 follow-up; blocks cluuterm + /bin/login end-to-end.
**Related:** `project_cluuterm_session_2026_05_12.md`, `feedback_vfs_view_caps_monotone.md`.

---

## 1. Problem

cluuterm calls `_open("/dev/pts/0")` (libcluu/posix/file.rs) → libcluu stores
`(vfs_endpoint, vfs_file.fd, client_id)` in `FdEntry`. cluuterm calls
`posix_spawn_file_actions_adddup2(fa, pts_fd, 0)`, then
`posix_spawn(/bin/login, fa, ...)`. procmgr's FDAC handler at
`userspace/procmgr/src/main.rs:4218` calls
`token_derive(parent_endpoint, child_rights, MAX)`.

Kernel `invoke_token_derive` rejects with `missing GRANT right` because
cluuterm's `vfs_endpoint` was minted by the registry with only
`IPC_SEND | IPC_CALL` (`userspace/libcluu/src/registry.rs:347`). Per the
caps-monotone-decrease invariant, cluuterm cannot — and must not — hold a
GRANT-capable token on `vfs:main`.

---

## 2. Audit Findings

Three kernel facts were confirmed before finalising this design.

**Finding 1 — `invoke_endpoint_create` grants full rights including GRANT**
(`kernel/src/syscall/handlers.rs:1224`, `kernel/src/token/rights.rs:195`):
when a thread calls `endpoint_create`, the kernel mints its first token with
`Rights::ipc_full() | Rights::GRANT`. So whoever creates an endpoint
legitimately holds GRANT on it.

**Finding 2 — No MSG_GRANT / no token-in-message transfer**
(`kernel/src/syscall/handlers.rs:2648`):
tokens never auto-transfer over IPC. The only way to hand a token to another
thread is `invoke_token_derive`, which produces a derived token in the
*caller's* table. The receiver never acquires the sender's rights from the
wire. Consequence: VFS receives only the raw handle integer during
`PTS_REGISTER_LABEL` — it has no GRANT-source for cluuterm's
`notify_endpoint`.

**Finding 3 — `invoke_token_derive` preserves scope verbatim**
(`kernel/src/syscall/handlers.rs:2648–2668`, `kernel/src/token/mod.rs:344`):
the derived token's `scope` (the `ObjectRef` that identifies the backing
Endpoint or Object) is copied unchanged from the source token. Only rights are
narrowed. Derived tokens therefore route IPC to the same backing object as
their source.

---

## 3. Failed Approaches

### 3a. Subagent attempt 1 — VFS token laundering

Added `VFS_FD_CLONE_LABEL` so cluuterm asked VFS to mint a
`IPC_SEND|IPC_RECV|IPC_CALL|GRANT` token from VFS's own full-rights endpoint,
then stored that token directly in `FdEntry.endpoint`. FDAC succeeded but the
design is wrong: VFS laundered rights cluuterm does not legitimately hold. The
derived `FdEntry.endpoint` gave cluuterm an effective GRANT-source with no
caps-lineage back to cluuterm's original rights. `map_elf_from_vfs` returned
`NotFound` after the first call, and a separate latent UAF surfaced in
procmgr's `load_elf` (heap reuse between `map_elf` and `load_elf` `format!`
calls). Reverted; commit `5ab351a` retains only the non-controversial procmgr
changes.

### 3b. Original Option B "Pts case" — derive from `notify_endpoint`

The Option B design proposed that the `VFS_DERIVE_CHILD_FD_LABEL` handler
branch on `OpenFile::Pts` and call `token_derive(notify_endpoint, ...)`.
Finding 2 above shows this is infeasible: VFS never held a GRANT-capable token
on `notify_endpoint` — cluuterm only sent the raw handle integer over IPC at
`PTS_REGISTER_LABEL`, and IPC does not transfer token rights. VFS cannot derive
from an object whose GRANT it never received.

---

## 4. Path A — Final Design

**Core insight:** VFS already proxies every pts read and write to cluuterm via
`PTS_READ_LABEL` / `PTS_WRITE_LABEL` (`vfs/main.rs:1551` for write,
`vfs/main.rs:2519` for read). The child's pts fd does not need to route
directly to cluuterm — the child sends `VFS_READ` / `VFS_WRITE` to VFS, and
VFS forwards to cluuterm. This is true for all `OpenFile` variants (Pts, Ext2,
MemFs), so no backend-specific branching is needed.

**Invariant:** VFS mints the child token from its own full-rights endpoint
(`self.endpoint`, assigned at `vfs/main.rs:165` from `info.tokens[TOKEN_EXTRA_0]`
— VFS called `endpoint_create` at boot and holds GRANT legitimately). cluuterm
never touches a GRANT-capable token; procmgr only acts as intermediary.

### 4.1 FdAction wire — 16 → 32 bytes

Current (`userspace/libcluu/src/posix/process.rs:533`):

```rust
#[repr(C)]
#[derive(Clone, Copy)]
struct FdAction {
    target_fd: u32,   // bytes 0–3
    flags: u32,       // bytes 4–7
    endpoint: usize,  // bytes 8–15
}
```

After:

```rust
#[repr(C)]
#[derive(Clone, Copy)]
struct FdAction {
    target_fd:     u32,    // bytes 0–3
    flags:         u32,    // bytes 4–7
    endpoint:      usize,  // bytes 8–15  (legacy path: pipes, tty)
    vfs_client_id: usize,  // bytes 16–23 (0 = not VFS-backed)
    vfs_remote_fd: usize,  // bytes 24–31 (VFS-side fd number)
}
```

`MAX_FD_ACTIONS = 4` (`process.rs:522`). Four actions × 32 bytes = 128 bytes
total payload. Verify that the procmgr incoming IPC buffer and libcluu's
outgoing buffer both accommodate this. The serialiser loop at `process.rs:655`
uses `size_of::<FdAction>()` — no manual byte count to fix, but the read side
in procmgr must be widened to match.

### 4.2 libcluu `posix_spawn_file_actions_adddup2` (process.rs:583)

After the existing `FD_TABLE` lookup, populate the new fields:

```rust
let vfs_client_id = entry.client_id;          // 0 for non-VFS fds
let vfs_remote_fd = entry.remote_fd.unwrap_or(0);
// ... existing flags/endpoint code ...
inner.actions[count] = FdAction {
    target_fd,
    flags,
    endpoint,          // kept for legacy path
    vfs_client_id,
    vfs_remote_fd,
};
```

For non-VFS fds (pipes, tty endpoints), `entry.remote_fd` is `None` → 0.
Existing FDAC behaviour for those fd types is unchanged.

### 4.3 procmgr FDAC branch (main.rs:4218)

After parsing the `FdAction`, branch on `vfs_remote_fd`:

```rust
let derived_handle = if vfs_remote_fd != 0 {
    // VFS-backed fd: ask VFS to mint a child token from its own endpoint.
    vfs_derive_child_fd(
        self.vfs_endpoint,   // procmgr's IPC_SEND|IPC_CALL token to vfs:main
        vfs_client_id,
        vfs_remote_fd,
        probe_rights,
        child_tid,
    )?
} else {
    // Legacy path (pipes, tty endpoints): direct token_derive.
    token_derive(endpoint, probe_rights, u64::MAX)?
};
```

`vfs_derive_child_fd` sends `VFS_DERIVE_CHILD_FD_LABEL` to VFS and reads back
the derived handle.

### 4.4 New VFS handler — `VFS_DERIVE_CHILD_FD_LABEL`

**Request words:**

| word | field           | notes                                           |
|------|-----------------|--------------------------------------------------|
| [0]  | parent_client_id | caller's (parent process) client_id in VFS       |
| [1]  | parent_remote_fd | VFS-side fd number for the parent's open file    |
| [2]  | child_rights     | rights bits to narrow to (e.g. READ\|WRITE)      |
| [3]  | child_tid        | new thread id — used as new client_id for the child |

**Reply words:**

| word | field           |
|------|-----------------|
| [0]  | status (0 or −errno) |
| [1]  | derived token handle  |
| [2]  | child_client_id (= child_tid passed in) |
| [3]  | child_remote_fd (freshly allocated fd slot under child_client_id) |

**Handler logic:**

1. Look up `(parent_client_id, parent_remote_fd)` in `self.files`. If missing,
   reply `Error::NotFound`.
2. Clone the `OpenFile` entry under `(child_tid, new_remote_fd)`. For
   `OpenFile::Pts`, increment the pts refcount via
   `pts_registry.inc_ref(pts_id)` so the pts slot remains live until the child
   closes it. No backend-specific branching is required at this layer — the
   same `OpenFile` clone works for Pts, Ext2, and MemFs because the child will
   reach cluuterm (for pts) through the normal VFS proxy path.
3. Call `token_derive(self.endpoint, child_rights, u64::MAX)` — `self.endpoint`
   is VFS's own full-rights endpoint (created at boot, GRANT held legitimately).
   This produces a narrowed VFS token scoped to `vfs:main`.
4. Reply with `[0, derived_handle, child_tid, new_remote_fd]`.

View / profile note: `views` are only consulted at path-resolution time (`open`
calls). Once an `OpenFile` exists, `read` and `write` bypass view gating. No
`set_view` call is needed when cloning an fd to the child.

### 4.5 Child fd_table rehydration

procmgr's `map_process_info_page` (procmgr/main.rs:4289) already publishes
inherited stdio endpoints. Extend the per-fd metadata in the process-info page
schema to carry:

```
(token_handle, vfs_client_id, vfs_remote_fd, flags)
```

Child libcluu bootstrap (`libcluu/src/fd_table.rs:215`) reads each slot. If
`vfs_remote_fd != 0`, construct:

```rust
FdEntry::file(token, remote_fd, client_id, readable, writable)
```

so that the child's subsequent reads and writes use the VFS protocol path
(`VFS_READ_LABEL` / `VFS_WRITE_LABEL`) rather than the tty path. If
`vfs_remote_fd == 0`, fall through to the existing `FdEntry::tty(token, ...)`
construction (pipes, tty fds).

---

## 5. Verification

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_smoke bash scripts/harness_run.sh
grep -E "spawn /bin/login|login:|cluuterm: shutdown" /tmp/cluu-serial-com2.log
```

Expect `cluuterm: /bin/login spawned` (existing debug_print) plus a
`login: ` prompt string in the serial trace (cluuterm pumps child output via
`TTY_WRITE_LABEL` handler, Task 15).

**Marker matrix:**

| marker               | state after this work |
|----------------------|-----------------------|
| `l2_cluuterm_smoke`  | must stay green       |
| `l2_cluuterm_login`  | goes green once login can spawn and run |
| `l2_cluuterm_ansi`   | goes green once shell runs in cluuterm  |
| `l2_cluuterm_keymap` | goes green once shell runs in cluuterm  |
| `l2_cluuterm_exit`   | goes green once shell runs in cluuterm  |

---

## 6. Risks

**a. VFS view bypass is intentional, not a hole.**
Views gate path resolution at `open` time only. Cloning an already-open
`OpenFile` to the child bypasses view gating, which is correct — the parent
already passed the view check when it opened the file. Confirm this reading
against `vfs/mount.rs` if any doubt arises during implementation.

**b. Pts refcount must be incremented on clone, decremented on child close.**
Forgetting `pts_registry.inc_ref` causes the pts slot to be freed while the
child still has it open. The child's first `VFS_READ` will get `NotFound` from
VFS. Decrement must occur in VFS's `VFS_CLOSE_LABEL` handler for the child's
`(client_id, remote_fd)` slot, same as the existing close path.

**c. `MAX_FD_ACTIONS` payload size doubles — verify buffer bounds.**
Four actions × 32 bytes = 128 bytes. Check:
- procmgr's incoming IPC word buffer for the `PROCMGR_CONTAINER_RUN_LABEL`
  message that carries the `FdAction` array.
- libcluu's outgoing buffer in `posix_spawn` (process.rs) before the write
  loop at line 655.
- If either is declared as a fixed-size array sized for 16-byte actions, widen
  it.

---

## 7. Order of Work

1. **Widen `FdAction` wire** — struct + serialiser in libcluu; parser in
   procmgr. Verify no hardcoded 16-byte offsets survive.
2. **Implement `VFS_DERIVE_CHILD_FD_LABEL` handler** in vfs/main.rs — lookup,
   clone, `token_derive(self.endpoint, ...)`, pts refcount, reply.
3. **Procmgr FDAC branch** — `vfs_remote_fd != 0` → `vfs_derive_child_fd(...)`;
   else legacy `token_derive`.
4. **Child fd_table rehydration** — extend process-info page schema; update
   libcluu bootstrap to build `FdEntry::file(...)` when `vfs_remote_fd != 0`.
5. **Verify markers** — `l2_cluuterm_smoke` stays green; `l2_cluuterm_login`
   goes green; manually confirm login prompt renders in cluuterm window on VT4.
