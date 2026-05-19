# Plan 1 Handoff — Unified Spawn Protocol

## State
- Branch: develop (13 commits)
- Build: `cargo xtask build` ✅
- Harness: reaches `compositor: ready` ✅
- Clippy: pre-existing warnings only ✅
- Plan 1 plan doc: `docs/superpowers/plans/2026-05-18-plan1-unified-spawn-protocol.md`
- Plan 1 spec doc: `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`
- Implementer brief: `docs/superpowers/plans/2026-05-19-implementer-brief.md` (READ FIRST)

## What landed (13 commits)

### New crate: `userspace/cluu_proto/`
Shared wire protocol types (no_std, postcard). All 4 specs share this crate.

| Module | Contents |
|--------|----------|
| `spawn.rs` | `SpawnEnvelope`, `ViewSource`(Derive|BootstrapRoot), `FdInherit`, `FdSource`(VfsFd|EndpointCap), `FdRights`, `RestartPolicy`, `SpawnReply`, `SpawnError`. Label: `PROCMGR_SPAWN_UNIFIED_LABEL = 80`. 4 round-trip tests. |
| `primordial.rs` | `PrimordialSeed`, `PrimordialSeedReply`. Label: `PROCMGR_PRIMORDIAL_SEED_LABEL = 81`. 2 tests. |
| `lib.rs` | `ABI_VERSION = 1`, `TokenHandle = u64`. Modules: `spawn`, `primordial`. Will grow: `pts`, `session`, `window`. |

### `userspace/libcluu/`
- `src/spawn.rs` — `pub fn spawn(envelope) -> Result<SpawnReply, SpawnError>`. Looks up `procmgr:spawn` via registry, serializes envelope via postcard, calls `call_with_reply_buf`, deserializes reply. Uses `IPC_MSG_HEADER_SIZE = 56` for reply payload offset.
- `src/posix/process.rs` — added `posix_spawn_v2()`: C ABI → SpawnEnvelope → libcluu::spawn::spawn(). Translates `adddup2` file actions into `ProtoFdInherit` (VfsFd/EndpointCap). Old `posix_spawn()` retained for backward compat.
- `Cargo.toml` — added `cluu_proto`, `postcard` deps
- `lib.rs` — `pub use cluu_proto as proto;`, `pub mod spawn;`

### `userspace/procmgr/`
- `src/spawn.rs` — `pub fn spawn(envelope, caller_pid) -> Result<SpawnReply, SpawnError>`. 10-step body (spec 1 §12). **HOOKS STUBBED** — every integration point uses `hooks::*` stubs returning Err(()) / None / false. See "What's not wired" below.
- `src/manifest_cache.rs` — `ManifestCache` with `get_or_load(image, loader)`. Singleton `MANIFEST_CACHE`.
- `src/view_table.rs` — `ViewTable` with `insert/inc_ref/dec_ref/snapshot`. `verify_monotone(child, parent)` for view derive. 4 unit tests.
- `src/main.rs` — dispatch: `PROCMGR_SPAWN_UNIFIED_LABEL` → `handle_spawn_unified()` → deserialize `SpawnEnvelope` → `::procmgr::spawn::spawn()` → postcard reply via `send_msg_with_payload`. Local const `PROCMGR_SPAWN_UNIFIED_LABEL` re-exported.
- `Cargo.toml` — added `cluu_proto`, `spin`, `postcard` deps
- `lib.rs` — `extern crate alloc;`, `pub use cluu_proto as proto;`, `pub mod spawn/manifest_cache/view_table;`

### `userspace/cluuterm/`
- `src/main.rs` — `spawn_shell_with_pts()`: opens `/dev/pts/<id>` with `_open`, reads `FD_TABLE` for VFS addresses, builds `SpawnEnvelope` with `FdSource::VfsFd` entries for fd 0/1/2, calls `libcluu::spawn::spawn()`. Deleted: `extern "C" posix_spawn/*`, `adddup2` calls.
- `Cargo.toml` — added `cluu_proto` dep

### `userspace/shell/`
- `src/commands/exec.rs` — `spawn_process_with_argv_and_redirs()`: builds `SpawnEnvelope` with FdInherit from fd table, calls `libcluu::spawn::spawn()`. Deleted: `PROCMGR_CONTAINER_RUN_LABEL` call.
- `src/pipeline.rs` — pipeline stages: builds `FdInherit` with `FdSource::EndpointCap` for pipe tokens, wraps in `SpawnEnvelope`, calls `libcluu::spawn::spawn()`. Deleted: `PROCMGR_CONTAINER_RUN_LABEL`, `build_container_run_payload_full`.
- `Cargo.toml` — added `cluu_proto` dep

### FdSource::EndpointCap (added for pipe support)
- `cluu_proto/src/spawn.rs` — new variant: `EndpointCap { endpoint_token: u64 }`
- `procmgr/src/spawn.rs` — new arm in FdInherit match: calls `hooks::inherit_endpoint_cap()`

## What's NOT wired (blocking Plan 1 tasks 12-14)

`procmgr/src/spawn.rs` hooks module — all 17 functions are stubs:

