# Task Context: Unified Spawn Protocol (Plan 1)

Session ID: 2026-05-19-plan1-unified-spawn
Created: 2026-05-19
Status: in_progress

## Current Request
Implement Plan 1 of the four-plan CLUU superpowers work: unify six existing spawn paths into one IPC verb (`PROCMGR_SPAWN_UNIFIED_LABEL = 80`) carrying a postcard-serialized `SpawnEnvelope`, plus a one-shot bootstrap verb (`PROCMGR_PRIMORDIAL_SEED_LABEL = 81`).

## Context Files (Standards to Follow)
- `.opencode/context/core/standards/code-quality.md` — Pure functions, immutability, small functions (<50 lines), dependency injection
- `.agents/skills/rust-best-practices/SKILL.md` — Borrowing over cloning, Result not panic, clippy linting, test naming conventions
- `docs/ARCHITECTURE.md` — Microkernel architecture, capability tokens, IPC labels
- `CONTRIBUTING.md` — Build workflow, SOLID principles, no panics in kernel

## Reference Files (Source Material to Look At)
- `Cargo.toml` — Workspace root; add postcard dep + cluu_proto member
- `userspace/libcluu/Cargo.toml` — Add cluu_proto dependency
- `userspace/libcluu/src/lib.rs` — Re-export cluu_proto
- `userspace/libcluu/src/ipc.rs` — Existing IPC helpers (call_procmgr, labels)
- `userspace/procmgr/Cargo.toml` — Add cluu_proto dependency
- `userspace/procmgr/src/main.rs` — Existing spawn handlers, IPC dispatch loop
- `userspace/procmgr/src/lib.rs` — Module declarations
- `userspace/cluuterm/src/main.rs` — Replace posix_spawn with libcluu::spawn
- `userspace/shell/src/` — Pipeline/external-command spawns
- `userspace/init/src/wiring.rs` — Kernel-spawn only procmgr, send PRIMORDIAL_SEED
- `kernel/src/` — Reduce launch_service to launch_procmgr

## External Docs Fetched
- postcard 1.x — standard Rust crate, `#[derive(Serialize, Deserialize)]` types, `no_std` compatible with `alloc` feature
- bitflags 2.4 — already in workspace
- serde 1.0 — already widely used; add `derive` feature for postcard derives

## Components
1. cluu_proto crate — shared wire-protocol types
2. SpawnEnvelope types — postcard-serializable spawn request
3. PrimordialSeed types — init → procmgr bootstrap
4. libcluu re-export — make proto types available to callers
5. procmgr integration — depend on cluu_proto
6. Manifest cache — lazy-loaded manifest metadata
7. ViewObject table — typed view objects with monotone-derive
8. procmgr::spawn() core function — single 10-step spawn entry point
9. IPC dispatch — PROCMGR_SPAWN_UNIFIED_LABEL handler
10. libcluu spawn API — caller-side surface
11. cluuterm flip — replace newlib posix_spawn with libcluu::spawn
12-20. Further caller migration + dead code deletion

## Constraints
- No new syscalls
- No timeouts (no recv_with_timeout)
- cap-revocation on service death
- Microkernel discipline: procmgr is sole process lifecycle owner
- Rust 2021, no_std workspace, postcard serialization
- Each task ends with a git commit
- Per-task gate: `bash scripts/harness_run.sh` reaches `compositor: ready`

## Exit Criteria
- [ ] cluu_proto crate exists and compiles
- [ ] SpawnEnvelope + PrimordialSeed types with round-trip tests
- [ ] libcluu re-exports proto types
- [ ] procmgr::spawn() function wired
- [ ] PROCMGR_SPAWN_UNIFIED_LABEL dispatch working
- [ ] libcluu::spawn() API usable
- [ ] cluuterm flipped to new spawn
- [ ] All grep zero-hit proofs pass
- [ ] All grep one-match proofs pass
- [ ] Harness reaches `compositor: ready` at every task boundary