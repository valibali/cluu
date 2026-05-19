# Plan 3 Handoff — Session Lifecycle

## State
- Branch: develop
- Plan 2 complete: 10 commits, `cargo xtask build` ✅, harness boot smoke ✅
- cluu_proto crate has `spawn`, `primordial`, `pts` modules
- libcluu has `session` module NOT YET created
- procmgr has spawn.rs with stubbed hooks (17 hooks still stubbed from Plan 1)
- Compositor runs as single persistent process (not yet session-aware)
- Login binary uses old PROCMGR_SESSION_LOGIN_LABEL path
- 2s COMPOSITOR_READY_LABEL timeout still present

## What Plan 3 does (12 tasks)

Replace compositor swap with persistent compositor + typed SessionObject capability model.

### Task 1: cluu_proto::session module
- File: `userspace/cluu_proto/src/session.rs` (create) + lib.rs (modify)
- Labels 82-88: SESSION_CREATE, SESSION_DESTROY, SESSION_QUERY, SESSION_SUBSCRIBE, SESSION_DERIVE_TOKEN, SESSION_ENDED, SESSION_SET_LEADER
- COMPOSITOR_SESSION_HANDOFF_LABEL = 200
- Rights bitmask: CONTROL/QUERY/SUBSCRIBE/JOIN
- Types: ProfileSpec, SessionCreateRequest/Reply/Ok, SessionDestroy/Query/Subscribe/Derive/SetLeader Request/Reply, SessionEndedEvent, CompositorSessionHandoffRequest/Reply
- Errors: SessionErr, SessionCreateErr
- 4 round-trip tests
- Build: `cargo test -p cluu_proto --features host-test`

### Task 2: libcluu::session client wrapper
- File: `userspace/libcluu/src/session.rs` (create) + lib.rs (modify)
- Wrapper functions: create(), destroy(), query(), subscribe(), derive_token(), set_leader()
- Uses `crate::ipc::call_procmgr()` + postcard serialization
- Build: `cargo build -p libcluu`

### Task 3: SessionObject table + handlers in procmgr
- File: `userspace/procmgr/src/session_table.rs` (create) + lib.rs (modify)
- SessionObject struct: id, user_name, profile, leader_pid, state, refcount, subscribers, created_at
- SessionTokenEntry: session_id, rights, owner_pid
- SessionTable: create(), resolve(), destroy(), set_leader(), subscribe(), derive_token()
- Token cap-narrow-derive: child rights ⊆ parent rights (capability discipline)
- Build: `cargo build -p procmgr`

### Task 4: Procmgr verb dispatch arms
- File: `userspace/procmgr/src/main.rs` (modify)
- Add dispatch arms for labels 82-88 in procmgr's IPC recv loop
- Each arm deserializes request via postcard → calls session_table method → serializes reply
- Wire SESSION_SET_LEADER: after leader exit notification, cascade SIGHUP to members + fanout SESSION_ENDED to subscribers
- Build: `cargo build -p procmgr`

### Task 5: Compositor handoff + subscribe
- Files: `userspace/compositor/src/main.rs`, `compositor/src/protocol.rs`, `compositor/src/state.rs`
- Add COMPOSITOR_SESSION_HANDOFF_LABEL = 200 to protocol.rs
- In state.rs: add `session_id: Option<u32>` per window
- In main.rs: handle COMPOSITOR_SESSION_HANDOFF → mark window as session-owned
- Subscribe to SESSION_ENDED events → close session windows + respawn login window
- Build: `cargo build -p compositor`

### Task 6: Login binary rewrite
- File: `userspace/login/src/main.rs`
- Post-auth flow: SESSION_CREATE(user_name, profile) → SESSION_DERIVE_TOKEN → COMPOSITOR_SESSION_HANDOFF → procmgr::spawn(shell) → SESSION_SET_LEADER → exit
- Delete: kill_system_compositor(), spawn_user_compositor(), session_mode PARAM, old SESSION_LOGIN swap
- Build: `cargo build -p login`

### Task 7: Manifest updates
- Files: `/var/images/login/manifest.toml`, `/var/images/compositor/manifest.toml`
- login: RIGHT_SESSION_CREATE, SESSIONLESS allow, RESTART never
- compositor: add COMPOSITOR_SESSION_HANDOFF subscriber
- Build: (no code — manifest changes only)

