# Gotchas

Load-bearing traps in CLUU. Each entry names the symptom, the code site, why
the trap exists, and the structural fix (shipped or planned).

## allocator-reentrancy-leak

The default Rust userspace allocator (`LockedAllocator` in
`userspace/libcluu/src/allocator.rs`) had two leak modes for `dealloc`:

### The original code

`allocator.rs`:

```rust
unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
    if let Some(mut guard) = self.inner.try_lock() {
        self.drain_deferred(&mut guard);
        guard.dealloc(ptr)
    } else {
        // Re-entrant: defer the free to the next successful alloc/dealloc.
        let mut deferred = self.deferred.lock();
        if deferred.count < DEFERRED_FREE_CAP {
            deferred.ptrs[deferred.count] = ptr as usize;
            deferred.count += 1;
        }
    }
}
```

### Why it existed

The allocator is a single `spin::Mutex`. A re-entrant call, a `free` that
fires while the mutex is already held by the same thread, would deadlock if
it used a blocking `lock()`. The classic trigger is a GC: an `alloc` holds
the mutex, the allocator runs a GC callback, and the GC tries to `free`
unreachable objects. The `try_lock` fallback turned the deadlock into a
deferred free.

### The two-thread leak (FIXED)

The `try_lock` + deferred-free approach silently leaked when two *separate*
threads competed for the lock — not re-entrancy, but contention. Thread A
holds the lock in `alloc`; thread B's `dealloc` fails `try_lock` and
defers. After 64 deferred deallocs, the list overflowed and all subsequent
deallocs were **permanently leaked**. cluuamp's audio + UI threads hit
this: each `format!` and Vec reallocation on one thread leaked its dealloc
while the other thread was in `alloc`.

**Fix:** `dealloc` now uses a blocking `lock()`. This is safe because
`dealloc` does not call `alloc` (no re-entrancy), and `spin::Mutex::lock`
does not allocate. The deferred-free list and `leaked_deallocs` counter
remain for diagnostics but should always read 0.

### Impact

- Two-thread programs (cluuamp, any server with a worker thread) no longer
  leak on allocator contention.
- `alloc` uses a blocking `lock()` and drains the deferred-free queue
  before each allocation.
- `dealloc` uses a blocking `lock()` — the deferred-free path is now only
  a diagnostic counter, never taken in normal operation.
- `AllocStats::leaked_deallocs` tracks any residual leaks (should be 0).

### See also

- [Memory Model](../memory_model/index.html) for allocator paths and region
  layout.
- [Interpreter Porting](../interpreter_porting/index.html) for why
  GC-bearing interpreters should bring their own heap and avoid the global
  allocator for GC-managed objects.
- AGENTS.md §7 for the canonical deadlock-avoidance rationale behind the
  async runtime; the allocator fix follows the same "don't block re-entrant"
  shape.

## cluuamp-pcm-s16-unbounded-growth

`AudioEngine::tick()` decoded up to 4 MP3 frames (~18KB) per tick before
submitting periods to the virtio-snd ring. Each `submit_period` drains
only 4KB. When the ring had 1 free slot: +18KB decode, -4KB submit =
+14KB net accumulation per tick. Over hundreds of ticks, `pcm_s16` grew
to megabytes, triggering Vec capacity doubling (16KB→32KB→...→1MB+).

### The fix

`tick()` now enforces submit-before-decode:
1. Submit pending periods first (drain `pcm_s16` → ring).
2. Only decode when `pcm_s16.len() < PERIOD_BYTES`.
3. Submit again after decoding.

`pcm_s16` is bounded at ~1 period + 1 frame (≈8.7KB).

### Lesson

In a producer-consumer pipeline where the consumer (ring submit) drains
less per tick than the producer (frame decode) produces, always submit
first and gate decoding on the buffer being below the period threshold.
The original code's decode-first-then-submit order assumed the ring would
always have enough free slots — it doesn't when the hardware is slow to
acknowledge completions.

## cluuamp-stream-buf-overshoot

`refill_stream()` read 64KB chunks from VFS with loop guard
`stream_buf.len() < STREAM_BUF_SIZE` (256KB). When `len()` was 248KB, one
more 64KB read pushed it to 312KB — exceeding the 256KB initial capacity.
Vec doubled to 512KB and never shrank (the drain path compacts bytes but
never calls `shrink_to_fit`).

### The fix

Clamp each read: `let want = want.min(STREAM_BUF_SIZE - stream_buf.len());`
so `len()` never exceeds `STREAM_BUF_SIZE`.

### Lesson

When filling a Vec to a target capacity via chunked reads, always clamp
the final chunk to `target - current_len`. A loop guard `len() < target`
allows one overshoot read whose size is the full chunk size, not the
remaining space. Vec doubling is amortized but permanent — the capacity
stays even after `drain` compacts the contents.

## cluuamp-mp3dec-struct-layout

The FFI `Mp3dec` struct in `mp3_ffi.rs` must exactly match the C
`mp3dec_t` layout from minimp3. A zero-sized or incorrectly-sized Rust
wrapper causes heap corruption: minimp3 writes ~6667 bytes of decoder
state (IMDCT overlap buffers, QMF state), and if the Rust struct is too
small, those writes land in adjacent heap metadata → page fault at
`0x800008` or similar.

### The fix

`Mp3dec` is `#[repr(C)]` with exact field layout matching the C struct:
`mdct_overlap: [[f32; 288]; 2]`, `qmf_state: [f32; 960]`, plus header
and reserv fields. A compile-time `assert!(size_of::<Mp3dec>() >= ...)`
would be ideal but the exact size depends on minimp3 version — the
current layout matches minimp3 commit as vendored.

### Lesson

FFI structs wrapping C decoder state must replicate the exact C struct
layout with `#[repr(C)]` and matching field types/sizes. A "thin
wrapper" that holds a pointer or is zero-sized is wrong — the C code
writes inline state into the struct itself, not through a pointer.

## single-threaded-mutual-blocking-ipc-deadlock

A single-threaded server that makes a synchronous IPC `call` to a service
which itself needs to `call` back into the first server deadlocks. Both
block forever: the first server is blocked in `call` and cannot run its
`recv` loop; the second server is blocked in `call` waiting for the first to
reply.

### The pattern

```text
  VFS (single thread)              procmgr (single thread)
    │                                 │
    │── CALL(procmgr_ep, /proc/x) ───→│   (VFS blocked, waiting for reply)
    │                                 │   (procmgr needs to query VFS
    │                                 │    to resolve the proc entry)
    │←── CALL(vfs_ep, view_lookup) ───│   (procmgr blocked, waiting for VFS)
    │                                 │
    │         both deadlock           │
```

Any pair of single-threaded services that mutually `call` each other during
request handling hits this. The trigger is not a bug in either service; it
is a structural property of synchronous IPC between single-threaded servers
with no re-entrancy point.

### Why it exists

CLUU's IPC is synchronous and rendezvous-based for `call`/`reply`, but
endpoints also have bounded buffered queues (`MAX_QUEUE_LEN=1024`,
`MAX_CALL_QUEUE_LEN=256`). When a queue is full, senders park in
`waiting_senders` and are woken on drain or on endpoint destruction
(`endpoint.rs:296-301, 574-614`). A `call` blocks the caller until the
server `reply`s; a `send` to a full queue blocks until space frees.
A single-threaded server running its `recv` loop cannot accept a new request
while it is blocked in a downstream `call`.

### The structural fix: async runtime

The async runtime in `libcluu::async_runtime` is the canonical
deadlock-avoidance mechanism for single-threaded servers. VFS and
session-procmgr use it. The runtime (`Runtime`, `IpcCallFuture`, `spawn`,
completion queue) lets a single-threaded server have multiple outstanding
downstream IPC calls without blocking its own `recv` loop. VFS dispatches
IPC-bound backend operations through `dispatch_async()`, so a `ProcfsBackend`
call to procmgr and a procmgr callback into VFS can both be in flight without
either side blocking itself.

devmgr stays sync. It is a leaf service with no downstream IPC, so the
deadlock class cannot arise.

### History

The original AGENTS.md §7 forbade async for `top` and `/proc` and mandated
the sync `call_with_reply_buf` path. That constraint was based on the
pre-async-runtime state. The async runtime has since proven stable and is the
only structural fix for this deadlock class. The sync-only constraint was
lifted on 2026-07-06.

### See also

