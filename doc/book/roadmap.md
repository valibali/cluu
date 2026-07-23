# Roadmap

CLUU's roadmap is a phased plan for taking the kernel from "solid 9/10" to a
shippable hobby OS. The kernel is finished enough; the remaining work is
userspace. This chapter is the distilled plan: what phase we are in, what
"done" means for each phase, and what we have explicitly chosen not to do.

The original prose has been distilled into this chapter. What follows is the
project's current state, not a copy of the original doc.

## State of the OS

CLUU is roughly 68K lines of Rust (kernel plus userspace) and 20K lines of
C and assembly (newlib port, boot stubs). 18 months of solo evenings and
weekends. The kernel is seL4-inspired, capability-based, single-CPU, and
audited at 9.4/10 (see the [audit chapter](audit/index.html)). IPC is
1,200 to 1,600 cycles for a full call/reply. SMAP, SMEP, Spectre
mitigations, and measured boot are in.

The kernel is 9/10. The apps are 3/10. This is a *finishing* problem, not
a proof-of-concept problem. Every hour spent polishing the kernel from
here is time not spent closing the apps gap.

## What CLUU is, and is not

CLUU is becoming a *usable hobby OS*: boot it, log in, navigate with a
shell, edit text, write a Python script, eventually browse a local web
page. TUI first. GUI is a post-2026 problem. Small and comprehensible,
not big and comprehensive.

CLUU is explicitly not:

- a Linux competitor (no Chromium, Postgres, systemd, no Linux personality
  beyond what newlib plus the compat layer already covers),
- a research artifact (no paper, no novel scheduler, no exotic capability
  calculus),
- a teaching kernel (clean code is a side effect, not a design goal),
- a performance vehicle (IPC is already fast enough; no more kernel
  microbenchmarks until something user-visible demands one).

## Kernel freeze

From 2026-04-21 through approximately 2026-10-21 (six months), the kernel
is frozen. No speculative kernel work. No new audits. No IPC Tier-2
optimizations. No SMP. No new security hardening. No GUI planning.

The only exception is a kernel bug that actively blocks a phase exit
criterion. The rule:

> Every kernel commit during the freeze MUST reference, in its first
> line, the userspace test case or failing scenario that forced it.
>
> Example: `Fixes shell/test_cd.sh hang in recv() when stdin is a pipe to
> a dead child (Phase 1).`
>
> If you cannot name the userspace failure, the commit does not go in. You
> are drifting.

The scope of any freeze-exception fix is exactly what the test needs. No
adjacent cleanup, no "while I'm here" refactors, no prophylactic changes.

The freeze ends on ~2026-10-21 or when Phase 3 completes, whichever is
*later*. At that point the question is: *is the kernel still the
bottleneck?* If the apps gap is closed enough to ship Phase 6, ship. Kernel
polish is a v1.1 problem.

### Commit discipline (separate from the freeze)

- No uncommitted WIP older than 3 days on `develop`. If 72h passes and the
  branch is still dirty, stop new work, split into logical commits, push.
- Prefer small bundled PRs over heroic 40-day ones. Review is cheap;
  retro-review of 40 days of entangled changes is not.
- Every commit message names *why*, not *what*. The diff tells you what.

## Drift patterns

Four patterns have cost real weeks in the past. Naming them makes them
harder to rationalize in the moment.

1. **"Quick kernel cleanup while I think."** A warmup kernel task before
   the messy userspace work of the day. Counter: if you open `kernel/src/`
   without a specific userspace testcase referenced, close it. No
   testcase, no kernel edit.
2. **"This optimization will pay back in Phase 4."** Speculative kernel
   work justified by imagined future needs. Counter: Phase 4 is not here.
   The optimization is imaginary; the lost week is not. Note the idea
   out-of-band and move on.
3. **"Just one more audit."** Measuring the state of the system instead of
   advancing it. Counter: if a prior audit document exists, re-read it.
   Do not write a new one. A new audit file during the freeze is a drift
   symptom.
4. **40-day WIP.** Finishing-state-avoidance via perpetual work in
   progress. Counter: the 3-day WIP rule above. This is the *only* rule
   whose violation triggers a hard stop.

