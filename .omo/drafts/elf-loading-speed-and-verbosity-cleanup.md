# ELF Loading Speed — Analysis Handoff

## TL;DR

Boot-to-first-shell takes ~4.3s (kernel entry 12.190s → shell: ready 16.491s).
134 ELF map_elf OK calls in output.log. The first 6 services (registry,
timeserver, root-procmgr, vfs, virtio-blk, tpmd) boot in ~660ms. The
remaining ~3.6s is procmgr entering its run loop (13.780s) → login start
(14.213s) → shell ready (16.491s), which includes spawning 8+ children
(cluuterm, shell, session-procmgr, session-vfs, completion thread, etc).

This is an **analysis** handoff, not an implementation plan. The next
session should:
1. Profile where the 3.6s goes (per-spawn timestamps)
2. Identify whether ELF loading (cache_fill + map_cached_seg) is the
   bottleneck or whether IPC round-trips dominate
3. Propose concrete optimizations with expected savings

## Current ELF loading flow

```
procmgr/session-procmgr            VFS
  open(path) ─────────────────────→ handle_open
  ← fd ──────────────────────────────
  map_elf(fd, target_space) ─────→ handle_map_elf
                                     │
                                     ├─ cache_fill (if inode not cached)
                                     │   └─ read entire ELF into VFS heap
                                     │       (ext2 read via blkdev IPC)
                                     │
                                     ├─ parse ELF header
                                     │
                                     └─ for each PT_LOAD segment:
                                         map_cached_seg
                                           └─ share_phys=true: map same
                                              physical frame into target
                                              space (COW not needed for
                                              read-only text)
                                           └─ share_phys=false: alloc new
                                              frame, copy bytes, map
  ← OK + entry_point ───────────────
  thread_create + thread_resume
```

## Observations from output.log

### Cache behavior
- 22 `cache_fill` events — ELFs read from ext2 into VFS heap
- 0 explicit cache hits logged (but `cache_entries` grows monotonically:
  2→4→6→8→10→14→16→17→18→19...)
- Each `cache_fill` is a full-file read via blkdev IPC
- Subsequent maps of the SAME inode (e.g. shell spawned multiple times
  across sessions) SHOULD hit cache — but output.log doesn't have a
  "cache hit" log line, so this is unverified

### Per-spawn timing (first boot)
| Service | map_elf START | map_elf OK | Duration |
|---------|---------------|------------|----------|
| console | 13.866 | 13.873 | 7ms |
| vtmgr | 13.891 | 13.898 | 7ms |
| (service) | 13.932 | 13.956 | 24ms |
| (service) | 13.993 | 14.015 | 22ms |
| (service) | 14.052 | 14.063 | 11ms |
| (service) | 14.135 | 14.160 | 25ms |
| (service) | 14.195 | 14.209 | 14ms |
| session-procmgr | 16.183 | 16.190 | 7ms |
| shell (1st) | 16.251 | 16.310 | 59ms |

Shell is 59ms — the largest ELF. Most services are 7-25ms.

### Bottleneck hypothesis
ELF loading itself is fast (7-59ms per binary). The 3.6s gap between
procmgr run loop (13.780) and first shell:ready (16.491) is likely:
1. **IPC round-trips**: each spawn = open + map_elf + space_create +
   thread_create + VFS_SET_VIEW + thread_resume = 6+ IPC calls
2. **Sequential spawning**: children spawn one-at-a-time, each waiting
   for the previous to register
3. **Registry grant latency**: SUBSCRIBE → pending → register → grant
   → forward — multi-hop IPC per service dependency
4. **Session setup**: session-procmgr + session-vfs must spawn and
   register before cluuterm/shell can spawn

## What to investigate next

### 1. Profile per-spawn breakdown
Add timestamps to:
- `procmgr: spawn_unified START name=X`
- `procmgr: open OK fd=X`
- `procmgr: map_elf OK entry=X`
- `procmgr: space_create OK`
- `procmgr: thread_create OK`
- `procmgr: VFS_SET_VIEW OK`
- `procmgr: thread_resume OK`

This will show which IPC round-trip is the bottleneck.

### 2. Cache hit rate
Add a `cache hit inode=X` log in `FileCache::get` when an inode is
already cached. Run a multi-session workload (open 5 cluuterms) and
check whether shell/cluuterm ELFs are re-read from ext2 each time.

