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
  - Incremental hardening target: direct-delivery waiter scan must not head-of-line block on one incompatible waiter; keep scanning boundedly and rotate retryable waiters.
  - Incremental hot-path target: avoid heap allocation for small (`<=64B`) `send/call` user payload staging before direct-delivery/queue handoff.

#### P0.3 Shared-ring bulk data path (library-first)
- Goal: stop pushing large VFS/console payloads through queue copies.
- Userspace-first changes:
  - Add shared ring abstraction in `userspace/libcluu/src/ipc.rs`. (Status: foundation landed)
  - Use `SpaceGrant` for shared page setup; IPC carries ring notifications/indices.
  - Integrate first with VFS read/write data plane. (Status: partial integration landed via `VFS_RING_SETUP` + `VFS_READ_RING`, shell `ringio` probe, and `m6_ring_io` harness gate; ring setup now rejects remap/regrant attempts to different targets for an already-bound client)
- Validation:
  - New marker mode: `m6_ring_io`.
  - Latest gate: `MARKER_MODE=m6_ring_io CLUU_SHELL_AUTOSTART_CMD=ringio CLUU_BOOTBOOT_ENV=cluu.ipc_direct=1 RUN_WAIT=20 ./test_hello.sh` (full rebuild) passes with `ringio: PASS`.

#### P0.4 IPC SLO re-baseline and enforcement
- Goal: make IPC gains durable through CI guardrails.
- Changes:
  - Extend `kernel/src/telemetry.rs` with queue-bytes/messages current+peak counters in addition to direct-path counters. (Status: landed)
  - Add threshold checks in `test_hello.sh` and case defaults in `scripts/harness_cases.conf` for `m6_ipc_compact` and `m6_ipc_rendezvous` (wait/scan + queue peak thresholds). (Status: landed)
- Validation:
  - `scripts/harness_suite.sh --case m6_ipc_compact` passes with SLO gates (`ipc_queue_bytes_peak=3298`, `ipc_queue_messages_peak=40` in latest run).
  - `scripts/harness_suite.sh --case m6_ipc_rendezvous` passes with SLO gates and direct delivery check (`ipc_direct_deliveries=215`, `ipc_queue_bytes_peak=2952` in latest run).

#### P0.5 Register IPC fast path (small-message ABI path)
- Goal: remove user-memory copies for common small IPC messages by using syscall register payload lanes.
- Constraints:
  - No new syscall numbers.
  - Keep existing pointer/length path as fallback for larger payloads and compatibility.
  - Feature-gate runtime selection to allow A/B validation (`cluu.ipc_reg_fast=0|1`).
- Implementation order:
  - `P0.5.a`: `Call/Reply` register fast path first. (Status: complete; `Reply` fast-path runtime-gated and validated)
  - `P0.5.b`: `Send/Recv` register fast path. (Status: complete; `Send` fast-path runtime-gated and validated)
  - Preserve existing sender-auth metadata and endpoint rights model.
- Validation:
  - New harness cases: `m6_ipc_reg_off`, `m6_ipc_reg_on`.
  - Compare against `b_spawn_warm`/M6 baseline:
    - `noop_spawn_reply_p95_cycles`
    - `noop_map_elf_reply_p95_cycles`
    - `ipc_queue_bytes_peak` / `ipc_queue_messages_peak`
  - Require no regression in existing M6 modes and shell-ready SLO (`<=15s`).
  - Current A/B snapshot (`b_spawn_warm`, full build):
    - `ipc_reg_fast=0` avg: `noop_spawn_reply_p95_cycles=80,297,410`, `noop_map_elf_reply_p95_cycles=17,959,514`.
    - `ipc_reg_fast=1` avg: `noop_spawn_reply_p95_cycles=67,147,413`, `noop_map_elf_reply_p95_cycles=14,878,020`.
    - Shell ready stayed `10s` in all runs; queue peaks unchanged (`2671` bytes, `30` messages).
    - Note: variance is still high; keep per-run ceiling looser for `m6_ipc_reg_on` while we finish `P0.5.b`.
  - Post-`P0.5.b` A/B snapshot (`REPEATS=1`, full build):
    - `ipc_reg_fast=0`: `noop_spawn_reply_p95_cycles=260,340,092`, `noop_map_elf_reply_p95_cycles=28,799,696`, `shell_ready_s=10`.
    - `ipc_reg_fast=1`: `noop_spawn_reply_p95_cycles=87,360,696`, `noop_map_elf_reply_p95_cycles=19,815,638`, `shell_ready_s=11`.
    - Net effect on this run pair: strong spawn-path p95 improvement with reg-fast enabled and no queue-peak regression.

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

