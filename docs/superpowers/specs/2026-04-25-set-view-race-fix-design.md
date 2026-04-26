# SET_VIEW vs Thread-Start Race Fix — Design

**Status:** Design — pending user review
**Date:** 2026-04-25
**Authors:** Balazs Valkony (user), Claude Opus 4.7 (collaborator)
**Tracks:** Task #71

## Problem

In `userspace/procmgr/src/main.rs::handle_container_run` (and 8 other call
sites that follow the same pattern), the spawn flow does:

1. `spawn_service_with_env` calls `thread_create` (`spawn_service_with_env:3765`)
   — the new thread is added to the scheduler in a runnable state.
2. Caller does its bookkeeping.
3. Caller calls `register_vfs_view_for_thread` (e.g. `handle_container_run:4654`)
   — sends VFS_SET_VIEW asynchronously.

Between (1) and the VFS receiving the SET_VIEW message in (3), the kernel can
preempt procmgr and schedule the new thread. The thread can then make its
first VFS call (e.g. `open("/manifest.toml")`) which arrives at VFS *before*
SET_VIEW does. VFS then resolves that call against the wrong (or empty)
view, returning `PermissionDenied` or `NotFound`.

**Observed symptoms (post-mount-policy, 2026-04-25 baseline 39/46 PASS):**

- `l2_argv`, `l2_sigint`, `f13_detach_survive`: flake in the suite, pass 3/3
  standalone.
