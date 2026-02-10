# CLUU Microkernel Audit (Strict)

**Date**: 2026-02-10  
**Scope**: kernel + core services + POSIX compatibility layer  
**Goal evaluated**: seL4-inspired safe microkernel with practical POSIX compatibility for userspace development.

---

## Executive Verdict

CLUU has strong architectural direction (small syscall ABI, capability-like API, clean service decomposition), but it is **not yet safe enough or complete enough** to be considered a robust microkernel base for untrusted userspace workloads.

The most serious blocker is a **capability forgery/exfiltration class issue**: token handles are global, predictable integers with no per-process namespace. That undermines the intended capability security model.

For trusted development and bring-up, CLUU is already useful. For "fairly usable" daily userspace development with a potent shell and reliable long-running behavior, several kernel/service gaps must be closed first.

---

## Scorecard (Current)

| Dimension | Rating | Rationale |
|---|---:|---|
| Correctness | **D+** | Multiple high-impact correctness/security issues in token authority and lifecycle handling. |
| Speed | **B-** | Fast structure choices exist, but IPC/capability paths add avoidable overhead and some lock contention. |
| Memory efficiency | **C-** | Per-message 4KB buffering, leak-prone map failure paths, and missing full process/space teardown. |
| Future readiness | **C** | Good modular structure and extensibility, but missing foundational security and lifecycle primitives. |

---

## Bleeding Issues (Priority Ordered)

## P0: Global token handle model is forgeable in practice

**Why this matters**: This breaks the core safety story of capability-based authorization.

Evidence:
- Token handles are plain `usize` values (`kernel/src/token/mod.rs:86`).
- Handles are allocated via global monotonic counter (`kernel/src/token/table.rs:129`, `kernel/src/token/table.rs:196`).
- Lookup is global and not bound to calling process/space (`kernel/src/token/table.rs:223`).
- Init root token is created with full rights (`kernel/src/bootstrap.rs:131`).

Impact:
- Any process that can guess/live-scan valid handles can potentially access authority not delegated to it.
- Security properties expected from seL4-like capabilities do not hold.

Minimum fix direction:
- Move to per-process capability spaces (CNode/CSpace-style), or at least unguessable high-entropy capability IDs bound to an owner namespace.
- Ensure kernel lookup requires both capability ID and owning CSpace context.

Medium fix direction:
- Add explicit capability transfer primitives (mint/move/revoke) with destination-slot semantics, instead of implicit global handle sharing.
- Add capability badges/labels for service-facing authorization and tracing.
- Add capability-space introspection/debug tooling (dump, leak detector, orphan scan) for bring-up and regression triage.

## P0: Process teardown is incomplete; long-run stability is at risk

Evidence:
- `ThreadDestroy` only marks thread dead (`kernel/src/syscall/handlers.rs:597`, `kernel/src/syscall/handlers.rs:614`), no full resource reclamation path.
- `SpaceDestroy` is unimplemented (`kernel/src/syscall/handlers.rs:742`).
- procmgr stores/reaps thread token only (`userspace/procmgr/src/main.rs:71`, `userspace/procmgr/src/main.rs:189`, `userspace/procmgr/src/main.rs:210`), not full address-space lifecycle.

Impact:
- Repeated spawn/exit can leak address-space structures and related mappings.
- Shell-driven iterative development can degrade over time.

Minimum fix direction:
- Implement deterministic teardown pipeline driven by userspace `procmgr`: thread(s) -> endpoint/token revocation -> space unmap/destroy -> page table/frame reclamation.

Medium fix direction:
- Add a userspace-owned lifecycle state machine in `procmgr` (Spawning -> Running -> Exiting -> Reaped), while kernel keeps strict thread/space invariants only.
- Add kernel accounting per thread/space (frames, mappings, endpoints, tokens) plus procmgr-side per-process aggregation and hard cleanup assertions at teardown boundaries.
- Add spawn/exit churn stress harnesses and leak checks in CI (frame counts, token counts, space/thread repository sizes).

## P1: `sys_recv` multi-endpoint registration has race/semantic hazard