#### B.3 Spawn hot-path performance (shell responsiveness)
- Goal: reduce spawn+wait latency for interactive shell/process-heavy workflows.
- Changes:
  - Add spawn-path telemetry stamps in procmgr/VFS (`spawn_request`, `elf_fetch`, `map_segments`, `stack_map`, `thread_start`, `first_user_ipc`).
  - Introduce warm-path optimizations:
    - cache parsed ELF metadata for hot binaries (`/bin/noop`, `/bin/hello`, shell utilities),
    - reduce repeated mapping/setup overhead for short-lived children,
    - avoid unnecessary control-plane IPC roundtrips on spawn completion path.
  - Keep process model userspace-owned (no kernel process object); optimize procmgr/VFS orchestration only.
- Status update:
  - Implemented bounded procmgr VFS file-handle cache for `map_elf` hot paths with stale-handle invalidate/retry.
  - Validation (`scripts/harness_suite.sh --case b_spawn_warm`, full build): `noop_spawn_reply_p95_cycles=66,427,414`, `noop_map_elf_reply_p95_cycles=17,456,814`, `shell_ready_s=9`.
  - Implemented VFS cached ELF metadata reuse (`entry_point` + segment descriptors) for hot `map_elf` paths, avoiding repeated ELF parse on warm cache hits.
  - Validation (`scripts/harness_suite.sh --case b_spawn_warm`, full build): `noop_spawn_reply_p95_cycles=43,351,902`, `noop_map_elf_reply_p95_cycles=14,803,132`, `shell_ready_s=10`.
- Validation:
  - New marker mode: `b_spawn_perf`.
  - `benchprobe` spawn metric target:
    - initial objective: at least 2x improvement vs current baseline.
  - Use `benchprobe` spawn-focused mode for this gate first, then keep full mixed probe as secondary check.
  - Shell-ready SLO remains <= 15s with full rebuild harness.

### C: Threading enablement

#### C.1 Futex primitive via `Invoke`
- Add futex wait/wake operation(s) under `Invoke`.
- Kernel holds wait queues keyed by `(space_id, user_address)`.
- Status update:
  - Added `InvokeOp::FutexWait`/`InvokeOp::FutexWake` in kernel and userspace syscall ABI.
  - Implemented kernel futex wait-queue manager (`kernel/src/sync/futex.rs`) keyed by `(space_id, user_addr)` with bounded wake count.
  - Implemented `invoke_futex_wait`/`invoke_futex_wake` handlers with:
    - space-token rights check (`SPACE_MAP`),
    - caller-space ownership check (caller must match token space),
    - value-compare gate (`WouldBlock` on mismatch),
    - timeout path (`Timeout`) and waiter cleanup.
  - Added userspace wrappers in `libcluu::syscall` (`futex_wait`, `futex_wake`).
  - Full-build regression check (`scripts/harness_suite.sh --case b_spawn_warm`): `shell_ready_s=10`, `noop_spawn_reply_p95_cycles=49,203,336`, `noop_map_elf_reply_p95_cycles=14,227,948` (`SLO PASS`).
- Validation:
  - New marker mode: `c_futex`.
  - Harness case `c_futex` (full build) passes with required marker `futexprobe: PASS` and shell-ready within policy.
  - Added race marker mode `c_futex_race` (in-process waiter/waker ordering with `ThreadCreate`).
  - `TOKEN_SPACE` policy now carries `THREAD_CONTROL` so user processes can create threads needed by futex wait/wake race probes.

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

1. Lock warm-cache baseline with full-build harness (`b_spawn_warm`, repeated sweep).
2. Re-baseline M6 + warm-cache SLOs and gate via CI harness cases.
3. Enter `B.3` spawn hot-path performance pass (roundtrip cuts + hot ELF metadata path).
4. Complete `C.1` validation harness (`c_futex`) and race probes, then continue with `C.2`.
