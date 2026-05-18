# Frame typing + unified process model

**Status:** spec, 2026-05-18
**Drives:** closes the frame-alias UAF that broke compositor RSV at runtime.
**Pauses:** cluuterm graphical session work until phases 1–3 land.

## Why

Two flaws meet in the wrong place.

**Flaw 1 — no kernel-side frame ownership.** PMM is a pure buddy
allocator with bitmap + intrusive free lists. It does not know what a
physical frame is being used for. `frame_registry` covers only
user-visible tokens (FrameAllocate) and SpaceGrant shares — never
intermediate page tables (PT/PD/PDPT/PML4) and never plain user
leaves. `teardown_user_pages` discovers PT/PD/PDPT addresses by
walking the live PML4; there is no global record of "this phys is
S's PDPT for the 0x400000–0x5fffff window". Two paths can end up
holding the same phys with no interlock.

**Flaw 2 — kernel duplicates procmgr's job.** Three parallel models
of "userspace process" exist today:

1. `sched/process.rs::Process` + `PROCESS_MANAGER`, only used for
   init's primordials.
2. `mm/space_repository.rs` storing `AddressSpace` per `AddressSpaceId`,
   used by everything procmgr spawns via `invoke_space_create`.
3. Procmgr's own `pid → space_token / cookie / container` tables.

Lifecycle gates live in different places: kernel space_destroy ↔
`Process::drop` ↔ procmgr cascade-kill ↔ vfs container_cleanup. None
of them know about the typed state of the frames being torn down.

Concrete failure: 2026-05-18 EventRing trace
`phys=0x2a04e000 curr=teardown_pdpt prior=teardown_leaf
intermediate_allocs=<none>`. Same frame freed once as login's user
leaf and again as user-compositor's PDPT, with no alloc in between.
The frame had two owners; the first to free poisoned the second's
table.

## Goal

The kernel knows exactly five things about userspace:

1. **Threads** (`Thread` in `sched/thread.rs`).
2. **Address spaces** (PML4, kernel-half copy, region descriptors).
3. **Endpoints** (IPC).
4. **Tokens** (capabilities).
5. **Typed frames** (Untyped / PageTable / UserData / Grant / Device
   / KernelHeap).

That is it. No `Process` struct. No primordial registry. No
process-state mirror.

Procmgr (userspace) owns:

- pid allocation
- parent / child trees
- containers
- session/login state
- exit notification fanout
- restart policy
- cascading kill
- name → space_token mapping

Init (userspace) owns primordial liveness monitoring, same way it
does today for non-procmgr roles. Procmgr's own death is observed by
init via PROC_EXIT_LABEL.

## Non-goals

- Not changing IPC semantics.
- Not changing how userspace looks (procmgr stays the
  spawn/exit/wait orchestrator).
- Not adding userspace syscalls.
- Not reworking PMM's buddy allocator internals. Buddy stays; only
  the metadata layer above it changes.
- Not changing the rendezvous fast path or `recv_any`.

## Frame-type model

```
              ┌───────────┐
   alloc ───▶ │  Untyped  │ ◀──── final dec_ref
              └───┬───┬───┘
                  │   │
       retype_pt  │   │  retype_user_data
                  ▼   ▼
       ┌──────────┐   ┌─────────────┐    retype_grant
       │ PageTable│   │  UserData   │ ─────────────┐
       │ level:   │   │ owner: SpId │              ▼
       │   PML4/  │   │ refcount    │       ┌─────────────┐
       │   PDPT/  │   └─────────────┘       │   Grant     │
       │   PD/PT  │                         │ refcount    │
       │ owner:   │                         │ (≥2 spaces) │
       │   SpId   │                         └─────────────┘
       └──────────┘
```

Other variants used at boot only (KernelHeap, Device, BootReserved)
follow the same enum. They never participate in user teardown.

### Invariants

- A frame's type is set by **retype**, never by direct write.
- `pmm::free_frame` accepts only frames whose state is `Untyped`
  with `refcount == 0`.