Evidence:
- First nonblocking probe is done (`kernel/src/syscall/handlers.rs:142`).
- Then kernel calls `recv_to_user` on each endpoint to register waiters, but ignores successful receives (`kernel/src/syscall/handlers.rs:168`, `kernel/src/syscall/handlers.rs:171`).
- It then blocks and later returns `WouldBlock` for retry (`kernel/src/syscall/handlers.rs:195`), while userspace loops on `WouldBlock` (`userspace/libcluu/src/syscall.rs:322`).

Impact:
- Message can be consumed/copied during registration pass but length/index signal discarded, causing retries, latent stalls, or confusing behavior under timing races.

Minimum fix direction:
- If any registration pass call returns `Ok(len)`, return success immediately with endpoint index.
- Use explicit waiter-registration API instead of "receive-as-register" side effect.

Medium fix direction:
- Introduce a first-class waitset/wait-queue abstraction to avoid N-endpoint linear probing in hot paths.
- Add fairness policy for multi-endpoint wake arbitration to prevent endpoint starvation.
- Add tracing counters for missed wakes, retries, and timeout wake reasons to catch regressions early.

## P1: Unsafe user-memory copy patterns in space map paths

Evidence:
- `invoke_space_map` validates pointer range then does raw `copy_nonoverlapping` from user virtual address (`kernel/src/syscall/handlers.rs:829`, `kernel/src/syscall/handlers.rs:831`).
- Similar pattern in range mapping helper (`kernel/src/syscall/handlers.rs:1270`).
- Safe page-table-root aware copy helper exists (`kernel/src/syscall/userptr.rs:145`) but is not used there.

Impact:
- Fragile behavior under stricter isolation or mapping edge cases.
- Potential faulting/correctness issues not surfaced as structured syscall errors.

Minimum fix direction:
- Use `copy_from_user(..., page_table_root)` consistently for all user buffers.
- Validate mapped/user-accessible pages before copying.

Medium fix direction:
- Consolidate all user-pointer handling behind a single hardened copy API and ban raw user `copy_nonoverlapping` in syscall paths.
- Add fault-injection tests for partially mapped and boundary-crossing user buffers.
- Add per-syscall copy metrics (bytes, faults, retries) to guide optimization work.

## P1: Mapping failure paths leak resources / counts

Evidence:
- `space_map` allocates frame and may fail mapping without rollback/free (`kernel/src/syscall/handlers.rs:818`, `kernel/src/syscall/handlers.rs:852`).
- Frame token map count incremented before mapping (`kernel/src/syscall/handlers.rs:804`) and not decremented on failure path.
- Range helpers allocate frames and return on error without cleanup of current/previous allocations (`kernel/src/syscall/handlers.rs:1261`, `kernel/src/syscall/handlers.rs:1300`, `kernel/src/syscall/handlers.rs:1334`, `kernel/src/syscall/handlers.rs:1353`).

Impact:
- Physical memory leaks and stale map counts under partial failures.

Minimum fix direction:
- Add transactional map semantics or explicit rollback list for frames mapped in current syscall.

Medium fix direction:
- Add two-phase map commit (prepare/commit) for batch operations so partial failures cannot leak resources.
- Add frame map-count auditing with periodic consistency checks against actual page tables.
- Add targeted chaos tests that force allocation/map failures at every step.

## P2: `TokenRevoke` syscall path is semantically incorrect

Evidence:
- Dispatch passes current token handle (`kernel/src/syscall/handlers.rs:486`).
- Handler revokes that same handle, ignores args/target and rights checks (`kernel/src/syscall/handlers.rs:1427`).

Impact:
- Operation does not match expected API intent; allows accidental self-revocation and confuses security contracts.

Minimum fix direction:
- Define explicit target-handle argument and require suitable right (e.g., `DESTROY` or dedicated revoke authority).

Medium fix direction:
- Add hierarchical revocation semantics (object-wide revoke vs single-cap revoke) with deterministic behavior.
- Add audit logging for destructive token ops (derive/revoke/free) with caller identity.
- Add negative tests for self-revocation, cross-object revocation, and stale-handle revocation races.

---

## Token System Audit (Safety, Speed, Correctness)

### What is strong

- Rights are explicit bitmasks with subset derivation logic (`kernel/src/token/rights.rs:33`, `kernel/src/token/mod.rs:298`).
- Derivation prevents right escalation and expiry extension (`kernel/src/token/mod.rs:305`).
- Signature uses HMAC-SHA256 with constant-time compare (`kernel/src/token/mod.rs:264`, `kernel/src/token/signature.rs`).
- Revocation generation + per-thread cache exists (`kernel/src/token/table.rs:421`, `kernel/src/token/table.rs:224`).