- `l2_rm`: ~25 % flake even standalone.
- `l2_owner_deny`: a long-standing flake explained by the same race
  (`project_l2_owner_deny_flaky.md`); now masking a separate test-design
  problem (#70).

The race is architectural, not specific to mount-policy — mount-policy made
the symptoms more visible by removing VFS's old "if container_id > 0,
unconditionally prepend mounts" fallback that masked some failure modes.

## Goal

Eliminate the race so that **a thread can never make a VFS call before its
view is installed at VFS**. Deliver the four currently-flaking l2_* cases
green 10/10 in a repeat sweep, plus full harness suite ≥ 45/46 (with
`l2_owner_deny` the lone remaining fail, which is #70's domain).

## Non-goals

- Synchronizing SET_VIEW into a call/reply (Approach 3 in brainstorming) —
  defer until a second per-thread setup actually demands it.
- Changing how `pthread_create` works in libcluu — pthreads start running.
- Fixing `l2_owner_deny` (#70 — separate test-design issue).
- Spawn-path performance work beyond the new syscall cost (kernel freeze
  spirit: one minimal change, named test case forcing it).

## Design

### Kernel change (`kernel/src/syscall/handlers.rs`, `kernel/src/sched/`)

`invoke_thread_create` reads a new `flags: u64` parameter from `args.arg6`
(today unused). One bit defined:

```
pub const THREAD_CREATE_START_SUSPENDED: u64 = 0x1;
```

When `flags & 1 != 0`, the thread is added to the scheduler with the
existing `ThreadFlags::SUSPENDED` (or equivalent state-machine entry —
reuse what `thread_suspend` already uses, do not introduce a parallel
mechanism). The scheduler's pick-next logic skips suspended threads. The
existing `thread_resume(thread_token)` syscall clears the suspended state
and makes the thread runnable.

If the existing thread-suspend mechanism is implemented as a status enum
rather than a flag, set the same enum value at create time. The point is:
**the new thread must be in the same kernel-visible state a `thread_suspend`
call would put a running thread in.**

### libcluu change (`userspace/libcluu/src/syscall.rs`)

Extend `thread_create` signature:

```rust
pub fn thread_create(
    space_token: usize,
    entry: usize,
    stack: usize,
    priority: usize,
    flags: usize,    // NEW — pass 0 for default (start running)
) -> Result<usize>
```

The wrapper passes `flags` as `args.arg6` to the existing invoke.

Add a public constant alongside other thread constants:

```rust
pub const THREAD_CREATE_START_SUSPENDED: usize = 0x1;
```

Existing callers (`pthread_create` in `posix/pthread.rs`, the bare
`spawn_service_with_env` call in procmgr) gain a trailing `, 0`. No
behavior change for them.

### Procmgr change (`userspace/procmgr/src/main.rs`)

Add a helper near the existing spawn helpers:

```rust
/// Spawn a service and install its VFS view atomically. Creates the thread
/// SUSPENDED so the view is guaranteed to land at VFS before the thread's
/// first IPC call. Resumes the thread on success; destroys it on view-install
/// failure (so we don't leak a suspended thread).
fn spawn_service_and_register_view(
    &mut self,
    /* same args as spawn_service_with_env, plus: */
    view_mounts: &ViewMountList,
    profile: CapProfile,
    container_id: u64,
) -> Result<SpawnResult> {
    let result = self.spawn_service_with_env(
        /* ..., */
        THREAD_CREATE_START_SUSPENDED,    // pass-through flag
    )?;
    self.register_vfs_view_for_thread(
        result.thread_token, view_mounts, profile, container_id,
    );
    if let Err(err) = thread_resume(result.thread_token) {
        let _ = thread_destroy(result.thread_token);
        return Err(err);
    }
    Ok(result)
}
```

`spawn_service_with_env` itself gains a new `thread_flags: usize`
parameter, forwarded straight into the `thread_create` call at line 3765.
Existing callers that don't need suspension pass `0`.

The nine sites that today call `spawn_service_with_env` followed by
`register_vfs_view_for_thread` migrate to the new helper. List (line
numbers as of HEAD `70931ec`):

- `:922`, `:1135`, `:1424`, `:2149`, `:2369`, `:2578`, `:3234`, `:3399`,
  `:4654`.

### Failure handling

- `thread_resume` failure in the helper → call `thread_destroy` on the
  suspended thread, propagate error. Prevents a leaked suspended-forever
  thread.
- `register_vfs_view_for_thread` self-queues a deferred view if
  `vfs_endpoint == 0`. The deferred-view installation path must call
  `thread_resume` once the view actually goes through. Today the deferred
  path doesn't track threads-needing-resume — add a small
  `pending_view_resume: BTreeMap<TidU64, ThreadToken>` keyed by the same
  tid the deferred view is keyed on, populated when the helper sees the
  endpoint isn't ready, drained when the deferred view installs.

### Race-correctness argument

Once the thread is created SUSPENDED and procmgr sends SET_VIEW before
calling `thread_resume`:

1. SET_VIEW message is queued in VFS's mailbox at time T1 (procmgr send).
2. Thread becomes runnable at time T2 > T1 (procmgr resume).
3. Thread eventually runs and sends its first VFS call at time T3 > T2.

VFS processes its mailbox in send-order. SET_VIEW (at T1) is processed
before the thread's call (at T3). Race-free by construction, regardless of
preemption: even if procmgr is preempted between T1 and T2, the thread
cannot run during that window.

Equally important: SET_VIEW does **not** need to be sync (call/reply).
Async send + suspend-bracket is sufficient.

## Test plan

### 1. Kernel-side micro-test

New `userspace/suspendprobe/` container:

- Probe creates a child thread with `THREAD_CREATE_START_SUSPENDED`. Child
  thread's entry point is a small function that writes to a known address
  ("ran=1 marker").
- Probe waits ~100 ms, asserts the marker is NOT set.
- Probe calls `thread_resume(child_token)`.
- Probe waits ~100 ms, asserts the marker IS set.

New harness case: `kernel_suspended_thread` with required marker
`suspendprobe: PASS suspended-thread did not run before resume`. Confirms
the kernel primitive in isolation, independent of VFS or procmgr.

### 2. Race-targeted repeat sweep

Add `scripts/harness_repeat.sh CASE N` (or a `--repeat N` flag on
`harness_run.sh`) that runs CASE N times and reports `M/N PASS`.

Gate: each of `l2_argv`, `l2_sigint`, `f13_detach_survive`, `l2_rm` must
report 10/10 PASS standalone post-fix.

### 3. Full matrix

`bash scripts/harness_suite.sh` must hit ≥ 45/46 PASS. The acceptable
fail is `l2_owner_deny` (#70). The 4 race-flaky cases above must move
to PASS. Cases with similar set_view-related flake — `l2_fg`,
`m5_fairness`, `p4_dev` — should be re-checked case-by-case to see if
their flake mode was the same race; if so, count them as bonus wins.

### 4. Negative control

Revert just the procmgr-side suspend-bracket (keep the kernel API change),
re-run the race sweep. Flakes must reappear. Confirms the bracket — not
some other side-effect — is what closes the race.

### 5. Performance check

Per `feedback_spawn_perf_baseline`: run `b_spawn_warm` and
`l2_jobchurn_heavy` before/after. Expected delta ≪ 5 % (added cost is one
extra `thread_resume` syscall plus one flag arg through invoke). If the
delta crosses 5 %, investigate before merging.

## Scope boundary

**In scope for the implementation plan:**

- Kernel: `THREAD_CREATE_START_SUSPENDED` flag bit + scheduler honors it +
  reuse of existing `ThreadFlags::SUSPENDED` mechanism.
- libcluu: `thread_create` 5-arg signature + flag constant; existing
  callers in tree updated to pass `0`.
- procmgr: `spawn_service_and_register_view` helper + nine call-site
  migrations + deferred-view resume bookkeeping.
- `userspace/suspendprobe/` + `kernel_suspended_thread` harness case.
- `scripts/harness_repeat.sh` (or equivalent flag) for the race sweep.
- Memory updates: `#71` closes; `project_mount_policy.md` notes the race
  fix.

**Out of scope (follow-ups if needed):**

- Sync IPC for SET_VIEW (Approach 3 from brainstorming).
- Suspended-create flag for pthread_create. Pthreads start running.
- Stabilizing `l2_fg`, `m5_fairness`, `p4_dev` if they don't fall out of
  this fix on their own.
- Redesigning `l2_owner_deny` (#70).

## Risks

- **Kernel state-machine drift.** A new `SUSPENDED` flag that doesn't
  reuse the existing thread_suspend mechanism would create a parallel
  state machine. Mitigation: implementation must use the same kernel
  primitive `thread_suspend` already sets — verify in code review.
- **Leaked suspended threads on view-install failure.** Helper destroys
  the thread on `thread_resume` error, and the deferred-view path resumes
  on eventual install. Both paths covered, but worth a code-review
  re-check before merging.
- **Performance regression on the spawn path.** Sub-microsecond delta
  expected, but the freeze spirit demands a measured gate. Performance
  check is part of the test plan.
- **Helper-vs-bare confusion.** If a future caller does
  `spawn_service_with_env` directly without `register_vfs_view_for_thread`
  and forgets to pass `flags=0` (or worse, accidentally passes
  `THREAD_CREATE_START_SUSPENDED`), a deadlock results. Mitigation: keep
  the bare helper's `flags` parameter explicit (no default); require
  every call site to specify intent.

## Decisions

- **Kernel primitive: flag arg, not new InvokeOp.** Wire-compatible
  extension; reuses existing dispatch path; one new bit instead of one
  new op slot. (Brainstorming Approach 1 over 2.)
- **SET_VIEW stays async.** Suspend-bracket already serializes; sync IPC
  would add a round-trip per spawn for no correctness gain. Defer to a
  follow-up if/when needed. (Brainstorming Approach 1 over 3.)
- **Helper covers all nine sites at once, not just `handle_container_run`.**
  All nine sites have the same race shape; fixing one would leave eight
  more to surface as flakes later.
