# procmgr Cap-Model Refactor — Design Spec

**Date:** 2026-05-21
**Status:** Design — pending implementation plan
**Owner:** Balazs Valkony
**Branch (target):** `procmgr-cap-refactor` (new branch off `develop`)

## 0. Motivation

Current `procmgr` (7,618-line `main.rs`, single instance, primordial) violates CLUU's cap/view philosophy. The model: **possession of a capability *is* the authority** — no runtime identity check, no ACL re-evaluation at IPC time. If something must be inaccessible, simply do not include it in the cap-set or view.

Four concrete violations exist today (catalogued in `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_procmgr_acl_redesign.md`):

1. `handle_container_run` calls `caller_profile.can_grant(requested_profile)` at IPC time.
2. `proc_query_list` walks the session-membership ancestor chain.
3. VFS `/proc/N/stat` opens do `caller_tid → caller_pid → session_match`.
4. `resolve_caller_session` itself is a runtime identity resolver.

These create TOCTOU windows, divergent enforcement paths, and force "what can X do?" audits to run code instead of read static envelopes/views. They also block the SOLID goals: today `main.rs` is a god-object holding state for spawn, sessions, pids, pipes, pgs, ctty, restart, faults, and proc queries simultaneously.

**Goal of this refactor:** make CLUU's cap/view model *structural* rather than *conventional* — possession-equals-authority enforced by the topology of the system, not by code paths that could regress.

**Drivers (ranked):**

1. **Cap-model correctness** — kill all runtime identity checks. (load-bearing)
2. **Isolation hardness** — session A crash/compromise must not touch session B. (load-bearing)
3. **Code complexity (SOLID)** — each component single-responsibility, testable in isolation.
4. **Future-proof scaling** — design must accept async/multithread later without architectural rewrite.

## 1. Architecture & Topology

Hierarchical multi-instance, Genode-`init`-style.

```
                       ┌──────────────┐
                       │     init     │  primordial; monitors exits
                       └──────┬───────┘
                              ▼
                       ┌──────────────┐
                       │ root-procmgr │  SYSTEM cap-set
                       │              │  ─ envelopes catalog (read-only static)
                       │              │  ─ session_table (Vec<SessionEntry>, by sid)
                       │              │  ─ pid allocator (high-bits stamp at session create)
                       │              │  ─ service_spawn / restart / shutdown
                       │              │  ─ proc_query_all (gated by SYSTEM cap)
                       │              │  ─ escalate / su
                       └──┬───┬───┬───┘
              ┌───────────┘   │   └────────────────┐
              ▼               ▼                    ▼
        ┌──────────┐    ┌──────────┐         ┌──────────────────┐
        │  vfs     │    │ registry │ …       │ session-procmgr  │  per session
        └──────────┘    └──────────┘         │  session-scoped cap-set:
                                             │   ─ vfs sub-cap (view-bound)
                                             │   ─ registry sub-cap
                                             │   ─ timeserver sub-cap
                                             │   ─ PID range [sid<<23 .. ]
                                             │   ─ child_table (HashMap<pid,…>)
                                             │   ─ pg_table (process groups)
                                             │   ─ pipe_registry (intra-session)
                                             │   ─ ctty for own session
                                             └─┬───┬───┬─┘
                                               ▼   ▼   ▼
                                            shell  ls  cat   (user procs)
```

### 1.1 Invariants

- **Possession = authority.** Every handler dispatches on `(cap, label)`. No identity lookup ever. No `resolve_caller_session`, `pid_to_session`, or `caller_profile.can_grant`.
- **Cap derivation is monotone.** Each step narrows authority (root → session-procmgr → child → grandchild). Enforces `feedback_vfs_view_caps_monotone` structurally.
- **Crash domain = instance.** session-procmgr crash kills exactly that session. root-procmgr crash = system reboot (primordial; init panics).
- **Spawn graph = supervision tree.** Parent holds children's caps; cascade-teardown on parent death is automatic via cap revocation.

