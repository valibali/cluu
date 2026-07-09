# Threading + ELF Load Performance Upgrades

> Created 2026-07-09. Synthesis of: doc/KB/source analysis, two explore-agent
> inventories (kernel scheduler, ELF load path), and a claude-fable-5 consult.
>
> Two assumption corrections from fable-5 (load-bearing):
> 1. Kernel is NOT generally preemptible — timer only reschedules at CPL=3 or
>    idle. This is the strongest correctness asset, and it's undocumented.
> 2. IPC is NOT purely rendezvous — endpoint.rs has buffered queues
>    (MAX_QUEUE_LEN=1024, waiting_senders backpressure). gotchas.md:110 is
>    outdated.

## Status: P0 + P1 implemented; P0-3 + P1-1/P1-2 deferred; side-quests done

### Shipped this session

- [x] **De-verbose output.log** — removed 912 ehci-core per-poll lines,
      137 cluuterm per-SGR/keystroke lines, ~128 kernel heap stack-trace
      lines, fixed kernel logger value-on-next-line bug (main.rs:193-197).
      Files: ehci-core/controller.rs:555, cluuterm/input.rs, cluuterm/tty_backend.rs:538-551,
      kernel/mm/heap.rs:93-114, kernel/main.rs:193-197.

- [x] **P0-2: async runtime WouldBlock livelock fix** — added `retry_queue`
      to `Runtime`; WouldBlock arm pushes there instead of `ready_queue`;
      `poll_ready` appends retry→ready at entry, then drains only what was
      ready. Breaks the livelock where a full downstream endpoint causes
      `poll_ready` to never return. File: libcluu/async_runtime.rs.

- [x] **64KB stack guard fix** — procmgr spawn sites (root-procmgr main.rs:4639,
      session-procmgr elf_spawn.rs:164) now use `map_stack_with_guard(..., 1)`
      instead of `map_stack(...)` / raw `space_map_range`. Previously
      procmgr-spawned processes had NO guard page (stack overflow = silent
      corruption, not a fault). init-spawned boot services already had guards.

### Not yet implemented (this plan)

- [ ] P0-1: Codify non-preemptible invariant (docs + debug asserts)
- [ ] Instrument: un-stub spawn-stage timers + H10/H9 overflow boot log
- [ ] P0-3: Parallel autostart
- [ ] P1-P2 items (see below)

---

## P0-1: Codify the "kernel non-preemptible except idle" invariant

### Problem

Every check-then-block sequence in the kernel is race-free ONLY because kernel
context can't be preempted (timer IRQ from kernel mode only reschedules if
current==idle; `interrupts.asm:661-668`, `idt.rs:1339-1347`). Only ISRs
interleave. Nothing states or enforces this. The 7-round ELF-spawn flake was
this class nibbling at the edge. SMP is a 2027 maybe — the invariant dies with
SMP and every check-then-block site needs revisiting.

### Change

**(a) threading.md** — new section after "Preemption":

```markdown
### The non-preemptible-kernel invariant (single-CPU)

Kernel syscall/IRQ-handler code runs to completion without preemption. The
APIC timer IRQ checks the interrupted CPL: if CPL=3 (userspace), it always
reschedules; if CPL=0 (kernel) it only reschedules when current==idle
(`interrupts.asm:661-668`, `idt.rs:1339-1347`). There is no `preempt_disable`
counter — non-preemptibility is structural, not counted.

This invariant is load-bearing for every check-then-block sequence:
- Futex `enqueue → block` (`handlers.rs:1993-2000`)
- Recv 3-tier arm/register/recheck (`handlers.rs:271-329`)
- Endpoint direct-deliver (`endpoint.rs:1022+`)
- `wake_thread` try_lock + queue_pending_wake fallback (`thread_manager.rs:941-972`)

If kernel preemption is ever introduced (SMP, or a preemptible-kernel
experiment), EVERY site listed above must be audited. The `PerCpuReplyMap`
`UnsafeCell<ReplyMap>` with `unsafe impl Sync` (`thread_manager.rs:179-192`)
is correct ONLY under this invariant + single-CPU.

**SMP note:** SMP is a post-v1 (2027) possibility. This invariant dies with
SMP. Every check-then-block site listed above needs a preempt_disable section
or a lock-ordering re-audit. The `cpu_id` field in `PerCpuData`
(`syscall.rs:84`) is a placeholder — no SMP abstraction is wired.
```

