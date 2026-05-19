# Plan 4 Handoff — Window Protocol Formalization

**For:** Next agentic session (deepseek v4 pro / Sisyphus).
**Action:** Execute Plan 4. Plan 3 complete. Builds clean (`cargo xtask build`).
**Branch:** `develop`. Commit after every task. Build green between tasks.

---

## State at Handoff

- **Plan 3** (Session Lifecycle): 12 tasks, 15 commits, 100% implemented. `cargo xtask build` ✅.
- **Plan 4**: 13 tasks. Last of the four-spec sequence.
- **Key bugs fixed**: reply-buffer length check, procmgr:session output registration, getty GPF.
- **Boot test pending**: interactive login → SESSION_CREATE → spawn cluuterm.

## Critical Constraints (READ FIRST)

- **No new syscalls.** All new verbs go through existing IPC endpoints.
- **No timeouts.** No `recv_with_timeout` or `call_with_timeout` added.
- **Capability discipline.** Tokens, not ambient authority. Narrow-derive only.
- **Postcard serialization** for all wire formats.
- **Frame typing** landed — use typed-frame alloc/free, not raw phys.
- **ABI_VERSION = 1.**
- **Commit after EVERY task.** Per-task gate: `cargo xtask build` clean.

## Key Files

- **Plan doc**: `docs/superpowers/plans/2026-05-18-plan4-window-protocol.md`
- **Implementer brief**: `docs/superpowers/plans/2026-05-19-implementer-brief.md`
- **Spec**: `docs/superpowers/specs/2026-05-18-window-protocol-design.md`
- **cluu_proto crate** (Task 1): `userspace/cluu_proto/src/` — add `window.rs` module
- **libcluu crate** (Task 2): `userspace/libcluu/src/` — add `window.rs` module
- **Compositor** (Tasks 3-7): `userspace/compositor/src/` — main.rs, surface.rs, buffer_table.rs, state.rs, render.rs, compose.rs, protocol.rs
- **cluuterm** (Task 8): `userspace/cluuterm/src/` — flip to `libcluu::window`
- **login** (Task 9): `userspace/login/src/` — flip to `libcluu::window`
- **Keymap** (Task 10): `/etc/keymap/us.toml` (or embedded default in compositor)
- **Per-frame callback** (Task 11): Retire `broadcast_frame_ready`
- **Plan doc**: `docs/superpowers/plans/2026-05-18-plan4-window-protocol.md`

## Known State

- **Compositor already has**: framebuffer, IPC recv loop, window state (legacy `COMP_WIN_*`), render/compose helpers.
- **cluu_proto already has**: TokenHandle, ABI_VERSION, postcard re-export.
- **libcluu already has**: session.rs (Plan 3), IPC wrappers, registry lookup, frame alloc.
- **cluuterm already has**: legacy window management via raw IPC. Needs flip to `libcluu::window`.
- **login already has**: session lifecycle (Plan 3), single login window. Needs flip to `libcluu::window`.
- **Existing patterns**: procmgr session dispatch (Plan 3) is the reference pattern for compositor dispatch.

## Plan 4 Task Summary

| # | Task | Files | Key Deliverable |
|---|---|---|---|
| 1 | cluu_proto::window types + labels | window.rs, lib.rs | 16 labels (210-226), all types, 5 round-trip tests |
| 2 | libcluu::window client wrappers + SurfaceBufferPool | window.rs, lib.rs | 9 client functions, recv_event, double-buffered pool |
| 3 | Compositor Surface + BufferTable state machines | surface.rs, buffer_table.rs, state.rs | State machine + buffer lifecycle |
| 4 | Per-client async event endpoint | main.rs, window.rs | Mint endpoint at WIN_CREATE, return to client |
| 5 | Dispatch arms for 9 client-facing verbs | main.rs, protocol.rs | CREATE/DESTROY/ATTACH/DETACH/COMMIT/CALLBACK/TITLE/GEOMETRY/FOCUS |
| 6 | Render loop — per-frame callback + buffer transitions | main.rs, render.rs, compose.rs | promote_for_render, buffer-released events, retire broadcast_frame_ready |
| 7 | Focus tracking + pre-translated input | main.rs, state.rs, hotkeys.rs, keymap | transfer_focus, US scancode→KeyEvent translation |
| 8 | cluuterm flips to libcluu::window | main.rs, render.rs | SurfaceBufferPool, WIN_CREATE, frame-callback |
| 9 | login flips to libcluu::window | main.rs | WIN_CREATE for login window |
| 10 | Keymap from /etc/keymap/us.toml | keymap.toml or embedded | TOML→scancode table, fallback to built-in US |
| 11 | Per-frame callback replaces broadcast_frame_ready | main.rs, render.rs | Surface-by-surface frame-ready events |
| 12 | Acceptance probes | probes/ | Per-task probe binaries |
| 13 | Integration test | harness_run.sh | Boot smoke + visual smoke (fb_dump.sh) |

## Build/Verify Cheat Sheet

```bash
# Full build
cargo xtask build

# Single crate
cargo build -p cluu_proto
cargo build -p libcluu
cargo build -p compositor
cargo build -p cluuterm
cargo build -p login

# Tests (cluu_proto)
cargo test -p cluu_proto --features host-test

# Boot smoke
bash scripts/harness_run.sh

# Visual smoke
bash scripts/fb_dump.sh

# Marker run
HARNESS_FORCE_BUILD=1 MARKER_MODE=<m> bash scripts/harness_run.sh
grep "<m>:" serial.log
```

## Reference — Plan 3 Patterns (Reuse These)

### cluu_proto module pattern:
- Module in `src/<name>.rs` with `#![no_std]` attribute
- `pub mod <name>;` in `lib.rs`
- Round-trip tests using `#[cfg(test)]` + `postcard::to_allocvec`/`from_bytes`

### libcluu wrapper pattern:
- Static cached endpoint via `spin::Mutex<Option<EndpointHandle>>`
- `call_compositor<Req, Rep>(label, request) -> Result<Rep, WinErr>`
- `build_words(payload_len)` helper: [payload_len, ABI_VERSION, 0, 0, 0, 0]

### Compositor dispatch pattern (from procmgr):
- `if msg.tag.label == WIN_CREATE_LABEL { return self.handle_win_create(...) }`
- Handler: deserialize payload, validate caller, operate on state, reply postcard

---

## To Execute

1. Read `docs/superpowers/plans/2026-05-19-implementer-brief.md` first.
2. Read `docs/superpowers/plans/2026-05-18-plan4-window-protocol.md` in full.
3. **Start at Task 1.** Execute in order. Each task ends with `git commit`.
4. Run `cargo xtask build` between tasks.
5. After Tasks 8-9: boot smoke test (`bash scripts/harness_run.sh`).
6. After Task 13: full integration test + visual smoke.

---

**Git tag point**: `plan4-start` — tag before first commit if you want a rollback point.

```bash
git tag plan4-start HEAD
```