### 1.2 Scope split

**root-procmgr (system-scope) handlers:**
- `service_spawn` (vfs, registry, timeserver, virtio-blk)
- `session_create`, `session_destroy`, `session_subscribe` (cross-session lifecycle)
- restart policies for services + session-procmgrs
- `escalate` / `su` (cross-session privilege transition)
- `proc_query_all` — SYSTEM-cap gated
- `shutdown` (Ctrl+Alt+Del, sequenced teardown)
- fault handling for own children (services + session-procmgrs)

**session-procmgr (per-session) handlers:**
- `spawn`, `container_run` (user procs within session)
- `kill` within session
- exit + fault notifications for own children
- `pipe_create`, `pipe_close` (intra-session only)
- process groups (`pg_*`, `pid_pgid_query`)
- `ctty_query`
- `proc_query` for own session
- restart policies for own user procs

### 1.3 PID layout

`pid_t` is `i32` (31 usable bits, sign reserved).

- **High 8 bits:** session_id (0–255). 256 concurrent sessions max.
- **Low 23 bits:** local pid within session (0–8,388,607).
- Globally unique by construction. No coordination per spawn.
- Session-id derivable from any PID — routes exit/fault messages without lookup.
- Reuse: session destroy releases sid; reused after recycle. Paired with **generation counter** in session caps to invalidate stale caps from the previous incarnation.

## 2. Components & Module Split (SOLID)

Three crates:

```
userspace/
├── libs/procmgr-common/         (new, shared library)
│   ├── envelopes.rs             ← move from current procmgr/
│   ├── manifest_cache.rs        ← move
│   ├── mount_policy.rs          ← move
│   ├── view_table.rs            ← move
│   ├── pid.rs                   (new: encode/decode 8|23 pid)
│   ├── labels.rs                (new: all PROCMGR_* label constants)
│   ├── wire.rs                  (new: SpawnReq, ExitNotif, etc. — IPC wire types)
│   └── handler.rs               (new: MsgHandler trait)
│
├── root-procmgr/                (renamed from current procmgr/)
│   ├── main.rs                  (~200 lines: bootstrap + recv loop + dispatcher)
│   ├── services.rs              (service_spawn, service restart)
│   ├── session_directory.rs     (session_create/destroy, sid alloc, generation counter)
│   ├── cap_broker.rs            (sub-mint vfs/registry/timeserver/virtio-blk per session)
│   ├── escalate.rs              (su / privilege transitions)
│   ├── proc_query_all.rs        (aggregate via session-procmgr queries; SYSTEM-cap gated)
│   ├── shutdown.rs              (sequenced teardown)
│   ├── init_monitor.rs          (PROC_EXIT_LABEL for own children)
│   └── restart.rs               (restart policies for services + session-procmgrs)
│
└── session-procmgr/             (new crate)
    ├── main.rs                  (~150 lines: bootstrap + recv loop + dispatcher)
    ├── spawn.rs                 (child spawn, FdInherit, sub-mint from session caps)
    ├── child_table.rs           (pid → ChildState; cookies; container_ids)
    ├── pg_table.rs              (process groups — port from current)
    ├── pipe_registry.rs         (intra-session pipes)
    ├── ctty.rs                  (controlling terminal)
    ├── proc_query_local.rs      (/proc filtered to own session)
    ├── child_monitor.rs         (PROC_EXIT_LABEL + fault + restart for own children)
    └── kill.rs                  (kill within session)
```

### 2.1 Handler dispatch trait (SOLID DIP+OCP)

```rust
// procmgr-common/handler.rs
pub trait MsgHandler {
    const LABEL: u32;
    type State;
    fn handle(state: &mut Self::State, msg: &Message, payload: &[u8]) -> Result<Reply>;
}
```