### Safety weaknesses

- Core authority is represented by **guessable global handle IDs**, not process-local capability slots.
- No kernel-level proof of "caller owns this token namespace."
- Expiry is mostly disabled in real use (`Timestamp::far_future()/NEVER`) (`kernel/src/token/mod.rs:129`, `kernel/src/syscall/handlers.rs:1863`, `kernel/src/bootstrap.rs:135`).

### Correctness weaknesses

- Scope->object map entries are not cleaned on token remove (`kernel/src/token/table.rs:88`), while scope resolution scans all shards (`kernel/src/token/table.rs:357`).
- Time source for expiry uses raw TSC with explicit TODO (`kernel/src/token/table.rs:504`), not a calibrated monotonic nanosecond clock.

### Speed profile

- Lookup involves expensive signature verification on cache miss (`kernel/src/token/table.rs:247`).
- Cache is single-entry per thread (`kernel/src/sched/thread.rs:196`), which can thrash in multi-token hot paths.

---

## IPC Audit (Safety, Speed, Correctness)

### What is strong

- Endpoint repository is sharded (`kernel/src/ipc/endpoint.rs:265`) and supports backpressure queues.
- Call/reply path supports one-time reply token injection/extraction (`kernel/src/ipc/endpoint.rs:538`, `kernel/src/ipc/endpoint.rs:688`).

### Correctness and safety gaps

- IPC implementation is queue-based (`kernel/src/ipc/endpoint.rs:78`), while rendezvous module exists but is not active path (`kernel/src/ipc/rendezvous.rs:90`).
- No strong sender identity/badging for regular messages; many services rely on caller-supplied fields (e.g., VFS `client_id`) (`userspace/vfs/src/main.rs:424`).
- API/doc mismatch around reply token naming in userspace wrapper (`userspace/libcluu/src/syscall.rs:423`, `userspace/libcluu/src/ipc.rs:182`).

### Speed and memory profile

- `EndpointMessage` stores fixed 4KB array per message (`kernel/src/ipc/endpoint.rs:14`, `kernel/src/ipc/endpoint.rs:17`).
- With queue caps (`1024` + `256` calls), peak endpoint memory pressure can be large (`kernel/src/ipc/endpoint.rs:93`).
- Multiple copy steps in common send/recv path add latency for small messages.

---

## POSIX Compatibility vs Production POSIX Kernels

## Current practical status

### File and directory operations

Working basics:
- `open/read/write/lseek/fstat/stat/isatty` via VFS client path (`userspace/libcluu/src/posix/file.rs:41`, `userspace/libcluu/src/posix/stat.rs:128`).
- `opendir/readdir/getcwd/chdir` present.
- `posix_spawn` + `waitpid` path is implemented and is the intended CLUU process model (`userspace/libcluu/src/posix/process.rs:266`, `userspace/libcluu/src/posix/process.rs:167`).

Major gaps:
- `_fork`/`_execve` are ENOSYS by design in the current spawn-first model (`userspace/libcluu/src/posix/process.rs:122`, `userspace/libcluu/src/posix/process.rs:134`); this is mainly a POSIX-portability gap, not a CLUU architectural blocker.
- `unlink/mkdir/rmdir/rename` are ENOSYS (`userspace/libcluu/src/posix/file.rs:521`, `userspace/libcluu/src/posix/file.rs:528`, `userspace/libcluu/src/posix/file.rs:535`, `userspace/libcluu/src/posix/file.rs:542`).
- VFS write support excludes `Memory`/`Ext2` in main write path (`userspace/vfs/src/main.rs:503`).
- Filesystem trait lacks write/create/remove methods (`userspace/libcluu/src/fs/traits.rs:63`).

### User model and privileges

- No UID/GID credential model in syscall/policy enforcement.
- `stat` exposes uid/gid fields but defaults to root-like values (`userspace/libcluu/src/posix/stat.rs:75`).
- No permission enforcement path comparable to Linux/FreeBSD DAC/ACL behavior.

### Signals and process control

