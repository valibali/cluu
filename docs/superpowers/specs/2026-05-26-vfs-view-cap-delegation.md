# VFS view-cap delegation

**Date:** 2026-05-26
**Branch:** procmgr-cap-refactor
**Status:** spec (pre-impl)
**Depends on:** docs/superpowers/specs/2026-05-21-procmgr-cap-refactor-design.md
**Resolves:** blocker discovered in Phase 12.4b-4b (session-procmgr cannot install VFS views for its children)

## Problem

VFS uses a single-manager ACL gate:

```rust
// userspace/vfs/src/main.rs:651,859
view_manager_tid: Option<usize>,
if manager_tid != sender_tid { return Err(PermissionDenied); }
```

`view_manager_tid` is bound on the first `VFS_SET_VIEW` carrying `CapProfile::ADMIN` (root-procmgr). All subsequent `VFS_SET_VIEW` and `VFS_CONTAINER_CLEANUP` calls are rejected unless `sender_tid == view_manager_tid`.

After Phase 12.4b, session-procmgr spawns children (e.g. cluuterm) and needs to install a per-client view. Its `set_view` call is denied by VFS:

```
[9.514] vfs: set_view denied sender_tid=14 manager_tid=4
[9.537] cluuterm: open /dev/pts for read failed
```

This is the same architectural violation already noted in
[[project_procmgr_acl_redesign]]: VFS gates a privileged operation on
`sender_tid` membership, not on a presented capability. Possession
must be the only proof of authority.

## Non-goals

- Do **not** allow runtime delegation (`session-procmgr → root-procmgr → VFS`). That re-introduces the same runtime-ACL hop we are trying to remove.
- Do **not** loosen VFS into a multi-manager set. That would still be tid-keyed.
- No "ambient authority" — every privileged VFS call must carry a cap.

## Design

A **view-manager handle** is a token whose ObjectRef points to VFS's "view-manager" object. VFS accepts `VFS_SET_VIEW` and `VFS_CONTAINER_CLEANUP` iff the IPC carries a valid view-manager handle (passed as the destination cap or as an explicit word in the message — see *Transport* below). Authority follows the handle, not the sender tid.

### Cap chain

```
kernel mints                root-procmgr derives          session-procmgr derives
view_mgr_root  ─┬─ GRANT ── view_mgr_session_1 (sid=1) ── view_mgr_session_1_child_1
                ├─ GRANT ── view_mgr_session_2 (sid=2) ── …
                └─ …
```

- `view_mgr_root` is the root capability; it can install views for any client (legacy unchanged for ext2/initrd autostart).
- Sub-mints carry a `view_scope` field on the kernel-side ObjectRef: `(sid: u32, max_mounts: u16)`. VFS reads the scope and rejects mounts that escape it (e.g. mounting `/var/images/*` from inside `sid=1` is denied at VFS, not at any sender-tid check).
- `Rights::GRANT` controls re-derivation.

### ObjectRef extension

```rust
// kernel/src/token/object_ref.rs (proposed)
ObjectRef::VfsViewManager {
    scope_sid: u32,      // 0 = root authority
    scope_mask: u8,      // bitmask of allowed roots (see "mount roots" below)
}
```

`scope_sid == 0` ⇔ root authority (full access). Sub-mint with `scope_sid != 0` constrains the holder to clients/mounts within that session.

### Mount roots

Each known mount root (`/`, `/dev`, `/var/images`, `/home`, …) gets a bit in `scope_mask`. Sub-mint must request only bits that the parent holds. Allows tight grants like "this view-manager can only touch `/` and `/dev`" — perfect for a session-procmgr.

### Transport

Option A (preferred): pass the view-manager handle as an **extra dest cap** alongside the IPC. Kernel resolves the cap and forwards `ObjectRef::VfsViewManager` to VFS. VFS reads `msg.aux_cap` and checks scope.

Option B (fallback if extra dest caps need plumbing): include the handle as `msg.words[X]`. VFS calls `token_resolve(handle)` to fetch ObjectRef.

We default to Option B: less kernel work, ships in the same phase.

### Wiring

