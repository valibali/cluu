# Storage Stack

CLUU's storage stack is entirely in userspace: `virtio-core` (framework) →
`virtio-blk` (driver) → `ext2` (filesystem) → VFS (mount).

## virtio-core — virtio device framework

`userspace/virtio-core/`

Abstracts the virtio device bus so driver code is bus-independent.

- **`Transport` trait** — abstracts the bus. Today the only impl is
  `ModernPciTransport` (virtio 1.0 over PCI). If CLUU runs on MMIO virtio
  (e.g. aarch64 with device tree), a new `MmioTransport` plugs in without
  touching the driver crate.
- **`virtqueue`** — virtqueue split-ring implementation.
- **`dma`** — DMA buffer management.
- **`irq`** — IRQ handling.
- **`pci`** — PCI bus helpers.
- **`transport/modern_pci`** — ModernPciTransport (virtio 1.0 PCI).

## virtio-blk — block device driver

`userspace/virtio-blk/`

Userspace virtio-blk driver for QEMU's `virtio-blk-pci` device. Exposes the
result through `libcluu::fs::BlockDevice`.

- **`ModernBlkAdapter`** — `BlockDevice` adapter over the modern PCI transport.
  Implements `read_bytes`, `write_bytes`, `sector_size`, `sector_count`.
- **`DriverState` / `DriverStateInner`** — driver-wide mutable state (request
  queue, pending async table, deferred completions, sync-cookie counter).
- **`PendingAsync`** — per-cookie bookkeeping for an async `BLK_SUBMIT` in
  flight.

### Sync vs async paths

- **Sync path**: `read_bytes`/`write_bytes` spin-poll the device used ring under
  `inner.lock()` and yield every 100 000 spins. The IRQ-via-IPC wait path is
  intentionally NOT used here because it triggers unrelated process-jump
  faults in CLUU's current IPC layer; spin-poll preserves the baseline.
  The main service loop has a 50 ms `recv_any` timeout fallback so a dropped
  IRQ still gets a `drain_and_route` pass within 50 ms.
- **Async path**: `BLK_SUBMIT` callers get a `BLK_COMPLETE` reply routed through
  the same completion path. `drain_and_route` is the IRQ-path counterpart: it
  acks the ISR, re-checks `deferred` against `pending`, drains fresh
  completions, and sends a `BLK_COMPLETE` for every async entry it can match.

### Deferred orphan completions

While spin-polling, any completion whose cookie does NOT match the sync waiter
is re-routed: if it matches a `pending` async entry it is turned into a
`BLK_COMPLETE` send; otherwise it is stashed in `deferred` so the rightful owner
can claim it on a later drain.

### Partial-sector writes

Partial-sector writes do a read-modify-write: the head/tail sector is read into
the scratch buffer first, the payload is overlaid, then the full sector is
written back.

### DMA scratch buffer

DMA uses a pre-mapped scratch buffer at `scratch_base` (page-aligned,
`scratch_pages` long); `virt_to_phys` is called per page at request time.
Requests larger than `scratch_pages * 4096` fail with `BufferTooSmall`.

Modules: `main` (binary entry), `pci` (PCI enumeration + BAR mapping),
`protocol` (virtio-blk request/response), `request_queue` (virtqueue
submission), `session` (session cookie packing: sid | rid).

## ext2 — ext2 filesystem driver

`userspace/ext2/`

Userspace ext2 driver that implements `libcluu::fs::Filesystem` over any
`BlockDevice` plugin. VFS mounts `/` through this driver.

- **`Ext2Fs<'a>`** — ext2 filesystem instance bound to a `BlockDevice`.
- **`Filesystem` trait impl** — `name`, `root_inode`, `lookup`, `stat`,
  `readdir`, `read`, `resolve_path`.

### Key invariants

- Root inode is always inode 2.
- Superblock is read from byte offset 1024; block size is
  `1024 << sb.log_block_size`. Disk images are created with `-b 4096`
  (4 KiB blocks) so the runtime block size is 4096.
- Inode size is `sb.inode_size` for rev_level ≥ 1, else 128.
- **Triple-indirect blocks are NOT implemented.** `get_block_num` returns
  `Error::InvalidOperation` for those indices. Files are bounded at the
  double-indirect range.
- `read_file` collapses runs of physically-contiguous or all-sparse logical
  blocks into one `read_bytes` call. Sparse runs zero-fill.