- `_kill` is procmgr IPC request, not Unix signal delivery semantics (`userspace/libcluu/src/posix/process.rs:81`).
- No `signal/sigaction` semantics in libc layer.
- `waitpid` exists, but the full POSIX signal/job-control environment is absent.

### Memory mapping

- `mmap` implemented as bounded bump allocator region (`userspace/libcluu/src/posix/memory.rs:28`, `userspace/libcluu/src/posix/memory.rs:179`).
- `mprotect` stubbed as success/no-op (`userspace/libcluu/src/posix/memory.rs:261`).

---

## Shell Readiness Assessment

Shell is useful for bring-up and simple workflows, but not yet "potent" in POSIX sense.

Evidence:
- Builtins and spawn path exist (`userspace/shell/src/commands.rs:165`).
- External process spawn is available through procmgr.
- Command execution currently rejects pipelines/redirections in builtin executor path (`userspace/shell/src/commands.rs:276`).

Missing for potent daily shell development:
- Pipelines/redirection/job control/signals.
- Reliable long-run process lifecycle cleanup (kernel + procmgr).
- Writable persistent FS semantics.

---

## Medium-Level Upgrade Map (All Listed Issues)

This matrix gives a medium-level upgrade path for every issue listed in this document.  
For the P0/P1/P2 bleeding issues, it mirrors and consolidates the "Medium fix direction" entries above.

| Listed issue | Medium-level upgrade/fix |
|---|---|
| Global token handle forgeability risk | Move to per-space capability slots + explicit cross-space cap transfer protocol + capability audit tooling. |
| Incomplete teardown path (`SpaceDestroy`/reclaim) | Add userspace-driven teardown state machine in `procmgr` with kernel thread/space invariant checks and leak assertions. |
| `sys_recv` registration race | Introduce waitset abstraction and fair wake policy with retry/wake instrumentation. |
| Unsafe raw user copies in map paths | Enforce one hardened user-copy API and add failpoint/fault-injection coverage. |
| Map failure leaks/count drift | Two-phase map commit + rollback invariants + map-count auditing against page tables. |
| `TokenRevoke` semantic mismatch | Hierarchical revoke model (single-cap/object-wide) + destructive-op audit events + negative tests. |
| No caller-ownership proof for token namespace | Bind capability lookup to `(space, slot)` and remove global-handle semantics. |
| Token expiry mostly disabled | Use phase-bounded and/or short-lived operational caps; add explicit post-boot revocation sweep. |
| Scope->object mappings can become stale | Add mapping garbage-collection/cleanup on last-cap removal and scope consistency checker. |
| Expiry timing uses raw TSC TODO | Switch to calibrated monotonic clock source for token expiry decisions. |
| Signature verification cost on cache miss | Add small multi-entry per-thread cache and fast-path instrumentation before changing crypto policy. |
| Single-entry token cache thrash | Upgrade to tiny LRU/N-way token cache per thread; gate by measured hit-rate improvements. |
| Queue-based IPC diverges from rendezvous design intent | Keep queue model but add bounded waitset/fairness guarantees and clear semantic contract docs/tests. |
| Sender identity is weak for regular messages | Add kernel-authenticated sender metadata/badges to receive path and consume it in core services. |
| Reply-token API naming mismatch in userspace | Rename wrappers/docs to `reply_token` semantics and add compatibility shim tests. |
| Fixed 4KB message backing per queue entry | Introduce variable-size/slab-backed message storage with caps for large payloads. |
| High endpoint queue memory pressure | Add per-endpoint memory budgets and backpressure policy metrics. |
| Multiple copy steps increase latency | Add zero-copy/grant fast paths for common payload classes and benchmark gating. |
| `_fork`/`_execve` ENOSYS portability gap | Provide compatibility adapters/documented porting profile around `posix_spawn` model. |
| `unlink/mkdir/rmdir/rename` ENOSYS | Implement minimal mutable filesystem API path in VFS + backend plugin support. |
| VFS write excludes `Memory`/`Ext2` | Add coherent write path for at least one persistent backend and explicit write capability checks. |
| Filesystem trait lacks mutating operations | Extend FS trait with create/remove/rename/write/truncate primitives and versioned backend migration plan. |
| No UID/GID credential model | Add userspace credential object managed by `procmgr`, validated via kernel-authenticated sender identity. |
| `stat` uid/gid always root-like | Fill uid/gid from credential model and enforce consistency across VFS/procmgr paths. |
| No permission enforcement | Add DAC-style checks in VFS path (owner/group/mode) with deny-by-default policy toggles. |
| `_kill` is not Unix-like signal delivery | Add minimal signal delivery contract (`SIGTERM`, `SIGKILL`, `SIGINT`, `SIGCHLD`) over process-manager policy. |
| Missing `signal`/`sigaction` semantics | Implement libc-facing signal registration/shim and map to CLUU event delivery model. |
| No full job-control behavior | Add process group/session model in shell/procmgr userspace layer first. |
| `mmap` is bounded bump allocator | Replace with region allocator supporting free-list reuse and fragmentation accounting. |
| `mprotect` no-op | Add page-permission update path and capability checks, with partial-range tests. |
| Shell lacks pipelines/redirection/job control | Implement parser/executor pipeline graph + fd redirection + foreground/background control in userspace. |
| Shell/runtime lifecycle reliability concerns | Add churn harness and teardown assertions (thread/space/token/frame deltas must return to baseline). |
| Missing writable persistent FS basis for shell/userspace | Prioritize one stable writable backend and integrate smoke tests (`create/write/read/rename/delete`). |

