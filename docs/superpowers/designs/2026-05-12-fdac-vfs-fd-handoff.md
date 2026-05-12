# FDAC fd handoff for VFS-backed fds (Option B design)

**Date:** 2026-05-12.
**Status:** Design — ready to implement next session.
**Context:** cluuterm Task 22 follow-up; blocks cluuterm + /bin/login end-to-end.
**Related:** `project_cluuterm_session_2026_05_12.md`, `feedback_vfs_view_caps_monotone.md`.

## Problem

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

## Failed approach (subagent attempt 1)

Added `VFS_FD_CLONE_LABEL` so cluuterm asks VFS to mint a
`IPC_SEND|IPC_RECV|IPC_CALL|GRANT` token from VFS's own full-rights
endpoint. cluuterm stored that token in `FdEntry.endpoint`. FDAC succeeded.

**Why wrong:** VFS launders rights cluuterm doesn't have. The resulting
`FdEntry.endpoint` is a vfs:main token with GRANT held by a caller
(cluuterm) that has no GRANT-source claim. Subsequent IPC paths broke:
`map_elf_from_vfs` started returning `NotFound` after the first call.
Also surfaced a separate latent UAF on the `path: &str` param in
procmgr's `load_elf` (heap reuse between map_elf and load_elf format!
calls).

Reverted in the working tree; commit `5ab351a` retains only the
non-controversial procmgr changes (FDAC rights now include `GRANT`;
auth-only-login when `vt_index >= VT_COUNT`).

## Option B — procmgr-mediated fd handoff

**Invariant preserved:** the GRANT-capable token never lives in
cluuterm's hands. VFS mints it from its own full-rights endpoint, hands
it to procmgr (which is doing the spawn on cluuterm's behalf), procmgr
installs it as the child's fd. cluuterm only ever holds its original
`vfs_endpoint` with `IPC_SEND | IPC_CALL`.

### Wire change — `FdAction`

Today (`userspace/libcluu/src/posix/process.rs:533`):

```rust
#[repr(C)]
#[derive(Clone, Copy)]
struct FdAction {
    target_fd: u32,
    flags: u32,
    endpoint: usize,
}
```

After:

```rust
#[repr(C)]
#[derive(Clone, Copy)]
struct FdAction {
    target_fd: u32,
    flags: u32,
    endpoint: usize,        // parent's view of the token (legacy path: pipes, tty)
    vfs_client_id: usize,   // 0 = not VFS-backed; otherwise procmgr looks up via VFS
    vfs_remote_fd: usize,   // VFS-side fd number
}
```

Bump `MAX_FD_ACTIONS` storage if needed (struct size goes 16 → 32 bytes).
Update FDAC serialiser (line 645+) to write all five fields. Existing
parsers that read 16-byte entries break — search procmgr for the read
side and widen.

### libcluu `posix_spawn_file_actions_adddup2` (process.rs:583)

After the existing FD_TABLE lookup, populate the new fields:

```rust
let vfs_client_id = entry.client_id;
let vfs_remote_fd = entry.remote_fd.unwrap_or(0);
// ... existing flags/endpoint code ...
inner.actions[count] = FdAction {
    target_fd, flags, endpoint,
    vfs_client_id, vfs_remote_fd,
};
```

For non-VFS fds (pipes, tty), `entry.remote_fd` is `None` → 0. Existing
behaviour preserved.

### procmgr FDAC handler (main.rs:4218)

After parsing the FdAction, branch:

```rust
let derived = if vfs_remote_fd != 0 {
    // VFS-backed fd: bounce through VFS to mint a child token.
    vfs_derive_child_fd(
        self.vfs_endpoint,
        vfs_client_id,
        vfs_remote_fd,
        probe_rights,
    )?
} else {
    // Legacy path (pipes, tty endpoints): direct token_derive.
    token_derive(endpoint, probe_rights, u64::MAX)?
};
```

`vfs_derive_child_fd` sends the new label to VFS and reads the token.

### New VFS handler — `VFS_DERIVE_CHILD_FD_LABEL`

Wire format (request):
- `words[0] = client_id`
- `words[1] = remote_fd`
- `words[2] = child_rights` (bits)

Wire format (reply):
- `words[0] = 0` or `-errno`
- `words[1] = token_handle`

Handler:
1. Look up `(client_id, remote_fd)` in `self.files`. If missing, reply
   `Error::NotFound`.
2. Branch by `OpenFile` kind:
   - **Pts**: the right source is the pts owner's `notify_endpoint`,
     because the child's I/O routes directly to the pts owner (cluuterm),
     not back through VFS. **Required:** VFS must hold GRANT on
     `notify_endpoint`. Verify by reading what rights the endpoint
     carries when it arrives via `PTS_REGISTER_LABEL`'s IPC. If cluuterm
     created the endpoint via `endpoint_create(ipc_cap)`, the resulting
     token's rights depend on `ipc_cap`. If the token transferred to
     VFS via IPC carries the parent's rights (modulo IPC strip/keep
     rules — verify in kernel), then VFS may have GRANT. If not, VFS
     must derive a separate "FDAC-friendly" sub-endpoint at
     `PTS_REGISTER` time and stash it alongside `notify_endpoint`.
   - **Other VFS-backed files** (ext2 via blkdev, etc.): derive from
     `self.endpoint` (VFS's own full-rights endpoint). The child can
     then send `VFS_READ`/`VFS_WRITE` to that derived token, routing to
     VFS normally. The remote_fd survives via the child's libcluu
     fd_table (see "child fd_table" below).
3. Call `token_derive(source, child_rights, MAX)`. Reply with the
   derived handle.

### Child fd_table rehydration

Child boots into libcluu's bootstrap path. Currently
`libcluu/src/fd_table.rs:215` initialises inherited stdio with
`FdEntry::tty(token, ...)`. For VFS-backed inheritance we need
`FdEntry::file(token, remote_fd, client_id, readable, writable)` so
reads/writes use the VFS protocol path.

Mechanism: procmgr's FDAC reply path (the `map_process_info_page`
around line 4268) already publishes stdin/stdout/stderr endpoints to
the child. Extend the inheritance metadata (process_info page or boot
manifest) to also carry `(vfs_client_id, vfs_remote_fd)` per fd. Child
boot code reads them and constructs the right `FdEntry`.