- `IndirectCache` holds a single indirect block; any mutation of an indirect
  block calls `invalidate_indirect_cache` so the next read re-fetches.
- `realpath_canonical` resolves `.` and `..` and follows symlinks at every
  directory hop, capped at `MAX_SYMLINK_HOPS` (32).
- `unlink_path` rejects directories; `rmdir_path` rejects non-empty directories
  and the root inode (2); `link_path` rejects directories and refuses to
  clobber an existing target.

### Block/inode allocation

Block and inode allocators walk the block-group bitmaps, set the first free
bit, zero newly-allocated data blocks, and decrement the superblock free
counters.

Modules: `dir` (directory entry parsing/writing), `inode` (inode read/write),
`superblock` (superblock parsing).

## The chain

```text
  VFS (mount /)
    → ext2 (Filesystem trait)
      → virtio-blk (BlockDevice trait)
        → virtio-core (ModernPciTransport)
          → QEMU virtio-blk-pci device
```

VFS calls `ext2.read_file(inode)` → ext2 calls `blk.read_bytes(lba)` →
virtio-blk translates to a virtio-blk request on the PCI transport → QEMU
serves it from the disk image file.

## Modern virtio-blk design (approved 2026-05-06)

The original driver was legacy-mode (pre-virtio-1.0), single-in-flight,
polled, and bounce-buffered — ~20-30 MB/s against a 4 KB/100µs round-trip
ceiling of ~40 MB/s. The redesign targets **≥150 MB/s** sustained sequential
read (expected ~200 MB/s) by closing the 7× gap that comes from one-in-flight
at a time, plus a smaller per-request memcpy.

Key decisions:

- **Modern virtio 1.0+ transport** via `ModernPciTransport` in a reusable
  `virtio-core` crate. `trait Transport` isolates the bus so virtio-net (Phase
  4) adds a second impl without touching `Virtqueue` or `IrqSource`.
- **Zero-copy**: device DMAs directly into the caller's granted pages. Caller
  `space_grant`s its page(s) to the driver; the driver translates
  `virt_to_phys` and fills descriptor addrs directly. No driver-owned bounce
  buffer.
- **Multiple in-flight** via per-session `queue_depth` (default 32).
  `BlkSession` owns only per-client lifecycle; `BlkRequestQueue` owns the
  virtqueue and is session-agnostic (SRP split). The cookie packed as
  `(session_id, request_id)` lets out-of-order `pop_used` completions route
  back to the right caller.
- **IRQ-driven completion** (not polled). `IrqSource` wraps `irq_attach`; the
  driver's recv loop multiplexes `[control, irq, session*]` endpoints.
- **Notify batching** — one `transport.notify(0)` per recv-burst, not per
  submit, is the single biggest throughput lever (4 callers in one quantum →
  one host exit).
- **Session lifecycle**: open via `BLK_OPEN_SESSION`, submit via
  `BLK_SUBMIT`, complete via `BLK_COMPLETE` on the caller's completion
  endpoint. Caller exit (procmgr `PROC_EXIT_LABEL`) revokes grants and frees
  the SessionId — no leak.
- **Pure userspace**, no kernel changes. Old driver retired in one shot once
  `l2_blk_basic` + `l2_blk_perf` pass. MSI-X deferred (kernel doesn't support
  yet); write-path optimization deferred (read is the boot/spawn bottleneck).

## Plan lessons — storage

Distilled implementation lessons from storage plans. 2-5 lines each; see
the dated plan file for the long form.

### virtio-notify-batching-lever (2026-05-06-virtio-blk-modern)

Notify batching is the biggest throughput lever, not queue depth or
zero-copy alone. Each `notify` is a MMIO exit; amortising one notify across
N submits collapses the exit cost. `DmaPool` must forbid a region crossing
a 4 KiB page boundary so the cached page-phys is unambiguous. WC perf gain
is invisible under QEMU TCG (every memory type behaves as WB); functional
correctness is TCG-verifiable, perf delta requires KVM. `BlkRequestQueue`
owns the virtqueue; cookie packed as `(session_id, request_id)` lets
out-of-order `pop_used` completions route back to the right caller.

## Storage throughput pass (2026-07-16)

Targeted optimization round: ext2 throughput from ~9 MB/s to 803 MB/s,
9p host-share throughput from ~596 KB/s to multi-MB/s.

### ext2 path (virtio-blk)

- **ext2 block size 1024→4096** (`xtask/src/main.rs`): `mke2fs -b 4096` with
  262 144 blocks (1 GB). 4× fewer block lookups, 4× larger coalesced runs in
  `ext2::read_file`.
