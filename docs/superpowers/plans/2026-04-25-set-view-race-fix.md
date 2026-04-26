# SET_VIEW vs Thread-Start Race Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the race where a newly-spawned thread can make VFS calls before its view is installed at VFS, by adding a kernel `THREAD_CREATE_START_SUSPENDED` flag and threading it through procmgr's spawn path.

**Architecture:** Kernel adds one bit to `invoke_thread_create`'s flags arg (currently `args.arg6`, unused). When set, the new thread is created with the existing `ThreadFlags::SUSPENDED` flag — same mechanism `thread_suspend` already uses, so no parallel state machine. libcluu's `thread_create` gains a `flags: usize` parameter. Procmgr adds a helper `spawn_service_and_register_view` that does suspended-create + register-view + resume in sequence; the nine sites that today follow that pattern migrate to the helper. SET_VIEW stays async — the suspend-bracket alone serializes ordering.

**Tech Stack:** Rust (no_std kernel and userspace), bash (harness scripts), QEMU integration tests.

---

## File Structure

**Created files:**
- `userspace/suspendprobe/Cargo.toml` — probe crate manifest
- `userspace/suspendprobe/src/main.rs` — probe binary that creates a suspended thread and verifies it doesn't run before resume
- `containers/suspendprobe/Cluufile` — suspendprobe container manifest
- `scripts/harness_repeat.sh` — runs a single harness case N times, reports pass/fail tally

**Modified files:**
- `kernel/src/syscall/handlers.rs:831-895` — `invoke_thread_create` reads `args.arg6` as flags; sets `ThreadFlags::SUSPENDED` if `flags & 1 != 0`
- `userspace/libcluu/src/syscall.rs:679-697` — `thread_create` signature gains `flags: usize` parameter; new `THREAD_CREATE_START_SUSPENDED` const
- `userspace/libcluu/src/posix/pthread.rs:401-406` — `pthread_create` passes `0` for the new flags arg
- `userspace/procmgr/src/main.rs` — extend `spawn_service_with_env` with `thread_flags` parameter; add `spawn_service_and_register_view` helper; migrate 9 call sites
- `Cargo.toml` — add `userspace/suspendprobe` to workspace members + per-crate profile blocks
- `scripts/harness_cases.conf` — register `kernel_suspended_thread` case
- `scripts/harness_case_defaults.sh` — autostart command for `kernel_suspended_thread`
- `scripts/harness_run.sh` — required_markers block for `kernel_suspended_thread`

**Memory updates:**
- `memory/MEMORY.md` — flip the project_l2_owner_deny_flaky line to current state and update project_mount_policy.md note about #71
- `memory/project_mount_policy.md` — strike #71 from the open-followups list

---

## Scope Check

Single subsystem: thread create-state semantics + procmgr spawn flow. One plan, one implementation cycle.

---

## Task 1: libcluu — extend `thread_create` signature

**Files:**
- Modify: `userspace/libcluu/src/syscall.rs:679-697` (`thread_create` definition)
- Modify: `userspace/libcluu/src/posix/pthread.rs:401-406` (one in-tree caller)

This task adds the API surface; existing callers pass `0`. The kernel still ignores the flag at this stage — runtime behavior is unchanged. This is a wire-compatible extension.

- [ ] **Step 1: Add `flags: usize` parameter to `thread_create`**

In `userspace/libcluu/src/syscall.rs`, replace the existing `thread_create` (line 679) with:

```rust
pub fn thread_create(
    space_token: usize,
    entry: usize,
    stack: usize,
    priority: usize,
    flags: usize,
) -> Result<usize> {
    unsafe {
        invoke(
            space_token,
            InvokeOp::ThreadCreate,
            entry,
            stack,
            priority,
            flags,
        )
    }
}
```

- [ ] **Step 2: Add `THREAD_CREATE_START_SUSPENDED` constant**

In `userspace/libcluu/src/syscall.rs`, add (near the other public constants — search for `pub const` near the top of the file or near `InvokeOp`):

```rust
/// Flag for `thread_create`: create the thread in the SUSPENDED state.
/// Caller must call `thread_resume` to make it runnable. Useful when
/// per-thread setup (e.g. installing a VFS view) must complete before
/// the thread is allowed to run.
pub const THREAD_CREATE_START_SUSPENDED: usize = 0x1;
```

- [ ] **Step 3: Update `pthread_create` to pass flags=0**

In `userspace/libcluu/src/posix/pthread.rs:401`, replace:

```rust
    let child_token = match crate::syscall::thread_create(
        space,
        pthread_trampoline as *const () as usize,
        startup_addr,
        128, // default priority
    ) {
```

With:

```rust
    let child_token = match crate::syscall::thread_create(
        space,
        pthread_trampoline as *const () as usize,
        startup_addr,
        128, // default priority
        0,   // flags — pthreads start running
    ) {
```

- [ ] **Step 4: Update procmgr's bare `thread_create` call site to pass flags=0**

