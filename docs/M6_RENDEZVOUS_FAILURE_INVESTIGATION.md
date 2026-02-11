# M6 Rendezvous IPC Failure Investigation

Date: 2026-02-11

## Scope

Investigate why rendezvous direct-delivery IPC (`cluu.ipc_direct=1`) fails during boot/service bring-up, before diving into narrow single-path debugging.

## Controlled Reproduction Matrix

All runs used the same workspace and harness, with full image rebuild between A/B toggles.

1. Direct OFF image (`CLUU_BOOTBOOT_ENV=cluu.ipc_direct=0`, then `MARKER_MODE=m1_recv`):
- Kernel confirms `ipc direct rendezvous=0`.
- Boot reaches `[USER] shell: ready`.
- System continues through VFS mount/open/spawn flow.
- Harness run failed only due strict marker threshold (`MIN_EXIT_COOKIES=3`) with 2 exits in that short run, not due boot deadlock.

2. Direct ON image (`CLUU_BOOTBOOT_ENV=cluu.ipc_direct=1`, then `MARKER_MODE=m1_recv`):
- Kernel confirms `ipc direct rendezvous=1`.
- Deterministic stall before shell readiness.
- Last stable logs:
  - `All critical processes initialized`
  - `Switching to NORMALMODE (preemptive)`
  - `[USER] vfs: setup_mounts start`
  - `[USER] vfs: initrd mounted`
- No shell readiness, no follow-up VFS/registry progress.

3. Direct ON long soak (`MARKER_MODE=none`, `RUN_WAIT=60`):
- Same terminal point (`vfs: initrd mounted`) with no further progress.
- Confirms liveness failure, not slow startup.

## Important Log Delta

Compared to direct-off runs, direct-on runs are missing post-initrd control-plane traffic such as:
- `registry: registered blkdev:main`
- `registry: subscribe vfs:main ...`
- subsequent VFS ready/open activity and shell startup sequence

This points to a rendezvous/control-message delivery problem rather than storage/driver initialization.

## High-Probability Failure Modes

1. **Armed-vs-blocked waiter ambiguity in direct path** (highest probability)
- Waiters are considered wakeable when `(is_blocked || is_recv_wait_armed)`:
  - `kernel/src/sched/thread_manager.rs:392`
  - `kernel/src/ipc/endpoint.rs:197`
- Direct path consumes message payload immediately (no queue fallback persistence):
  - `kernel/src/ipc/endpoint.rs:775`
- `sys_recv` arms + registers waiters, then blocks:
  - `kernel/src/syscall/handlers.rs:202`
- If a receiver is selected while armed but not yet safely blocked/ready to consume, a direct delivery can be "consumed" without durable queue state, leading to stuck waiters and lost progress.

2. **Endpoint-lock-held cross-subsystem operations in direct path**
- Direct path performs user copy + thread delivery bookkeeping while endpoint lock is held:
  - `kernel/src/ipc/endpoint.rs:785`
- This increases timing sensitivity and can amplify races/lock-order stress versus queue path.

3. **Harness ambiguity (fixed in this branch)**
- `m6_ipc_rendezvous` case previously did not force `cluu.ipc_direct=1`, allowing accidental stale-image outcomes.
- This investigation adds explicit per-case BOOTBOOT env toggles in `scripts/harness_cases.conf`.

## Investigation Conclusion

Rendezvous direct-delivery is currently not safe to enable globally during bootstrap/service discovery. It introduces a liveness regression in control-plane IPC before shell readiness.

## Immediate Next Steps

1. Add reason-coded telemetry in direct path:
- attempted
- no_waiter
- waiter_armed_not_blocked
- copy_fail
- delivery_store_fail
- delivered

2. Tighten direct-delivery eligibility:
- Require receiver to be truly blocked on current ticket before direct consume.
- Keep queue path as fallback for armed-only waiters.

3. Reduce blast radius while validating:
- Keep global kill-switch default-off.
- Add scoped enablement only for targeted endpoints/workloads during test phases.

4. Re-run deterministic matrix:
- `m6_ipc_compact` with direct off
- `m6_ipc_rendezvous` with direct on
- long soak and SLO checks after each candidate fix