- **IRQ poll fallback** (`virtio-blk/src/main.rs`): main loop uses a 50 ms
  `recv_any` timeout instead of blocking forever. If `dispatch_irq` drops an
  IRQ message (shard lock busy), the poll fallback still drains completions
  within 50 ms.
- **Spin-poll yield frequency** (`virtio-blk/src/lib.rs`): `yield_cpu` every
  100 000 spins (was 1024). The old frequency caused a scheduler round-trip
  every ~100 µs, capping throughput at ~9 MB/s. The new frequency lets the
  spin loop run at full speed while still yielding eventually.
- **`dispatch_irq` retry** (`kernel/src/devices/irq.rs`): on `WouldBlock`
  (shard lock busy), retry `try_send` up to 8 times with `spin_loop` between
  attempts. Reduces IRQ message drop rate under contention.

### 9p path (virtio-9p host share)

- **Scatter-gather per-page descriptors** (`virtio-9p/src/main.rs`):
  `round_trip` now builds one descriptor per response-buffer page using
  `virt_to_phys(space_token, va + i * PAGE_SIZE)`. Fixes the
  `DmaPool::alloc_contiguous` physical-contiguity bug that corrupted memory
  for >4 KB 9p reads.
- **MSIZE 64 KB→256 KB** (`virtio-9p/src/main.rs`): QEMU 11.0.2 accepts
  256 KB via `TVERSION` negotiation. Virtqueue expanded 64→128 descriptors
  (1 req + 64 response pages). 4× fewer round-trips for large reads.
- **mp3player READ_CHUNK 4 KB→64 KB** (`mp3player/src/main.rs`): 16× fewer
  IPC round-trips during file load. `SCRATCH_PAGES` 16→24 to fit the larger
  grant buffer window.

### VFS bulk read

- **`VFS_READ_FILE_BULK` IPC** (`libcluu/src/fs/protocol.rs`,
  `libcluu/src/fs/client.rs`, `vfs/src/main.rs`): new IPC label 0x212 that
  reads an entire file (≤4 MB) into the caller's grant buffer in one
  round-trip. `VfsClient::read_file_bulk(file, target_space, target_base)`
  skips the per-chunk loop. Server-side `handle_read_file_bulk` mirrors
  `handle_read_grant`'s Ext2/Memory/Virtual/MemFs arms but passes
  `offset=0, len=file_size`. Files >4 MB get `BufferTooSmall` (caller falls
  back to chunked `read_grant`).

### Indirect descriptors (virtio-blk)

- **`VIRTIO_F_RING_INDIRECT_DESC`** (feature bit 28, `virtio-core/src/transport/mod.rs`):
  negotiated at driver init. Allows a single main-ring descriptor to point
  to an indirect descriptor table — a separate DMA buffer containing up to
  256 `VRingDesc` entries (1 page = 4096/16).
- **`submit_read`/`submit_write`** (`virtio-blk/src/request_queue.rs`):
  when a request needs >254 data-page descriptors (would overflow the
  256-desc queue), the driver switches to indirect mode. Main-ring chain:
  `header → indirect_table_0 → … → indirect_table_K → status`. Each
  indirect table holds 256 data-page descriptors, chained internally via
  `VRING_DESC_F_NEXT`. A 4 MB read (1024 pages) uses 6 main-ring
  descriptors + 4 indirect table pages.
- **Recycling**: indirect table `DmaRegion`s are stored in `InflightSlot`
  and returned to `free_indirect` on completion, same as header/status
  pairs. Steady-state pool usage is bounded by high-water-mark depth.

### What was NOT done

- **IRQ-driven `read_bytes`**: spin-poll retained. The `try_send` drop-on-
  `WouldBlock` path still exists (bounded retry mitigates but does not
  eliminate). Converting `read_bytes` to block on the IRQ endpoint requires
  a reliable IRQ delivery guarantee first.

### Harness

- **`l2_blk_basic`**, **`l2_blk_perf`**, **`l2_blk_concurrent`** registered
  in the Python harness (`markers.py`, `catalog.py`, `case_defaults.py`).
  `blkprobe` binary already existed with the right markers
  (`blkprobe: ALL OK`, `blkprobe: [FAIL]`, `blkprobe: perf … mb_per_s=N`).
  `l2_blk_perf` gates on a 150 MB/s floor; measured 803 MB/s.