In `userspace/procmgr/src/main.rs:3765`, replace:

```rust
        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority)?;
```

With:

```rust
        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority, 0)?;
```

(Procmgr's other paths go through `spawn_service_with_env`, which is updated in Task 7.)

- [ ] **Step 5: Build to verify the API change compiles**

Run: `cargo xtask build`
Expected: clean build. All callers compile with the new signature; behavior is unchanged because everyone passes `flags=0`.

- [ ] **Step 6: Commit**

```bash
git add userspace/libcluu/src/syscall.rs userspace/libcluu/src/posix/pthread.rs userspace/procmgr/src/main.rs
git commit -m "libcluu: thread_create gains flags arg for THREAD_CREATE_START_SUSPENDED

Wire-compatible extension: existing callers (pthread_create, procmgr)
pass 0. The kernel still ignores the flag at this stage — behavior is
unchanged. Sets up the API surface for the upcoming kernel-side honor
of THREAD_CREATE_START_SUSPENDED, which procmgr will use to install
VFS views before threads run.

Refs #71."
```

---

## Task 2: suspendprobe binary + container

**Files:**
- Create: `userspace/suspendprobe/Cargo.toml`
- Create: `userspace/suspendprobe/src/main.rs`
- Create: `containers/suspendprobe/Cluufile`
- Modify: `Cargo.toml` (workspace `members`, `default-members`, profile blocks)

The probe is the kernel-level unit test for `THREAD_CREATE_START_SUSPENDED`. It will FAIL until Task 5 lands kernel support — that's the TDD red phase.

- [ ] **Step 1: Create the probe Cargo.toml**

Create `userspace/suspendprobe/Cargo.toml`:

```toml
[package]
name = "cluu-suspendprobe"
version = "0.1.0"
edition.workspace = true
authors.workspace = true
license.workspace = true

[[bin]]
name = "suspendprobe"
path = "src/main.rs"

[dependencies]
libcluu = { path = "../libcluu", features = ["posix"] }
```

(Cross-check against `userspace/mountprobe/Cargo.toml` if any field differs — match the sibling exactly. Profile blocks live in the workspace root, not here.)

- [ ] **Step 2: Create the probe source**

Create `userspace/suspendprobe/src/main.rs`:

```rust
//! Verifies the kernel honors THREAD_CREATE_START_SUSPENDED:
//! a thread created with that flag must not run until thread_resume is called.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use core::sync::atomic::{AtomicU32, Ordering};
use libcluu::syscall::{
    space_create, thread_create, thread_resume, thread_destroy,
    yield_cpu, THREAD_CREATE_START_SUSPENDED,
};
use libcluu::debug_print;

static RAN: AtomicU32 = AtomicU32::new(0);

#[no_mangle]
extern "C" fn child_entry() -> ! {
    RAN.store(1, Ordering::SeqCst);
    loop { yield_cpu(); }
}

const STACK_BYTES: usize = 16 * 1024;
static mut CHILD_STACK: [u8; STACK_BYTES] = [0; STACK_BYTES];

fn yield_some(times: u32) {
    for _ in 0..times {
        yield_cpu();
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // Create child thread in the SAME address space (ours) suspended.
    // Use our own space token (TOKEN_SELF or equivalent — match how
    // pthread_create gets the current space; if no helper exists, the
    // probe falls back to space_create + duplicate, but the simpler
    // path is to reuse our own).
    //
    // For test simplicity we use a fresh space if needed; the only thing
    // that matters is whether the child runs.
    let space = match space_create(libcluu::syscall::TOKEN_SELF) {
        Ok(s) => s,
        Err(e) => {
            let _ = debug_print(&alloc::format!("suspendprobe: FAIL space_create {:?}", e));
            return 1;
        }
    };
    let stack_top = unsafe { CHILD_STACK.as_mut_ptr().add(STACK_BYTES) as usize };
    let child = match thread_create(
        space,
        child_entry as usize,
        stack_top,
        128,
        THREAD_CREATE_START_SUSPENDED,
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = debug_print(&alloc::format!("suspendprobe: FAIL thread_create {:?}", e));
            return 1;
        }
    };

    // Yield several times. If the kernel honored the flag, child has not run.
    yield_some(8);
    if RAN.load(Ordering::SeqCst) != 0 {
        let _ = debug_print("suspendprobe: FAIL ran before resume");
        let _ = thread_destroy(child);
        return 1;
    }

    // Resume — child should now run.
    if let Err(e) = thread_resume(child) {
        let _ = debug_print(&alloc::format!("suspendprobe: FAIL thread_resume {:?}", e));
        let _ = thread_destroy(child);
        return 1;
    }
    yield_some(8);
    if RAN.load(Ordering::SeqCst) != 1 {
        let _ = debug_print("suspendprobe: FAIL did not run after resume");
        let _ = thread_destroy(child);
        return 1;
    }

    let _ = debug_print("suspendprobe: PASS suspended-thread did not run before resume");
    let _ = thread_destroy(child);
    0
}
```

NOTE: cross-reference against `userspace/mountprobe/src/main.rs` (Task 9 of the mount-policy plan, commit 70931ec) for the exact runtime/main signature and any required panic handler boilerplate. If `space_create` / `TOKEN_SELF` aren't in scope, check the actual public API in `userspace/libcluu/src/syscall.rs` and adapt the imports. The semantic test is: create-suspended → assert not run → resume → assert run; the specific syscalls used to set up the child are implementation detail.

If `space_create(TOKEN_SELF)` is the wrong shape, an alternative is to spawn a child thread in the probe's own space using whatever helper `pthread_create` uses internally (see `userspace/libcluu/src/posix/pthread.rs:397-405`). Pick the simpler path that compiles.

- [ ] **Step 3: Create the container Cluufile**

Create `containers/suspendprobe/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo build --manifest-path userspace/suspendprobe/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/suspendprobe.elf /bin/suspendprobe
ENTRYPOINT /bin/suspendprobe
```

- [ ] **Step 4: Register workspace member**

In root `Cargo.toml`:
- Add `"userspace/suspendprobe"` to the `members = [...]` list, alphabetically (between `userspace/stack-string` and `userspace/timeserver` or similar; match the sibling pattern set by mountprobe in commit 70931ec).
- Add it to `default-members = [...]` if other probes are listed there.
- Add `[profile.dev.package."cluu-suspendprobe"]` and `[profile.release.package."cluu-suspendprobe"]` blocks matching siblings (e.g. `cluu-mountprobe`).

Run `grep -n mountprobe /home/vlb2bp/git/cluu/Cargo.toml` to find exact line numbers and replicate for suspendprobe.

- [ ] **Step 5: Build to verify the new crate compiles**

Run: `cargo xtask build`
Expected: clean build. The probe compiles against the new `thread_create` signature. The container manifest is written to `target/containers/suspendprobe/manifest.toml`.

- [ ] **Step 6: Commit**

```bash
git add userspace/suspendprobe containers/suspendprobe Cargo.toml
git commit -m "suspendprobe: container that verifies kernel SUSPENDED start

Probe creates a child thread with THREAD_CREATE_START_SUSPENDED, yields
to give the scheduler a chance, asserts the child has not written its
'ran' marker. Then calls thread_resume and asserts the marker IS set.

Refs #71."
```

---

## Task 3: Harness wiring for `kernel_suspended_thread`

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Register the harness case**

Edit `scripts/harness_cases.conf`. Insert alphabetically — `kernel_suspended_thread` sorts before `l2_*` cases:

```
kernel_suspended_thread|full|MARKER_MODE=kernel_suspended_thread TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

- [ ] **Step 2: Register the autostart command**

Edit `scripts/harness_case_defaults.sh`. Add a case arm before `l2_argv`:

```sh
            kernel_suspended_thread)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn suspendprobe"
                ;;