- [Virtual Filesystem](../vfs/index.html#sync-vs-async-backends) for the
  `AsyncMountBackend` dispatch path.
- [allocator-reentrancy-leak](#allocator-reentrancy-leak) for the same
  single-threaded-mutual-blocking shape in the allocator.
- AGENTS.md §7 for the canonical statement of the async runtime policy.

## pts-plumbing-fix (2026-05-21)

After the login → cluuterm → shell path landed green to the `read(0)`
loop, pts bidirectional plumbing was broken: shell `write(1)` → pts →
cluuterm dropped all bytes (nothing reached the terminal window), and
kbd → cluuterm → pts → shell `read(0)` delivered nothing. Architect
review found four issues:

- **Bug A — `Pts::handle_pts_write` drops bytes on missing reply_token.**
  VFS forwards shell stdout via `send_msg_with_payload`
  (fire-and-forget, no reply slot). Cluuterm's handler bailed when
  `extract_reply_id` returned `None`, before calling
  `line_discipline.process_output()`. Fix: cook bytes unconditionally;
  reply becomes optional (send ack only if `reply_token` present).
- **Bug B — Dual parallel stdin buffers, mismatched.** `Pts` owned
  `ready_bytes` + `pending_readers`; `Cluuterm` owned `stdin_buf` +
  `pending_pts_read`. The `run()` `PTS_READ_LABEL` handler used the
  Cluuterm-level pair; `apply_service_actions::DeliverBytes` wrote to
  the Pts-level pair. Kbd bytes went to a dead buffer; shell read
  waited on the other buffer forever. Fix: drop the Cluuterm-level
  duplicates; `Pts.ready_bytes` + `Pts.pending_readers` become
  canonical. Route `PTS_READ_LABEL` through `Pts::handle_pts_read`.
- **Bug C — VFS `derive_child_fd` cap-monotone violation.**
  `token_derive` minted from VFS's own full-rights endpoint without
  clamping `child_rights` against the parent `OpenFile`'s rights. Child
  could receive caps strictly broader than parent's — violated
  `vfs-view-caps-monotone`. Fix: each `OpenFile` variant carries an
  effective rights mask; `derive_child_fd` clamps
  `child_rights & parent_rights` before `token_derive`.
- **Bug D — Cross-process deadlock risk in VFS `PTS_READ`.** VFS used
  synchronous `call_with_reply_buf` to cluuterm for pts reads.
  Cluuterm could defer reply (`PendingRead`); during that window VFS
  was blocked inside `call_with_reply_buf`. If cluuterm called VFS for
  any reason during that window → cycle. Fix: new label
  `PTS_READ_DELIVER = 112`. VFS parks the shell's `reply_token` in
  `pending_pts_reads[pts_id]`, sends `PTS_READ_LABEL`
  fire-and-forget to cluuterm (signals "drain"), does NOT reply to
  shell. Cluuterm sends `PTS_READ_DELIVER` with bytes when ready; VFS
  pops the parked read, grants bytes, replies. VFS thread never blocks
  on cluuterm.

## plan-lessons-overview

The 39 implementation plans (preserved in git history) carry two kinds of
load-bearing knowledge: design decisions (extracted into chapter content by
the spec-extraction pass) and *lessons learned* — the traps, diagnostic
moves, and discipline rules that surfaced during execution. The entries
below distill the latter, 2-5 lines per plan. Cross-reference the plan file
by date-stamped slug for the long form.

## pipe-token-revocation-cascade (2026-04-27-pipes)

A pipe is one IPC endpoint with two rights-restricted tokens minted via
`kernel::TokenDerive`. Revoking the parent endpoint invalidates all derived
child tokens — procmgr relies on this cascade for `PIPE_CLOSE` and
per-process exit cleanup. The YAGNI deferral on the `generation` counter
(ABA protection for `pipe_id` reuse) was deliberate: index reuse never
mattered for the demo runs. If a harness case ever shows a stale-id bug,
the gen-counter path is the structural fix.

## cluufile-strict-mount-mismatch (2026-04-28-user-envelope)

`MOUNT /etc readwrite` in a Cluufile against an envelope that grants `/etc`
read-only fails spawn with `EACCES`. Strict mode is the only correct mode:
silent narrowing surprises users, silent widening violates the monotone cap
discipline (binary ⊆ shell ⊆ envelope ⊆ procmgr). `validate_cluufile_against_parent`
does longest-prefix-match per directive and rejects any Rw demand on a Ro
parent mount.

## vfs-open-o-wronly-creat-on-memfs-timeout (2026-04-29-editor)

`VfsClient::open_with(path, O_WRONLY | O_CREAT, 0o644)` on a shell MemFs
`/tmp` path can time out (task #80). Until that root cause is fixed, harness
cases that exercise save target ext2-backed paths (`/home/root/...`), not
`/tmp`. Atomic save is `open_with(tmp, ...)` → `write` → `close` →
`rename(tmp, final)`; `VfsClient::rename` exists and is the rename
primitive.

## harness-cannot-inject-escape-bytes (2026-04-29-editor)

`KEYSTROKE_COMMANDS` always types whole lines + Enter; `POST_SENDKEY` sends
one key. There is no native path for raw escape-sequence byte streams. The
workaround for `l2_edit_*` cases: drive the editor from a parent shell that
injects bytes via `send_with_payload(child_stdin, TTY_READ_LABEL, ...)`,
the same pattern `SuBuiltin` uses. The console SGR parser silently consumes
`CSI 7 m` (reverse video), `CSI ?25 l/h` (cursor hide/show), `CSI 39 m` /
`CSI 49 m` (default fg/bg) — use `CSI 0 m` to reset, color bg for status
lines instead of reverse video.

## virtio-notify-batching-lever (2026-05-06-virtio-blk-modern)

Notify batching is the biggest throughput lever, not queue depth or
zero-copy alone. Each `notify` is a MMIO exit; amortising one notify across
N submits collapses the exit cost. `DmaPool` must forbid a region crossing
a 4 KiB page boundary so the cached page-phys is unambiguous. WC perf gain
is invisible under QEMU TCG (every memory type behaves as WB); functional
correctness is TCG-verifiable, perf delta requires KVM.

## probes-out-of-default-build (2026-05-07-phase4-A-workspace-cleanup)

11 probe crates moved under `userspace/probes/` and dropped from
`default-members`. `cargo xtask build` does not compile probes;
`cargo xtask build-probes` builds them. The 3,612-line `commands.rs` was
split into `commands/` module hierarchy with a `BuiltinRegistry` trait —
each file ≤ ~400 LOC. Test-only builtins (19 of them) were extracted into
probe binaries invoked via `/probes/<name>`, not as shell builtins.

## shared-cli-parser-dry (2026-05-07-phase4-B-cli-and-utils)

A single-pass arg parser in `libcluu::cli` (clustered short flags, long
opts, optional/required attachment, `--`, auto `--help`/`--version`) is
shared by every util. DRY across 11 existing + 15 new utils. GNU exit-code
convention: 0 success, 1 runtime failure, 2 usage error. Without the shared
parser, every util reinvents the same flag-loop bugs.

## vfs-wire-protocol-bump-cost (2026-05-07-phase4-C-ls-and-vfs-stat)

Bumping `VfsStat` to carry mtime/nlink/uid/gid/blocks and `readdir` to
return `(name, stat)` pairs in one round trip required touching the wire
format, every backend (ext2, ramfs, memfs, procfs, devfs), and the client.
Wire-format changes are expensive — they cascade through every backend and
every caller. Plan them as discrete phases; don't sneak fields in.

## zero-kernel-commits-job-control (2026-05-07-phase4-D-job-control)

Full POSIX job control (Ctrl-Z, fg, bg, SIGSTOP/SIGCONT/SIGINT, job table,
`kill %N`) shipped with zero kernel commits — `InvokeOp::ThreadSuspend` /
`ThreadResume` already existed. Three-component split: procmgr owns
`pgid → [pid]` lifetime + state machine; TTY tracks `fg_pgid_per_session`
and decodes Ctrl-C / Ctrl-Z; shell carries `JobTable`. The lesson: before
proposing a kernel change for a userspace feature, audit the existing
invoke-op surface.

## diagnostic-first-pipe-reverify (2026-05-07-phase4-E-pipe-reverify)

The plan was diagnostic-first: run a 3-stage smoke against the existing
executor, capture exact failure or success, *then* fix. The 3-stage path
worked; the real gap was env propagation through pipe stages, not the pipe
mechanism. The fix was lifting the ENV trailer from the single-cmd path
into a shared payload builder reused by `pipeline.rs`. Document the wait
semantics; don't leave a TODO in the executor.

## framebuffer-pat-msr-wc (2026-05-09-framebuffer-perf-wc)

Program the x86_64 PAT MSR (0x277) at boot to install a Linux-compatible
layout where index 1 = WC. UC-, UC, WB stay where firmware put them;
existing PTE encodings keep their semantics. `MAP_DEVICE_WC = 0x200` is the
new `SpaceMap` flag; PTE bits PCD=0, PWT=1, PAT=0 → index 1. The new flag
bit is unused on old kernels, so the same flag value falls back gracefully.
WC perf gain is real only under KVM or baremetal — TCG treats every memory
type as WB.

## fast-symlink-60-byte-i-block (2026-05-09-symlink-following-resolution)

Fast symlinks store their target inline in the 60 bytes that would
otherwise hold direct/indirect block pointers. `Inode::parse` originally
decoded those bytes as `[u32; 12] + 3 * u32`, throwing away the raw view.
`inline_block_bytes()` re-serialises them so targets ≤60 bytes read without
a data-block fetch. Four hard-coded `strip_prefix("/bin/")` sites in the
shell were replaced with `VfsClient::realpath()` + image-name extraction.
Procmgr is hardened to reject image names containing `/`.

## glyph-atlas-precomputed-mask (2026-05-10-fb-atlas-and-devfb0)

Per-cell `render_glyph` was a per-bit branch + 16 row writes per cell. The
atlas swaps in a precomputed `[u32; GLYPH_W*GLYPH_H]` mask template per
char (`0xFFFF_FFFF` / `0x0000_0000`) plus an SIMD-friendly
`(mask & fg) | (!mask & bg)` blend, then `put_pixels_row`. `/dev/fb0` is a
`DeviceBackend::Fb` variant: `open` returns the device file, `read` returns
geometry, `write` clamps a buffer onto the front-buffer, `mmap` routes
through `MAP_DEVICE_WC`. No new syscalls.

## broadcast-frame-ready-damage-gate (2026-05-13-vt-hardening)

The compositor flushed the fb at 60 Hz and broadcast `COMP_FRAME_READY` to
every window regardless of damage. cluuterm's `posix_spawn` of `/bin/login`
blocked its recv loop ~0.5 s; 30 FRAME_READY messages piled up at ep=84
(q=30 in serial). Fix: gate broadcast on actual damage since last
broadcast per window. Uncontrolled fire-and-forget fan-out to a
single-threaded server is a queue-depth bug waiting to happen.

## vtmgr-active-vt-init-race (2026-05-12-vtmgr-boot-vt-fix)

`VtmgrContext::active_vt` was initialised to `0`; boot relied on the
compositor-grant arrival happening *after* `VTMGR_PIN_VT_LABEL` to switch
to the compositor VT. The race: if the compositor grant arrived first,
`active_vt` was still 0 and the switch never fired. Fix: init
`active_vt = DEFAULT_COMPOSITOR_VT` and drop the `boot_switch_pending`
machinery entirely. The lesson: don't make boot depend on the order of
independent grant arrivals.

## vtmgr-single-input-decider (2026-05-13-input-routing-vtmgr)

vtmgr is the single source of truth for active-VT AND the sole input
router. kbd shrinks to pure IRQ/decoder/forward driver; vtmgr subscribes to
`compositor:input` and `tty:0..3:main`, holding all 5 outbound send-tokens.
Two IPC hops per keystroke (kbd → vtmgr → target); negligible latency at
human typing rates. The win: true single-decider, modal-lock trivially
enforceable, SOLID for future `inputd` extraction (literal rename of
`vtmgr:input` → `inputd:input`).

## autologin-gate-on-build-constant (2026-05-12-autologin-rip)

`try_auto_login` becomes a no-op when `SHELL_AUTOSTART_CMD.is_empty()`.
`tty.auto_login_pending` mirrors the same gate via a shared `libcluu`
constant so both crates read the same value. The text-mode interactive
login in `tty/src/context.rs` becomes the default user-facing entry point
on every text VT. Harness cases that depended on `CLUU_SHELL_AUTOSTART_CMD`
keep working without per-test changes.

## posix-read-0-unification (2026-05-14-bug-c-shell-stdin-via-fd0)

Bug C: shell input via `TOKEN_STDIN` push (`TTY_READ_LABEL`) breaks under
cluuterm (VFS-backed fd 0). The legacy VT0 path pushed; the cluuterm path
used POSIX `read(0)` via pts. Dual paths were technical debt. The fix:
procmgr opens the right `/dev/...` node at shell-spawn time using its own
`VfsClient`, builds an FDAC payload, and injects it through the existing
`spawn_service_with_env` path. The tty service shrinks to a VFS backend
that only answers `TTY_READ_REQUEST_LABEL` pulls; all push-send sites die.

## vt-text-vs-vt-graphical-envelope (2026-05-14-plan2-envelope-vt-user-substitution)

`/etc/envelopes.toml` carries per-shape mount lists (`vt_text` vs
`vt_graphical`); `{vt}` and `{user}` substitutions apply at SESSION_LOGIN.
Each session sees the strict subset of `/dev` (and elsewhere) defined by
its envelope and VT index. Root needs a real `env_template` (HOME etc.) —
an empty root template was the root cause of HOME-not-propagating (Bug B).
`vfs_view.rs` already enforces monotone narrowing; substitution must not
slip past that check.

## compositor-swap-on-login (2026-05-14-plan3-compositor-swap)

At login on VT4, procmgr kills the system-mode compositor (autostarted at
boot to host the login modal) and spawns a fresh compositor under the
user's envelope inside the session container. The *same* binary runs in
both modes; what differs is the VFS view + envelope env it inherits. On
logout (Plan 5), procmgr respawns the system compositor. No new compositor
binary — mode is a flag read from `ProcessInfo` params.

## procmgr-spawn-broker-not-capability (2026-05-14-plan4-procmgr-spawn-broker)

The user-mode compositor holds zero spawn capability of its own. To open a
menu app, it sends `PROCMGR_SPAWN_SESSION_LABEL` to procmgr; procmgr
verifies the caller is the live session compositor (sender_tid lookup) and
spawns the named image as a sibling in the same session container. Pure
broker pattern — no additional capability handed to the compositor. A
separate label from `PROCMGR_SPAWN_LABEL` ensures arbitrary processes can't
trigger the broker path.

## session-cascade-teardown (2026-05-14-plan5-logout-teardown)

When a session-root process exits (clean logout, crash, or `exit`),
procmgr walks `container_children[session_cid]` in reverse-dependency
order, sends `THREAD_KILL` to each, reaps exit cookies, drops the
session_table entry, then respawns the appropriate stand-in (system
compositor for VT4, login prompt for VT0-3). The exit-cookie handler is
the hook point; existing `poll_exit_notifications` already drains the
channel.

## postcard-wire-format (2026-05-18-plan1-unified-spawn-protocol)

Six existing spawn paths (init kernel batch, procmgr autostart,
SESSION_LOGIN internal, PROCMGR_SPAWN, PROCMGR_CONTAINER_RUN, cluuterm
posix_spawn) collapsed into one IPC verb (`PROCMGR_SPAWN_UNIFIED_LABEL =
80`) carrying a postcard-serialized `SpawnEnvelope`. Postcard is the wire
format for all new verbs; `cluu_proto` is the shared types crate. A
one-shot `PROCMGR_PRIMORDIAL_SEED_LABEL = 81` handles init → procmgr
handoff. `ViewObject` becomes a procmgr-owned typed object; restart policy
moves from envelope to manifest.

## cap-possession-equals-authority (2026-05-21-procmgr-cap-refactor)

All runtime identity checks in procmgr were deleted. Authority is
structural: `root-procmgr` mints session-scoped caps; each
`session-procmgr` sub-mints child-scoped caps; cap derivation is
monotone-narrowing. PIDs encode `(8-bit session_id | 23-bit local pid)`.
Cascade teardown on session-procmgr death is via cap revocation. Three
crates: `procmgr-common` (lib), `root-procmgr` (bin), `session-procmgr`
(bin). `MintGuard` is RAII rollback for failed multi-step mints. A
`cap-purity` lint gate (`xtask check-cap-purity`) grep-rejects new identity
checks.

## env-merge-caller-wins (2026-05-21-spawn-env-merge)

`procmgr::handle_spawn_unified` merges `/etc/envelopes.toml` defaults
*under* the caller-supplied env: resolve caller's session → look up user
profile → resolve envelope → `resolve_env` → overlay caller's
`envelope.env` on top. Caller wins on key conflict. No merge on the
no-session (boot/service) path — boot services don't have user envelopes.
No new IPC verb, no wire change.

## kernel-freeze-discipline (2026-05-27-post-cap-refactor-backlog)

Kernel freeze is active through ~2026-10-21. No kernel commit lands without
naming the userspace failure that forced it. No new syscalls — every verb
goes through existing IPC + tokens. No timeouts as deadlock guards —
cap-revocation unblocks waiters. Commit per task; `cargo xtask build` clean
between tasks; no multi-day WIP on `develop`. The coverage gate
(`cargo xtask coverage-check`) must stay green. Deleting legacy code is the
highest-ROI cleanup during the freeze.

## frame-aliasing-double-dec-ref (2026-07-07-memory-model-fixes)

**Symptom:** 1094 `FRAME_TABLE WARN: dec_ref on refcount=0` warnings during boot, micropython spawn crash at CR2=0x20001 RIP=0x4348c1.

**Code sites:**
- `kernel/src/syscall/handlers.rs`: `invoke_space_grant` (line ~2100)
- `kernel/src/elf.rs`: `map_user_page`, `map_shared_page` Phase 2.6 overwrite path
- `kernel/src/syscall/handlers.rs`: `map_range_4kb`, `invoke_space_map`, `map_remaining_4kb`

**Three root causes:**

1. `invoke_space_grant` explicitly `dec_ref`'d the prior mapping at the target address, then `map_user_page`'s Phase 2.6 overwrite path `dec_ref`'d the SAME prior phys again (PTE still present). Double-dec. Fix: remove the explicit dec_ref; let `map_user_page` handle it.

2. `map_user_page` and `map_shared_page` unconditionally `dec_ref`'d `old_phys` on PTE overwrite, even when `old_phys == new phys` (same-frame re-grant via `space_grant` ring buffer reuse). `dec_ref` auto-freed the frame to PMM, then `inc_ref` raced with a re-allocation. Fix: skip `dec_ref` when `old_phys == new phys`.

3. `map_range_4kb`, `invoke_space_map`, and `map_remaining_4kb` allocated frames via `pmm::alloc_frame` (Untyped, rc=0) but never `retype_to_user`'d them. `space_grant`'s `inc_ref` bumped rc from 0 to 1, but teardown of BOTH spaces `dec_ref`'d twice, so the second hit rc=0 warning, and the auto-free between the two `dec_ref`s let PMM re-allocate the frame while still mapped (text corruption). Fix: `retype_to_user` before `map_user_page` (rc=1, tag=UserData). `space_grant` `inc_ref` to rc=2 (Grant). Both teardowns `dec_ref` to 0. Balanced.

**Why the trap existed:** The frame refcount invariant ("every mapped frame has rc≥1, every Grant frame has rc≥2") was enforced inconsistently across the four allocation paths. Only `load_segment_batch` (the in-kernel ELF loader) called `retype_to_user`; the syscall-driven paths assumed `pmm::alloc_frame` returned UserData-tagged frames, but it returns Untyped.

**Impact before fix:** 1094 warnings per boot, frame aliasing causing text corruption, micropython spawn crash.

**Impact after fix:** 0 warnings, no corruption, micropython spawns cleanly.

## nursery-sweep-use-after-sweep (2026-07-07-memory-model-fixes)

**Symptom:** virtio-blk crash at CR2=0x20001 RIP=0x4348c1 during micropython spawn. `InflightSlot.status_region.virt` corrupted from valid DMA pool address (0x51001a20) to garbage (0x20001).

**Code site:** `userspace/libcluu/src/allocator.rs`: `NurseryAllocator::alloc`

**Root cause:** The `NurseryAllocator`'s `sweep` reclaims ALL nursery memory when the nursery fills up, including live allocations. The nursery's own safety contract states: "sweeping reclaims all nursery memory regardless of liveness. This is safe only when callers do not retain nursery pointers across a sweep."

`Vec` backing buffers violate this contract. virtio-blk's `Vec<InflightSlot>` (56 bytes per slot, under the 256-byte nursery threshold) had its backing buffer in the nursery. When the nursery swept, new small allocations overwrote the Vec's buffer, corrupting `InflightSlot.status_region.virt` from a valid DMA pool address (0x51001a20) to garbage (0x20001). The subsequent dereference in `drain_completions` crashed with a page fault at the corrupted address.

**Fix:** When the nursery is full, fall through to the linked-list allocator instead of sweeping. This preserves live nursery allocations at the cost of wasting nursery memory after it fills up once. For long-running services like virtio-blk, the nursery fills up early and subsequent small allocations go to the linked-list allocator, which is correct and only slightly slower.

**Why the trap existed:** The nursery was designed as a tcache-style fast path with bulk-free semantics, assuming all nursery allocations were short-lived. `Vec<InflightSlot>` is a long-lived allocation that happens to be small enough to fit in the nursery. The safety contract was documented but not enforced; there was no mechanism to prevent long-lived allocations from landing in the nursery.

**Impact before fix:** virtio-blk crash on micropython spawn, intermittent memory corruption in any service using small long-lived heap allocations.

**Impact after fix:** No crash, no corruption. Micropython spawns successfully.

**See also:** [Memory Model](../memory_model/index.html) for the allocator paths. The nursery allocator is in `userspace/libcluu/src/allocator.rs`.

## m9-single-text-region (2026-07-06-m9-demand-paged-text)

**Symptom:** M9 demand-paged text crashed when an ELF had multiple executable segments. Only the first segment would demand-page; the second would overwrite the recorded text region, causing entry-point faults.

**Code site:** `userspace/libcluu/src/process.rs`: `map_segment` (the `text_demand_paged` guard)

**Root cause:** `set_text_with_source` records a single `TextSource` per address space. When M9 was first enabled, every executable non-writable segment called `space_protect_unmapped`, overwriting the previously recorded text source. The second segment's source bytes would replace the first's, so when the first segment's pages faulted, they'd be filled with the second segment's bytes.

**Fix:** Only the FIRST executable non-writable segment is demand-paged. Subsequent executable segments are eagerly mapped. The `text_demand_paged` flag in `map_segments` ensures `set_text_with_source` is called at most once per space.

**Why the trap existed:** M9 was designed assuming one text segment per ELF (the common case for Rust binaries). Some ELFs (e.g., C programs with separate `.text` and `.init` sections) have multiple executable segments. The `TextSource` struct was a single-slot field, not a list.

**Impact before fix:** Any ELF with multiple executable segments would crash on entry.

**Impact after fix:** Only the first executable segment is demand-paged; subsequent ones are eagerly mapped (small memory cost, correct behavior).

**See also:** [Memory Model](../memory_model/index.html) for M9 demand-paged text. `kernel/src/mm/space.rs`: `set_text_with_source`. `kernel/src/architecture/x86_64/idt.rs`: `handle_text_fault`.

## usb-input-irq11-conflict (2026-07-09-usb-input-ehci-irq)

**Symptom:** `usb-input` cannot use EHCI IRQ for interrupt-IN completion. Attaching IRQ 11 steals it from virtio-blk, breaking all disk reads — login fails because procmgr cannot load container images from `/var/images`.

**Code site:** `kernel/src/devices/irq.rs`: `attach()` — `IRQ_ENDPOINTS[irq_index].store(endpoint, Release)` overwrites the previous binding. Single endpoint per IRQ line, no chaining.

**Root cause:** QEMU's PCI topology assigns both the virtio-blk PCI device and the usb-ehci controller to legacy IRQ 11. The kernel's IRQ routing is a flat array indexed by IRQ number — last `irq_attach` wins. `virtio-blk` attaches first (boot order), then `usb-input` overwrites IRQ 11's endpoint. All subsequent IRQ 11 deliveries go to usb-input's endpoint; virtio-blk never receives its completions.

**Why the trap exists:** The kernel does not support shared IRQ lines (no interrupt chaining, no level-triggered multicast). This is a pre-v1 simplification — real hardware shares IRQs routinely.

**Workaround (shipped):** `usb-input` uses polling instead of IRQ. The main loop calls `ipc_recv_any` with a 10ms timeout, then polls `EhciController::poll_interrupt` on all device slots. The kernel blocks the thread during the recv timeout (CPU HLTs), so 100 wakeups/sec × microseconds of register reads = negligible host CPU. Do NOT attempt `irq_attach` for EHCI until the kernel supports shared IRQ delivery.

**Prior bug:** The original `usb-input` main loop also used `ipc_recv_any` with timeout `0` (non-blocking) + `yield_cpu()` on error — a tight busy loop that caused 100% host CPU. The 10ms timeout fixes both the original busy-loop and the IRQ conflict.

**Impact:** `usb-input` works correctly with polling. Keyboard/mouse input is forwarded to `inputd:input` → `vtmgr` → `tty`/`compositor`. Latency is bounded by the 10ms poll interval (imperceptible for human input).

**See also:** `userspace/usb-input/src/main.rs` (poll loop), `userspace/virtio-core/src/irq.rs` (`IrqSource` — the IRQ attach helper that virtio-blk uses), `kernel/src/devices/irq.rs` (single-endpoint routing).

## isig-checked-before-raw-canonical-split (2026-07-12-edit-isig)

**Symptom:** Edit (or any TUI app behind cluuterm PTS) gets killed by Ctrl+C. Process never runs cleanup() — alt screen not exited, TTY not restored, shell left with broken terminal (white screen, no echo).

**Code site:** `userspace/libcluu/src/tty_core/line_discipline.rs:207-242` — `feed_byte()`. `userspace/libcluu/src/posix/tty.rs:98` — `enter_raw()`.

**Root cause:** CLUU's line discipline checks ISIG **before** the canonical/raw mode split. Even in raw mode (ICANON cleared), 0x03 is intercepted as SIGINT if ISIG is still set. `enter_raw()` only cleared `ICANON | ECHO` — NOT ISIG.

**Fix:** `enter_raw()` now also clears `TTY_LFLAG_ISIG = 0x01` in both the legacy TTY_CTL path and the PTS tcsetattr path. `restore()` re-enables ISIG from saved termios.

**Key insight:** Standard termios treats ISIG as independent from ICANON. Raw mode doesn't automatically disable signal generation. You MUST explicitly clear ISIG.

**See also:** `userspace/libcluu/src/posix/tty.rs`, `userspace/cluu_wire/src/pts.rs:93` (`ISIG: u32 = 0x0001`), `include/sys/termios.h:23`.

## shell-termios-not-restored-after-child-exit (2026-07-17-ctrl-d-bug)

**Symptom:** After Ctrl+C kills a micropython REPL process, Ctrl+D does
not close the shell. The shell appears stuck — keystrokes are swallowed
or produce garbage.

**Root cause:** Children like micropython enter raw termios mode via
`tcsetattr` on startup. If the child exits abnormally (crash, `_exit`,
signal from Ctrl+C), `restore_repl_tty_mode` is never called — the PTS
stays in raw mode. The shell regains foreground pgrp via `tcsetpgrp`
but never restores termios. In raw mode, Ctrl+D (0x04) is delivered as
a literal byte, not the VEOF character — the shell's line discipline
never sees EOF.

**Fix:** `spawn_and_wait` in `userspace/shell/src/commands/exec.rs` now
saves termios via `tcgetattr(0)` before spawning a child and restores
via `tcsetattr(0, TCSANOW, &saved)` after the child exits (both normal
and error paths). This matches POSIX shell behavior — never trust the
child to restore terminal state.

**Key insight:** `tcsetpgrp` restores foreground process group routing,
but does NOT restore termios. These are independent: pgrp controls who
receives signals, termios controls input processing (canonical vs raw,
signal generation, echo). Both must be saved and restored.

**See also:** `userspace/shell/src/commands/exec.rs` (`spawn_and_wait`),
`userspace/libcluu/src/posix/termios.rs` (`tcgetattr`/`tcsetattr`),
`doc/book/terminal.md` (Shell termios save/restore).

## procmgr-name-ambiguity-session-escape (2026-07-12-registry-rename)

**Symptom:** Session processes could spawn via root procmgr instead of session-procmgr, bypassing session isolation. Edit plugins failed with `InvalidArgument` because root procmgr can't map ELF into session address space.

**Code site:** `userspace/root-procmgr/src/main.rs:1103` — `registry::init()`. `userspace/libcluu/src/registry.rs` — `lookup_service()` and `subscribe_output()`.

**Root cause:** Root procmgr registered as `"procmgr"` — ambiguous. `libcluu::spawn::spawn()` hardcoded `lookup_service("procmgr:spawn")` — always went to root procmgr. `subscribe_output("procmgr", "spawn")` had no session routing — 15 callers all got root procmgr.

**Fix:** Root procmgr renamed to `"root-procmgr"`. `"procmgr:spawn"` is now purely virtual in `lookup_service` and `subscribe_output` — routes based on `CLUU_SESSION_ID` env var. Session → `session-procmgr:spawn:{sid}`, boot → `root-procmgr:spawn`. No fallthrough either way.

**Key insight:** Both `lookup_service` AND `subscribe_output` need the virtual routing — only updating one creates an escape path. The env var is set at spawn (ProcessInfo page is read-only) — declarative, not runtime ACL.

**See also:** `userspace/libcluu/src/registry.rs`, `userspace/session-procmgr/src/spawn.rs` (sets CLUU_SESSION_ID env for children), `doc/book/sessions.md`.

## top-cid-overflow-format-width (2026-07-12-top-wrapping)

**Symptom:** `top` data rows wrapped by 2 chars on 80-col terminal. Header aligned but data rows exceeded 80 columns.

**Root cause:** CLUU CIDs are 7 digits (8388609 = 0x800001). `W_CID` was 5. Rust's `format!("{:>5}", 8388609)` does NOT truncate — it overflows to 7 chars. Format width is a MINIMUM, not a maximum.

**Fix:** `W_CID` and `W_PCID` changed from 5 to 7 (matching `W_PID`).

**See also:** `userspace/top/src/main.rs`.

## program-run-no-resize-detection (2026-07-12-libtui-resize)

**Symptom:** Edit doesn't detect terminal resize until a key is pressed. Old cells persist — white status bar fragments, misaligned content.

**Root cause:** `Program::run()` uses blocking `decode()` read. Model's viewport only updates on key event. `Model::on_resize()` existed but was never called by Program.

**Fix:** Query console dims via `_ioctl(1, TIOCGWINSZ)` at top of run loop. On change: clear screen, reset `prev_buffer`, call `model.on_resize()`.

**Key insight:** Do NOT add SIGWINCH handler or timeout reads — previous attempts lost keypresses. The blocking `decode()` must stay. ioctl at loop top is cheap and detects resize on next iteration.

**See also:** `userspace/libtui/src/program.rs`, `userspace/libtui/src/lib.rs:57` (`Model::on_resize()`), `userspace/edit/src/main.rs` (`EditModel::on_resize()`).

## session-procmgr-children-invisible-to-set-leader (2026-07-13-session-child-register)

**Symptom:** `login: set_leader failed` → `userspace: exiting with error`. Login crashes after spawning cluuterm. System survives only because cluuterm was already running.

**Root cause:** Root-procmgr's `set_leader` calls `check_member(leader_pid)` → `pid_to_session.get(pid)`. Session-procmgr spawns children (cluuterm, shell) but root-procmgr never learns their PIDs — they bypass root-procmgr's spawn path entirely. `pid_to_session` has no entry → `LeaderNotMember` → login treats it as fatal.

**Fix:** Added `PROCMGR_SESSION_CHILD_REGISTER_LABEL` (label 96). Session-procmgr sends child PID + session token to root-procmgr after each successful spawn. Root-procmgr resolves the token via `resolve_by_possession` (capability-based — possession = authority, no owner_pid check) and inserts the PID into `pid_to_session`. The session token is passed to session-procmgr via envelope caps at spawn time.

**Key insight:** This is NOT runtime ACL. `pid_to_session` is session lifecycle bookkeeping (membership tracking for exit cleanup + leader validation). Root-procmgr already maintains it for directly-spawned children. Session-procmgr children were simply missing from it.

**See also:** `userspace/root-procmgr/src/main.rs` (`handle_session_child_register`), `userspace/session-procmgr/src/spawn.rs` (`register_child_with_root`), `userspace/root-procmgr/src/session_table.rs` (`resolve_by_possession`), `doc/book/sessions.md`.

## session-service-tids-vs-tokens (2026-07-13-thread-destroy-token)

**Symptom:** `invoke_thread_destroy: missing DESTROY right` (x2) during session teardown. Threads never get destroyed — leaked.

**Root cause:** `session_service_tids` stored TIDs (from `thread_get_id()`) but `destroy_session` passed them to `thread_destroy()` which expects **token handles**. The kernel's `invoke_thread_destroy` looks up the token, checks `Rights::DESTROY` — a raw TID is not a valid token handle → `PermissionDenied`.

**Fix:** Store thread **tokens** instead of TIDs. Renamed field to `session_service_tokens`. `thread_destroy(token)` now receives a valid token with `DESTROY` right (kernel mints thread tokens with `thread_full()` which includes `DESTROY`).

**See also:** `userspace/root-procmgr/src/main.rs` (`spawn_session_procmgr_for`, `destroy_session`), `kernel/src/syscall/handlers.rs` (`invoke_thread_create` — mints `thread_full()` rights, `invoke_thread_destroy` — requires `DESTROY`).

## dead-sub-mint-grant-warnings (2026-07-13-spawn-sub-mint)

**Symptom:** `invoke_token_derive: missing GRANT right` (x2) on every session-procmgr spawn.

**Root cause:** `handle_spawn` in session-procmgr called `sub_mint` on `state.vfs_cap`, `state.registry_cap`, and `state.timeserver_cap`. These caps lack `GRANT` right — they were derived by root-procmgr with `IPC_SEND | IPC_CALL` only (no GRANT). The minted caps were stored in `minted_caps` for later revocation, but `begin_spawn` does its own token derivation independently — the sub_minted caps were never used.

**Fix:** Removed the 3 dead `sub_mint` calls. `minted_caps` is now an empty `Vec::new()`. Revocation still works (empty loop is a no-op).

**Key insight:** The sub_mint calls were a vestige from when session-procmgr used `MockKernel` for testing. The production path (`begin_spawn`) always did its own derivation. The dead code produced kernel warnings and minted caps that were either 0 (failed) or unused.

**See also:** `userspace/session-procmgr/src/spawn.rs` (`handle_spawn`), `userspace/session-procmgr/src/elf_spawn.rs` (`begin_spawn` — the real token derivation), `userspace/session-procmgr/src/cap_broker_session.rs` (`sub_mint`).

## ipc-recv-wouldblock-spin (2026-07-15-wouldblock-yield)

**Symptom:** QEMU burns 100% of a host CPU core when a userspace service blocks on `ipc_recv_any_with_sender` with `timeout_ms = u64::MAX`. The kernel idle loop uses `hlt` (0% CPU when idle), so the spin is userspace.

**Root cause:** The userspace `ipc_recv_any_with_sender` wrapper retried on `Err(WouldBlock)` in a tight `loop { continue; }` without yielding. When the kernel's `block_current_recv_wait` returned false (spurious wakeup race — ticket mismatch or pending direct delivery), the kernel returned `WouldBlock`, and userspace re-issued the syscall immediately. This starved the scheduler, prevented HLT, and burned a full core.

The same pattern existed in VFS, shell, and netd main loops: `Err(Timeout) | Err(WouldBlock) => {}` — bare continue without yield.

**Fix:** `yield_cpu()` on the `WouldBlock` retry path in `ipc_recv_any_with_sender`. Same fix applied to all service main loops (`vfs`, `shell`, `netd`): `Err(Timeout) | Err(WouldBlock) => { let _ = yield_cpu(); }`.

**Key insight:** The wrapper's job is to retry after a spurious wakeup, but it must yield between retries so the scheduler can HLT. Without the yield, the tight retry loop starves the scheduler. `yield_cpu()` is not a band-aid — it's the correct fix: the syscall contract says "try again later", and yield is how you say "later" in a cooperative scheduler.

**See also:** `userspace/libcluu/src/syscall.rs` (`ipc_recv_any_with_sender`), `userspace/vfs/src/main.rs` (main loop), `userspace/shell/src/main.rs` (async loop), `userspace/netd/src/main.rs` (recv loops), `kernel/src/syscall/handlers.rs` (`sys_recv` — `block_current_recv_wait` returning false).

## usb-hid-no-hardware-typematic (2026-07-15-usb-key-repeat)

**Symptom:** Held USB-HID keys never repeat. Unlike PS/2 keyboards (which have hardware typematic — the controller auto-repeats held keys), USB-HID keyboards only report current key state. The `handle_kbd_report` function in `usb-input` skipped held keys (`if dev.last_keys.contains(&key) { continue; }`), so no key repeat was ever generated.

**Root cause:** USB-HID boot-protocol keyboard reports are state snapshots (which keys are down right now), not transition events (press/release). The HID spec has no typematic — repeat is entirely the host's responsibility. The original code treated "key still held" as "no event", which is correct for event detection but wrong for typematic.

**Fix:** Software typematic in `usb-input/src/main.rs`. A `RepeatState` struct tracks the held key, press timestamp (`clock_now` TSC), last repeat timestamp, and cached event fields. After 500ms initial delay, repeats at 50ms intervals (20/sec — standard USB HID typematic rate). Ctrl+Alt+key shortcuts (VT switch, shutdown) are exempt — they never auto-repeat. Key release clears repeat state.

Previously a 500ms debounce was bolted onto the compositor's `SpawnCluuterm` hotkey to work around the missing repeat. That debounce was removed once proper key repeat landed in the driver.

**Key insight:** The debounce was a band-aid that masked the real bug. The correct fix is in the input driver, not the consumer. Any hotkey consumer would have needed its own debounce — N band-aids instead of 1 root-cause fix.

**See also:** `userspace/usb-input/src/main.rs` (`handle_kbd_report`, `RepeatState`, `tsc_to_ms`), `userspace/compositor/src/main.rs` (`SpawnCluuterm` hotkey — debounce removed), `doc/book/terminal.md` (usb-input section — key repeat documented).

## allocator-magic-corruption-detection (2026-07-15-alloc-hardening)

**Symptom:** Session-VFS crash during 19-terminal flood (38 clients). RIP=0x6d00cdd0, RSP=0x6d00cd60 (RIP=RSP+0x70) — `ret` jumped into NX stack page. objdump identified crash in `BTreeMap::deallocating_next_unchecked`. Root cause: heavy BTreeMap churn → heap metadata corruption → corrupted free list → dealloc followed bad pointers → wrote stack address over return address.

**Root cause:** The linked-list allocator's `dealloc` blindly trusted the `AllocHeader` — a corrupted header (from a use-after-free or buffer overflow elsewhere) would poison the free list, turning a localized corruption into a cascading smash that overwrites return addresses.

**Fix:** `ALLOC_MAGIC` (0xA110_C8ED_BEEF_F00D) written to `AllocHeader` at allocation time, validated in `dealloc`. Magic mismatch → leak the block + warn (do NOT add to free list). Size sanity check (size==0 or size > heap range → leak + warn). This stops the cascade: a corrupted header is quarantined rather than propagated.

**Key insight:** The magic doesn't fix the underlying corruption (that's a use-after-free or buffer overflow somewhere in the BTreeMap churn path). It stops the cascade from turning a localized bug into a stack smash. The OS must be resilient to churn — leak-and-warn is better than corrupt-and-crash. The 4KB VFS IPC buffer was also moved from stack to `Box<[u8]>` to reduce stack pressure during deep handler call chains.

**See also:** `userspace/libcluu/src/allocator.rs` (`AllocHeader`, `ALLOC_MAGIC`, `dealloc`), `userspace/vfs/src/main.rs` (Box IPC buffer), `doc/book/memory_model.md`.

## virtio-snd-qemu-ignores-buffer-params (2026-07-16-virtio-snd)

**Symptom:** MP3 playback at 48kHz sounds 2x speed with periodic gaps/pops. 22kHz plays correctly. Decode timing shows 1x realtime for both.

**Root cause:** QEMU's virtio-snd implementation does NOT use `buffer_bytes`/`period_bytes` for playback. `AUD_open_out` receives only `{freq, fmt, nchannels}` — no buffer/period sizes. The host audio backend (PulseAudio) handles all buffering and rate-control. `AUD_write` is rate-limited by actual playback speed.

The "2x speed" was actually severe underrun: at 48kHz, 8×4KB = 170ms of buffer drains while waiting ~400ms for the next 9p read. Half the audio content is gaps → file finishes in half the time → perceived as 2x. At 22kHz, the same 8×4KB = 372ms — barely enough to cover the 9p latency → sounds correct.

**Fix:** Full-file-to-memory load before playback starts. Eliminates all I/O stalls during playback. The device is never starved.

**Key insight:** `buffer_bytes`/`period_bytes` in `set_params` are stored by QEMU but never used for TX buffering. Don't rely on device-side buffering to smooth I/O jitter. Either pipeline enough data ahead or preload entirely.

**See also:** `userspace/mp3player/src/main.rs` (`play_mp3` full-file load), `userspace/virtio-snd/src/session.rs`, QEMU `hw/audio/virtio-snd.c` (`virtio_snd_pcm_prepare`, `virtio_snd_pcm_out_cb`).

## idle-until-runnable-missing-cli (2026-07-16-virtio-snd-irq)

**Symptom:** Kernel crash to RIP=0x2 when virtio-snd and virtio-net share IRQ 10. Only happens when both devices are active.

**Root cause:** `idle_until_runnable` did `sti; hlt` but never `cli`. The comment "Interrupts are disabled again after hlt returns" was FALSE — `hlt` does not clear IF. After the wake IRQ's `iretq` restored RFLAGS (IF=1 from the `sti`), the post-idle critical section ran with interrupts enabled. A nested device IRQ (shared IRQ 10) landing in that window corrupted the `iretq` frame under construction → jump to RIP=0x2.

**Fix:** Add `disable()` after `hlt()` in `idle_until_runnable`.

**See also:** `kernel/src/sched/thread_manager.rs:1477`, `doc/book/kernel.md`.

## dma-pool-alloc-contiguous-not-physical (2026-07-16-storage-throughput)

**Symptom:** 64KB 9p reads hang the virtio-9p service. Smaller reads work fine.

**Root cause:** `DmaPool::alloc_contiguous(N)` allocates N pages from the pool's virtual address range, but the kernel's `space_map_range` (with `source_ptr=0`) calls `pmm::alloc_frame()` per page — individual frame allocations with NO physical contiguity guarantee. `alloc_contiguous` returns `phys` = the first page's physical address only. When used as a single virtio descriptor for a multi-page buffer, the device writes past the first page into unrelated physical memory.

With 4KB reads (1 page), this is invisible — one page, one descriptor, correct phys. With 64KB reads (16 pages), QEMU writes 64KB to the first page's physical address, corrupting 15 pages of unrelated memory and never completing the transfer.

**Fix (applied 2026-07-16):** For multi-page DMA buffers, build a scatter-gather descriptor chain with one descriptor per page, using `virt_to_phys(space_token, va + i * PAGE_SIZE)` for each page's physical address. Do NOT rely on `alloc_contiguous` returning physically contiguous memory. The virtio-9p `round_trip` now uses 65 descriptors (1 req + 64 resp pages) for 256KB MSIZE.

**Additional limitation:** `DmaPool::alloc` (not just `alloc_contiguous`) rejects any single allocation >1 page (`len > PAGE_SIZE → Overflow`). This means virtqueue descriptor tables must fit in one 4KB page, limiting the main virtqueue to 256 descriptors. Large scatter-gather requests use virtio indirect descriptors (`VIRTIO_F_RING_INDIRECT_DESC`, bit 28) instead — each indirect table is a separate 1-page buffer containing 256 entries, chained in the main ring.

**Key insight:** The name `alloc_contiguous` is misleading — it means contiguous in virtual address space, not physical. Virtio devices operate on physical addresses. Always use per-page `virt_to_phys` for descriptor setup when the buffer spans multiple pages.

**See also:** `userspace/dma-core/src/dma.rs` (`alloc_contiguous`, `alloc`), `kernel/src/syscall/handlers.rs` (`invoke_space_map_range`, `pmm::alloc_frame` per page), `userspace/virtio-9p/src/main.rs` (`round_trip`), `doc/book/storage.md` (Storage throughput pass).

## drivermgr-fadt-size-check-acpi10

`cluu_acpi::Fadt::from_bytes` rejected ACPI 1.0 FADT tables because the
minimum size check was `data.len() < 244` (ACPI 2.0+ size), but QEMU's
ACPI 1.0 FADT is ~128 bytes.

### The code

`userspace/acpi/src/fadt.rs`:

```rust
if data.len() < 244 { return None; }  // WRONG — rejects ACPI 1.0
```

### Why this was hard to spot

The FADT parser was written for ACPI 2.0+ tables. QEMU's RSDP reports
revision 0 (ACPI 1.0), so the RSDT (not XSDT) is walked. The RSDT
contains the FADT at entry 0, but `Fadt::from_bytes` silently returned
`None` because the table was 128 bytes, not 244. The `acpiprobe` test
binary hit the same failure: `acpi: no FADT in RSDT`.

### Fix

Changed minimum to 112 (covers fields through `flags` at offset 108).
All fields from offset 108 onward wrapped in bounds checks (`if
data.len() >= offset+size, else 0/default`). ACPI 1.0 FADTs now parse;
ACPI 2.0+ FADTs still parse (bounds checks pass). The `acpiprobe` test
case went from FAIL to PASS.

**See also:** `userspace/acpi/src/fadt.rs`, `userspace/drivermgr/src/acpi_scan.rs`.

---

## drivermgr-cookie-to-name-overflow

Adding `drivermgr` and `drivermon` to `PRIMORDIAL_SERVICES` (D2.7)
caused an init panic: `index out of bounds: the len is 8 but the index
is 8`.

### The code

`userspace/init/src/main.rs`:

```rust
let mut cookie_to_name: [(usize, &str); 8] = [(0, ""); 8];  // FIXED SIZE 8
// ...
cookie_to_name[num_primordials] = (exit_cookie, service.name);  // OVERFLOW
```

### Why this was hard to spot

The array was sized for the original 8 primordial services. D2.7 added
2 more (drivermgr, drivermon), making 10. The panic happened during
boot, AFTER init launched the services — so the serial log showed
drivermgr and drivermon starting, then init panicking. The panic
message pointed at `main.rs:72`, not at the array declaration.

### Fix

Changed array size from 8 to 16. The fix is one character, but finding
it required reading the panic message and tracing the `num_primordials`
counter.

**Lesson:** Fixed-size arrays for primordial tracking must be sized
with margin. Use `PRIMORDIAL_SERVICES.len()` or a power-of-2 margin.

**See also:** `userspace/init/src/main.rs:54`, `userspace/init/src/services.rs:140`.

---

## drivermgr-toml-parser-bare-int-arrays

The `libcluu::toml` parser rejected manifests containing
`devices = [0x1001, 0x1042]` because `parse_string_array` only accepted
quoted strings inside arrays.

### The code

`userspace/libcluu/src/toml.rs`:

```rust
if !rest_trimmed.starts_with('"') {
    return Err(ParseError { line: lineno, msg: "array elements must be quoted strings" });
}
```

### Why this was hard to spot

The parser was written for `envelopes.toml` and `autostart.toml`, which
use quoted-string arrays. Container manifests (generated by
`container-build` D2.2) emit hex integer arrays like
`devices = [0x1001, 0x1042]`. The parser's scalar handler already
treated bare values as strings (`vendor = 0x1af4` →
`TomlValue::String("0x1af4")`), but the array handler didn't follow the
same pattern.

### Fix

Extended `parse_string_array` to accept bare values: if an element
doesn't start with `"`, read up to the next `,` or `]`, trim, store as
a string. This is backward-compatible (quoted strings still work) and
consistent with the scalar bare-value handling.

**See also:** `userspace/libcluu/src/toml.rs:239`, `tools/container-build/src/main.rs`.

---

## drivermgr-vfs-view-required-for-readdir

drivermgr's `vfs.readdir("/var/images/")` returned `PermissionDenied`
because drivermgr hadn't registered a VFS view.

### Why this was hard to spot

Other services (procmgr, cat, shell) get their VFS views registered by
procmgr at spawn time. drivermgr is spawned by init, not procmgr, so no
view was registered. The VFS client calls succeeded for
`subscribe_output("vfs", "main")` but failed on the actual readdir
because VFS checks the client's view on every operation.

### Fix

1. Wired `TOKEN_VFS_VIEW_MGR` to drivermgr in `init/src/wiring.rs`
2. drivermgr calls `register_vfs_view()` (sends `VFS_SET_VIEW_LABEL`
   with `/` → `/` mount) before readdir

**See also:** `userspace/drivermgr/src/main.rs` (`register_vfs_view`),
`userspace/init/src/wiring.rs` (`ServiceKind::DriverMgr`).

---

## drivermgr-irq-token-scoping-not-supported

The kernel's `invoke_token_derive_scoped` does not support
`ObjectRef::Irq` — only `VfsViewManager`, `BlockRegion`, and
`DeviceRegion`.

### Why this was hard to spot

The D3.1 plan specified `TokenDeriveScoped` for per-IRQ-line scoping.
The kernel handler has a wildcard `_` arm that rejects with "token type
not scoping-capable", but this isn't documented anywhere. The existing
per-driver IRQ tokens in `init/src/context.rs` all use `token_derive`
(non-scoped), which inherits the parent's obj_ref — but nobody connected
this to the kernel limitation.

### Fix

Used `token_derive` (non-scoped) with `IRQ_HANDLE | IRQ_ACK` rights
instead of `token_derive_scoped`. IRQ-line scoping is advisory:
drivermgr tracks the requested irq_line and passes it as a spawn param.
The driver passes it to `irq_attach(token, ep, irq_line)` which checks
only the `IRQ_HANDLE` right, not the obj_ref.

**See also:** `userspace/devmgr/src/handlers.rs` (`handle_mint_irq_cap`),
`kernel/src/syscall/handlers.rs` (`invoke_token_derive_scoped`).

## drivermgr-spawn-mode-toml-section-header

### The code

`etc/drivermgr.toml`:
```toml
spawn_mode = "observe"
```

`parse_spawn_mode_from_initrd` in `userspace/drivermgr/src/main.rs`:
```rust
if let Some(t) = doc.table("drivermgr") {
    if let Some(s) = t.get_str("spawn_mode") { ... }
}
```

`read_skip_drivers_flag` in `userspace/init/src/main.rs`:
```rust
if let Some(t) = doc.tables.first() { ... }
```

### Why this was hard to spot

Init correctly read `spawn_mode = "spawn"` and skipped driver services
(logged "init: skipping virtio-blk (spawn mode)"). But drivermgr
defaulted to Observe and subscribed to `vfs:mounted` — which blocks
forever because virtio-blk was never spawned. System hung with no
error message. Both functions parsed the same file but used different
TOML APIs: `doc.table("drivermgr")` expects a `[drivermgr]` section;
`doc.tables.first()` grabs the first table regardless of name.

### Fix

Added `[drivermgr]` section header to `etc/drivermgr.toml`:
```toml
[drivermgr]
spawn_mode = "observe"
```

**See also:** `userspace/drivermgr/src/main.rs` (`parse_spawn_mode_from_initrd`),
`userspace/init/src/main.rs` (`read_skip_drivers_flag`).

## drivermgr-two-phase-boot-vfs-blocked

### The code

drivermgr's `load_bind_rules` subscribes to `vfs:mounted` and reads
manifests from `/var/images/` via VFS. In spawn mode, VFS blocks in
`setup_mounts` waiting for `blkdev:main` — which is virtio-blk, which
drivermgr is supposed to spawn. Circular dependency.

### Why this was hard to spot

The deadlock is silent: VFS never logs "mounted", drivermgr never
logs "bind rules loaded", and there's no timeout on `subscribe_output`.
The system just stops at "vfs: initrd mounted" + "drivermgr: PCI scan
complete".

### Fix

Two-phase boot: drivermgr reads `sys/virtio-blk.manifest.toml` directly
from initrd memory (mapped at `INITRD_USER_BASE` via `PARAM_INITRD_SIZE`),
spawns virtio-blk, THEN subscribes to `vfs:mounted`. Phase 2 loads
remaining manifests from userdisk after VFS unblocks.

**See also:** `userspace/drivermgr/src/main.rs` (`run` two-phase flow,
`load_initrd_bind_rules`), `userspace/vfs/src/main.rs` (`setup_mounts`
blocking on `blkdev:main`).

## drivermgr-param-device-path-bdf-packing

### The code

`PARAM_DEVICE_PATH` is a `u64` in `ProcessInfo.params[19]`. Device
paths are strings (`/pci/00:04.0`), but params are u64. No string
encoding exists.

### Why this was hard to spot

The plan said "Read PARAM_DEVICE_PATH from process_info().params" and
"query drivermgr for DeviceNode" — implying a string path. But the
spawn IPC only passes u64 param overrides. There's no mechanism to
pass a string through params (CWD uses offset+length into ProcessInfo
page, but that's procmgr-controlled).

### Fix

For PCI devices, `PARAM_DEVICE_PATH` packs BDF as
`(bus << 16) | (dev << 8) | func`. Drivers reconstruct the canonical
path from BDF. ACPI device spawn (D6) will pack the HID differently.

**See also:** `userspace/libcluu/src/ipc.rs` (PARAM_* constants),
`userspace/virtio-core/src/pci.rs` (`find_virtio_device_with_params`).

## drivermon-fault-ipc-reply-required

### The code

Kernel sends fault IPC (label 0xFA017) to the thread's fault endpoint.
words[5] = reply_id. If reply_id != 0, the thread is suspended waiting
for a handler decision. If the handler doesn't reply, the thread hangs
forever in a faulted state.

### Why this was hard to spot

The kernel fault IPC is fire-and-forget from the sender's perspective
(the kernel sends via `try_send`), but the *reply* is blocking — the
faulted thread won't be scheduled until the handler replies with RESUME
(label=0) or KILL (label≠0). No reply = silent hang.

### Fix

drivermon's `handle_fault_ipc` checks `msg.words[5]` (reply_id). If
non-zero, it replies with `FAULT_REPLY_KILL` (label=1) via
`reply_to_sender`. The kernel kills the thread. The actual restart
goes through the proven PROC_EXIT_LABEL path (D4.1), not the fault
IPC — avoiding a parallel restart mechanism.

**See also:** `userspace/drivermon/src/main.rs` (`handle_fault_ipc`),
`kernel/src/syscall/handlers.rs` (`handle_fault_reply`).

## drivermon-exit-notify-pid-correlation

### The code

procmgr's PROC_EXIT_LABEL sends `words[0]=exit_cookie, words[1]=exit_code`.
drivermon's RuntimeTable is keyed by PID. No way to look up the entry
by cookie — the cookie is procmgr-internal.

### Why this was hard to spot

The plan said "Look up RuntimeEntry by cookie (not PID — race guard per
Oracle)." But drivermon never sees the cookie until exit time, and
procmgr assigns it internally at spawn. There's no IPC for procmgr to
tell drivermon "this PID has this cookie."

### Fix

Extended PROC_EXIT_LABEL to include PID in `words[2]` (5 exit
notification sites in procmgr). drivermon looks up by PID, not cookie.
The race guard (cookie vs PID) is handled by the fact that drivermon
receives the exit AFTER procmgr has already cleaned up — by the time
drivermon looks up the PID, the entry is stale but the PID is still
in the table (drivermon doesn't remove on exit, only on re-register).

**See also:** `userspace/root-procmgr/src/main.rs` (5 `notify_msg.words[2] = pid` sites),
`userspace/drivermon/src/supervision.rs` (`handle_exit_notify`).

## cluu-vfs-blocking-subscribe-before-main-loop

**Context:** D5 removed virtio-9p from init's PRIMORDIAL_SERVICES (VFS
mounts /host from virtio-9p's "hostfs" endpoint). Nothing boot-critical
uses /host — only cluuamp and file browser.

**Bug:** VFS called `subscribe_output("hostfs", "main")` in `setup_mounts()`
BEFORE its async main loop started. `subscribe_output` calls
`wait_for_grant` which blocks forever (`u64::MAX` timeout — by design,
comment: "A hang is data"). VFS is single-threaded, so the blocking
subscribe hung the entire VFS before it could serve ANY request. System
never reached login.

The `match Err(e)` arm around `subscribe_output` only catches IPC errors,
not "service not yet registered" — that case blocks forever inside
`wait_for_grant`.

**Fix:** Use `request_subscription` (fire-and-forget) for hostfs, then
mount /host lazily in `handle_registry_message` when the grant arrives.
Mirrors the existing tty:N subscribe pattern (line 394: fire-and-forget,
grants handled in main loop).

**Lesson:** In a single-threaded service, never call a blocking subscribe
for a non-critical dependency before the main loop starts. The
"no-timeouts" rule (memory/feedback_no_timeouts.md) means a missing
producer hangs forever — fine for critical dependencies (blkdev,
procmgr), fatal for optional ones (hostfs). Use fire-and-forget
`request_subscription` + lazy mount in the event loop instead.

## cluu-fadt-dsdt-pointer-offset

**Context:** D6 ACPI DSDT enumeration. drivermgr reads FADT to find the
DSDT pointer, then maps and parses the DSDT for PNP devices.

**Bug:** `dsdt_phys()` returned the value at FADT offset 36, which in
QEMU's FADT rev=1 is the FIRMWARE_CONTROL (FACS) pointer, not the DSDT
pointer. DSDT is at offset 40.

The existing `Fadt` struct names the field at offset 36 `sci_command`
and the field at offset 40 `smi_command_port` — both misnomers from a
misreading of the ACPI 2.0 FADT layout. In ACPI 1.0 (FADT rev=1):
- Offset 36: FIRMWARE_CONTROL (FACS pointer, 32-bit)
- Offset 40: DSDT pointer (32-bit)

In ACPI 2.0+ (FADT rev>=3):
- Offset 36: DSDT (32-bit legacy)
- Offset 40: SMI_CMD (was Reserved+Profile+SCI_INT)

**Fix:** `dsdt_phys()` returns `smi_command_port` (offset 40) for ACPI
1.0 FADTs, and `x_dsdt` (64-bit) for ACPI 2.0+ FADTs when non-zero.

**Lesson:** The ACPI FADT layout changed between revisions. Don't assume
a single field mapping — check the FADT revision and use the correct
offsets. The field names in existing code may be wrong — verify against
the actual byte values in QEMU's tables, not against the field names.

## cluu-eisa-id-big-endian

**Context:** D6 DSDT parser extracts _HID EISA IDs from AML Device()
objects. EISA IDs are 32-bit DWORDs encoding 3 letters + 4 hex digits
(e.g. PNP0303 = 0x41D00303).

**Bug:** The parser read EISA ID DWORDs with `u32::from_le_bytes` —
little-endian. But QEMU's AML stores EISA IDs in big-endian byte order
within the DWORD: bytes `41 d0 03 03` = 0x41D00303 = PNP0303. Reading
as little-endian gives 0x0303D041, which decodes to garbage HID strings
like `@@KD041` instead of `PNP0303`.

**Symptom:** All EISA-encoded _HID values in the DSDT produce garbage
PNP IDs. String-encoded _HID values (e.g. `"PNP0A06"`) work fine because
they don't go through the DWORD path.

**Fix:** Use `u32::from_be_bytes` for EISA ID DWORDs in `parse_hid`.

**Lesson:** AML DWORDs are spec'd as little-endian, but EISA IDs are
defined as big-endian 32-bit values in the EISA specification. QEMU's
AML compiler stores EISA ID bytes in big-endian order within the DWORD.
When parsing AML data that represents EISA IDs, use big-endian; for
general AML DWORDs (buffer sizes, etc.), use little-endian.

## cluu-rc-boot-replaces-autostart-toml

### The code

CLUU boot service startup was declarative via `etc/autostart.toml`
(`[[service]] image = "console"`). Replaced with a shell-script-style
`etc/rc.boot` — a text file with `start`, `wait`, `probe`, `mount` verbs
interpreted line-by-line by procmgr at boot. After rc.boot completes,
procmgr proceeds to login.

### Why this was hard to spot

The first attempt had procmgr spawn the shell binary in "script mode"
(argv[1] = `/etc/rc.boot`) to interpret rc.boot via `shellrc::source_file`.
The shell binary was mapped and `thread_resume`'d, but `main()` never
executed — `_start` → `posix::init_tls()` / `init_stdio()` /
`registry::init()` silently failed because FdInherit and token wiring
weren't set up for a boot-time shell. No debug_print output, no fault,
no panic — just silence. The serial log showed the ELF mapped and the
thread resumed, but nothing after.

### Fix

procmgr reads rc.boot directly (like busybox init reads `/etc/inittab`).
No shell binary is involved at boot. Each line is parsed as:

- `start <image>` → calls `autostart_container(image)` (same code path
  as the old autostart.toml)
- `wait <service> [output]` → polls `registry::subscribe_output` until
  the service registers (blocking with yield)
- `probe <bus>` → no-op (drivermgr auto-probes PCI + ACPI at its own
  startup)
- `mount <path> <service>` → deferred to service self-registration via
  `VFS_MOUNT_LABEL`

Shell builtins (`start`, `wait`, `probe`, `mount`) still exist in
`userspace/shell/src/commands/builtins/rc.rs` for **interactive** use,
but they use different IPC paths (`PROCMGR_START_IMAGE_LABEL`,
`DRIVERMGR_PROBE_LABEL`) — procmgr's internal rc.boot interpreter calls
`autostart_container()` directly.

### Key insight

At boot time, procmgr **IS** the script interpreter. The shell binary is
for interactive sessions. Trying to use the shell binary as a boot script
interpreter fails because the shell's `_start` assumes it was spawned by
a fully-booted system (FdInherit, token table, VFS view all wired by
procmgr's normal spawn path). The boot script interpreter must run
**inside** procmgr, before any user shell exists.

### Evolution: rc.boot → system.toml

The rc.boot shell-script approach was replaced with `etc/system.toml` —
a declarative TOML config (like Linux's `/etc/fstab` + systemd units).
Two sections, two readers:

- `[[mount]]` — VFS reads from initrd at boot (like fstab). `required=true`
  blocks VFS until the service registers; `required=false` uses
  fire-and-forget (mounts lazily when the service registers via registry
  grant or `VFS_MOUNT_LABEL`).
- `[[service]]` — procmgr reads from VFS at boot (like systemd units).
  Each entry has an `image` field; procmgr calls `autostart_container()`.

Driver bringup stays in `etc/drivermgr.toml` (TOML, read by drivermgr).
Shell builtins (`mount`, `start`, `probe`, `wait`) remain for interactive
use — manual driver probing, manual service start, manual mounts from
the shell prompt.

## cluu-wait-for-grant-drops-non-matching-grants

### The code

VFS optional mount `/host` (hostfs) was fire-and-forget
(`request_subscription`), sent before the blocking
`subscribe_output("blkdev")` completed. The hostfs grant arrived on the
shared control endpoint while `wait_for_grant` was blocking for blkdev.
`wait_for_grant` checked `svc == service_name` (hostfs != blkdev) →
silently discarded the grant. Session VFS never mounted `/host`.

### Why this was hard to spot

The serial log showed `hostfs:main grant forwarded` — the registry
delivered the grant. But VFS never logged `mounted /host` for the
session VFS. No error, no timeout — the grant was silently consumed by
the wrong `wait_for_grant` call. The root VFS worked because
`request_subscription` was called after its blocking subscribes (the
system.toml path), but the session VFS (empty initrd, fallback path)
called `request_subscription` before `subscribe_output("blkdev")`.

### Fix

Move `request_subscription("hostfs")` to AFTER `setup_mounts()` returns.
All blocking `subscribe_output` calls complete first; the
fire-and-forget hostfs subscribe is sent only when the control endpoint
is free. The grant is then processed by `handle_registry_message` in
the main loop.

### Key insight

`wait_for_grant` is a filter, not a queue. It receives ALL messages on
the control endpoint and discards non-matching ones. Never mix
fire-and-forget `request_subscription` with blocking `subscribe_output`
on the same control endpoint — the blocking caller will eat the
fire-and-forget grant.

## cluu-compositor-cursor-clobbered-by-animated-win

### The code

Compositor main loop (`userspace/compositor/src/main.rs`):

```rust
compose::recompute_dirty(&mut comp);   // reads SHM → writes cell_grid
compose::render_status_row(&mut comp);

if comp.cursor_needs_render {          // ← only re-applies cursor when flag set
    comp.render_cursor();
    comp.cursor_needs_render = false;
}
```

`recompute_dirty` walks every dirty cell and overwrites `cell_grid[idx]`
with the composed result (client SHM content for interior cells).
`cursor_needs_render` is set **only** in `handle_mouse_event`
(`window_mgr.rs`).

### Why this was hard to spot

Over a static window (terminal, shell, edit), the cell under the mouse
pointer is not damaged every frame, so the cursor block written by
`render_cursor` survives in `cell_grid` until the next mouse move. The
bug is invisible there.

cluuamp is an animated visualizer that continuously `WIN_DAMAGE`s every
cell it draws — including the one under the pointer. Every compositor
iteration:

1. `recompute_dirty` overwrites `cell_grid[cursor_cell]` with cluuamp's
   wave content.
2. `render_cursor` runs only if `cursor_needs_render` is set — and with
   the mouse stationary, no mouse event arrived that iteration, so the
   flag is false.
3. The cursor glyph is gone for that frame.

With mouse jitter within the same cell, the flag toggles on/off across
iterations → wave content and cursor block alternate rapidly → visible
flicker / "wrong draw artifact." With the mouse fully stationary, the
cursor simply disappears over cluuamp.

The symptom looked like a cursor-blink bug (cluuterm's 500 ms text-cursor
blink is a separate mechanism in `tty_backend.rs`), but the actual cause
was the compositor's software mouse cursor being clobbered by an
animated client's damage stream.

### Fix

Call `render_cursor()` unconditionally after `recompute_dirty`:

```rust
compose::recompute_dirty(&mut comp);
compose::render_status_row(&mut comp);
comp.render_cursor();
comp.cursor_needs_render = false;
```

Idempotent and cheap: `render_cursor` is a single `u64` write to
`cell_grid[cursor_cell]`. When the cell already holds the cursor block,
`prev_cell_grid == cell_grid` at the next `tick_frame` and no extra
frame is scheduled. The `cursor_needs_render` flag is still useful as an
optimization hint for callers that *know* they didn't move the cursor,
but it must not be the only thing keeping the cursor alive across
arbitrary client damage.

### Key insight

In a dirty-cell compositor, any overlay the compositor itself paints on
top of client content (mouse cursor, status bar, focus chrome) must be
re-applied **after** every pass that pulls client content into the cell
grid — not gated on a flag set only by the overlay's own input handler.
Clients can damage the overlay's cells at any time; gating re-apply on
"the overlay moved" works only when no client ever redraws underneath
the overlay.

Same bug class as [[cluu-compositor-focus-chrome-stale]] and
[[cluu-modal-damage-clamps-border-out]]: a state-dependent overlay and
a damage-driven re-render disagree about who owns a cell, and the
damage path wins silently.

## cluu-eq-gradient-per-cell-color

### The code

cluuamp EQ response curve (`userspace/cluuamp/src/view.rs::draw_eq_window`):

The EQ graph area is only **3 cell rows tall** (`layout.rs`:
`eq_graph = Rect::new(15, 13, 52, 3)`). The curve is drawn with braille
dots, each cell getting a color from a 25-entry gradient palette
(`viscolor.rs::EQ_CURVE_COLORS`, red → yellow-orange → green, mapping
`f=0`..`24` where 0 = -12 dB and 24 = +12 dB).

### Why this was hard to spot

The first implementation colored each cell by its **row position**:

```rust
let f = 24 * (graph.height - 1 - row) / denom;
let curve_fg = viscolor::eq_curve_color(f);
```

With `graph.height = 3`, this produces exactly 3 values of `f`: 24, 12,
0 → green, yellow-orange, red. The 25-entry palette was correctly
defined, the color emission path (SGR `38;5;N`) was correct, the ANSI
parser and compositor palette were correct — but the user saw only 3
colors because the color was derived from the cell's Y position, not
from the curve's actual dB value at that position.

The bug is invisible in a flat curve (all bands at the same gain → all
cells get the same color regardless of approach), and it produces
"correct-looking" output for a flat curve at any gain level. It only
reveals itself when the curve slopes: the gradient should vary
horizontally across the graph, but a row-based coloring can only vary
vertically — and with 3 rows, that's 3 colors max.

### Fix

Color each cell by the **actual interpolated dB value of the curve at
that column**, not by the cell's row. A parallel `f_sum`/`f_cnt` array
accumulates the `f` value per cell during dot drawing; the average `f`
per cell selects the gradient color:

```rust
let mut f_sum = alloc::vec![0u32; graph.width * graph.height];
let mut f_cnt = alloc::vec![0u8; graph.width * graph.height];
// ... in the dot loop:
f_sum[idx] += f as u32;
f_cnt[idx] += 1;
// ... when rendering:
let avg_f = f_sum[idx] / f_cnt[idx] as u32;
view.set(..., Cell::new(ch).fg(viscolor::eq_curve_color(avg_f as usize)));
```

Now a curve sloping from -12 to +12 across the graph width shows the
full red → orange → yellow → green gradient horizontally.

### Key insight

When a display region has few cells in one dimension (here, 3 rows),
deriving a gradient color from that dimension's position quantizes the
gradient to that many steps. To get the full palette resolution, derive
the color from the **data value** being rendered, not from the cell's
spatial position. The palette can have 25 entries; the spatial grid
only has 3 — the data is the only source that can exercise all 25.

This is the same trap as coloring a heatmap by cell row when the value
range is wider than the row count: the palette is defined, the rendering
is correct, but the color resolution is capped at the grid resolution.