---

## seL4 Comparison (Where It Makes Sense)

| Area | CLUU current | seL4 baseline |
|---|---|---|
| Capability isolation | Global handle table, predictable IDs | Per-address-space CSpace, unforgeable capability derivation path |
| Capability transfer model | Handle passing is ad hoc | Explicit kernel-mediated cap transfer/badging |
| IPC semantics | Queue-based endpoint + copy buffers | Synchronous IPC fastpath, well-defined endpoint semantics |
| Assurance | No formal proof | Formal verification for core properties |
| Lifecycle rigor | Partial; key destroy paths missing | Mature object lifecycle/authority model |

Conclusion: CLUU is seL4-inspired in interface style and minimization, but not yet equivalent in authority isolation rigor.

---

## Hierarchical Upgrade Plan

This section is the execution source of truth for upgrades.

### Scope constraints

1. Kernel remains thread/space-centric; no kernel-owned process object model is introduced.
2. Process discipline stays in userspace (`procmgr` + services).
3. Keep syscall surface minimal (extend existing contracts where feasible).

### Level 0: Safety Baseline (blocking)

#### 0A. Capability namespace and bootstrap trust

1. Replace global token-handle authority with per-space capability slot lookup.
2. Use split namespace model: kernel enforces per-space caps; `procmgr` owns per-process policy namespace.
3. Remove init godmode bootstrap:
`kernel` root authority stays internal; init receives scoped caps only.
4. Introduce bootstrap broker endpoint for privileged actions with strict policy checks.
5. Add signed initrd manifest validation (service hash + allowed rights + device-cap constraints) with fail-closed boot.
6. Add deterministic boot grant/revoke audit trail.

#### 0B. Lifecycle and memory integrity

1. Implement deterministic teardown pipeline driven by `procmgr`:
thread(s) -> endpoint/token revoke -> space unmap/destroy -> frame/page-table reclaim.
2. Implement `SpaceDestroy` plus invariant checks for zero leaked thread/space/frame/token deltas.
3. Harden mapping/copy paths:
transactional rollback, no raw user copies, failpoint coverage.

#### 0C. IPC correctness and identity

1. Fix `sys_recv` semantics (no consume-without-return behavior).
2. Add waitset-based multi-endpoint waiting and fair wake policy.
3. Add authenticated sender metadata/badges and remove trust in caller-supplied IDs (e.g. VFS `client_id`).
4. Fix reply-token API naming/contract mismatch in userspace wrappers.

### Level 1: Medium hardening (reliability/operability)

#### 1A. Observability and diagnostics

1. Add structured token lifecycle audit events (derive/revoke/free).
2. Add leak diagnostics report (thread/space/token/frame counts) and scope consistency checks.
3. Add telemetry for queue depth, retries, wake reasons, timeout behavior.

#### 1B. Fault-injection and CI hard gates

1. Add failpoints in mapping/copy/allocation paths.
2. Assert rollback/accounting invariants after each forced failure.
3. Gate CI on churn + failpoint suites (QEMU headless).