```

- [ ] **Step 3: Register required markers**

Edit `scripts/harness_run.sh`. Add a case arm in the `required_markers` switch (near `l2_mount_private`):

```sh
    kernel_suspended_thread)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "suspendprobe: PASS suspended-thread did not run before resume"
        )
        ;;
```

- [ ] **Step 4: Commit**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: register kernel_suspended_thread case

Spawns suspendprobe under shell. Required marker is the probe's PASS
line. Today this case will FAIL because the kernel doesn't yet honor
THREAD_CREATE_START_SUSPENDED — that's the TDD red phase. Task 5 makes
it green.

Refs #71."
```

---

## Task 4: Confirm the test FAILS (TDD red)

**Files:**
- No code changes — verification only.

- [ ] **Step 1: Run the new harness case**

Run: `RUN_WAIT=45 MARKER_MODE=kernel_suspended_thread bash scripts/harness_run.sh 2>&1 | tail -10`
Expected: FAIL — marker `suspendprobe: FAIL ran before resume` appears in the serial log (or `*** REQUIRED SUCCESS MARKERS MISSING ***`). The PASS marker does NOT appear.

This confirms the kernel today schedules the thread regardless of the flag. The probe and harness wiring are correct; the kernel-side fix is what's missing.

- [ ] **Step 2: No commit. Move to Task 5.**

---

## Task 5: Kernel — honor `THREAD_CREATE_START_SUSPENDED`

**Files:**
- Modify: `kernel/src/syscall/handlers.rs:831-895` (`invoke_thread_create`)