| Hook | What it needs to do | Where existing code lives |
|------|---------------------|--------------------------|
| `resolve_token` | Resolve caller's TokenHandle → procmgr-side raw endpoint | procmgr token table (in main.rs) |
| `derive_send` | Derive IPC_SEND cap into procmgr's table | `resolve_notify_endpoint` pattern (a597e09) |
| `vfs_derive_child_fd` | Derive child fd token from VFS (parent_cid, parent_fd) | existing VFS derive helpers |
| `alloc_child_space_and_thread` | Space + Thread allocation | `space_create` + `thread_create` syscalls in main.rs |
| `load_elf` | Map ELF into child space | existing ELF loader in main.rs |
| `write_process_info` | Write PI page with argv/env/inherited FDs | existing PI writer |
| `insert_process_entry` | Create ProcessEntry in procmgr table | existing table insertion |
| `resume_thread` | Start suspended thread | `thread_resume` or similar |
| `derive_thread_token_for_caller` | Give caller a child-thread handle | existing token derive |
| `resolve_session_token` | Look up session by token, check Live, bump refcount | session_table (Plan 3 will create) |
| `dec_session_refcount` | Rollback: decrement session refcount | session_table |
| `revoke_procmgr_token` | Revoke a token in procmgr's table | existing token_revoke |
| `destroy_space` | Tear down partially-built space | `invoke_space_destroy` |
| `caller_can_spawn_sessionless` | Check manifest right | manifest_cache/manifest right check |
| `init_pid` | Return INIT_PID constant | init spawning code |
| `procmgr_self_pid` | Return procmgr's own pid | self-pid tracking |
| `build_root_view_for_primordial` | Mint bootstrap view for primordial | view_table + mount_policy |
| `narrow_for_manifest` | Derive narrowed child view from parent | `mount_policy.rs` narrowing logic |

**To wire a hook:** find the existing equivalent in `procmgr/src/main.rs`, make it `pub(crate)`, call it from the hook body. Delete `unimplemented!()`. Verify with `grep -n "unimplemented\|Err(())" userspace/procmgr/src/spawn.rs` returns 0.

After hooks are wired: Plan 1 tasks 12 (autostart flip), 13 (SESSION_LOGIN flip), 14 (init PRIMORDIAL_SEED) become implementable. Task 15 (delete dead labels) should be done after all callers migrate.

## Dependency chain for Plans 2-4

```
Plan 1 tasks 1-4 (cluu_proto + libcluu re-export) ✅ DONE
    ↓
Plan 2 (terminal/PTY unification) — can start
Plan 3 (session lifecycle) — can start
    ↓
Plan 4 (window protocol) — depends on Plan 3 task 5
```

## What Plan 2 needs from Plan 1

- `cluu_proto` crate exists → add `cluu_proto::pts` module (labels 100-110, Termios, Winsize, PtsErr, etc.)
- `SpawnEnvelope` type → used for TERM env propagation in shell spawn
- `FdInherit` + `FdSource` → PTS endpoints are VFS-backed files, use `FdSource::VfsFd`
- `postcard` in workspace → serialization for PTS verbs
- `libcluu` re-exports `cluu_proto` → `libcluu::proto::pts::*` available
- `procmgr::spawn()` exists → shell spawn during PTS handoff uses it

## What Plan 3 needs from Plan 1

- `cluu_proto::session` module to add (labels 82-88)
- `TokenHandle` type for session tokens
- `procmgr::spawn()` with `envelope.session: Option<TokenHandle>` field
- `procmgr` depends on `cluu_proto` → can use session types directly
- `libcluu::spawn::spawn()` → login binary spawns cluuterm

## Key files to modify in Plan 2

| File | Why |
|------|-----|
| `userspace/cluu_proto/src/pts.rs` | NEW: 11 PTS_* labels + types |
| `userspace/cluu_proto/src/lib.rs` | add `pub mod pts;` |
| `userspace/libcluu/src/tty_core/line_discipline.rs` | expand to LineDiscOutput API |
| `userspace/libcluu/src/tty_core/mod.rs` | add routing helper |
| `userspace/libcluu/src/posix/termios.rs` | NEW: tcgetattr/setattr/flush/ioctl shims |
| `userspace/cluuterm/src/tty_backend.rs` | implement all 11 PTS_* verbs |
| `userspace/cluuterm/src/main.rs` | SIGWINCH on WIN_CONFIGURE |
| `userspace/tty/src/main.rs` | replace TTY_* dispatch with PTS_* |
| `userspace/shell/src/` | drop `tty_endpoint != 0` branch |
| `userspace/vfs/src/` | per-session /dev/pts overlay |

## Key files to modify in Plan 3

| File | Why |
|------|-----|
| `userspace/cluu_proto/src/session.rs` | NEW: labels 82-88, rights, types |
| `userspace/procmgr/src/session_table.rs` | NEW: SessionObject table + verb handlers |
| `userspace/libcluu/src/session.rs` | NEW: client wrappers |
| `userspace/procmgr/src/main.rs` | session dispatch arms, delete SESSION_LOGIN swap |
| `userspace/procmgr/src/spawn.rs` | consult SessionObject for `envelope.session` |
| `userspace/login/src/main.rs` | rewrite post-auth flow |
| `userspace/compositor/src/` | COMPOSITOR_SESSION_HANDOFF + SESSION_ENDED subscriber |
| `userspace/getty/` | NEW: text-VT login binary |

## Build/verify commands

```bash
cd /home/vlb2bp/git/cluu
cargo xtask build                          # full build
cargo build -p cluu_proto                  # single crate
cargo test -p cluu_proto --features host-test  # proto tests
bash scripts/harness_run.sh                # boot smoke (expect "compositor: ready")
```