#### 1C. Performance and fairness

1. Add fairness harness for mixed workloads (interactive shell + background I/O + timers).
2. Add P95/P99 latency and retry SLO thresholds.
3. Improve hot paths (token cache policy, IPC buffering strategy) only when telemetry justifies.

### Level 2: POSIX and shell practicality

#### 2A. Filesystem mutability and permissions

1. Implement mutable VFS operations (`unlink/mkdir/rmdir/rename`) on at least one persistent backend.
2. Extend FS trait with mutating primitives (create/remove/rename/write/truncate).
3. Add DAC-style permission checks and credential propagation (`uid/gid` consistency).

#### 2B. Signals and process UX

1. Add minimal signal contract (`SIGINT`, `SIGTERM`, `SIGKILL`, `SIGCHLD`) in spawn-first model.
2. Provide libc-facing `signal/sigaction` compatibility layer.
3. Add shell process-group/session and job-control behavior.

#### 2C. Virtual memory ergonomics

1. Replace bump-only `mmap` with reusable region allocator.
2. Implement real `mprotect` semantics with permission-update tests.

### Milestone ladder (execution order)

| Milestone | Target level | Primary outcome |
|---|---|---|
| M0 (Week 1) | Level 0/1 prep | Baseline telemetry + boot grant trace + initrd manifest schema |
| M1 (Weeks 2-3) | Level 0A/0C | Waitset receive path + manifest verifier + fail-closed boot gate |
| M2 (Weeks 4-5) | Level 1A | Capability observability + leak diagnostics |
| M3 (Weeks 6-7) | Level 0B/1B | Fault-injection + rollback invariants + CI failpoint matrix |
| M4 (Weeks 8-9) | Level 0C | Sender identity hardening across core services |
| M5 (Weeks 10-11) | Level 1C | Fairness/SLO validation under mixed load |
| M6 (Week 12) | Level 1 closeout | Stabilization, documentation, and release readiness review |

### Implementation Plan (active tracker)

This is the execution-oriented plan (what to code, where, and how to validate).