**(b) gotchas.md:110** — fix the outdated statement:

```markdown
CLUU's IPC is synchronous and rendezvous-based for `call`/`reply`, but
endpoints also have bounded buffered queues (`MAX_QUEUE_LEN=1024`,
`MAX_CALL_QUEUE_LEN=256`). When a queue is full, senders park in
`waiting_senders` and are woken on drain or on endpoint destruction
(`endpoint.rs:296-301, 574-614`). A `call` blocks the caller until the
server `reply`s; a `send` to a full queue blocks until space frees.
```

**(c) Debug asserts (kernel, debug builds only):**

Add `IN_ISR: AtomicBool` in `thread_manager.rs`, set/cleared in
`timer_interrupt_dispatch` / `schedule_next_from_fault` entry/exit. Add
`debug_assert!(!IN_ISR.load(Relaxed))` in `THREAD_REPOSITORY.lock()` and
`SCHEDULER.lock()` wrappers (the blocking-lock variants, not `try_lock`).
Zero release cost. Catches "someone adds a blocking lock reachable from ISR".

### Risk

None (docs + debug asserts). No behavior change in release.

### Verify

Harness full run with debug kernel; asserts never fire.

---

## P0-3: Parallel autostart (CLOSED — not worth pursuing)

> **Instrumented 2026-07-09 (SPAWN_PROFILE + ELF_PROFILE baseline run):**
>
> Serial autostart window = ~640ms (8 spawns × ~80ms avg). Per-stage split:
>
> | Stage | Range | % of total |
> |-------|-------|------------|
> | space_create | 137-258us | <1% |
> | elf_fetch (VFS IPC round-trip) | 53-151ms | **81-94%** |
> | map_segments | 1.5-2.5ms | 2-4% |
> | stack_map | ~2ms | 2-3% |
> | thread_start | ~3ms | 3-5% |
>
> VFS map_elf breakdown: request→elf_cached 3-6ms (P2-prewarm cache hit),
> elf_cached→segments_mapped 14-20ms (space_map_range syscalls),
> segments_mapped→reply 3-5ms.
>
> P2-prewarm already eliminated disk I/O — `elf_cached` is 3-6ms, not the
> 30+ms it would be cold. The dominant remaining cost is the IPC round-trip
> (procmgr→VFS call + reply) per spawn, NOT map compute.
>
> **Verdict: NOT worth pursuing.** Parallelizing 8 spawns saves ~475ms
> (640ms serial - 165ms max-single), which is 5.7% of the 8.3s boot wall
> clock. Requires giving procmgr an async runtime (moderate complexity:
> &mut self into async, completion-queue drain). The cost/benefit doesn't
> justify the complexity at pre-v1 scale. Revisit only if boot time
> becomes a user-facing complaint AND the IPC round-trip is confirmed as
> the bottleneck (not VFS recv-loop starvation, which P0-3 wouldn't fix
> anyway since VFS is single-threaded).

### Prerequisite: instrument first (user's Q3 answer)

Before implementing, un-stub the spawn-stage timers to get a baseline:

**procmgr main.rs:728** — un-stub `log_spawn_stage`:
```rust
fn log_spawn_stage(&self, seq: usize, stage: &str, start_ts: u64) {
    const SPAWN_PROFILE: bool = false; // flip to true to measure
    if !SPAWN_PROFILE {
        return;
    }
    let now = self.clock_sample();
    let elapsed_us = now.saturating_sub(start_ts);
    let _ = debug_print(&format!(
        "procmgr: spawn[{}] {} +{}us", seq, stage, elapsed_us
    ));
}
```

**vfs main.rs:953** — un-stub `log_map_elf_stage` similarly.