For Pts fds the routing target is cluuterm directly (the derived
notify_endpoint), so child's `FdEntry::tty(token, ...)` works — child
sends `TTY_READ_REQUEST_LABEL` / `TTY_WRITE_LABEL` to the derived
token, cluuterm receives them. cluuterm's recv loop already handles
those labels (Task 15).

For ext2-backed files we won't hit this yet (no use case in v1), but
the design naturally extends.

### Verification

After implementation, the following should pass:

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_smoke bash scripts/harness_run.sh
grep -E "spawn /bin/login|login:|cluuterm: shutdown" /tmp/cluu-serial-com2.log
```

Expect `cluuterm: /bin/login spawned` (already a debug_print) plus
`login: ` prompt rendered in serial trace (since cluuterm pumps shell
output via TTY_WRITE_LABEL handler).

Marker matrix:
- `l2_cluuterm_smoke` already green; must stay green.
- `l2_cluuterm_login` — should go green once login can run.
- `l2_cluuterm_ansi` / `l2_cluuterm_keymap` / `l2_cluuterm_exit` — go
  green once shell runs in cluuterm.

### Risks

- **Token scope after derive.** Derived tokens inherit the source's
  scope. The child sends IPC to a token whose scope routes to the pts
  owner (cluuterm) or to VFS. The narrowed-rights derive preserves
  scope. Verify in `kernel/src/syscall/handlers.rs::invoke_token_derive`
  and `kernel/src/token/scope.rs` that scope survives.
- **GRANT on notify_endpoint** (Pts case). If VFS lacks GRANT,
  fallback: VFS asks the pts owner (cluuterm) via a new IPC to mint a
  derived token. But that re-introduces the laundering problem unless
  cluuterm has GRANT on its own notify_endpoint. cluuterm got
  notify_endpoint from `endpoint_create(ipc_cap)` — kernel returns
  what rights? Verify in `kernel/src/syscall/handlers.rs`
  `invoke_endpoint_create`. If full rights, cluuterm can derive
  legitimately (its own object, not laundered).
- **MAX_FD_ACTIONS struct size doubling.** Bumps the FDAC payload size
  per action; verify caller payload buffers + procmgr parser bounds.

### Order of work

1. Audit token-rights flow: `endpoint_create` rights, IPC token grant
   semantics, `token_derive` scope preservation. Write findings into
   this design before implementing.
2. Extend `FdAction` struct + serializer + procmgr parser.
3. Implement `VFS_DERIVE_CHILD_FD_LABEL` handler.
4. Procmgr FDAC branch (vfs_remote_fd != 0 → IPC to VFS).
5. Child fd_table rehydration with `(client_id, remote_fd)` from
   process_info or boot manifest.
6. Bring up `l2_cluuterm_smoke` + manually verify login prompt renders
   in cluuterm window.
7. Once login works, bring remaining 7 markers green one-by-one.
