# Virtual Filesystem

VFS is the single filesystem authority for the whole system. It owns the global
namespace and routes every `VfsOp` through a unified `MountTable`.

## Two instances

VFS runs as two variants from the same binary:

- **Root-VFS** — system-wide. Mounts `/` (ext2 via virtio-blk), `/dev/initrd`,
  `/proc` (procfs via root-procmgr), `/dev` (devfs), `/dev/pts` (PTS), and
  `/dev/input` (devreg).
- **Session-VFS** — per-login. View layered on top of root-VFS backends.
  Registers as `session-vfs` and forwards `/proc` to the session-procmgr.

The `is_session` branch in `run_vfs()` selects which variant to run, based on
whether a session envelope was passed via `ProcessInfo` params.

## Mount table

The `MountTable` is the core data structure. It maps path prefixes to mount
backends:

| Mount | Backend | Type |
|-------|---------|------|
| `/` | ext2 (via virtio-blk) | Async (IPC to ext2 service) |
| `/dev/initrd` | initrd (TAR) | Sync (in-process) |
| `/proc` | procfs | Async (IPC to procmgr) |
| `/dev` | devfs | Sync (in-process) |
| `/dev/pts` | PTS registry | Async (IPC to cluuterm) |
| `/dev/input` | devreg | Sync (in-process) |

### Sync vs Async backends

Two backend traits:

- **`MountBackend`** (sync) — for in-process backends that never cross a
  process boundary: `memfs`, `devfs` (null/zero/urandom), ext2 cached reads.
- **`AsyncMountBackend`** (async) — for IPC-bound backends: `ProcfsBackend`
  (→ procmgr), tty-read (→ tty driver), PTS-verb dispatch (→ cluuterm).

The async runtime (`Runtime`, `IpcCallFuture`, `spawn`, completion queue) is
wired into the VFS main loop and is the dispatch path for all `VfsOp` variants
on async mounts. This is the **canonical deadlock-avoidance mechanism** for
single-threaded VFS: `ProcfsBackend` and the PTS-verb dispatch arms go through
`dispatch_async()` so VFS never blocks on a downstream IPC that itself needs
VFS.

## VfsOp

The `VfsOp` enum defines every filesystem operation:

- `VFS_OPEN`, `VFS_CLOSE`, `VFS_READ_GRANT`, `VFS_READDIR`, `VFS_MAP_ELF`
- `VFS_STAT`, `VFS_FSTAT`, `VFS_REALPATH`
- `VFS_WRITE`, `VFS_MKDIR`, `VFS_RMDIR`, `VFS_UNLINK`, `VFS_RENAME`, `VFS_LINK`
- `VFS_BOUNCE_SETUP`, `VFS_READ_RING`, `VFS_RING_SETUP`
- `PTS_REGISTER`, `PTS_UNREGISTER`, `PTS_READ_DELIVER`, `PTS_SET_PGRP`
- `VFS_SET_VIEW`, `VFS_CONTAINER_CLEANUP`, `VFS_DERIVE_CHILD_FD`
- `VFS_REGISTER_DEV`

## View scoping

`VfsViewTable` is the procmgr-owned table of `ViewObject`s. Each view describes
the filesystem namespace a process sees: a list of `(path, rights, backend)`
mounts.

### Monotone-narrowing

When a process spawns a child, the child's view is derived from the parent's by
narrowing rights and adding/replacing mounts — **never by widening**.
`verify_monotone` checks:
- Same or more-specific path prefix.
- Rights ≤ parent's rights.

A child that asks for more than its parent has is denied at spawn. This is the
structural enforcement of CLUU's monotone-narrowing authority model at the VFS
layer.

### View-object caps

Views are first-class procmgr-grade typed objects. A `VfsViewManager` cap is
required to install a view (`VFS_SET_VIEW`). VFS checks the cap's type tag
before any `set_view` lands (`resolve_view_mgr_cap`).

`VIEW_SCOPE_*` masks bound which mount roots a sub-minted VfsViewManager cap
may install: `VIEW_SCOPE_ROOT`, `VIEW_SCOPE_DEV`, `VIEW_SCOPE_VAR_IMAGES`,
`VIEW_SCOPE_HOME`, `VIEW_SCOPE_TMP`, `VIEW_SCOPE_ALL`.

### View-manager cap delegation (designed 2026-05-26)

The original VFS gate was a single-manager ACL: `view_manager_tid` was bound
on the first `VFS_SET_VIEW` carrying `CapProfile::ADMIN` (root-procmgr) and
all subsequent calls were rejected unless `sender_tid == view_manager_tid`.
After Phase 12.4b, session-procmgr needs to install per-client views for its
children (e.g. cluuterm) and is denied — a tid-keyed gate is the wrong
authority model.

The fix replaces the tid-check with a **view-manager handle**: a token whose
kernel-side `ObjectRef::VfsViewManager { scope_sid, scope_mask }` is the only
proof of authority. VFS accepts `VFS_SET_VIEW` / `VFS_CONTAINER_CLEANUP` iff
the IPC carries a valid handle (passed as a message word; VFS calls
`token_resolve`). No runtime delegation hop, no multi-manager tid set, no
ambient authority — every privileged VFS call carries a cap.