Each handler module exports one struct implementing `MsgHandler`. Dispatcher = static `label → fn pointer` table built at compile time. Adding a new IPC = add a new handler module + register. Future async migration: trait becomes `async fn`, dispatcher becomes an executor poll loop. Mechanical.

### 2.2 No god-object state

Tables are split, narrow APIs. `child_table` does not know about `pg_table`; both are pure data containers with explicit operations. Cross-table effects live in handlers, which compose multiple tables. Each handler holds `&mut` only to the tables it actually touches — testable in isolation.

### 2.3 Identity-keyed maps deleted

- `pid_to_session` — gone (PID encodes session in high bits).
- `tid_to_pid` for ACL — gone (cap presence is the authority).
- `caller_profile` checks — gone.
- `resolve_caller_session` — gone.

`tid_to_pid` survives only as a cookie store inside session-procmgr's `child_table` (needed for exit-notification lookup), never consulted for authorisation.

### 2.4 procmgr-common is a library

Both binaries link it. Single source of truth for wire types and label constants prevents drift.

## 3. Data Flow

### 3.1 Boot

```
init → spawn root-procmgr (primordial cap-set, label = PROCMGR_INIT)
root-procmgr → spawn vfs, registry, timeserver, virtio-blk
root-procmgr loads envelopes.toml (read-only static, shared with session-procmgr via FS)
root-procmgr → spawn login service (no session yet; runs under root cap-set, view = login-view)
```

### 3.2 Login → session create → shell spawn

```
login validates user, asks root-procmgr → SESSION_CREATE(user, role)
root-procmgr:
  ├ allocate sid (8-bit, with generation counter)
  ├ stamp PID base = sid << 23
  ├ mint session-scoped caps: vfs (user view), registry, timeserver, fb, kbd
  ├ build SessionEnvelope = {sid, generation, caps, view, env defaults, pid_base}
  ├ spawn session-procmgr binary with envelope as FdInherit payload
  └ return session-procmgr's spawn-endpoint to login

login → session-procmgr SPAWN(shell, fd_table, env)
session-procmgr:
  ├ alloc local pid (= 1) → global pid = sid<<23 | 1
  ├ sub-mint from session-held caps → child caps for shell
  ├ spawn shell via kernel
  └ track in child_table
```

### 3.3 User proc spawn (shell exec's `ls`)

```
shell → session-procmgr SPAWN("/bin/ls", argv, fd_table)
session-procmgr.spawn::handle:
  ├ verify shell's spawn-cap was presented (no identity lookup — cap presence = authority)
  ├ alloc local pid
  ├ sub-mint child caps (further narrowed: read-only vfs view, etc.)
  ├ spawn → kernel returns thread token
  ├ child_table.insert(pid, ChildState{thread_tok, cookie, …})
  └ reply pid
```

### 3.4 User proc exit

```
ls calls _exit(0) → crt0 sends PROC_EXIT_LABEL to its exit_endpoint
  (envelope baked exit_endpoint = session-procmgr's exit-endpoint at spawn)

session-procmgr.child_monitor:
  ├ recv on exit_endpoint
  ├ lookup cookie → pid → child_table entry
  ├ revoke child's sub-minted caps (cascade)
  ├ remove from child_table, pg_table
  └ post WAIT-resolution for any waitpid waiters
```

### 3.5 Cross-session admin query (`ps -ef` as root)

```
ps holds SYSTEM cap (root user's login session got it from root-procmgr).
ps → root-procmgr PROC_QUERY_ALL(SYSTEM_cap)
root-procmgr.proc_query_all:
  ├ verify cap_id matches SYSTEM_PROC_QUERY_CAP (capability presence = authority)
  ├ for each session in session_table:
  │    └ send PROC_QUERY_LOCAL to session-procmgr (root holds parent cap)
  │       (session-procmgr does NOT identity-check root — root's cap is its parent cap)
  ├ aggregate replies, decorate with sid
  └ reply to ps
```