Honorable mentions: writing yet another `*probe` container instead of the
shell builtin it probes; entertainment-driven prioritization ("wire up
Quake so it looks cool"); re-planning as a substitute for executing.

## Phases

Each phase has a goal, exit criteria (user-visible, all must be true), and
known unknowns. No dates. A phase is done when the capabilities exist.

### Phase 0: Seal the 40-day WIP (DONE)

Goal: get `develop` into a reviewable, mergeable, CI-verified state.

Exit criteria (all met):

- R1 (SysV ABI preservation check) committed.
- R2 (RDRAND zero-salt fix) committed.
- WIP split into 4 logical commits: IPC Tier-1 optimizations, security
  hardening (SMAP/SMEP/Spectre/retpoline), async notifications (A2), TPM
  plus userspace auth.
- `bash scripts/harness_matrix.sh` runs green end-to-end.
- Every commit message names *why*, not *what*.
- `git status` clean on `develop`.

Allowed kernel work: whatever was needed to make the split clean and the
matrix green. Nothing else.

### Phase 1: Shell usability (DONE 2026-04-27)

Goal: the shell feels like a shell, not a launcher.

Exit criteria (all met):

- `cd /path`, `cd ..`, `cd` (home), `pwd` work across sub-shells.
- Current working directory persists through spawned processes.
- `cat foo.txt | grep pattern | head -5` runs end-to-end with real pipe
  execution.
- Redirection works: `> file`, `>> file`, `< file`.
- `mkdir`, `rm`, `rm -r`, `cp`, `mv`, `grep`, `head`, `tail`, `wc` exist
  and behave like their Unix counterparts for the common cases.
- Line editing: backspace, left/right arrow, home, end, Ctrl-A, Ctrl-E,
  Ctrl-K, Ctrl-U.
- Command history: up/down arrows retrieve previous commands within a
  session. Persistent history deferred.
- Tab completion for files and directories.
- `echo $?` returns the last command's exit status correctly.

Allowed kernel work: only bugs surfaced by Phase 1 tests, specifically the
known `read(0, ...)` TTY deadlock.

Known unknowns (retrospective): pipe execution did expose fd-inheritance
bugs in the spawn path. Fixed in procmgr/VFS, no kernel changes forced.

### Phase 2: Write code in CLUU (DONE 2026-05-06)

Goal: you can run a script and edit a file without leaving the OS.

Exit criteria (all met):

- MicroPython starts, runs a one-liner, and reads a file from disk
  (`open('/etc/users.toml').read()`).
- MicroPython writes a file (`open('/tmp/x.txt', 'w').write('hi')`).
- MicroPython REPL handles multi-line input, Ctrl-C, Ctrl-D.
- One text editor works: `/bin/edit`, a minimal vi-flavored TUI built from
  scratch (~3.8k LOC, piece-table, vim keymap, `:set`, atomic save).
- You can edit a script, save it, run it, and see output without
  rebooting (verified with `ls / edit hello.txt / :w / :q / ls`).

Allowed kernel work: `sched_yield` only if MicroPython's stub could not be
worked around in userspace; plus any I/O completeness bug found by the
editor port.

Closing notes:

- MicroPython end-to-end confirmed 2026-04-29 (`l2_mp_etc` green, REPL
  interactive).
- Editor shipped 2026-04-30. `:w` persistence and the
  `ls / edit / :w / :q / ls` cycle confirmed after fixing a deterministic
  console crash on the second `ls`.
- The console crash had a kernel root cause: PI24's `MAP_SHARE_PHYS`
  shared physical frames between VFS's ELF cache and consumer address
  spaces *without refcounting them in `frame_registry`*. Disabled in
  `vfs/main.rs` as the fastest correctness fix; spawn cost is back to
  pre-PI24 (~600ms hot, per-segment memcpy). Re-enabling it correctly is
  the first item in the Phase 2 to Phase 3 transition. The named
  userspace failure is the console wild-jump that triggered the
  investigation.
- virtio-blk modernized 2026-05-07 on branch `virtio-modern`: rebuilt on a
  reusable `userspace/virtio-core/` crate with virtio 1.0+ modern PCI
  transport, IRQ-driven completion, and a public `BlkSessionClient` IPC
  surface. `l2_blk_basic` and `l2_blk_concurrent` green; system boots
  end-to-end on the modern stack. Writes still go through legacy code
  (modern `write_bytes` returns `NotImplemented`); the performance floor
  (at least 150 MB/s) is deferred, needs T5.7 multi-in-flight at the IPC
  boundary. The reusable virtio-core is the foundation for Phase 5's
  virtio-net.

### Phase 3: Resource discipline (IN PROGRESS)

Goal: the OS can run a workload for an hour without leaking.

Exit criteria:

- [x] `SpaceDestroy` invoke op lands. Shipped: kernel handler in
  `syscall/handlers.rs::invoke_space_destroy`, libcluu wrapper, called by
  procmgr at exit/kill paths. This was the longest-deferred memory-leak
  source.
- [x] Userspace `poll()`/`select()` work for pipes, TTYs, and `/dev`
  pseudo-files. Sockets deferred to Phase 5.
- [ ] Compiler warnings across the tree below 5 total (currently ~30).
- [ ] H9/H10 overflow counters exposed in `/proc` and visible from `top`.
- [ ] Soak test: a shell session with roughly 1000 repeated
  `cat | grep | head` pipelines shows bounded memory in `/proc/meminfo`
  and no orphan processes in `ps`.

Allowed kernel work: `SpaceDestroy` (a pre-planned phase deliverable, not
a drift exception) and H9/H10 exposure plumbing. Named fix rule applies
for anything else.

Known unknowns and pivot triggers: the soak test may reveal leaks beyond
`SpaceDestroy`. Pivot trigger: if a second non-trivial leak surfaces,
extend Phase 3 until it is closed. Do not skip ahead to "chase Phase 4
momentum." Leaks in a shipping OS are a credibility killer.

### Phase 4: Userspace polish and coreutils (DONE 2026-05-08)

Inserted between Phase 3 and the original Phase 4 (network). Network
bumped to Phase 5; old Phase 5 (Ship) bumped to Phase 6.

**Cross-cutting design discipline (SOLID):** every Phase 4 implementation
follows SOLID — single responsibility (each builtin/util has one job),
open/closed (builtin registry stays open via a trait; new builtin = new
file, no edit to dispatcher), Liskov (every util obeys `main() -> i32`
with the same exit-code semantics), interface segregation (`VfsClient`
does not grow into a god trait; `readdir`/`stat`/`open` are separable),
dependency inversion (utils depend on `libcluu::fs::traits`, not on a
concrete `VfsClient`). Job control is **pure userspace** via existing
`InvokeOp::ThreadSuspend`/`ThreadResume` — zero kernel commits. The
3-stage pipe reverify (`cat | grep | head`) confirmed the n-stage path
works; env propagation through pipeline stages was the real gap.

Shipped:

- **Plan A (workspace cleanup):** 10 probe crates moved under
  `userspace/probes/`, dropped from `default-members` so the default
  `cargo xtask build` no longer compiles them. `commands.rs` (3,612 LOC)
  split into a `commands/` module hierarchy with a `Builtin` trait and a
  registry. 19 test-only shell builtins culled from the registry
  (47 to 28 entries) and rebuilt as 13 probe binaries; harness invocations
  updated to `container run <name>` form.
- **Plan E (pipe reverify):** confirmed `l2_pipe_3stage`
  (`cat | grep | head`) green. The "wire protocol unfinished" memory was
  diagnostically wrong. Environment propagation through pipeline stages
  closed (`pipeline.rs` no longer passes `&[]` for env). Sequential vs
  multiplexed wait decision documented.
- **Plan C (ls + extended VfsStat):** VFS protocol bumped to v2 with
  `VfsStat { size, mode, mtime, nlink, uid, gid, blocks }`; `readdir`
  returns `(name, stat)` pairs in one round trip. `ls` rewritten
  (53 to ~520 LOC) with `-l -a -h -R -1 -S -t -r --color=auto`. All
  backends (ext2, ramfs/memfs, procfs, devfs, virtio-blk) updated.
- **Plan B (cli + utils):** new `libcluu::cli` POSIX-style argument parser
  (12 host-side unit tests). 11 existing utils (cat, cp, mv, rm, mkdir,
  touch, head, tail, wc, grep, ps) migrated to `cli` with GNU-close
  short-flag matrices. 15 new utils shipped: env, sleep, basename,
  dirname, date, kill, printf, which, sort, uniq, cut, tr, find, du,
  stat. Stage 0 fix added a `WriteSink` enum (Tty / Pipe / File) and
  `run_with_sink` on `BuiltinCommand` so builtins (echo, env, jobs,
  alias, type, help, etc.) pipe correctly to containers. `echo foo | cat`
  now works.
- **Plan D (job control):** all userspace via existing kernel
  `ThreadSuspend`/`Resume` invoke ops, zero kernel commits. procmgr
  gained `PgTable` plus 6 new IPC labels
  (`PROCMGR_PG_{CREATE,ATTACH,SUSPEND,RESUME,SIGNAL}_LABEL`,
  `PROCMGR_PID_PGID_QUERY_LABEL`, `PROCMGR_JOB_NOTIFY_LABEL`). TTY
  tracks `fg_pgid_per_session`, decodes Ctrl-C / Ctrl-Z to `SIGINT` /
  `SIGTSTP`. Shell gained `JobTable` plus `jobs/fg/bg/wait/kill`
  builtins; pipeline executor creates a pgid per pipeline, attaches every
  spawned pid, sets TTY foreground. Grammar fix: `&` was being consumed
  as a bare word; now parses as `Pipeline { bg: true }`.
- **Plan F (misc builtins):** `exit` (sets `ctx.exit_requested`),
  `alias`/`unalias` with first-token expansion and recursion guard,
  `type` (alias / builtin / external lookup), `help` (lists registered
  builtins), persistent `history` (`~/.cluu_history`, load on startup,
  save on exit and every 10 commands), `set`/`unset` cleanup (rejects
  unsupported `-e/-x/-u` with a clear error).

Closing notes:

- All 6 plans merged on `develop` across 23 commits.
- ~30+ harness smokes added (`l2_<util>_basic` for new utils, `l2_pipe_*`,
  `l2_jobs_*`, `l2_alias_basic`, `l2_type_basic`, `l2_help_basic`,
  `l2_exit_status`, `l2_ls_long`, `l2_ls_color`, `l2_ls_recursive`).
- Open TODOs: `& ;` separator quirk, SpawnBuiltin/legacy `bg_jobs`
  cleanup, Ctrl-Z input-injection harness, SIGTTIN on bg-stdin-read (TTY
  wire format change), `$?` substitution, file-redir for builtins
  (`WriteSink::File` path).
- Discovered and fixed mid-flight: `harness_run.sh` ignores the
  positional `$1` (it uses the `MARKER_MODE` env); earlier "PASS" reports
  during sub-agent runs were false positives. Real verification requires
  `CLUU_SHELL_AUTOSTART_CMD=... MARKER_MODE=... bash scripts/harness_run.sh`.
  All Plan D and F smokes were reverified inline with the correct
  invocation.

### Phase 5: Network (PENDING)

Goal: the OS talks to the network.

Exit criteria:

- virtio-net driver attaches, link comes up in QEMU.
- DHCP client acquires an IP from QEMU's user-mode network.
- ARP table builds, `ping 8.8.8.8` replies.
- Userspace BSD-style socket API (`socket`, `bind`, `connect`, `listen`,
  `accept`, `send`, `recv`) covers TCP and UDP.
- `wget http://example.com` (or an equivalent tiny HTTP/1.1 client)
  fetches and prints a page.
- DNS resolution works: simple recursive with hardcoded roots, or via the
  router's resolver over DHCP.

Allowed kernel work: only if the virtio-net driver forces a kernel-side
IRQ-delivery fix.

Known unknowns and pivot triggers: biggest risk in the whole plan. TCP is
genuinely hard. Pivot trigger: if after 3 weeks of Phase 5 you do not have
DHCP plus ping, ship UDP-only and defer TCP to v1.1. Note the pivot
decision somewhere durable; do not silently slip.

### Interlude: virtio-snd audio driver (DONE 2026-07-16)

Userspace virtio-snd driver + mp3player container, built as a side quest
between Phase 4 and Phase 5. Proves the userspace driver model extends to
streaming devices, not just block/net.

Shipped:

- **virtio-snd driver** (`userspace/virtio-snd/`): PCI probe, 4 virtqueues,
  control queue lifecycle, grant-based TX. Registered as `snddev:main`.
  Rate enum matches virtio-snd spec exactly.
- **mp3player** (`userspace/mp3player/`): nanomp3 decoder, auto-detect
  rate/channels, full-file-to-memory load before playback.
- **libcluu audio_client**: `AudioSessionClient` with grant-based PCM
  submission, completion polling, rate constants.
- **Kernel fix**: `idle_until_runnable` missing `cli` after `hlt` —
  nested IRQ on shared IRQ 10 corrupted `iretq` frame → RIP=0x2 crash.
- **IPC_MESSAGE_MAX**: 4096 → 8192 to fit audio IPC metadata.

Key finding: QEMU's virtio-snd ignores `buffer_bytes`/`period_bytes` for
playback — PA backend handles all buffering. Device underruns are caused
by I/O stalls (9p read latency), not buffer misconfiguration. Full-file
load eliminates the underrun class for mp3player.

### Interlude: storage throughput pass (DONE 2026-07-16)

Targeted optimization round after the audio driver exposed 9p read latency
as the underrun root cause. ext2 throughput: ~9 MB/s → 803 MB/s. 9p
host-share: ~596 KB/s → multi-MB/s.

Shipped:

- **9p scatter-gather** (`virtio-9p`): per-page descriptors in `round_trip`
  via `virt_to_phys` — fixes `DmaPool::alloc_contiguous` physical-
  contiguity bug. Unblocks >4 KB 9p reads.
- **9p MSIZE 64 KB→256 KB**: QEMU 11.0.2 accepts via `TVERSION`. 4× fewer
  round-trips.
- **ext2 block size 1024→4096**: `mke2fs -b 4096`. 4× fewer block lookups.
- **VFS `read_file_bulk` IPC** (0x212): one round-trip for files ≤4 MB.
- **IRQ poll fallback + retry**: 50 ms `recv_any` timeout in virtio-blk
  main loop; `dispatch_irq` retries `try_send` 8× on `WouldBlock`.
- **Spin-poll yield frequency**: every 100 000 spins (was 1024).
- **mp3player READ_CHUNK 4 KB→64 KB**: 16× fewer IPC round-trips.
- **Virtio indirect descriptors** (`VIRTIO_F_RING_INDIRECT_DESC`): large
  scatter-gather requests use 1-page indirect tables instead of overflowing
  the 256-desc main queue. Supports 4 MB reads.
- **Harness**: `l2_blk_basic`/`l2_blk_perf`/`l2_blk_concurrent` registered.

Not done: IRQ-driven `read_bytes` (blocked by `try_send` drop reliability).

### Interlude: DOOM port (DONE 2026-07-23)

Ported doomgeneric (ozkl/doomgeneric) to CLUU. First third-party C application
ported to the newlib toolchain with a Rust platform backend. Proves the
C-runtime + compositor + audio + input stack works for real software.

Shipped:

- **doom-cluu** (`userspace/doom-cluu/`): Rust staticlib implementing the 6
  `DG_*` platform functions + C entry point. Compositor window with chrome
  border, PixelRegion SHM (1280×800 ARGB32, 2× nearest-neighbor scaling from
  DOOM's native 640×400). WAD loaded via chunked 64KB `read_grant` into a 28MB
  buffer (9p can't handle >4MB grants). Frame throttled to 35fps (TICRATE).
- **Key release events**: kbd + usb-input drivers now emit `kind=2` release
  events (scancode|0x80, ascii=0). Compositor parses `kind` and forwards it.
  `libcluu::input::ForwardedKey` provides a shared typed parser. cluuterm
  ignores releases (was double-stepping arrows); DOOM handles both.
- **Compositor dead-window reaper**: ~1Hz probe of window input endpoints;
  `ipc_send` failure → endpoint destroyed → window reaped. Workaround for
  missing SIGINT handler delivery (TODO: proper fix via procmgr signals).
- **Compositor pixel_dirty flag**: PixelRegion windows' pixel content changes
  every frame but `cell_grid` stays `PIXEL_CELL`, so `tick_frame` skipped
  flushes. Added `pixel_dirty` flag set on WIN_DAMAGE for pixel-region windows.
- **Kernel PMM MAX_ORDER 10→11**: 1280×800 PixelRegion needs 1000 pages
  (order 10). PMM buddy allocator max was 10 (order 0..9). Raised to 11.
  `frame_registry` order cap 9→10 to match.
- **Harness**: `l2_doom` test case (DG_Init marker, WAD from /host share).

Key findings:

- 9p `read_grant` hangs on >4MB chunks — `GRANT_BUF_SIZE=4MB` in VFS, and 9p
  blkdev can't handle large grant requests. 64KB chunks work (cluuamp pattern).
- USB HID maps ENTER to ASCII 10 (LF), not 13 (CR). DOOM expects KEY_ENTER=13.
  Scancode-first mapping for special keys fixes press/release key-ID mismatch.
- `i_input.c` has a `break` after the first key-release event per
  `I_GetEvent` call — only one release processed per tick. Not a bug for
  normal play but limits release throughput.

### Phase 6: Ship (PENDING)

Goal: a stranger can run CLUU from a download link and see it work.

Exit criteria:

- `make iso` (or equivalent) produces `cluu.iso` that boots in stock QEMU
  with a one-line command.
- `README.md` at repo root: 200 lines max, includes a GIF of login to
  shell to Python to edit to save to run.
- Build instructions a Linux user can follow in under 15 minutes,
  verified by at least one dry-run from a clean checkout.
- Blog post or GitHub release notes: what CLUU is, what it runs, what it
  does not, known limits. Honest framing: "hobby OS, two years solo,
  here is what it does."
- Posted to /r/osdev or Hacker News. Posting is the last action, not
  "maybe I'll post when I feel good about it."

Allowed kernel work: bug fixes surfaced by the clean-checkout dry-run
only. No polishing.

Known unknowns and pivot triggers: the instinct to keep polishing instead
of posting is a drift pattern. Counter: a post with a flaw gets feedback.
A perfect unposted OS gets nothing. Post.