- Retype back to Untyped requires `refcount == 0` and removal from
  all containing tables (caller's job).
- PageTable refcount is the number of child PT/PD/PDPT/PML4 entries
  that point to it (the seL4 "vstore" semantics — every parent
  reference is one ref). For non-shared user PTs, refcount is 1
  exactly (one PD entry).
- UserData refcount is the number of leaf PTEs pointing to it.
  Typically 1; ≥2 for grants / MAP_SHARE_PHYS.
- Grant is the unified form for any shared user leaf; once any
  refcount hits 2, the frame is retyped UserData → Grant atomically.
  When refcount drops back to 1 the inverse retype happens (Grant →
  UserData) — implementation can defer this if too costly; just keep
  Grant alive until refcount == 0.

### Storage

```rust
// In kernel/src/mm/frame_table.rs (new file)
#[repr(u8)]
enum FrameTag {
    Untyped     = 0,  // implicit: zero entry == untyped
    UserData    = 1,
    PageTable   = 2,
    Grant       = 3,
    Device      = 4,
    KernelHeap  = 5,
    BootReserved= 6,
}

struct FrameMeta {
    tag: FrameTag,
    refcount: u16,        // 0..=N references
    owner: u16,           // AddressSpaceId.0 truncated to u16 (0 for Untyped/Device/Heap)
    extra: u8,            // PT level (1..4) when tag=PageTable, else 0
}

// Per-frame array sized to max_managed_frame. ~6 bytes/frame.
static FRAME_META: Mutex<Box<[FrameMeta]>> = ...;
```

Memory cost: ~6 bytes/frame. 8 GB RAM = ~2M frames × 6 = ~12 MB
static kernel BSS. Allocated once during init.

(`extra` exists because we need to know which level a PT is at when
we free it — different levels need different refcount semantics.)

### API surface

```rust
// kernel/src/mm/frame_table.rs
pub fn retype_to_pt(phys: u64, level: u8, owner: AddressSpaceId) -> Result<(), Error>;
pub fn retype_to_user(phys: u64, owner: AddressSpaceId) -> Result<(), Error>;
pub fn retype_to_grant(phys: u64) -> Result<(), Error>;
pub fn retype_to_untyped(phys: u64) -> Result<(), Error>;

pub fn inc_ref(phys: u64) -> Result<u16, Error>;  // returns new refcount
pub fn dec_ref(phys: u64) -> Result<u16, Error>;  // returns new refcount; auto-untype at 0

pub fn tag_of(phys: u64) -> FrameTag;
pub fn owner_of(phys: u64) -> Option<AddressSpaceId>;
```

PMM exposes `alloc_*_untyped` returning a typed Untyped frame; callers
must retype before passing to any mapper. Existing public alloc fns
are renamed to make the boundary explicit:

```rust
pub fn alloc_frame_untyped() -> Option<u64>;     // was alloc_frame
pub fn alloc_order_untyped(order: usize) -> Option<u64>;  // was alloc_order
pub fn free_frame_untyped(phys: u64);            // was free_frame; asserts tag=Untyped, refcount=0
```

Tag and ring instrumentation already in tree (commits 0519228,
c2d02cc, c8cc035) become assertions: every alloc records the tagged
caller, every retype records the type transition. If invariants
hold, no double-free can ever fire.

## Unified process model

### What leaves the kernel

- `kernel/src/sched/process.rs` — `Process`, `ProcessId`,
  `ProcessState`, `ProcessType`, `ProcessInitState`, `Process::Drop`.
- `kernel/src/sched/process_manager.rs` — `PROCESS_MANAGER`,
  `ProcessManager::spawn_user`, `spawn_kernel`, `reap`, `with_process*`.
- `kernel/src/sched/spawn.rs` — anything that wraps Process.

### What stays in the kernel

- `Thread` + `THREAD_MANAGER` (kernel needs to schedule threads).
- `AddressSpace` + `space_repository` (kernel maps page tables).
- `Token` table + `OpaqueScope` + revocation (kernel enforces caps).
- IPC endpoints.

### What procmgr gains (already mostly has)

- Single authoritative `Process` model in procmgr's address space.
- Boot-time injection: init hands procmgr the initial token set
  (kernel does not need a `Process` for `init` itself — `init` is
  just thread 1 in kernel terms, with space 0 as the kernel space).
- Primordial monitoring via `PROC_EXIT_LABEL` is already there
  (commit f3be5cf and predecessors); we just promote it to be the
  only mechanism.

### Lifecycle gating

Today's bug: `Process::drop` and `invoke_space_destroy` both call
`teardown_user_pages` and don't interlock cleanly. After redesign,
**only one path** exists: a thread (or procmgr on behalf of a dying
process) invokes `space_destroy(space_token)`. The kernel:

1. Removes from `space_repository`. If already gone → return
   `NotFound` (caller's race).
2. Walks the PML4, calling `dec_ref` on each user-half PT/PD/PDPT.
3. For each leaf PTE: `dec_ref` on the user data / grant frame.
4. `dec_ref` on the PML4 itself.

`dec_ref` automatically retypes Untyped + `pmm::free_frame_untyped`
when the count hits zero. The teardown loop never directly calls
`pmm::free_*`. No more `freed: BTreeSet`. No alias possible because
shared frames have refcount ≥ 2 and stay alive until everyone is
done.

## Migration phases

This is too big for one PR. Plan four phases; each leaves the kernel
green and the login flow runnable.

### Phase 1 — typed frames, behaviorally identical

Land the `FrameMeta` table + `retype_*` API. Wire every existing
alloc/free site to retype on alloc and to retype back on free.
Leave logic unchanged (refcount inc/dec at every map/unmap is a
no-op because we treat refcount as advisory in this phase).

Goal: kernel runs exactly as today; the new `FrameMeta` state
matches what we already do (alloc → mark UserData / PT, free → mark
Untyped). Add audit assertions behind a feature flag; expect them to
fire on the existing alias bug, but only as warnings.

Exit: clean boot. Smoke harness green. RSV bug still happens
(because we haven't enforced refcount yet).

### Phase 2 — refcount semantics + SHARED_PHYS / SpaceGrant routed through Grant

Replace ad-hoc `SHARED_PHYS` PTE flag handling and the
`frame_registry` for grants with the unified Grant variant. Every
mapping of a user phys does `inc_ref(phys)`. Every unmap does
`dec_ref(phys)`. `teardown_user_pages` becomes a `dec_ref` loop.

`pmm::free_frame_untyped` asserts `refcount == 0`. Any path that
double-frees triggers the assertion (we keep the soft-fail under a
debug flag during the migration, then turn it into a hard error).

Exit: the 2026-05-18 alias trace no longer happens. Compositor RSV
fault is gone. Manual root/root login → cluuterm → /bin/shell works
end-to-end without leaks.

### Phase 3 — retire kernel-side Process

Delete `sched/process.rs`, `sched/process_manager.rs`, the
`PROCESS_MANAGER` static, the `Process::Drop` teardown path. Adjust
the boot path: init does not need a `Process` struct; it is a Thread
running on the kernel address space with primordial tokens minted at
boot.

Procmgr already has a process table — verify it covers primordials
too (or that they're outside its purview because they don't reap).

Move telemetry that today reads `PROCESS_MANAGER` to consume
`THREAD_MANAGER` + `space_repository`. Add a `procmgr_query_state`
IPC for anything that needs procmgr's view from inside the kernel
(should be zero such sites).

Exit: kernel knows only the five things listed above. `Process` is
gone. Boot + login still green.

### Phase 4 — diagnostic cleanup + spec for retypes back-to-Grant

Remove the temporary soft-fails in PMM (audit at line 367, pre-check
at free_order_tagged that double-checks bitmap). The frame_table
layer now guarantees these invariants by construction.

Decide on the Grant → UserData reverse-retype (when refcount drops
from 2 → 1). Either:
- (a) Keep Grant alive until refcount = 0 (simpler).
- (b) Implement the reverse retype with a per-frame lock.

Recommend (a) for now; visit (b) if memory pressure complains.

Then trim the diagnostic ring: bump back to 1024 slots or keep at
4096 but compile out behind a debug feature.

## Out-of-scope items (call them out)

- Procmgr changes are explicitly out of scope here. Procmgr already
  has process discipline; we just stop shadowing it in the kernel.
- The pid → tid mapping question (procmgr surfaces "pid" but kernel
  only knows tids) is unchanged.
- C runtime / newlib touching: none.
- Compositor / cluuterm / shell / vfs: none. The change is invisible
  above the kernel ABI line.

## Open questions to resolve before Phase 2

1. **Grant → UserData reverse-retype timing.** Punt to phase 4.
2. **What kernel-internal allocations need typed frames?**
   `KernelHeap`, `Device`, `BootReserved` — these never participate
   in user teardown but the frame_table needs to mark them so
   user-teardown `dec_ref` paths never accidentally reach them.
3. **Bootloader / physmap pages.** Same — mark `BootReserved` at
   init, never retype.
4. **AddressSpace::new_user's copy_kernel_half.** This currently
   reads PML4[256..512] from CR3 and writes to the new space. The
   kernel-half PDPTs are *shared* across every space. Under Phase 2,
   inc_ref every kernel-half PDPT on every new_user call; in
   practice this just means N copies hold ref to the same kernel
   PDPT frames. Verify no path frees them.
5. **Token revocation cascade.** When a space is torn down, do its
   minted tokens need to be revoked here, or already handled by
   userspace? Verify against current behavior.

## Risk

- Phase 2 is where the load-bearing semantic change happens. If
  refcount accounting has an off-by-one anywhere (e.g. inc on map but
  miss dec on unmap), we leak frames or free too early. Mitigation:
  the existing alloc/free ring catches both directions if we keep it
  on through Phase 2.
- Performance: every map/unmap now does an inc/dec on an array.
  Should be sub-microsecond on the common path. Measure before/after
  on `b_spawn_warm` or `l2_jobchurn_heavy`.

## Anchor memories

- [[frame-alias-root-cause-2026-05-18]] — what we observed.
- [[unified-process-model-decision-2026-05-18]] — the call this
  document executes.
- [[feedback_procmgr_stateless]] — preexisting bias toward keeping
  procmgr's state procmgr-side and not mirroring in kernel; this
  spec extends that to the rest of process state.
- [[feedback_always_commit_plans]] — commit this on landing.