**H10/H9 overflow boot log** — add to the existing telemetry snapshot in
`kstart` (or in procmgr's first telemetry read):
```rust
// In telemetry snapshot, after existing counters:
klibcluu::info("  pending_wake_overflow=");
klibcluu::log_dec(Info, "", pending_wake_overflow_count());
klibcluu::info("  deferred_fault_overflow=");
klibcluu::log_dec(Info, "", deferred_fault_overflow_count());
```

Run one boot with `SPAWN_PROFILE = true`, capture the per-stage split.
This tells you: (a) how the ~5s serial window splits between disk I/O,
VFS map compute, and procmgr work; (b) whether H10 has ever been nonzero.

### Implementation

`run_autostart` (`root-procmgr/main.rs:1103-1146`) currently spawns 6 services
strictly serially in a `for svc in &services` loop. Each `autostart_container`
call blocks until the service is fully spawned (manifest read → ELF map →
thread_create → resume).

procmgr already has the async runtime available. The change:

```rust
fn run_autostart(&mut self) {
    // ... parse autostart.toml as before ...

    // Phase 1: VFS is a hard dependency — spawn it first, serially.
    // (VFS must be ready before other services can open files.)
    let vfs_idx = services.iter().position(|s| {
        s.get_str("image") == Some("vfs")
    });
    if let Some(idx) = vfs_idx {
        let _ = self.autostart_container("vfs", &services[idx]);
    }

    // Phase 2: remaining services in parallel via async tasks.
    // VFS is single-threaded so disk reads still serialize, but manifest
    // parsing, map syscalls, thread_create/set_view/resume of service N
    // overlap with disk I/O of service N+1.
    //
    // Dependency edges: add `after = ["vfs"]` in autostart.toml for
    // services that need VFS. For now, all non-VFS services depend on
    // VFS (which we spawned in Phase 1), so they can all go parallel.
    let rt = match self.runtime.as_mut() {
        Some(rt) => rt,
        None => { /* procmgr doesn't have a runtime yet — see note below */ return; }
    };

    for (i, svc) in services.iter().enumerate() {
        if i == vfs_idx.unwrap_or(usize::MAX) { continue; }
        let image_name = match svc.get_str("image") {
            Some(n) => n.to_string(),  // owned for the async block
            None => continue,
        };
        // procmgr can't easily move &mut self into async — use the
        // completion-queue pattern: spawn a task that does the async
        // ELF fetch via IpcCallFuture, then push a completion that the
        // main loop drains to do the &mut self spawn work.
        //
        // ALTERNATIVE (simpler): keep autostart_container synchronous
        // but prefetch the ELF into VFS cache by issuing VFS_MAP_ELF
        // for all binaries BEFORE spawning any of them. The VFS cache
        // is 128MB and never evicts. First-spawn cost drops to just
        // the map syscalls (no disk I/O).
        //
        // The prefetch approach is simpler and doesn't require moving
        // &mut self into async. See "P2: bulk /bin pre-warm" below.
    }
}
```

**NOTE:** procmgr (`root-procmgr`) is currently single-threaded and does NOT
have an async runtime. Adding one is a moderate change. The SIMPLER win that
captures most of the benefit is the **bulk /bin pre-warm** (P2 below): wire
the existing `preload_marked_binaries` dead code at VFS startup. This fills
the VFS file cache for all /var/images/*/bin/* in one streaming read pass
before procmgr starts spawning. Then each autostart spawn hits the cache
instead of paying the ext2/virtio-blk read. Combined with the existing
`MAP_SHARE_PHYS` zero-copy text mapping, per-spawn cost drops to just the
map syscalls + thread create/resume.

**Recommended path:** P2-prewarm first (wire dead code, ~20 lines), measure,
then P0-3 if the remaining serial window still matters.

### Expected impact

- P2-prewarm alone: autostart-done 5.4s → est. 3.5-4s (eliminates disk I/O
  from the spawn path; manifest reads + map syscalls remain serial)
- P0-3 on top of P2-prewarm: autostart-done → est. 2.5-3s (overlaps the
  remaining serial compute)
- Login 9.8s → est. 6.5-7s (combined)

### Risk

Ordering — services with registry dependencies must express them. Today the
implicit order in autostart.toml encodes dependencies. For the parallel path,
add explicit `after = ["vfs"]` (or a 2-phase split: infra serial, rest
parallel). The prefetch approach has no ordering risk.

### Verify

COM2 boot-phase markers (0.5/3/5/5.4/9.8s timeline in boot.md). Baseline
first with SPAWN_PROFILE=true, compare after.

---

## P1 items

### P1-1: Lossless pending-wake (DEFERRED — measured zero overflow)
Instrumentation shows `pending_wake_overflow=0` across all harness runs.
Downgraded to P2. The 32-slot queue is sufficient at current scale.

### P1-2: Futex stale-entry dedup (DEFERRED — low frequency)
`ipc_stale_waiters` telemetry shows ~500 across a full boot+login cycle —
~2% of ~23K wait events. The stale entries are cleaned up on next wake
without correctness impact. The 16 bytes/TCB cost and code complexity
aren't justified until futex contention becomes a measured bottleneck.

### P1-3: Real waker + dead-task detection (DONE)
Replaced noop waker with RawWaker encoding `task_id`; `wake` pushes to
ready_queue via `CURRENT_RUNTIME`. Added `detect_dead_tasks()` — after
`poll_ready`, tasks that are Pending with no pending cookie are reaped
with a `debug_print` warning. Makes arbitrary futures correct and
silent-hang loud. File: libcluu/async_runtime.rs.

### P1-4: `async_server_main` skeleton (DONE — skeleton only)
New module `libcluu/server_main.rs` with `AsyncServerMain` struct
wrapping `Runtime` + recv loop helpers. Not adopted by any server yet.
Adoption in cluuterm/tty/kbd is future work.

### P1-5: Cancel cookies on downstream death (DONE)
Added `cookie_targets: BTreeMap<usize, usize>` (cookie → endpoint) to
`Runtime`. `cancel_endpoint(ep)` finds all pending cookies targeting
that endpoint, pushes them to `cancelled_cookies`, and re-queues the
waiting tasks. `IpcCallFuture::poll` checks `take_cancelled_cookie` in
the Waiting state and returns `Err(NotFound)`. No timeout — event-driven.

### P1-6: Dead fault-handler → reap faulted threads (DONE)
`mark_thread_dead` now mirrors the CALL_REPLY_MAP cleanup for
FAULT_REPLY_MAP: when the dead thread is a fault server
(`server_thread_id == thread_id`), the faulted threads it was handling
are killed via recursive `mark_thread_dead`. Safe because FAULT_REPLY_MAP
entries are removed before the recursive call, preventing re-entrancy.

### P1-7: Priority bands + idle watchdog (DONE)
- Kernel: `invoke_thread_set_priority` implemented (was a stub returning
  `NotImplemented`). New `ThreadManager::set_thread_priority` — updates
  priority and reschedules only if thread is Ready (not Suspended, not
  Blocked). Prevents adding a suspended thread to the scheduler run queue.
- Manifest: `priority` field added to `CachedManifest` + Cluufile TOML
  `exec.priority`. Default 96 (USER). Session-procmgr spawns at 96,
  session services at 128 (SYS), sudo/su at 96.
- Spawn wiring: `install_view_and_run` takes `priority` param, calls
  `thread_set_priority` after `thread_set_session`, before `thread_resume`.
  All 11 call sites updated.
- Idle watchdog: `pick_next` calls `idle_watchdog_dump` when it returns
  None. Scans THREAD_REPOSITORY for live non-idle threads (priority >= 32,
  not Blocked, not Suspended). If found, dumps tid+priority once (atomic
  flag prevents repeat). Diagnostic only — no recovery.

### P1-8: Backpressure for fire-and-forget fan-out (DONE — helper only)
New module `libcluu/coalesced_notify.rs` with `CoalescedNotify` struct.
Level-triggered: `notify(ep, label, key)` marks a pending bit and sends
one `ipc_send`. `ack(ep, label)` clears it. Duplicate `notify` before
`ack` is coalesced. `cancel_endpoint(ep)` drops all pending for that ep.
Not adopted by compositor yet — adoption is future work.

---

## Side-quests (not in original plan, done this session)

- [x] **USB HID scancode fix** — `usb-input/src/layout.rs` had a wrong
      HID→PS/2 mapping: `u` (HID 0x18) → 0x16 was emitting `z`, and `v`
      (HID 0x19) → 0x2F was wrong. Table corrected. Also disabled the
      `hu-layout` feature in `usb-input/Cargo.toml` (default features now
      `[]`) — the HU layout was remapping USB keys incorrectly; the PS/2
      path in `kbd` keeps `hu-layout` enabled (correct for PS/2 set 2).
      Live-verified: `u` types `u`.

- [x] **cpuburn probe** — new `userspace/c-programs/cpuburn.c` with three
      modes (`cpu`, `ipc`, `mixed`). `cpu` = tight arithmetic loop,
      `ipc` = call/reply storm against procmgr, `mixed` = both. Container
      at `containers/cpuburn/Cluufile`, wired in `xtask/src/main.rs`.
      Live-verified: `cpuburn cpu 50000` PASS, `cpuburn mixed 50000` PASS.

- [x] **s_stress_churn harness case** — new harness case replacing the
      removed `b_spawn_warm`. Uses `cpuburn mixed 200` + `cpuburn cpu 50 &`
      to exercise spawn + IPC + CPU under load. Markers in `markers.py`,
      catalog in `catalog.py`, defaults in `case_defaults.py`.

- [x] **Smaller ELFs** — `Cargo.toml` `[profile.release]`: `opt-level="z"`,
      `lto=true`, `strip=true`, `codegen-units=1`. Reduces userspace
      binary sizes for faster ELF load.

- [x] **CALL_REPLY_MAP insert-fail counter** — `kernel/src/telemetry.rs`
      + `thread_manager.rs`. Surfaces the 128-slot cliff if it ever fires.

- [x] **De-verbose output.log** — removed ~1400 lines of per-poll /
      per-keystroke / per-SGR debug spam from ehci-core, cluuterm, kernel
      heap, and logger.

- [ ] **top display bugs (UNRESOLVED)** — three issues: (1) table wraps
      by 1 char, (2) CPU% shows 0, (3) mem/heap shows `---`. Debug output
      was added to `session-procmgr/proc_query.rs` and
      `root-procmgr/main.rs:proc_query_stat` but never observed in serial
      — root cause unclear (possibly procfs routing, possibly top failing
      on readdir). Debug output removed before commit. Deferred.

---

## P2 items (not yet implemented)

### P2-prewarm: Bulk /bin pre-warm (ENABLED — post-boot responsiveness)
Wire the existing `preload_marked_binaries` dead code (`vfs/main.rs:4303`).
It walks `/var/images/*/manifest.toml`, scans for `preload = true`, reads
each `bin/` into `FileCache` via `cache_ext2_file`. Called at VFS startup
after ext2 mount. `PRELOAD` directive added to 7 boot-critical Cluufiles
(console, vtmgr, inputd, kbd, mouse, compositor, login).

**MEASURED 2026-07-09:** Preload fills 38 binaries in ~1.5s, adding ~2s to
boot wall-clock (7.2s → 9.2s). BUT post-boot `spawn` commands hit warm VFS
cache instead of paying ext2 read latency — the tradeoff is intentional:
slower boot for snappier interactive spawn. The `PRELOAD` directive is
per-Cluufile, so non-boot-critical containers can opt out.

**Boot time could be recovered** by moving preload after the "mounted"
signal (procmgr starts spawning while VFS preloads in background), but VFS
is single-threaded — the recv loop would starve during preload. The real
fix is P0-3 (parallel autostart / async procmgr), deferred as future work.

### P2-misc
- Smaller ELFs: strip + LTO + `opt-level="z"` + `panic=abort`
- `CALL_REPLY_MAP` 128-slot cliff: add telemetry counter on insert-fail
- M9 multi-slot TextSource: wait for named failure (C binary with split text)
- Fair share across sessions: skip pre-v1

---

## Clarifying-question answers (user's responses)

1. **H10 overflow:** "Don't know — instrument first" → add H10/H9 to boot
   telemetry snapshot (see P0-3 prerequisite), run spawn-storm harness,
   decide P1-1 vs P2.
2. **Revocation wakes waiting_senders?** "Not sure — audit" → **AUDITED:
   YES.** `destroy_endpoint_full` (`endpoint.rs:605-607`) wakes all
   `waiting_senders`. Cap-revocation unblock is intact. P1-8 stays
   userspace-only.
3. **Serial-load breakdown:** "Instrument one boot first" → un-stub
   `log_spawn_stage` + `log_map_elf_stage` (code shape above), run one boot,
   then commit to P0-3 vs P2-prewarm ordering.
4. **SMP:** "Maybe post-v1 (2027)" → P0-1 docs include the SMP site list
   and the "this invariant dies with SMP" warning.
5. **64KB stack:** investigated. Two issues: (a) 64KB fixed, no growth — P1
   item (manifest `stack_pages` field); (b) no guard page on procmgr spawns
   — **FIXED this session** (map_stack → map_stack_with_guard(..., 1)).
