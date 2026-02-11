# CLUU Deep Dive Implementation Plan

**Date**: 2026-02-10  
**Source baseline**: `docs/DEEP_DIVE_ANALYSIS.md` (Section 12)  
**Execution rule**: IPC optimization is mandatory first, then continue with the deep-dive concrete next steps.

## 1. Scope and Ordering

This plan replaces ad-hoc phase ordering with one strict sequence:

1. IPC Optimization Track (new Phase 0, mandatory first)
2. Phase A: Software porting unblockers (`poll`, `signal`, `fcntl`)
3. Phase B: Resource lifecycle and leak closure (`SpaceDestroy`, teardown)
4. Phase C: Threading enablement (`futex`, pthread layer)
5. Phase D: Multiple TTYs

No new syscall numbers are introduced. The syscall ABI remains:
`Send`, `Recv`, `Call`, `Reply`, `Yield`, `Invoke`, `DebugPrint`.

## 2. Milestone Hierarchy

### P0: IPC optimization first (must complete before A/B/C/D)

#### P0.1 Compact message queue representation
- Goal: remove fixed 4KB storage for common small IPC messages.
- Kernel changes:
  - Refactor `kernel/src/ipc/endpoint.rs` message storage:
    - `Compact` form for small messages (metadata + inline words/bytes).
    - `Large` form for larger payloads (boxed exact-size payload).
  - Keep sender-auth metadata unchanged.
- Validation:
  - New marker mode: `m6_ipc_compact`.
  - Harness: queue-memory telemetry present and reduced under small-message churn.

#### P0.2 Rendezvous direct-transfer fast path
- Goal: when receiver is already armed/waiting, bypass queue and deliver immediately.
- Kernel changes:
  - Extend active queue IPC path in `kernel/src/syscall/handlers.rs` + `kernel/src/ipc/endpoint.rs`.
  - Use current thread wait state and endpoint waiter lists to select direct-delivery path.
  - Preserve fallback queue path for async/non-rendezvous cases.
- Validation:
  - New marker mode: `m6_ipc_rendezvous`.
  - SLO deltas better than M5 baseline (`ipc_wait_p95_ms`, `ipc_scan_avg_steps_x100`).

#### P0.3 Shared-ring bulk data path (library-first)
- Goal: stop pushing large VFS/console payloads through queue copies.
- Userspace-first changes:
  - Add shared ring abstraction in `userspace/libcluu/src/ipc.rs`. (Status: foundation landed)
  - Use `SpaceGrant` for shared page setup; IPC carries ring notifications/indices.
  - Integrate first with VFS read/write data plane. (Status: partial integration landed via `VFS_RING_SETUP` + `VFS_READ_RING`, shell `ringio` probe, and `m6_ring_io` harness gate)
- Validation:
  - New marker mode: `m6_ring_io`.
  - Latest gate: `env -u CLUU_SHELL_AUTOSTART_CMD MARKER_MODE=m6_ring_io RUN_WAIT=12 ./test_hello.sh` (full rebuild) passes with `ringio: PASS`.

#### P0.4 IPC SLO re-baseline and enforcement
- Goal: make IPC gains durable through CI guardrails.
- Changes:
  - Extend `kernel/src/telemetry.rs` with queue-bytes/direct-path counters.
  - Add threshold checks in `test_hello.sh` and `scripts/harness_matrix.sh`.
- Validation:
  - `cargo xtask harness-matrix` includes `m6_*` gates and fails on regression.

### A: Portability unblockers (`poll`, `signal`, `fcntl`)

#### A.1 `signal` and `fcntl` compatibility stubs
- Add `signal()/sigaction()/raise()` minimal semantics in userspace POSIX layer.
- Add `fcntl` support for `F_GETFL`, `F_SETFL`, `F_DUPFD`.
- Validation:
  - C probes compile/link and execute without ENOSYS on these calls.

#### A.2 `poll()` over existing recv_any semantics
- Implement `poll` in userspace as fd-to-endpoint multiplexing wrapper around `sys_recv`.
- Behavior:
  - TTY/IPC-backed descriptors: readiness via recv path.
  - File descriptors backed by blocking VFS IPC: conservative readiness policy.
  - Timeout maps to `sys_recv` timeout.
- Validation:
  - New marker mode: `a_poll`.
  - Run editor-style loop probe (input + timeout wakeups).

### B: Resource lifecycle and teardown correctness

#### B.1 Implement `InvokeOp::SpaceDestroy`
- Implement actual space teardown in kernel:
  - unmap user pages
  - reclaim page-table pages
  - release accounted mappings/frames
- Add hard invariants and explicit error semantics for busy/in-use spaces.
- Validation:
  - New marker mode: `b_space_destroy`.
  - Spawn/exit churn converges (no monotonic PMM growth trend).

#### B.2 Procmgr lifecycle discipline (userspace process model)
- Keep process semantics in userspace (as intended).
- Add strict procmgr state model:
  - `Spawning -> Running -> Exiting -> Reaped`
- On exit path:
  - destroy threads
  - revoke owned tokens/endpoints
  - invoke space destroy
- Validation:
  - New marker mode: `b_teardown_churn`.
  - Leak deltas bounded and stable across repeated runs.

### C: Threading enablement

#### C.1 Futex primitive via `Invoke`
- Add futex wait/wake operation(s) under `Invoke`.
- Kernel holds wait queues keyed by `(space_id, user_address)`.
- Validation:
  - New marker mode: `c_futex`.
  - Wait/wake race probes pass.

#### C.2 pthread subset in userspace
- Add pthread create/join/mutex/cond subset over thread create + futex.
- Add TLS setup path for thread start.
- Validation:
  - New marker mode: `c_pthread`.
  - MicroPython thread-enabled smoke build target reaches REPL.

### D: Multi-TTY capability

#### D.1 Virtual terminals in console and keyboard routing
- Add N VT instances with independent state.
- Add foreground VT switching and input routing.
- Validation:
  - New marker mode: `d_vt_switch`.
  - Deterministic switch/input routing probe.

#### D.2 `/dev/ttyN` integration path
- Extend VFS device mapping for tty nodes.
- Validate shell interaction correctness per VT.
- Validation:
  - New marker mode: `d_tty_devfs`.
  - Multi-shell sessions across VTs.

## 3. Commit and Gate Policy

For each sub-milestone (`P0.1`, `P0.2`, ...):

1. Land code + docs in one scoped commit.
2. Run full rebuild harness (`./test_hello.sh`) with the sub-milestone marker mode.
3. Record observed metrics in `docs/KERNEL_MATURITY_ANALYSIS.md` implementation tracker.
4. Only then advance to the next sub-milestone.

## 4. Immediate Execution Queue

1. Start `P0.1` (compact IPC message storage).
2. Then `P0.2` (rendezvous direct-transfer fast path).
3. Then `P0.3` (shared ring bulk path for VFS).
4. Then `P0.4` (SLO rebaseline and CI thresholds).
5. Enter Phase A.