### 3.6 session-procmgr crash → cascade teardown

```
session-procmgr dies (PF, OOM, panic).
Kernel forwards fault to root-procmgr (session-procmgr's fault endpoint = root).
root-procmgr.init_monitor:
  ├ receive PROC_EXIT_LABEL or fault for session-procmgr
  ├ session_directory.destroy(sid):
  │    ├ revoke session's parent caps held by session-procmgr (cascade revokes sub-mints)
  │    ├ any child user-proc syscall on revoked cap → EBADTOK (acceptable; cascade is fast)
  │    │   (or send SIGKILL via thread cap before revoke for graceful drain)
  │    └ free pid range, bump session generation, mark sid reusable
  └ notify subscribers (display manager etc.) of session loss
```

### 3.7 Service crash → restart

```
vfs (or registry, etc.) dies → exit notification to root-procmgr (parent).
root-procmgr.restart:
  ├ check restart policy from envelope (Always / OnFailure / Never)
  ├ if restart → respawn with same primordial cap-set, re-publish service cap
  ├ existing client caps to dead service are stale → clients reconnect (registry helps)
  └ if crash-loop threshold hit → escalate to init, system panic
```

## 4. Error Handling & Edge Cases

### 4.1 Cap revocation cascade timing

- Revoke is synchronous on root's request to the kernel. Kernel walks the cap tree, marks revoked, broadcasts cap-id invalidation.
- Children making in-flight IPC with a revoked cap receive `EBADTOK` synchronously; queued kernel ops short-circuit.
- Order on session destroy: (1) signal SIGKILL to child threads via thread caps (graceful chance to crt0-cleanup, ~5 ms grace), (2) revoke cap tree, (3) kernel reaps blocked threads.

### 4.2 Stale cap from recycled session id

- Generation counter (`u32`) embedded in every session-derived cap.
- Check: `cap.session_id == X && cap.generation == N`. Reuse of sid X → generation becomes N+1. Old cap presenting generation N → cap-table miss → `EBADTOK`.
- Generation lives in `SessionEntry` in root-procmgr's `session_directory`.

### 4.3 Spawn failure rollback

- Spawn path mints sub-caps → kernel ELF load → context setup → thread start. Failure mid-way must revoke every cap minted in this attempt.
- Pattern: `MintGuard` RAII struct, holds minted cap-ids, drops → revoke on early return. Single happy-path `mem::forget(guard)` after successful start.
- Prevents cap leak (cap pointing to a half-built process).

### 4.4 IPC errors

- `BufferTooSmall`: dispatcher logs label + sender, drops message. Existing 4 KiB buffer matches established limits.
- `NoSender` on reply: caller died mid-call. Drop reply, free reply slot, continue.
- `Timeout` (set to soonest-pending-timer): unchanged from current; timers tied to restart policy and waitpid expiry.

### 4.5 PID exhaustion in session

- Local 23-bit space = 8M procs. Hitting it = bug or DoS. Policy: reject SPAWN with `EAGAIN`. No reuse within a session generation — pids monotonic. Reset on next session.

### 4.6 Restart loop

- Each restart entry carries `{attempts, first_attempt_ts}`. Threshold: 5 restarts in 30 s. Exceeded → restart policy forced to Never, log fatal.
- session-procmgr crash-loop: root-procmgr destroys the session entirely (cascade), notifies display manager. User logs in fresh.
- Service crash-loop (vfs etc.): root-procmgr panics → init notices → init panics → kernel halt. By design — system unusable without vfs.

### 4.7 OOM in session-procmgr

- Per-instance heap from libcluu allocator. OOM → panic (cleanest). root cascade-teardown takes over. Session lost, rest of system fine.

### 4.8 Bootstrap failure

- root-procmgr fails to spawn vfs/registry/etc. → panic → init halts the system.
- session-procmgr fails to bootstrap (malformed envelope, missing caps) → exit nonzero. `root.init_monitor` catches → reports back to login → login displays error, no session created. System keeps running.