| Work package | Milestone | Status | Code touchpoints | Validation gate |
|---|---|---|---|---|
| WP-M0.1 Telemetry baseline (token/ipc/boot grant counters) | M0 | DONE | `kernel/src/telemetry.rs`, `kernel/src/token/table.rs`, `kernel/src/syscall/handlers.rs`, `kernel/src/bootstrap.rs`, `kernel/src/main.rs` | `cargo check -p cluu-kernel` + boot log contains telemetry snapshot |
| WP-M0.2 Initrd manifest schema + parser stub | M0 | DONE (permissive mode) | `userspace/init`, `userspace/libcluu` (manifest type/parser) | malformed manifest vectors rejected by strict parser when manifest exists; boot still allows missing manifest |
| WP-M1.1 Fail-closed manifest verify gate | M1 | DONE (hash+policy+signature gate) | `userspace/init` (mandatory manifest + hash/rights enforcement), `userspace/libcluu` (signature-required parser), `xtask` (signed manifest generation), `kernel/src/bootstrap.rs` (`sys/init` hash+signature verification) | boot fails if manifest missing/invalid/mismatched/tampered |
| WP-M1.2 Waitset receive syscall path | M1 | DONE (fair scan + armed waiter + churn harness; retry contract preserved) | `kernel/src/syscall/handlers.rs`, `kernel/src/ipc/*`, `kernel/src/sched/*`, userspace recv wrappers, `test_hello.sh` | `MARKER_MODE=m1_recv TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh` passes (full rebuild) |
| WP-M2.1 Token lifecycle structured audit stream | M2 | DONE (bounded ring + monotonic sequence + create/derive/revoke hooks + harness assertions) | `kernel/src/token/*`, `kernel/src/telemetry.rs`, `test_hello.sh` | `MARKER_MODE=m2_token_audit TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh` passes (full rebuild), `token_audit_dropped=0` |
| WP-M2.2 Leak diagnostics and delta accounting | M2 | DONE (live resource counters + baseline delta logs + harness mode + optional delta thresholds) | `kernel/src/mm/*`, `kernel/src/sched/*`, `kernel/src/ipc/*`, `kernel/src/telemetry.rs`, `kernel/src/syscall/handlers.rs`, `test_hello.sh` | `MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh` emits resource-delta samples during churn; optional `MAX_DELTA_*` env vars can enforce limits |
| WP-M3.1 Mapping/copy failpoints + rollback checks | M3 | DONE (space_map rollback hardening + injectable map-range failpoint + harnessed shell self-test) | `kernel/src/syscall/handlers.rs`, `kernel/src/syscall/userptr.rs`, `userspace/shell/src/commands.rs`, `test_hello.sh` | `CLUU_SHELL_AUTOSTART_CMD='mapfail 12 4' MARKER_MODE=m3_mapfail TEST_COMMAND='' TEST_COMMAND_REPEAT=1 ./test_hello.sh` passes |
| WP-M3.2 CI churn + leak detection harness | M3 | DONE (matrix script + xtask entrypoint + make target over harness marker modes) | `scripts/harness_matrix.sh`, `xtask/src/main.rs`, `Makefile`, `test_hello.sh` | `cargo xtask harness-matrix` (or `--no-build`) runs `m1_recv`/`m2_token_audit`/`m2_leakdiag`/`m3_mapfail` and hard-fails on regressions |
| WP-M3.4 Leak SLO threshold enforcement in matrix | M3 | DONE (matrix now enforces conservative `MAX_DELTA_*` bounds for `m2_leakdiag`) | `scripts/harness_matrix.sh` | `MAX_DELTA_SPACES=14 MAX_DELTA_TOKENS=180 MAX_DELTA_ENDPOINTS=80 MAX_DELTA_PMM_USED_FRAMES=46000 MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh --no-build` passes; latest run observed `delta_spaces=13`, `delta_tokens=174`, `delta_endpoints=73`, `delta_pmm_used_frames=45374` |
| WP-M3.3 Copy/map error rollback matrix coverage | M3 | DONE (shell self-tests for `copy_from_user` and `map_user_page` error branches + harness marker modes + matrix integration) | `userspace/shell/src/commands.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `cargo xtask harness-matrix` runs `m3_mapcopyfail` + `m3_maperror` and fails on any `FAIL` marker |
| WP-M4.1 Sender identity/badge hardening | M4 | DONE (kernel `sys_recv` sender metadata + VFS authenticated caller binding + procmgr PID-owner enforcement for `PROC_KILL` + procmgr reply-token + sender-auth notify routing for spawn replies) | `kernel/src/syscall/handlers.rs`, `kernel/src/ipc/endpoint.rs`, `userspace/libcluu/src/syscall.rs`, `userspace/vfs/src/main.rs`, `userspace/procmgr/src/main.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `cargo xtask harness-matrix` passes including `m4_sender_auth`; VFS logs `ignoring claimed client_id=... authenticated=...` during shell spawn churn |
| WP-M4.2 Registry sender-bound control endpoint + ownership checks | M4 | DONE (registry subscribes/registers now consume authenticated sender metadata, bind sender->control endpoint, and enforce producer ownership on register/unregister) | `userspace/registry/src/main.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `cargo xtask harness-matrix` passes including `m4_registry_sender_auth`; registry logs include authenticated sender ids for subscribe flows |
| WP-M4.3 Procmgr notify binding lifecycle cleanup | M4 | DONE (procmgr now tracks sender active-child counts and clears sender notify bindings when last owned child is reaped/killed) | `userspace/procmgr/src/main.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `cargo xtask harness-matrix` passes including `m4_notify_lifecycle`; logs contain `procmgr: cleared sender notify binding sender_tid=` |
| WP-M4.4 Sender-auth denial-path regression coverage | M4 | DONE (shell builtin + harness marker mode now forces shell autostart command and asserts `PermissionDenied` path for unauthorized procmgr `PROC_KILL`) | `userspace/shell/src/commands.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `MARKER_MODE=m4_deny_paths ./test_hello.sh` passes (full build); logs contain `killdeny: PASS permission denied` and `procmgr: deny kill pid` |
| WP-M4.5 Registry ownership denial-path regression coverage | M4 | DONE (shell builtin + deterministic autostart marker mode now assert `PermissionDenied` for unauthorized registry `unregister`, with explicit deny logs) | `userspace/shell/src/commands.rs`, `userspace/registry/src/main.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `MARKER_MODE=m4_registry_deny_paths ./test_hello.sh` passes (full build); logs contain `regdeny: PASS permission denied` and `registry: deny unregister` |
| WP-M5.1 Fairness + latency SLO instrumentation | M5 | DONE (IPC recv wait/scan telemetry + percentile snapshot + mixed-load harness mode + optional SLO env thresholds) | `kernel/src/telemetry.rs`, `kernel/src/syscall/handlers.rs`, `test_hello.sh`, `scripts/harness_matrix.sh` | `MARKER_MODE=m5_fairness TEST_COMMAND_REPEAT=1 RUN_WAIT=16 MIN_EXIT_COOKIES=6 ./test_hello.sh` passes (full rebuild); latest run observed `ipc_wait_p95_ms=4`, `ipc_wait_p99_ms=4`, `ipc_scan_avg_steps_x100=158` |
| WP-M5.2 Fairness SLO threshold enforcement in matrix | M5 | DONE (matrix now enforces conservative fairness limits for `m5_fairness`) | `scripts/harness_matrix.sh` | `MAX_IPC_WAIT_P95_MS=16 MAX_IPC_WAIT_P99_MS=16 MAX_IPC_SCAN_AVG_STEPS_X100=250 MARKER_MODE=m5_fairness TEST_COMMAND_REPEAT=1 RUN_WAIT=16 MIN_EXIT_COOKIES=6 ./test_hello.sh` passes (full rebuild); latest run observed `ipc_wait_p95_ms=4`, `ipc_wait_p99_ms=4`, `ipc_scan_avg_steps_x100=152` |
| WP-L2.1 Mutable FS operations + DAC checks | L2A | IN PROGRESS (`write` + append/grow ext2 writes landed; mutable ops wired end-to-end for ext2 backend: `mkdir/rename/rmdir/unlink`; `O_CREAT` path now creates files through VFS->blk->ext2; VFS now tracks per-path owner (authenticated sender) and enforces owner checks for mutate/create flows in addition to protected-path denies; explicit multi-client non-owner deny coverage landed via spawned `ownerprobe` flow) | `userspace/ext2`, `userspace/virtio-blk`, `userspace/vfs`, `userspace/libcluu`, `userspace/shell`, `userspace/c_hello`, `xtask`, `test_hello.sh` | `MARKER_MODE=l2_ext2write ./test_hello.sh --no-build`, `MARKER_MODE=l2_ext2append ./test_hello.sh --no-build`, `MARKER_MODE=l2_ext2mutate ./test_hello.sh --no-build`, `MARKER_MODE=l2_ext2unlink ./test_hello.sh --no-build`, `MARKER_MODE=l2_owner_deny ./test_hello.sh --no-build` pass |
| WP-L2.2 Minimal signals + shell job control | L2B | TODO | `userspace/procmgr`, `userspace/shell`, `userspace/libcluu` | interactive `SIGINT` + `SIGCHLD` behavior works in shell |
| WP-L2.3 Real `mmap` allocator + `mprotect` | L2C | TODO | `userspace/libcluu/src/posix/memory.rs`, kernel VM path | map/unmap/reuse/protection tests pass |

### Next execution batch

1. Tighten M5 limits from conservative to data-backed bounds once more CI samples are collected (track variance first).
2. Tighten M3 leak limits from conservative to data-backed bounds once more CI samples are collected (track variance first).
3. Consolidate denial-path modes into one matrix bundle for faster pre-commit coverage while preserving deterministic shell autostart triggers.

### Completion criteria by level

1. Level 0 complete:
capability forgeability closed, non-godmode init booting from initrd, teardown/mapping invariants pass, IPC receive/identity semantics stable.
2. Level 1 complete:
observable and diagnosable runtime with CI gates for leak/failure/fairness regression.
3. Level 2 complete:
practical userspace development baseline (mutable FS, minimal signals, stronger shell behavior).

---

## Bottom Line

CLUU is a promising microkernel project with good modularity and strong momentum, but it currently sits in a **trusted-development prototype** maturity tier, not a secure general-purpose microkernel tier.

If Level 0 and Level 1 are completed, CLUU becomes a strong base for sustained userspace evolution and a genuinely credible seL4-inspired platform with practical POSIX-facing ergonomics.