### 3. Parallel spawning
Can session-procmgr spawn cluuterm + shell in parallel? Currently
shell is spawned by cluuterm (cluuterm owns the shell), so they're
sequential by design. But session-procmgr + session-vfs could
potentially spawn in parallel.

### 4. ELF cache sharing across spaces
`share_phys=true` maps the same physical frame into multiple spaces
for read-only segments. This is already implemented. Verify it works
for shell spawned across multiple sessions (output.log shows 11
shell:ready events — are all sharing the same text frames?).

### 5. Demand paging
Currently the entire ELF is read into VFS heap (cache_fill) then
segments are mapped. Demand paging (map empty frames, page-fault on
access, read from ext2 on fault) would spread the cost over execution
but not reduce total I/O. Probably not worth it for small ELFs.

### 6. Pre-linking / share_phys for data segments
`share_phys=false` (data segments) allocates + copies per-space.
Could zero-fill BSS on first fault instead of copying. Minor win.

## Key files
- `userspace/vfs/src/main.rs:4655` — `handle_map_elf`
- `userspace/vfs/src/main.rs:4768` — `map_elf_segments`
- `userspace/vfs/src/main.rs:4787` — `map_elf_segment`
- `userspace/vfs/src/main.rs:458` — `FileCache` struct
- `userspace/vfs/src/main.rs:522` — `FileCache::get` (cache hit path)
- `userspace/session-procmgr/src/elf_spawn.rs` — spawn orchestrator
- `userspace/procmgr/src/main.rs` — root-procmgr spawn path

## output.log verbosity inventory (for cleanup)

The user noted output.log is "super verbose, many instrumentation
debug output left in there." Inventory:

### Kernel `[INFO]` (4-space indent) — 7694 lines
- `resource delta:` + 24 fields × 246 occurrences = ~6000 lines
  - Source: `kernel/src/telemetry.rs:392` `log_resource_delta()`
  - Called on every `thread_destroy` and `space_destroy`
  - **Fix**: gate behind a `TELEMETRY_VERBOSE` config flag, or only
    log on significant deltas (threads_live change > 5)
- `ep` + hex × 252 lines — endpoint creation log
- `ThreadID/Entry/Stack` × 1008 lines — per-thread-create dump
- `tokens_created/tokens_revoked` × 1 — fine

### Userspace `[USER]` — 6652 lines
- `vfs: open` × 446 — every VFS open. Useful for debugging, noisy for
  normal operation. Gate behind `VFS_TRACE` env var?
- `vfs: map_cached_seg` × 402 — per-segment map log. Same.
- `vfs: map_elf` × 268 — per-ELF map log.
- `vfs: handle_map_elf START` × 134
- `vfs: derive_child_fd` × 198
- `cluuterm: ansi sgr fg=XXXXXX` × 379 — per-color-change in renderer.
  Source: `tty_backend.rs:580`. This is harness-observable marker
  for color rendering tests. Gate behind a feature flag.
- `cluuterm: input ascii=XX` × 162 — every keystroke. Source:
  `input.rs:50`. Useful for debugging, noisy otherwise.
- `session-procmgr: elf_spawn` × 378 — per-spawn stage logging
- `registry: SUBSCRIBE` × 689 — every service subscription
- `registry-client: handle_grant_request` × 345
- `completion: cached N entries for X` × 300 — per-directory cache
- `compositor: ansi sgr` × 257
- `shellrc: sourcing` × 120
- `TRACE: reaped thread token X` × 122

### Recommended cleanup approach
1. Add a `debug_print_verbose!` macro that's compiled out in release
   builds (or gated by a runtime flag read from env var)
2. Move per-operation traces (open, map_cached_seg, ansi sgr, input
   ascii, SUBSCRIBE, handle_grant_request) to verbose
3. Keep lifecycle events (started, ready, registered, shutdown, exit)
   at info level
4. Keep `log_resource_delta` but only log on significant changes, not
   every thread_destroy

## Commits in this session
- `ee4b87c5` — session-procmgr PG_SIGNAL cleanup + remove
  patch_vfs_stdio_endpoints + wire PTS winsize
- `c19711c0` — Python gen2 harness (11 cases, event-driven)