### 4.9 Cross-session race (admin query during session destroy)

- ps queries root → root iterates sessions → during iteration session N is destroyed.
- `session_directory` uses RwLock-style snapshot: iteration takes `Vec<sid>` snapshot, then queries each. If a session-procmgr is dead → query fails → root marks slot as `<sid N: gone>` in reply.
- No partial corruption: each session's reply is atomic.

### 4.10 Stale FDs in pipe_registry

- session-procmgr-backed pipes die with session-procmgr (cap revocation). No cross-session pipes by design. No leak.

### 4.11 Login bypass during refactor

- `project_spawn_hooks_unwired` notes current login uses a spawn bypass. This refactor removes the bypass — every spawn flows through proper cap-derive in session-procmgr. Bypass code deleted as part of this work.

## 5. Testing Strategy

**No reliance on the legacy harness `l2_*` markers as a migration gate.** The harness carries many tests with pre-refactor assumptions. The refactor introduces a **fresh procmgr-specific test suite** and uses *only that* as the bar.

### 5.1 Coverage targets

- **C1 (statement + branch): ≥ 95 % per crate.** Measured with `cargo llvm-cov --branch`. CI gate: build fails if any crate drops below 95 %.
- **C2 (path coverage): ≥ 90 % on critical handlers; 100 % on cap-mint paths.** Critical = `spawn`, `cap_broker::sub_mint`, `session_directory::create`/`destroy`, `child_monitor::on_exit`, `kill`, `pg_*`, `proc_query_all`.

### 5.2 Per-handler unit test discipline

- For each `handle()` function, enumerate every branch in PR review (`#[doc]` checklist). One named test per branch, e.g.: `spawn_handle__bad_cap_returns_ebadtok`, `spawn_handle__elf_load_fail_revokes_minted_caps`, `spawn_handle__pid_exhausted_returns_eagain`, `spawn_handle__success_path`.
- Boundary tests: edge of every range (sid 0/255, local pid 0/0x7FFFFF, generation 0/u32::MAX).
- State-transition matrix: for stateful tables (`session_directory`, `child_table`, `pg_table`) every (state, op) pair has a test.

### 5.3 Property tests (proptest)

- `cap_broker::sub_mint`: invariant — child cap rights ⊆ parent cap rights (monotone). 10 K random parent caps.
- `pid::encode/decode`: roundtrip on full range.
- `session_directory`: any sequence of create/destroy preserves uniqueness + generation monotonicity.
- `restart`: any timeline of exits never exceeds crash-loop threshold without triggering escalation.

### 5.4 Mock kernel surface

- `procmgr-common/test_kernel.rs`: trait wrapping `syscall::ipc_recv_any`, `cap::mint`, `cap::revoke`, `thread::spawn`. Production uses real syscalls; tests use a mock recording all calls. Unit tests verify the exact cap-derivation sequence without booting QEMU.

### 5.5 Cap-purity lint (compile-time + grep gate)

- `xtask check-cap-purity` greps `root-procmgr/` and `session-procmgr/` for forbidden patterns:
  - `pid_to_session`, `tid_to_pid` (for ACL), `resolve_caller_session`, `caller_profile`, `can_grant`, `session_match`.
- Pre-commit hook + CI step. Build fails on hits.
- Each handler doc-comment declares: "Requires caps: X, Y. No identity check." Reviewed in PR template.

### 5.6 Integration tests (new, procmgr-specific)

QEMU boot harness wrapper `scripts/harness_procmgr.sh`. Markers prefixed `pm_*` (distinct from legacy `l2_*`):