### Task 8: Compositor boot-time login spawn
- File: `userspace/compositor/src/main.rs` (modify)
- On startup, compositor spawns /bin/login as its initial window
- Login window is sessionless (compositor's own window)
- Build: `cargo build -p compositor`

### Task 9: Delete PROCMGR_SESSION_LOGIN_LABEL + 2s timeout
- Files: `userspace/procmgr/src/main.rs`, `userspace/libcluu/src/ipc.rs`
- Delete: SESSION_LOGIN swap, kill_system_compositor, spawn_user_compositor, 8 admin force-unregisters
- Delete: COMPOSITOR_READY_LABEL const + 2s wait
- Git grep verification: zero hits for PROCMGR_SESSION_LOGIN_LABEL, COMPOSITOR_READY_LABEL, kill_system_compositor, spawn_user_compositor
- Build: `cargo xtask build`

### Task 10: Getty binary for text-VT
- Files: `userspace/getty/Cargo.toml` (create) + `getty/src/main.rs` (create)
- Simple text-VT login binary
- Opens /dev/tty<n>, displays login prompt, reads username/password
- On auth success: SESSION_CREATE → spawn /bin/shell → SESSION_SET_LEADER → exit
- Add to Cargo.toml workspace members
- Add /etc/autostart.toml entries for getty on tty1, tty2, tty3
- Build: `cargo build -p getty`

### Task 11: procmgr::spawn consumes envelope.session
- File: `userspace/procmgr/src/spawn.rs` (modify)
- If envelope.session is Some(token), resolve token → SessionObject
- Add child PID to session's member_pids list
- Increment session refcount
- On child exit: decrement refcount; if leader and state=Live → set Dying → SIGHUP members → SESSION_ENDED fanout
- Build: `cargo build -p procmgr`

### Task 12: Acceptance markers
- 8 probes in `userspace/probes/l3_*/`
- Markers: session_create_destroy, session_derive_narrow, session_query, session_set_leader_monotone, session_leader_exit_cascades, session_end_removes_pts, compositor_receives_session_ended, getty_auth_spawns_shell
- Add to Cargo.toml workspace + containers
- Build: `cargo xtask build`

## Dependency chain

```
Task 1 (session types) ──────────────────────────────────────────────┐
  └→ Task 2 (libcluu wrapper) ───────────────────────────────────────┤
       └→ Task 3 (session table) ────────────────────────────────────┤
            └→ Task 4 (procmgr dispatch) ────────────────────────────┤
                 ├─→ Task 11 (spawn envelope.session) ───────────────┤
                 └─→ Task 9 (delete SESSION_LOGIN) ──────────────────┤
                                                                      │
  Task 1 ─────────────────────────────────────────────────────────────┤
  └─→ Task 5 (compositor handoff) ───────────────────────────────────┤
       └─→ Task 6 (login rewrite) ───────────────────────────────────┤
            ├─→ Task 7 (manifest updates) ───────────────────────────┤
            └─→ Task 8 (compositor boot-time login) ─────────────────┤
                                                                      │
  Task 1 ─────────────────────────────────────────────────────────────┤
  └─→ Task 10 (getty binary) ────────────────────────────────────────┤
                                                                      │
  Task 12 (markers) ← depends on ALL above ──────────────────────────┘
```

## Parallel batches
| Batch | Tasks | Rationale |
|-------|-------|-----------|
| A | 01, 10 | Different crates (cluu_proto vs getty) |
| B | 02 | Needs 01 |
| C | 03, 05 | Different crates (procmgr vs compositor), both need 01 |
| D | 04 | Needs 03 |
| E | 06, 11 | Different crates (login vs procmgr), both need 03+04+05 |
| F | 07, 08 | Sequential within same crate |
| G | 09 | Needs 11 done (spawn.session wired first) |
| H | 12 | Needs all |

## Key constraints (same as Plan 2)
- No new syscalls
- No recv_with_timeout/call_with_timeout
- Commit after every task
- Build green between tasks (`cargo xtask build`)
- Capability discipline: tokens, not ambient authority
- postcard serialization for all wire formats

## Build/verify commands
```bash
cargo xtask build                          # full build
cargo test -p cluu_proto --features host-test  # proto tests
cargo build -p libcluu/procmgr/compositor/login/getty  # single crate
bash scripts/harness_run.sh                # boot smoke (expect "compositor: ready")
```

## Starting point
Session file: `.tmp/sessions/2026-05-19-plan3-session-lifecycle/context.md`
Plan doc: `docs/superpowers/plans/2026-05-18-plan3-session-lifecycle.md`
Spec: `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`