`ThreadFlags::SUSPENDED` already exists at `kernel/src/sched/thread.rs:143`. The scheduler already skips threads with this flag (otherwise `thread_suspend` wouldn't work). The change is: read `args.arg6` as flags; if bit 0 is set, OR `ThreadFlags::SUSPENDED` into the new thread's flags before `ThreadManager::add_thread`.

- [ ] **Step 1: Add the flag bit constant**

In `kernel/src/syscall/handlers.rs` near the other syscall-level constants (search for `const` definitions at the top of the file or near `invoke_thread_create`):

```rust
/// Flag bit on `invoke_thread_create`'s `args.arg6`: create the thread
/// SUSPENDED. Userspace must call `thread_resume` to make it runnable.
const THREAD_CREATE_START_SUSPENDED: u64 = 0x1;
```

- [ ] **Step 2: Read `args.arg6` and set the flag**

In `kernel/src/syscall/handlers.rs::invoke_thread_create` (around line 845, after the `priority` parsing), insert:

```rust
    let create_flags = args.arg6 as u64;
```

Then in the same function, locate the block that constructs `ThreadFlags` (around line 870):

```rust
    let flags = if ThreadManager::is_init_mode() {
        ThreadFlags::COOPERATIVE
    } else {
        ThreadFlags::empty()
    };
```

Replace with:

```rust
    let mut flags = if ThreadManager::is_init_mode() {
        ThreadFlags::COOPERATIVE
    } else {
        ThreadFlags::empty()
    };
    if create_flags & THREAD_CREATE_START_SUSPENDED != 0 {
        flags = flags.with(ThreadFlags::SUSPENDED);
    }
```

The `flags` value is then passed into `Thread::new` at the existing call site, unchanged.

- [ ] **Step 3: Build the kernel**

Run: `cargo xtask build`
Expected: clean build.

- [ ] **Step 4: Run the suspendprobe — verify it now PASSES**

Run: `RUN_WAIT=45 MARKER_MODE=kernel_suspended_thread bash scripts/harness_run.sh 2>&1 | tail -5`
Expected: `No faults detected and all required markers found.` Serial log contains `suspendprobe: PASS suspended-thread did not run before resume`.

If the probe still FAILs, debug:
- Confirm `args.arg6` is what userspace passes — print it from `invoke_thread_create` and compare to the libcluu `invoke()` arg ordering (`syscall.rs::invoke`).
- Confirm `ThreadFlags::SUSPENDED` is the same flag the scheduler honors. Search for `ThreadFlags::SUSPENDED` references in `kernel/src/sched/`.

- [ ] **Step 5: Commit**

```bash
git add kernel/src/syscall/handlers.rs
git commit -m "kernel: honor THREAD_CREATE_START_SUSPENDED in invoke_thread_create

Reads args.arg6 as flags. Bit 0 (THREAD_CREATE_START_SUSPENDED) means
the new thread enters scheduler with ThreadFlags::SUSPENDED set —
reuses the existing suspend mechanism that thread_suspend uses. The
scheduler already skips suspended threads.

Userspace contract: caller must invoke thread_resume to make the
thread runnable. Closes the spawn race for procmgr's set_view path
(refs #71); the matching procmgr migration follows in subsequent tasks.

Test: kernel_suspended_thread harness case — suspendprobe creates a
child SUSPENDED, yields, asserts marker not set, resumes, asserts
marker set."
```

---

## Task 6: procmgr — `spawn_service_with_env` accepts thread_flags

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (`spawn_service_with_env` signature + thread_create call site at ~line 3765)

This is a silent passthrough — all existing callers pass `0`, no behavior change. Sets up the parameter so Task 8's helper can pass `THREAD_CREATE_START_SUSPENDED`.

- [ ] **Step 1: Add `thread_flags: usize` parameter to `spawn_service_with_env`**

In `userspace/procmgr/src/main.rs`, find `fn spawn_service_with_env` (around line 3520). Add `thread_flags: usize` as the LAST parameter of the function signature.

- [ ] **Step 2: Forward the parameter to `thread_create`**

Around line 3765, the existing call (already updated in Task 1 Step 4):

```rust
        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority, 0)?;
```

Replace with:

```rust
        let thread_token = thread_create(space_token, entry_point, SERVICE_STACK_TOP, priority, thread_flags)?;
```

- [ ] **Step 3: Update all `spawn_service_with_env` callers to pass `0`**

The existing call sites (line numbers as of HEAD `bb466cb`): 916, 1111, 1414, 2125, 2347, 2556, 3371, 3499, 4539. Each is a `match self.spawn_service_with_env(...)` or `self.spawn_service_with_env(...)` invocation. Add a trailing `, 0` to the argument list.

Run `grep -n 'spawn_service_with_env' userspace/procmgr/src/main.rs` to enumerate the actual sites — the line numbers may have drifted by ±10 lines.

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: clean build. No behavior change because all callers pass `thread_flags=0`.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: spawn_service_with_env gains thread_flags pass-through

Silent extension: all existing callers pass 0 (default), no behavior
change. Sets up the parameter so the upcoming view-aware spawn helper
can pass THREAD_CREATE_START_SUSPENDED.

Refs #71."
```

---

## Task 7: procmgr — add `spawn_service_and_register_view` helper

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (new helper near other spawn helpers)

The helper combines suspended-create + register-view + resume. It also handles the deferred-view path (when `vfs_endpoint` isn't ready yet) by recording the thread that needs eventual resume.

- [ ] **Step 1: Add a `pending_view_resume` map**

In `userspace/procmgr/src/main.rs`, find the `Procmgr` struct (or whatever holds the procmgr's mutable state — search for `struct Procmgr` or `pub struct ServiceManager`). Add a field:

```rust
    /// Threads created SUSPENDED while VFS wasn't ready. When the deferred
    /// view eventually installs, the thread must be resumed. Keyed by the
    /// thread tid (same key the deferred view list uses).
    pending_view_resume: BTreeMap<usize, usize>, // tid -> thread_token
```

Initialize it as `BTreeMap::new()` in the same constructor where other fields like `pid_to_view` are initialized.

- [ ] **Step 2: Drain the resume queue when the deferred view installs**

Find `queue_pending_vfs_view` (search for `fn queue_pending_vfs_view` and `fn drain_pending_vfs_views` or similar). The drain path runs when `vfs_endpoint` becomes available. Wherever the deferred SET_VIEW is sent successfully, also resume:

```rust
                if let Some(token) = self.pending_view_resume.remove(&tid) {
                    if let Err(e) = thread_resume(token) {
                        let _ = debug_print(&format!(
                            "procmgr: deferred thread_resume tid={} err={:?}", tid, e));
                    }
                }
```

This block runs INSIDE the loop that drains pending views, immediately after the `send_vfs_set_view` succeeds for that tid.

If the existing code structure makes this nontrivial (e.g. there's no explicit drain function), the smaller change is to track resume separately: when `register_vfs_view_for_thread` falls through to `queue_pending_vfs_view`, also push the thread_token to a resume queue, and drain it in lockstep.

- [ ] **Step 3: Add the `spawn_service_and_register_view` helper**

Near the existing helpers (search for `fn spawn_service_with_env`), add:

```rust
/// Spawn a service and install its VFS view atomically. Creates the thread
/// SUSPENDED so the view is guaranteed to land at VFS before the thread's
/// first IPC call. Resumes the thread on success; destroys it on
/// thread_resume failure (no leaks).
///
/// If VFS endpoint isn't ready yet (early boot), the view installation is
/// deferred — `pending_view_resume` records the thread_token so the eventual
/// drain resumes it. Caller doesn't need to track this.
fn spawn_service_and_register_view(
    &mut self,
    /* SAME ARGS AS spawn_service_with_env, BUT WITHOUT thread_flags */
    /* PLUS: */
    view_mounts: &ViewMountList,
    profile_for_view: CapProfile,
    container_id_for_view: u64,
) -> Result<SpawnResult> {
    let result = self.spawn_service_with_env(
        /* ... forward all args ... */,
        THREAD_CREATE_START_SUSPENDED,    // thread_flags
    )?;

    // Install the view. If the endpoint is ready, the SET_VIEW message
    // goes into VFS's mailbox right now (before the thread runs). If the
    // endpoint isn't ready, the view is queued and we defer the resume.
    self.register_vfs_view_for_thread(
        result.thread_token, view_mounts, profile_for_view, container_id_for_view,
    );

    // If the view actually installed (vfs_endpoint != 0 and send_vfs_set_view
    // returned Ok), resume now. Otherwise the deferred path will resume.
    if self.vfs_endpoint != 0 {
        if let Err(err) = thread_resume(result.thread_token) {
            let _ = thread_destroy(result.thread_token);
            return Err(err);
        }
    } else {
        // VFS not ready; record for deferred resume.
        let tid = thread_get_id(result.thread_token).unwrap_or(0);
        self.pending_view_resume.insert(tid, result.thread_token);
    }

    Ok(result)
}
```

EXACT signature: copy `spawn_service_with_env`'s signature verbatim, then drop the `thread_flags: usize` parameter (the helper hardcodes `THREAD_CREATE_START_SUSPENDED`), and add the three view-related parameters (`view_mounts`, `profile_for_view`, `container_id_for_view`). Match the surrounding code's style for borrowed slices vs owned Vecs — `&ViewMountList` if callers can borrow, `Vec<...>` if they need to consume.

NOTE: the call to `register_vfs_view_for_thread` is the EXISTING method (no change to its signature). It internally checks `vfs_endpoint == 0` and queues if needed; the helper distinguishes the two paths to decide whether to resume now or defer.

- [ ] **Step 4: Build**

Run: `cargo xtask build`
Expected: clean build. The helper exists but has no callers yet — expect a `dead_code` warning. Suppress with `#[allow(dead_code)]` on the helper and add a comment `// Called by Task 8's migration.`. Remove the suppression in Task 8 once callers exist.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: add spawn_service_and_register_view helper

Helper wraps suspended-create + register-view + resume so the new
thread can never make a VFS call before its view is installed.
Handles the early-boot deferred-view path via pending_view_resume
bookkeeping.

Not yet called — Task 8 migrates the nine spawn-with-view sites to
use it. dead_code suppression is intentional and removed once callers
exist.

Refs #71."
```

---

## Task 8: procmgr — migrate the nine spawn-with-view sites

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (nine call sites)

Each site today does `spawn_service_with_env(...) + register_vfs_view_for_thread(thread_token, view, profile, container_id)`. Switch to the new helper.

- [ ] **Step 1: Find the call sites**

Run: `grep -n 'register_vfs_view_for_thread' userspace/procmgr/src/main.rs`
Expected output: lines 715, 922, 1135, 1424, 2149, 2369, 2578, 3234, 3399, 4654 (line numbers approximate; use the actual grep output).

Line 715 is INSIDE `register_vfs_view_for_thread` itself (the recursive call from `clear_vfs_view_for_tid`) — skip it. The other 9 are call sites.

- [ ] **Step 2: Migrate each site**

For each of the 9 call sites, the pattern today is:

```rust
match self.spawn_service_with_env(
    /* args */,
) {
    Ok(result) => {
        // ... bookkeeping (pid_to_*, etc.) ...
        self.register_vfs_view_for_thread(result.thread_token, &view_mounts, profile, container_id);
        // ... more bookkeeping ...
    }
    Err(e) => { ... }
}
```

Replace with:

```rust
match self.spawn_service_and_register_view(
    /* args, omit thread_flags */,
    &view_mounts,
    profile,
    container_id,
) {
    Ok(result) => {
        // ... bookkeeping (pid_to_*, etc.) ...
        // (the register_vfs_view_for_thread call is now inside the helper — remove it)
        // ... more bookkeeping ...
    }
    Err(e) => { ... }
}
```

Each site has slightly different surrounding code (it might be `match self.spawn_service_with_env(args, 0) {` after Task 6, since Task 6 added `, 0` to all callers). Drop the `, 0` (since the new helper doesn't take thread_flags), and add the three view parameters.

CRITICAL: do this site-by-site, build between each migration to catch type-mismatch errors early. Use `cargo xtask build` after each site.

- [ ] **Step 3: Remove `#[allow(dead_code)]` from the helper**

Now that there are callers, drop the suppression added in Task 7 Step 4.

- [ ] **Step 4: Build the full system**

Run: `cargo xtask build`
Expected: clean build, no warnings about `spawn_service_and_register_view` being unused.

- [ ] **Step 5: Run `m1_recv` smoke test**

Run: `MARKER_MODE=m1_recv bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: PASS. Confirms basic spawn flow still works.

- [ ] **Step 6: Run `l2_rm` (the original race victim)**

Run: `RUN_WAIT=45 MARKER_MODE=l2_rm bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: PASS. The race window is now closed.

- [ ] **Step 7: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: migrate spawn-with-view sites to use the suspend-bracket helper

Nine sites that today do spawn_service_with_env + register_vfs_view_for_thread
now use spawn_service_and_register_view, which suspends the new thread,
installs the view, and resumes — guaranteeing the view lands at VFS
before the thread's first IPC call.

Closes the race that caused l2_argv, l2_sigint, f13_detach_survive,
and l2_rm to flake post-mount-policy.

Refs #71."
```

---

## Task 9: Add `harness_repeat.sh` helper script

**Files:**
- Create: `scripts/harness_repeat.sh`

A simple wrapper that runs a single MARKER_MODE N times and reports the pass/fail tally. Used to gate Task 10's race sweep.

- [ ] **Step 1: Create the script**

Create `scripts/harness_repeat.sh`:

```bash
#!/bin/bash
# Run a single harness case N times, report the pass/fail tally.
#
# Usage: bash scripts/harness_repeat.sh <CASE> <N> [extra-env=val ...]
#
# Example: bash scripts/harness_repeat.sh l2_rm 10 RUN_WAIT=45

set -u

CASE=${1:?usage: harness_repeat.sh <case> <n> [extra-env...]}
N=${2:?usage: harness_repeat.sh <case> <n> [extra-env...]}
shift 2

PASS=0
FAIL=0
FAILED_RUNS=()

for i in $(seq 1 "$N"); do
    output=$(env "$@" MARKER_MODE="$CASE" bash scripts/harness_run.sh 2>&1)
    if echo "$output" | grep -q "No faults detected and all required markers found"; then
        PASS=$((PASS + 1))
        echo "Run $i: PASS"
    else
        FAIL=$((FAIL + 1))
        FAILED_RUNS+=("$i")
        echo "Run $i: FAIL"
    fi
done

echo "==================================="
echo "$CASE: $PASS/$N passed"
if [ $FAIL -gt 0 ]; then
    echo "Failed runs: ${FAILED_RUNS[*]}"
    exit 1
fi
exit 0
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/harness_repeat.sh`

- [ ] **Step 3: Smoke-test the script with m1_recv**

Run: `bash scripts/harness_repeat.sh m1_recv 2 'TEST_COMMAND_REPEAT=3' 'MIN_EXIT_COOKIES=3' 'CLUU_SHELL_AUTOSTART_CMD=spawn hello' 'RUN_WAIT=30'`
Expected: `m1_recv: 2/2 passed` (or similar; m1_recv is reliable so 2/2 should hold).

If this fails because of env-var passing quirks, debug the bash quoting until the script reliably runs cases. The exact env-passing form may need to be `env VAR=value MARKER_MODE=...`.

- [ ] **Step 4: Commit**

```bash
git add scripts/harness_repeat.sh
git commit -m "harness: add scripts/harness_repeat.sh for race-stability gates

Runs a single case N times and reports pass/fail tally. Needed for
race-fix verification (#71): gating Task 10 requires 10/10 standalone
runs of the previously-flaky cases.

Refs #71."
```

---

## Task 10: Race-targeted repeat sweep — verify the fix holds

**Files:**
- No code changes — verification only.

- [ ] **Step 1: Run l2_rm 10 times**

Run: `bash scripts/harness_repeat.sh l2_rm 10 'RUN_WAIT=45'`
Expected: `l2_rm: 10/10 passed`.

If less than 10/10, the race is not fully closed. Investigate by inspecting the failing run's serial log for `vfs: set_view` ordering relative to the spawned thread's first VFS call.

- [ ] **Step 2: Run l2_argv 10 times**

Run: `bash scripts/harness_repeat.sh l2_argv 10 'RUN_WAIT=45'`
Expected: `l2_argv: 10/10 passed`.

- [ ] **Step 3: Run l2_sigint 10 times**

Run: `bash scripts/harness_repeat.sh l2_sigint 10 'RUN_WAIT=45'`
Expected: `l2_sigint: 10/10 passed`.

- [ ] **Step 4: Run f13_detach_survive 10 times**

Run: `bash scripts/harness_repeat.sh f13_detach_survive 10 'RUN_WAIT=45'`
Expected: `f13_detach_survive: 10/10 passed`.

- [ ] **Step 5: No commit. Move to Task 11.**

If any of the four cases is below 10/10, STOP and investigate before proceeding. This is the race-fix proof point.

---

## Task 11: Negative control — confirm the bracket is what closes the race

**Files:**
- No code changes — verification only. Stash + restore.

- [ ] **Step 1: Stash the procmgr migration only**

Find the commit that migrated the spawn sites (Task 8). Run:

```bash
git log --oneline | head -10
```

Identify Task 8's commit SHA (commit message `procmgr: migrate spawn-with-view sites...`). Then:

```bash
git revert --no-commit <task-8-sha>
```

This reverts only the procmgr migration, leaving the kernel + libcluu + helper changes in place.

- [ ] **Step 2: Build and re-run l2_rm 10 times**

```bash
cargo xtask build
bash scripts/harness_repeat.sh l2_rm 10 'RUN_WAIT=45'
```

Expected: l2_rm flakes again — likely 6-8/10 PASS. This confirms the procmgr migration (the suspend-bracket) is what closed the race, not some other side effect.

- [ ] **Step 3: Restore the migration**

```bash
git revert --abort   # if revert is still in progress
# OR if revert was committed, redo it:
git revert --no-commit HEAD   # un-revert
git checkout -- userspace/procmgr/src/main.rs   # discard if still uncommitted
```

Verify the migration is back: `grep -c spawn_service_and_register_view userspace/procmgr/src/main.rs` should return ≥ 9.

- [ ] **Step 4: Re-build and re-confirm green**

```bash
cargo xtask build
bash scripts/harness_repeat.sh l2_rm 5 'RUN_WAIT=45'
```

Expected: 5/5 PASS again.

- [ ] **Step 5: No commit.**

---

## Task 12: Full harness matrix regression

**Files:**
- No code changes — final gate.

- [ ] **Step 1: Run the full suite**

Run: `bash scripts/harness_suite.sh > /tmp/harness_post_71.log 2>&1`
This will take ~30 min.

- [ ] **Step 2: Tally pass/fail**

Run: `grep -E 'case (PASS|FAIL):' /tmp/harness_post_71.log | sort | uniq -c | head`
Expected: ≥ 45 PASS, ≤ 1 FAIL. The acceptable fail is `l2_owner_deny` (#70 — known-broken, separate test-design issue).

- [ ] **Step 3: Compare to pre-fix baseline (39/46)**

Cases that should flip from FAIL to PASS post-fix:
- `l2_argv`, `l2_sigint`, `f13_detach_survive`, `l2_rm` — direct race victims.
- Stretch: `l2_fg`, `m5_fairness`, `p4_dev` — investigate case-by-case if their FAIL mode was the same race; if so, they may also flip to PASS.

If any case that was PASS pre-fix now FAILs, that's a regression caused by this work. STOP and investigate before merging.

- [ ] **Step 4: No commit.**

---

## Task 13: Performance check — spawn latency before/after

**Files:**
- No code changes — measurement only.

Per `feedback_spawn_perf_baseline` memory: spawn perf is the regression-guard for kernel/procmgr changes that touch the spawn path.

- [ ] **Step 1: Run b_spawn_warm 3 times**

Run: `for i in 1 2 3; do bash scripts/harness_run.sh MARKER_MODE=b_spawn_warm 2>&1 | grep -E 'spawn|cycles|elapsed' | head -5; done`

Capture the spawn-latency numbers reported by `benchprobe spawnonly`. Keep them.

- [ ] **Step 2: Run l2_jobchurn_heavy 3 times**

Run: `for i in 1 2 3; do bash scripts/harness_run.sh MARKER_MODE=l2_jobchurn_heavy 2>&1 | grep -E 'jobchurn|elapsed' | head -3; done`

Capture the wall-clock times.

- [ ] **Step 3: Compare against pre-fix baseline if available**

If a pre-fix baseline run exists in your notes, compare. Acceptable delta: < 5 % regression. The added cost per spawn is one extra `thread_resume` syscall (one IPC round-trip to the kernel) plus one extra flag arg through invoke — should be sub-microsecond at the syscall level.

If the regression is > 5 %, investigate. Common culprits would be: an unnecessary extra syscall in the helper, a missed `thread_resume` failure path, or the deferred-view bookkeeping growing unbounded.

If no pre-fix baseline, capture this run's numbers as the new baseline and continue.

- [ ] **Step 4: No commit.**

---

## Task 14: Memory updates and close #71

**Files:**
- Modify: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md`
- Modify: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_mount_policy.md`

- [ ] **Step 1: Update `project_mount_policy.md`**

Find the section "Open follow-ups" and remove the line about `#71`. Or rewrite it as `Resolved 2026-04-XX`.

- [ ] **Step 2: Update `MEMORY.md` index**

If the index has a line about `#71` or the spawn race, mark it resolved or remove it.

- [ ] **Step 3: Update `feedback_spawn_perf_baseline.md` if a new baseline was captured**

If Task 13 produced new baseline numbers, append them to the memory file.

- [ ] **Step 4: Mark task #71 completed in TaskList**

(Done via TaskUpdate by the controller, not the implementer.)

- [ ] **Step 5: Commit (no — memory is outside the repo)**

Memory files live outside the cluu git repo and are not version-controlled. No commit needed; just save the files.

---

## Self-Review

### Spec coverage

| Spec section | Plan task |
|---|---|
| Kernel: `flags: u64` arg + `THREAD_CREATE_START_SUSPENDED = 0x1` + reuse `ThreadFlags::SUSPENDED` | Task 5 |
| libcluu: 5-arg `thread_create` + `THREAD_CREATE_START_SUSPENDED` const | Task 1 |
| Update existing `thread_create` callers (pthread.rs, procmgr bare site) | Task 1 |
| Procmgr `spawn_service_and_register_view` helper | Task 7 |
| `pending_view_resume` bookkeeping for deferred-view path | Task 7 |
| Migrate 9 spawn-with-view sites | Task 8 |
| Failure path: `thread_resume` error → `thread_destroy` | Task 7 (helper body) |
| `userspace/suspendprobe/` + `kernel_suspended_thread` harness case | Tasks 2, 3 |
| `scripts/harness_repeat.sh` | Task 9 |
| Race-targeted repeat sweep (10/10 gate) | Task 10 |
| Negative control | Task 11 |
| Full harness matrix ≥ 45/46 | Task 12 |
| Performance check | Task 13 |
| Memory updates / close #71 | Task 14 |

All spec sections covered.

### Placeholder scan

No "TBD"/"TODO"/"implement later" markers in the plan. Tasks 7 and 8 contain explicit "match the surrounding code's style" notes for ambiguous integration shapes — those aren't placeholders, they're flexibility hooks for code differences the implementer will see firsthand.

### Type consistency

- `flags: usize` (libcluu) vs `flags: u64` (kernel) — wire-compatible because kernel reads `args.arg6` which is `u64` everywhere; libcluu's `usize` matches x86_64 register width.
- `THREAD_CREATE_START_SUSPENDED = 0x1` defined in BOTH libcluu (Task 1) and kernel (Task 5) — values match.
- `ViewMountList` (existing type) used in helper signature (Task 7) and call sites (Task 8) — matches `userspace/procmgr/src/main.rs:67`.
- `ThreadFlags::SUSPENDED` (kernel) is the existing flag at `kernel/src/sched/thread.rs:143` — Task 5 reuses it directly.
- `pending_view_resume: BTreeMap<usize, usize>` keyed by tid → thread_token — matches the deferred-view list's keying pattern.

All names and signatures consistent across tasks.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-25-set-view-race-fix.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task with two-stage review (spec compliance + code quality) between tasks. Fast iteration, context-isolated per task.

**2. Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batched with checkpoints for review.

**Which approach?**