- `pm_bootstrap_two_pmgr` — boot, verify root-procmgr running, zero session-procmgrs until login, exactly one after login, zero after logout.
- `pm_session_crash_cascade` — spawn session, force-kill session-procmgr, verify all children gone, caps invalid, sid generation bumped, sid reusable.
- `pm_cap_revoke_stale` — revoke session, attempt syscall with old child cap, expect `EBADTOK`.
- `pm_session_id_recycle` — destroy + recreate session with same sid; old caps must fail (generation gate).
- `pm_cross_session_no_leak` — session A holds vfs cap; verify session B's procmgr cannot see/touch session A's files even via crafted IPC.
- `pm_proc_query_all_cap` — proc holding SYSTEM cap sees all sessions; proc without cap gets `EBADTOK`; multi-session boot shows correct aggregation.
- `pm_pid_layout` — spawn 100 children in session 5, verify all PIDs in `[0x2800001 .. 0x2800064]`. Spawn in session 7, verify range distinct.
- `pm_service_restart` — kill vfs once, verify root respawns; kill 6× in 30 s, verify root panics → init halts.

### 5.7 Coverage tooling

- `cargo llvm-cov` for C1; HTML report uploaded as CI artifact.
- C2 tracked in `docs/superpowers/specs/PROCMGR_CAP_REFACTOR_COVERAGE.md` (handler ↔ branches ↔ test-names matrix).
- New `xtask coverage-check` runs `cargo llvm-cov`, parses summary, fails if thresholds not met.

### 5.8 Mutation testing (stretch)

- `cargo mutants` over critical modules. Goal: ≥ 80 % mutants caught. Reveals tests that pass without exercising logic. Stretch, not v1 blocker.

### 5.9 Performance ratchet

- `b_spawn_warm` (per `feedback_spawn_perf_baseline`) re-run pre/post. Acceptable regression budget: +15 % (extra RPC per spawn for sub-mint). Beyond that → investigate.

## 6. Migration Strategy

**Branch big-bang.** New branch `procmgr-cap-refactor` off `develop`. Forked code, build root + session in parallel, rewire init bootstrap, swap.

- Userspace-only — does not violate the kernel freeze (active through ~2026-10-21).
- 40-day WIP rule still applies. Mitigation: structure the branch as a sequence of self-contained commits (skeleton → procmgr-common extraction → root-procmgr handlers → session-procmgr handlers → bootstrap rewire → bypass deletion → coverage). The implementation plan (next document) breaks this into checkpoint-sized phases.
- Final merge to `develop` is a single fast-forward once the `pm_*` suite passes the coverage gates.

**No coexistence period.** Legacy procmgr is deleted on merge, not flagged. New is canonical from day one.

## 7. Out of Scope (v1)

- Async/await runtime — deferred to a separate phase after this lands. Modules are pre-shaped for the migration (each `handle()` becomes `async fn`; dispatcher becomes an executor).
- Cross-session pipes — by design, never supported. If a use case appears, it goes through root-procmgr as a separate ticket.
- Fuzzing `cap_broker` — nice-to-have, not blocking.
- Formal verification of cap derivation — over hobby scope.
- `cargo mutants` 80 % gate — stretch, not blocker.
- Display-friendly PID form (`sid/local`) — cosmetic, defer.

## 8. Open Questions

None at design time. The next document (implementation plan) will enumerate per-phase work units, dependencies, and acceptance criteria for each phase.

## 9. References

- Memory: `project_procmgr_acl_redesign.md` — concrete ACL violations and intended redesign direction.
- Memory: `project_spawn_cap_composable.md` — capability-scoped binaries compose; child can sub-mint further.
- Memory: `feedback_vfs_view_caps_monotone.md` — child views/caps never broader than parent.
- Memory: `project_spawn_hooks_unwired.md` — current login bypass deleted by this refactor.
- Memory: `feedback_procmgr_stateless.md` — keep state inline on spawn where feasible.
- Memory: `feedback_no_timeouts.md` — replace time-bounded recv with cap-revocation unblock.
- Genode `init` hierarchy — architectural precedent for hierarchical multi-instance.
- seL4 capability model — possession-equals-authority principle.