The cap chain is monotone-decreasing:

```text
kernel mints view_mgr_root (scope_sid=0, scope_mask=0xFF)
  └─ root-procmgr sub-mints view_mgr_session_{sid} (scope_sid=sid, narrowed mask)
      └─ session-procmgr uses it on VFS_SET_VIEW for its children
```

`scope_sid == 0` is root authority (full access). Sub-mints constrain the
holder to clients/mounts within that session; `scope_mask` is a bitmask of
well-known mount roots (`/`, `/dev`, `/var/images`, `/home`, …) so a
session-procmgr can be granted exactly the roots it needs (e.g. `/` + `/dev`,
never `/var/images`). VFS rejects mounts that escape the scope at VFS, not at
any sender-tid check. The legacy `view_manager_tid` path stays as a
soft fallback for the bootstrap window, gated behind a feature flag until
init's cap path is verified.

## File cache

`FileCache` is an LRU-ish whole-file cache backed by a pinned `MAP_SHARE_PHYS`
region. It caches frequently-read files (ELF binaries for spawn) to avoid
repeated ext2 reads.

- `CacheRegion` — a pinned physical region for cached file data.
- `CacheEntry` — a cached file: path, inode, data region, length.
- `CachedElfMeta` / `CachedElfSegment` — cached ELF metadata for fast spawn.

## Bulk pools

VFS uses pre-mapped memory pools for zero-copy I/O:

- **Grant pool** — for `VFS_READ_GRANT` replies (VFS writes into a shared
  region, caller reads from it).
- **Ring pool** — for `VFS_READ_RING` / `VFS_RING_SETUP` (ring-buffer bulk
  reads).
- **Bounce pool** — for small payloads that don't warrant a full ring.

These are pinned at fixed virtual addresses:
- `GRANT_BUF_BASE`, `GRANT_BUF_SIZE`
- `CACHE_BUF_BASE`, `CACHE_BUF_SIZE`
- `RING_POOL_BASE`, `BOUNCE_POOL_BASE`

## PTS registry

`PtsRegistry` manages pseudo-terminal slave devices. Each cluuterm instance
registers a `/dev/pts/<id>` node. The shell opens it as a tty device file using
the same code path as legacy `/dev/tty<N>` services.

- `PTS_REGISTER_LABEL` — cluuterm registers a new pts.
- `PTS_UNREGISTER_LABEL` — cluuterm unregisters.
- `PTS_READ_DELIVER_LABEL` — VFS forwards shell stdin reads to cluuterm.
- `PTS_SET_PGRP_LABEL` — set the pts's process group.

## Dev registry

`DevRegistry` manages `/dev` entries. Device drivers register block/char
devices via `VFS_REGISTER_DEV_LABEL`. VFS enumerates them for `/dev` directory
listings.

## Key modules

| Module | Role |
|--------|------|
| `mount.rs` | `MountTable`, `DirEntry`, `DeviceBackend`, `MemFsBackend`, `DevRegistry` |
| `view.rs` | `VfsViewTable`, `set_view`, view scoping |
| `fd_table.rs` | `FdTable`, `OpenFile` |
| `memfs.rs` | `MemFsBackend` (in-process memory fs) |
| `procfs.rs` | `ProcfsBackend` (async, → procmgr) |
| `bulk_pool.rs` | `BulkPool` (bounce/ring pools) |
| `pts.rs` | `PtsRegistry` |

## Plan lessons — VFS

Distilled implementation lessons from VFS-related plans. 2-5 lines each;
see the dated plan file for the long form.

### vfs-wire-protocol-bump-cost (2026-05-07-phase4-C-ls-and-vfs-stat)

Bumping `VfsStat` to carry mtime/nlink/uid/gid/blocks and `readdir` to
return `(name, stat)` pairs in one round trip required touching the wire
format, every backend (ext2, ramfs, memfs, procfs, devfs), and the client.
Wire-format changes are expensive — they cascade through every backend and
every caller. Plan them as discrete phases; don't sneak fields in. The
ext2 backend reads extended fields from inode; the other backends supply
defaults.

### fast-symlink-realpath (2026-05-09-symlink-following-resolution)

Fast symlinks store their target inline in the 60-byte `i_block` window.
`Inode::parse` originally decoded those bytes as `[u32; 12] + 3 * u32`,
throwing away the raw view. `inline_block_bytes()` re-serialises them so
targets ≤60 bytes read without a data-block fetch. Four hard-coded
`strip_prefix("/bin/")` sites in the shell were replaced with
`VfsClient::realpath()` + image-name extraction. `FS_REALPATH = 0x30D` on
the remote-FS server; `VFS_REALPATH = 0x210` on VFS. Non-ext2 backends
return the path unchanged. Procmgr rejects image names containing `/`.