1. **Kernel bootstrap** mints `view_mgr_root` (Rights = SEND|RECV|CALL|GRANT, ObjectRef = `VfsViewManager { scope_sid: 0, scope_mask: 0xFF }`). Hand to init.
2. **Init** forwards `view_mgr_root` to root-procmgr in its `TOKEN_EXTRA_1` slot (or new `TOKEN_VFS_VIEW_MGR`).
3. **root-procmgr autostart** uses `view_mgr_root` for any direct `VFS_SET_VIEW` it issues (compositor, login, kbd, etc.). Existing call path unchanged except for the cap passed.
4. **root-procmgr SESSION_CREATE** sub-mints `view_mgr_session_{sid}` with `scope_sid = sid` and `scope_mask` restricted (e.g. `/` + `/dev` only — never `/var/images`). Passes it to session-procmgr in `TOKEN_VFS_VIEW_MGR`.
5. **session-procmgr.elf_spawn** uses the inherited view-manager handle when issuing `VFS_SET_VIEW` for its children.
6. **VFS** drops `view_manager_tid: Option<usize>`. New gate: read the aux/embedded cap; reject if resolve fails or `ObjectRef != VfsViewManager` or `scope_sid` doesn't authorize the requested client/mounts.

### Backward compatibility

The single-manager check (`view_manager_tid`) becomes a soft fallback for the bootstrap window: if no cap is presented and the legacy bootstrap conditions hold (`requested_client_id == 0`, `CapProfile::ADMIN`), bind sender_tid as before. Remove the fallback when init carries the cap.

## Implementation plan (Phase 12.4b-VFS-CAP)

T1. `kernel/src/token/object_ref.rs`: add `ObjectRef::VfsViewManager { scope_sid, scope_mask }`.
T2. `kernel/src/bootstrap.rs`: mint `view_mgr_root` near `clock_token_handle`; write to `boot_info.view_mgr_token`.
T3. `userspace/libcluu/src/boot.rs`: add `view_mgr_token` field to `BootInfo` and `TOKEN_VFS_VIEW_MGR` index.
T4. `userspace/init/src/context.rs`: thread the cap through, fill `tokens[TOKEN_VFS_VIEW_MGR]` for root-procmgr.
T5. `userspace/root-procmgr/src/main.rs`: load `self.view_mgr_token` from ProcessInfo; pass it on every VFS_SET_VIEW; in SESSION_CREATE, sub-mint with narrowed `scope_sid + scope_mask`; forward to session-procmgr in its `TOKEN_VFS_VIEW_MGR` slot.
T6. `userspace/session-procmgr/src/main.rs` + `elf_spawn.rs`: store `view_mgr_cap`; pass it on `VFS_SET_VIEW`.
T7. `userspace/vfs/src/main.rs`: extend `handle_set_view` + `handle_container_cleanup` to resolve the cap via a new syscall wrapper (`vfs_resolve_view_mgr_cap`) and check scope. Keep `view_manager_tid` fallback gated by a feature flag until everyone is on the cap.
T8. Smoke: `MARKER_MODE=l2_cluuterm_login`. Verify cluuterm opens `/dev/pts` and spawns `/bin/shell`.
T9. Add `pm_vfs_view_scope` integration test (Phase 13.2): try to mount `/var/images` from a sid=1 view-mgr → expect denial; mount `/dev` → expect success.

## Risks

- ObjectRef variants are kernel state — adding one is invasive but matches existing pattern (Clock, Frame).
- Per-mount scope bitmasks may not fit if mount-root set grows past 8 entries. Reserve as `u16` for headroom.
- Removing `view_manager_tid` fallback is the migration cliff; keep behind a feature flag until init's cap path is verified.

## Out of scope

- Multi-VFS / federated VFS — single VFS process keeps owning the storage. Only the *authority to install per-client views* is delegated.
- Reworking `VFS_FILE_OPEN` / `VFS_READ_GRANT` — these already check the per-client view, not the sender tid directly.

## Open questions

Q1. Do we keep `scope_mask` as a bitmask of well-known roots, or store an actual prefix path string in the ObjectRef? Bitmask is simpler but assumes a fixed mount-root taxonomy. Recommend bitmask now; revisit if it bites.

Q2. Should `VFS_CONTAINER_CLEANUP` be in scope, or stay root-only? Recommend in scope — session-procmgr should clean up its own sessions on shutdown.

Q3. Is there a simpler half-step (e.g. "VFS accepts set_view from anyone who can produce sender_tid == some pre-registered list") that ships in a day? Reject — that's still tid-keyed.
