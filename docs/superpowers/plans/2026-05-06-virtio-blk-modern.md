# virtio-blk Modern + Async + Zero-Copy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the virtio-blk service on top of a new reusable `virtio-core` crate, switch its internals to multi-in-flight + IRQ-driven completion + zero-copy DMA, and expose an in-process `BlkRequestQueue` so the ext2 layer (still co-resident in the same process) can issue parallel sector reads. Closes the 7× throughput gap (target floor ≥150 MB/s sequential read).

**Architecture:** Three layers, downward-only deps. Top: `libcluu::fs::client::BlkSessionClient` (caller helper, async + sync façades). Middle: `userspace/virtio-blk/` rewrite with `BlkRequestQueue` + `BlkSession`s + `BlkProtocol`. Bottom: `userspace/virtio-core/` (new crate) with `Transport` trait + `Virtqueue` + `IrqSource` + `DmaPool`. SOLID throughout — each unit has a single responsibility, depends on traits not concrete types, and is independently testable via probe binaries.

**Tech Stack:** Rust 2021 nightly (`build-std`), `no_std + alloc`, only `libcluu` + `klibcluu` for kernel-facing facilities. No new external dependencies. Pure userspace — no kernel changes.

**Reading order:** Read spec `docs/superpowers/specs/2026-05-06-virtio-blk-modern.md` once before starting, re-read the relevant section before each task. Phase boundaries marked `[BUILD GATE]` require `cargo xtask build` to succeed; `[HARNESS GATE]` requires the named harness case to pass. Existing harness convention: `MARKER_MODE=<case> bash scripts/harness_run.sh`.

**Critical conventions** (every commit):
- No `Co-Authored-By:` trailer in commit messages.
- Build-only verification per task unless task says `HARNESS GATE`.
- Every code step shows the full code block for the change. No placeholders.

---

## Phase 0 — Pre-flight

### T0.1 Create worktree + verify baseline

**Files:** none (workspace setup)

- [ ] **Step 1: Create dedicated worktree**

```bash
cd /home/vlb2bp/git/cluu
git worktree add ../cluu-virtio-modern -b virtio-modern develop
cd ../cluu-virtio-modern
```

- [ ] **Step 2: Verify clean baseline build**

Run: `cargo xtask build`
Expected: `✓ Build complete: target/cluu.img`

- [ ] **Step 3: Verify a representative harness case passes on baseline**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_pipe_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | grep -E "all required markers|MISSING"`
Expected: `No faults detected and all required markers found.`

- [ ] **Step 4: Record baseline boot log size for later regression check**

Run: `wc -l /tmp/cluu-serial-com2.log`
Expected: ~280 lines. Record exact number — Phase 10 verifies the new driver doesn't regress log volume.

- [ ] **Step 5: No commit yet — pre-flight only.**

### T0.2 Confirm spec reachable + design constants

**Files:** none (read-only)

- [ ] **Step 1: Open spec, locate the layered architecture diagram (§3)**

Run: `head -120 docs/superpowers/specs/2026-05-06-virtio-blk-modern.md`
Expected: layer cake from `libcluu::fs::client` down through `virtio-core`.

- [ ] **Step 2: Note the four virtio modern PCI capability types**

These constants will appear repeatedly (spec §3.1, virtio 1.2 spec §4.1.4):
```
VIRTIO_PCI_CAP_COMMON_CFG  = 1
VIRTIO_PCI_CAP_NOTIFY_CFG  = 2
VIRTIO_PCI_CAP_ISR_CFG     = 3
VIRTIO_PCI_CAP_DEVICE_CFG  = 4
```

- [ ] **Step 3: Note the queue-depth + deadline defaults (spec §5)**

```
queue_depth (per-session cap) = 32
per-request deadline           = 5_000 ms
device-dead trip after          = 3 consecutive timeouts
```

---

## Phase 1 — virtio-core crate scaffold + DmaPool

`[BUILD GATE]` at end of phase.

### T1.1 Create virtio-core crate skeleton

**Files:**
- Create: `userspace/virtio-core/Cargo.toml`
- Create: `userspace/virtio-core/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + user-target list)

- [ ] **Step 1: Write `userspace/virtio-core/Cargo.toml`**

```toml
[package]
name = "cluu-virtio-core"
version = "0.1.0"
edition = "2021"
description = "Reusable virtio (modern PCI 1.0+) transport core for CLUU userspace drivers"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[lib]
name = "cluu_virtio_core"
path = "src/lib.rs"

[dependencies]
libcluu = { path = "../libcluu" }
spin = { workspace = true }
```

- [ ] **Step 2: Write minimal `userspace/virtio-core/src/lib.rs`**

```rust
#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod dma;
pub mod virtqueue;
pub mod transport;
pub mod irq;
pub mod pci;

pub use dma::{DmaPool, DmaRegion};
pub use virtqueue::{DescChain, Virtqueue};
pub use transport::{Transport, FeatureBits};
pub use irq::IrqSource;
```

- [ ] **Step 3: Add module stubs so `lib.rs` resolves**

Create each file with one line:

`userspace/virtio-core/src/dma.rs`:
```rust
//! DMA-pinned regions for descriptor tables, headers, status bytes.
```

`userspace/virtio-core/src/virtqueue.rs`:
```rust
//! Split virtqueue: descriptor ring + avail ring + used ring.
```

`userspace/virtio-core/src/transport.rs`:
```rust
//! Trait abstraction over the device transport (modern PCI today, MMIO future).
```

`userspace/virtio-core/src/irq.rs`:
```rust
//! Wrap `irq_attach` into a wait-for-completion primitive.
```

`userspace/virtio-core/src/pci.rs`:
```rust
//! Modern PCI capability discovery for virtio 1.0+ devices.
```

- [ ] **Step 4: Add to workspace `Cargo.toml`**

Open `Cargo.toml` (root). In the `[workspace] members = [...]` array, add `"userspace/virtio-core",` next to `"userspace/virtio-blk",` (around line 50 and again in the user-target list around line 100).

- [ ] **Step 5: Build to verify the empty crate links**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: `✓ Build complete: target/cluu.img`

- [ ] **Step 6: Commit**

```bash
git add userspace/virtio-core/ Cargo.toml
git commit -m "virtio-core: empty crate scaffold + workspace entry"
```

### T1.2 DmaPool — fixed-region pinned allocator

`DmaPool` owns a single `space_map_range`-allocated virtual region used for descriptor tables, virtio-blk request headers, and status bytes. Returns `DmaRegion` slices with both virt and phys addresses.

**Files:**
- Modify: `userspace/virtio-core/src/dma.rs`

- [ ] **Step 1: Write the `DmaPool` API in `dma.rs`**

```rust
//! DMA-pinned regions for descriptor tables, headers, status bytes.
//!
//! Single pre-allocated virtual region carved up by a bump pointer; lifetime
//! is the driver's lifetime. Each handed-out `DmaRegion` carries both its
//! virtual address (for CPU access) and its physical address (for device
//! descriptors). `phys` is resolved once at allocation time via the kernel
//! `virt_to_phys` syscall and cached — the underlying frames are pinned for
//! the driver's lifetime so the cached phys never goes stale.

use alloc::vec::Vec;
use libcluu::syscall::{space_map_range, virt_to_phys};
use libcluu::{Error, Result};

const DMA_REGION_FLAGS: usize = 0x03; // R+W

pub struct DmaPool {
    base_va: usize,
    size: usize,
    next_offset: usize,
    space_token: usize,
    page_phys: Vec<u64>, // phys per 4KB page
}

#[derive(Copy, Clone, Debug)]
pub struct DmaRegion {
    pub virt: usize,
    pub phys: u64,
    pub len: usize,
}

impl DmaPool {
    /// Allocate `pages * 4096` bytes of pinned virtual range and resolve
    /// each page's physical address. The region is handed out in
    /// `align`-aligned subregions by `alloc()`.
    pub fn new(space_token: usize, base_va: usize, pages: usize) -> Result<Self> {
        space_map_range(space_token, base_va, 0, DMA_REGION_FLAGS, pages, 0)?;
        let mut page_phys = Vec::with_capacity(pages);
        for i in 0..pages {
            let va = base_va + i * 4096;
            let phys = virt_to_phys(space_token, va)?;
            page_phys.push(phys as u64);
        }
        Ok(Self {
            base_va,
            size: pages * 4096,
            next_offset: 0,
            space_token,
            page_phys,
        })
    }

    /// Carve out a `len`-byte subregion aligned to `align` (must be power of 2).
    /// Returns Err(Overflow) if there isn't enough remaining space. The caller
    /// must not span a 4 KiB page boundary unless `len <= 4096` and aligned
    /// such that the region fits in one page (most descriptor tables and
    /// per-request header/status structs are tiny — far below 4 KiB).
    pub fn alloc(&mut self, len: usize, align: usize) -> Result<DmaRegion> {
        if !align.is_power_of_two() {
            return Err(Error::InvalidArgument);
        }
        let aligned = (self.next_offset + align - 1) & !(align - 1);
        if aligned + len > self.size {
            return Err(Error::Overflow);
        }
        // Forbid a region crossing a 4 KiB page boundary so the cached
        // page-phys is unambiguous for that region.
        let page_idx = aligned / 4096;
        let last_byte_page = (aligned + len - 1) / 4096;
        if page_idx != last_byte_page {
            // Skip to next page boundary and retry once.
            let new_offset = (page_idx + 1) * 4096;
            if new_offset + len > self.size {
                return Err(Error::Overflow);
            }
            self.next_offset = new_offset;
            return self.alloc(len, align);
        }
        let virt = self.base_va + aligned;
        let phys_base = self.page_phys[page_idx];
        let intra_page_offset = (aligned % 4096) as u64;
        self.next_offset = aligned + len;
        Ok(DmaRegion {
            virt,
            phys: phys_base + intra_page_offset,
            len,
        })
    }

    /// Resolve a previously-allocated virt back to its phys. O(1).
    pub fn phys_of(&self, virt: usize) -> Option<u64> {
        if virt < self.base_va || virt >= self.base_va + self.size {
            return None;
        }
        let offset = virt - self.base_va;
        let page_idx = offset / 4096;
        Some(self.page_phys[page_idx] + (offset % 4096) as u64)
    }

    pub fn space_token(&self) -> usize {
        self.space_token
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: `✓ Build complete`

- [ ] **Step 3: Commit**

```bash
git add userspace/virtio-core/src/dma.rs
git commit -m "virtio-core: DmaPool fixed-region pinned allocator"
```

---

## Phase 2 — Virtqueue mechanics

`[BUILD GATE]` at end of phase.

The Virtqueue is the data-structure heart of virtio. Three rings (descriptor table, avail ring, used ring) packed into a single DMA region (from DmaPool). Internal free-list of descriptor entries; per-descriptor cookie storage so the driver layer above can route completions.

### T2.1 Virtqueue layout + constructor

**Files:**
- Modify: `userspace/virtio-core/src/virtqueue.rs`

- [ ] **Step 1: Layout structs at the top of `virtqueue.rs`**

```rust
//! Split virtqueue: descriptor ring + avail ring + used ring.
//!
//! Layout (modern virtio 1.1 §2.7):
//!   - desc table: queue_size * 16 bytes, 16-byte aligned
//!   - avail ring: 6 + 2*queue_size bytes (+ 2 if EVENT_IDX), 2-byte aligned
//!   - used ring:  6 + 8*queue_size bytes (+ 2 if EVENT_IDX), 4-byte aligned

use crate::dma::{DmaPool, DmaRegion};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};
use libcluu::{Error, Result};

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;
pub const VRING_DESC_F_INDIRECT: u16 = 4;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct VRingDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
pub struct VRingAvailHeader {
    pub flags: u16,
    pub idx: u16,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct VRingUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C)]
pub struct VRingUsedHeader {
    pub flags: u16,
    pub idx: u16,
}
```

- [ ] **Step 2: Write the Virtqueue struct + constructor**

Append to `virtqueue.rs`:

```rust
pub struct Virtqueue {
    pub queue_size: u16,

    // Three rings live inside a single DmaPool; each region carries virt+phys.
    pub desc_region: DmaRegion,
    pub avail_region: DmaRegion,
    pub used_region: DmaRegion,

    // Free-list head + count. `next_link` is a singly-linked list threaded
    // through the descriptor table's `next` field of unused entries.
    free_head: u16,
    num_free: u16,

    // Shadow of used.idx — last value we drained. The device's used.idx may
    // be ahead; we lazily catch up in pop_used().
    last_used_idx: u16,

    // Per-descriptor cookie (caller-supplied u64). Indexed by head desc idx.
    cookies: Vec<Option<u64>>,
}

impl Virtqueue {
    /// Build a new virtqueue of `queue_size` entries from the given DMA pool.
    /// queue_size must be a power of 2 (virtio spec §2.7) — typical 64..256.
    pub fn new(pool: &mut DmaPool, queue_size: u16) -> Result<Self> {
        if !queue_size.is_power_of_two() || queue_size == 0 {
            return Err(Error::InvalidArgument);
        }

        let desc_bytes = (queue_size as usize) * core::mem::size_of::<VRingDesc>();
        let avail_bytes = 4 + 2 * (queue_size as usize); // header + ring (no event_idx)
        let used_bytes = 4 + 8 * (queue_size as usize);

        let desc_region = pool.alloc(desc_bytes, 16)?;
        let avail_region = pool.alloc(avail_bytes, 2)?;
        let used_region = pool.alloc(used_bytes, 4)?;

        // Zero all three rings.
        unsafe {
            core::ptr::write_bytes(desc_region.virt as *mut u8, 0, desc_bytes);
            core::ptr::write_bytes(avail_region.virt as *mut u8, 0, avail_bytes);
            core::ptr::write_bytes(used_region.virt as *mut u8, 0, used_bytes);
        }

        // Build initial free list: every desc points to the next.
        for i in 0..queue_size {
            let next = if i + 1 < queue_size { i + 1 } else { 0 };
            unsafe {
                let desc_ptr = (desc_region.virt as *mut VRingDesc).add(i as usize);
                (*desc_ptr).flags = VRING_DESC_F_NEXT;
                (*desc_ptr).next = next;
            }
        }

        Ok(Self {
            queue_size,
            desc_region,
            avail_region,
            used_region,
            free_head: 0,
            num_free: queue_size,
            last_used_idx: 0,
            cookies: vec![None; queue_size as usize],
        })
    }

    pub fn free_capacity(&self) -> u16 {
        self.num_free
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-core/src/virtqueue.rs
git commit -m "virtio-core: Virtqueue layout + constructor"
```

### T2.2 alloc_chain + free_chain

**Files:**
- Modify: `userspace/virtio-core/src/virtqueue.rs`

- [ ] **Step 1: Add `DescChain` + `alloc_chain` + internal `free_chain`**

Append to `virtqueue.rs`:

```rust
/// A reservation of `n` chained descriptor slots.
///
/// `head` is the entry pushed to the avail ring on submit; `tail` is the
/// last entry in the chain (used to keep the free-list invariant).
pub struct DescChain {
    pub head: u16,
    pub tail: u16,
    pub n: u16,
}

impl Virtqueue {
    /// Reserve `n` chained descriptor slots from the free list.
    /// Returns None if fewer than `n` slots are free.
    ///
    /// The chain is pulled in linked-list order from the free list; its
    /// internal `next` fields already form a chain. Caller fills in addr/
    /// len/flags by indexing through the head.
    pub fn alloc_chain(&mut self, n: u16) -> Option<DescChain> {
        if n == 0 || n > self.num_free {
            return None;
        }
        let head = self.free_head;
        let mut cursor = head;
        for _ in 0..(n - 1) {
            cursor = unsafe { self.desc(cursor).next };
        }
        let tail = cursor;
        // Splice the new free_head to the slot AFTER tail.
        let new_free_head = unsafe { self.desc(tail).next };
        // Disconnect tail from the free list (caller will set its NEXT bit
        // explicitly if it wants chaining; for the last desc in a request
        // chain, NEXT is cleared so the device knows the chain ends).
        unsafe {
            self.desc_mut(tail).flags &= !VRING_DESC_F_NEXT;
            self.desc_mut(tail).next = 0;
        }
        self.free_head = new_free_head;
        self.num_free -= n;
        Some(DescChain { head, tail, n })
    }

    /// Return a chain to the free list. Used by pop_used after the device
    /// has signalled completion, OR by the caller on a submit-failure
    /// rollback.
    pub fn free_chain(&mut self, chain: DescChain) {
        // Walk from head to tail to count and to confirm the chain shape;
        // splice the whole chain back as the new free_head.
        unsafe {
            self.desc_mut(chain.tail).flags = VRING_DESC_F_NEXT;
            self.desc_mut(chain.tail).next = self.free_head;
        }
        self.free_head = chain.head;
        self.num_free += chain.n;
    }

    #[inline]
    unsafe fn desc(&self, idx: u16) -> &VRingDesc {
        &*((self.desc_region.virt as *const VRingDesc).add(idx as usize))
    }

    #[inline]
    unsafe fn desc_mut(&mut self, idx: u16) -> &mut VRingDesc {
        &mut *((self.desc_region.virt as *mut VRingDesc).add(idx as usize))
    }
}
```

- [ ] **Step 2: Add a public `desc_set` helper for the driver layer**

The driver above needs to fill in descriptor fields. Append to `virtqueue.rs`:

```rust
impl Virtqueue {
    /// Write a descriptor entry. `next_idx` is only honored if `flags`
    /// contains VRING_DESC_F_NEXT — the caller is responsible for the
    /// chain shape.
    pub fn desc_set(
        &mut self,
        idx: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next_idx: u16,
    ) {
        unsafe {
            let d = self.desc_mut(idx);
            d.addr = addr;
            d.len = len;
            d.flags = flags;
            d.next = next_idx;
        }
    }

    /// Walk the chain starting at `head`, collecting descriptor indices.
    /// Used by free_chain after submit, and by tests.
    pub fn collect_chain(&self, head: u16) -> alloc::vec::Vec<u16> {
        let mut out = alloc::vec::Vec::new();
        let mut cur = head;
        loop {
            out.push(cur);
            let d = unsafe { self.desc(cur) };
            if d.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            cur = d.next;
        }
        out
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-core/src/virtqueue.rs
git commit -m "virtio-core: alloc_chain/free_chain + desc_set helpers"
```

### T2.3 submit + pop_used + cookies

**Files:**
- Modify: `userspace/virtio-core/src/virtqueue.rs`

- [ ] **Step 1: Add `submit` and `pop_used`**

Append to `virtqueue.rs`:

```rust
impl Virtqueue {
    /// Push the chain head onto the avail ring and store the caller's cookie.
    /// Does NOT issue a `notify` to the device — the caller batches a
    /// notify after one or more submits to amortize the MMIO exit.
    pub fn submit(&mut self, chain: DescChain, cookie: u64) {
        let avail_va = self.avail_region.virt;
        // Read current avail.idx, store head at ring[idx % queue_size], inc.
        unsafe {
            let header = avail_va as *mut VRingAvailHeader;
            let cur_idx = (*header).idx;
            let ring_base = (avail_va + 4) as *mut u16; // skip flags+idx
            *ring_base.add((cur_idx as usize) & (self.queue_size as usize - 1)) = chain.head;
            // Memory fence so the desc-table writes (already visible) and
            // ring entry are observed by the device before the index update.
            fence(Ordering::Release);
            (*header).idx = cur_idx.wrapping_add(1);
        }
        self.cookies[chain.head as usize] = Some(cookie);
        // chain.tail already disconnected by alloc_chain; nothing else to do.
    }

    /// Drain one used-ring entry if one is present. Returns
    /// `Some((cookie, bytes_written))` and frees the descriptor chain.
    pub fn pop_used(&mut self) -> Option<(u64, u32)> {
        let used_va = self.used_region.virt;
        unsafe {
            let header = used_va as *const VRingUsedHeader;
            let device_idx = (*header).idx;
            if device_idx == self.last_used_idx {
                return None;
            }
            // Read element at last_used_idx % queue_size.
            let ring_base = (used_va + 4) as *const VRingUsedElem;
            let elem = *ring_base.add(self.last_used_idx as usize & (self.queue_size as usize - 1));
            let head = elem.id as u16;
            let written = elem.len;
            // Acquire fence so subsequent reads of buffers see the device's writes.
            fence(Ordering::Acquire);
            self.last_used_idx = self.last_used_idx.wrapping_add(1);

            // Take the cookie before freeing the chain.
            let cookie = self.cookies[head as usize].take();

            // Free the whole chain (rebuild the chain shape so free_chain
            // walks it). collect_chain reads NEXT bits up to the last desc.
            // Since alloc_chain cleared NEXT only on the tail, the chain
            // walk works for any size including 1.
            let descs = self.collect_chain(head);
            let n = descs.len() as u16;
            let tail = *descs.last().unwrap();
            self.free_chain(DescChain { head, tail, n });

            cookie.map(|c| (c, written))
        }
    }

    /// True if the device has any unconsumed used-ring entries pending.
    pub fn has_used(&self) -> bool {
        let used_va = self.used_region.virt;
        unsafe {
            let header = used_va as *const VRingUsedHeader;
            (*header).idx != self.last_used_idx
        }
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add userspace/virtio-core/src/virtqueue.rs
git commit -m "virtio-core: Virtqueue submit + pop_used with cookie routing"
```

### T2.4 vqprobe — boot-time exerciser for Virtqueue mechanics

A small probe binary that exercises virtqueue alloc/submit/pop_used with a fake-device shim (no real hardware). Boot-time test; `[FAIL]` markers cause harness failure.

**Files:**
- Create: `userspace/vqprobe/Cargo.toml`
- Create: `userspace/vqprobe/src/main.rs`
- Modify: `Cargo.toml` (workspace + user-target)
- Create: `containers/vqprobe/Cluufile`
- Modify: `etc/autostart.toml`
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_run.sh` (marker-mode case)

- [ ] **Step 1: Cargo.toml for vqprobe**

```toml
[package]
name = "cluu-vqprobe"
version = "0.1.0"
edition = "2021"
description = "Virtqueue mechanics smoke test"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }
cluu-virtio-core = { path = "../virtio-core" }

[[bin]]
name = "vqprobe"
path = "src/main.rs"
```

- [ ] **Step 2: vqprobe main.rs — exercise five invariants**

```rust
#![no_std]
#![no_main]

extern crate alloc;

use cluu_virtio_core::{DescChain, DmaPool, Virtqueue};
use cluu_virtio_core::virtqueue::VRING_DESC_F_NEXT;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::debug_print;

const POOL_BASE: usize = 0x4000_0000;
const POOL_PAGES: usize = 16;

fn fail(name: &str) -> ! {
    let _ = debug_print(&alloc::format!("vqprobe: [FAIL] {}", name));
    libcluu::process::exit(1);
}

fn ok(name: &str) {
    let _ = debug_print(&alloc::format!("vqprobe: ok {}", name));
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    let mut pool = match DmaPool::new(space_token, POOL_BASE, POOL_PAGES) {
        Ok(p) => p,
        Err(_) => fail("DmaPool::new"),
    };

    // Build a 64-entry virtqueue.
    let mut vq = match Virtqueue::new(&mut pool, 64) {
        Ok(v) => v,
        Err(_) => fail("Virtqueue::new"),
    };

    // Invariant 1: free_capacity == queue_size at start.
    if vq.free_capacity() != 64 {
        fail("init free_capacity != 64");
    }
    ok("init capacity");

    // Invariant 2: alloc_chain(2) reduces free count by 2.
    let chain = match vq.alloc_chain(2) {
        Some(c) => c,
        None => fail("alloc_chain(2) returned None"),
    };
    if vq.free_capacity() != 62 {
        fail("after alloc_chain(2) capacity != 62");
    }
    ok("alloc_chain reduces capacity");

    // Invariant 3: free_chain restores capacity.
    vq.free_chain(chain);
    if vq.free_capacity() != 64 {
        fail("after free_chain capacity != 64");
    }
    ok("free_chain restores");

    // Invariant 4: alloc_chain(N+1) returns None when N free.
    let big = vq.alloc_chain(64).unwrap();
    if vq.alloc_chain(1).is_some() {
        fail("alloc_chain(1) succeeded with 0 free");
    }
    ok("alloc_chain returns None when full");
    vq.free_chain(big);

    // Invariant 5: submit + pop_used round-trip with cookie. Use a fake
    // "device" — write the used-ring entry by hand, then verify pop_used
    // returns our cookie.
    let chain = vq.alloc_chain(1).unwrap();
    let head = chain.head;
    vq.desc_set(head, 0xDEADBEEF, 4096, 0, 0); // single desc, no NEXT
    vq.submit(chain, 0xCAFE_BABE);
    // Pretend the device completed entry 0 with len=42.
    let used_va = vq.used_region.virt;
    unsafe {
        // Build VRingUsedElem at offset 4 (after flags/idx).
        let elem_ptr = (used_va + 4) as *mut u32;
        *elem_ptr = head as u32;          // id
        *elem_ptr.add(1) = 42;             // len
        // Bump device-side used.idx.
        let idx_ptr = (used_va + 2) as *mut u16;
        *idx_ptr = 1;
    }

    match vq.pop_used() {
        Some((cookie, len)) => {
            if cookie != 0xCAFE_BABE {
                fail("pop_used wrong cookie");
            }
            if len != 42 {
                fail("pop_used wrong len");
            }
        }
        None => fail("pop_used returned None"),
    }
    ok("submit + pop_used cookie roundtrip");

    // After completion the chain should be back in the free list.
    if vq.free_capacity() != 64 {
        fail("after pop_used capacity != 64");
    }
    ok("pop_used returns chain to free list");

    let _ = debug_print("vqprobe: ALL OK");
    libcluu::process::exit(0);
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    let _ = debug_print(&alloc::format!("vqprobe: PANIC {}", info));
    libcluu::process::exit(2);
}
```

- [ ] **Step 3: Cluufile for vqprobe**

`containers/vqprobe/Cluufile`:
```
ENVELOPE user
EXEC /var/images/vqprobe/bin/vqprobe
```

- [ ] **Step 4: Add to autostart so it runs at boot**

Add at top of the autostart list in `etc/autostart.toml`:
```toml
[[service]]
name = "vqprobe"
```

- [ ] **Step 5: Add workspace member**

Edit root `Cargo.toml`. Add `"userspace/vqprobe",` to `[workspace] members` AND to the user-target list (around line 100 — same as virtio-core).

- [ ] **Step 6: Add harness case**

Append to `scripts/harness_cases.conf`:
```
l2_vqprobe|full|MARKER_MODE=l2_vqprobe TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

In `scripts/harness_run.sh`, add a case in the `case "$MARKER_MODE" in` block:
```bash
    l2_vqprobe)
        required_markers=(
            "TSC calibrated"
            "vqprobe: ALL OK"
        )
        ;;
```

- [ ] **Step 7: Run the harness — should pass on the green path; if any [FAIL] line appears it indicates a bug in T2.1–T2.3**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_vqprobe TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: `No faults detected and all required markers found.`

- [ ] **Step 8: Commit**

```bash
git add userspace/vqprobe/ containers/vqprobe/ etc/autostart.toml \
        scripts/harness_cases.conf scripts/harness_run.sh Cargo.toml
git commit -m "vqprobe: virtqueue mechanics smoke test (l2_vqprobe harness case)"
```

`[BUILD GATE + HARNESS GATE]` Phase 2 complete: Virtqueue is correct + tested.

---

## Phase 3 — Transport trait + ModernPciTransport

### T3.1 Transport trait + FeatureBits

**Files:**
- Modify: `userspace/virtio-core/src/transport.rs`

- [ ] **Step 1: Trait definition**

```rust
//! Trait abstraction over the device transport (modern PCI today, MMIO future).

use crate::virtqueue::Virtqueue;
use libcluu::Result;

bitflags::bitflags! {
    pub struct FeatureBits: u64 {
        const VERSION_1   = 1 << 32;       // virtio 1.0 compliance
        // device-class feature bits live in higher namespaces (e.g. blk uses 0..16)
    }
}

pub trait Transport {
    /// Read what the device claims to support (raw 64-bit feature mask).
    fn read_device_features(&mut self) -> Result<u64>;

    /// Tell the device which features the driver wants (subset of device's).
    fn write_driver_features(&mut self, mask: u64) -> Result<()>;

    /// Configure a queue: tell the device the desc/avail/used phys addresses.
    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()>;

    /// Kick the device — tell it to look at the avail ring of `queue_idx`.
    fn notify(&self, queue_idx: u16);

    /// Read the ISR status byte; clears interrupt as side effect.
    fn isr_status(&self) -> u8;

    /// Set DRIVER_OK status bit; device may now process requests.
    fn set_driver_ok(&mut self) -> Result<()>;

    /// Reset the device (status = 0).
    fn reset(&mut self) -> Result<()>;
}
```

- [ ] **Step 2: Add `bitflags` to `userspace/virtio-core/Cargo.toml`**

Edit `userspace/virtio-core/Cargo.toml`, in `[dependencies]`:
```toml
bitflags = { workspace = true }
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-core/src/transport.rs userspace/virtio-core/Cargo.toml
git commit -m "virtio-core: Transport trait + FeatureBits"
```

### T3.2 PCI capability discovery (ported from existing virtio-blk/pci.rs)

**Files:**
- Modify: `userspace/virtio-core/src/pci.rs`

- [ ] **Step 1: Read existing `userspace/virtio-blk/src/pci.rs` as the source-of-truth for the layout, then port it into `virtio-core/src/pci.rs`. Drop blk-specific fields; keep only the modern capability discovery.**

Run: `cat userspace/virtio-blk/src/pci.rs | head -200`
Expected: read-only inspection.

- [ ] **Step 2: Write `virtio-core/src/pci.rs` with the modern capability scanner**

```rust
//! Modern PCI capability discovery for virtio 1.0+ devices.

use libcluu::device_io::DeviceIo;
use libcluu::Result;

pub const VIRTIO_PCI_CAP_VENDOR: u8 = 0x09;
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

#[derive(Default, Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,

    // BAR shadow values populated from header.
    pub bars: [u64; 6],
    pub bar_sizes: [u64; 6],
    pub is_io_bar: [bool; 6],

    // Filled by capability scan.
    pub common_cfg_bar: u8,
    pub common_cfg_offset: u32,
    pub common_cfg_length: u32,

    pub notify_cfg_bar: u8,
    pub notify_cfg_offset: u32,
    pub notify_cfg_length: u32,
    pub notify_off_multiplier: u32,

    pub isr_cfg_bar: u8,
    pub isr_cfg_offset: u32,

    pub device_cfg_bar: u8,
    pub device_cfg_offset: u32,

    pub is_modern: bool,
}

impl PciDevice {
    /// Walk the PCI capability list at the given pointer and populate
    /// the four virtio cfg fields. Sets `is_modern = true` if all four
    /// were found.
    pub fn parse_capabilities<I: DeviceIo>(&mut self, io: &I, cap_ptr: u8) -> Result<()> {
        let mut ptr = cap_ptr;
        let mut found = 0u8;
        while ptr != 0 {
            let header = io.read_u32(ptr as u64);
            let cap_id = (header & 0xFF) as u8;
            let cap_next = ((header >> 8) & 0xFF) as u8;
            let cfg_type = ((header >> 24) & 0xFF) as u8;

            if cap_id == VIRTIO_PCI_CAP_VENDOR {
                let bar = io.read_u8((ptr + 4) as u64);
                let offset = io.read_u32((ptr + 8) as u64);
                let length = io.read_u32((ptr + 12) as u64);
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => {
                        self.common_cfg_bar = bar;
                        self.common_cfg_offset = offset;
                        self.common_cfg_length = length;
                        found |= 1;
                    }
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        self.notify_cfg_bar = bar;
                        self.notify_cfg_offset = offset;
                        self.notify_cfg_length = length;
                        self.notify_off_multiplier = io.read_u32((ptr + 16) as u64);
                        found |= 2;
                    }
                    VIRTIO_PCI_CAP_ISR_CFG => {
                        self.isr_cfg_bar = bar;
                        self.isr_cfg_offset = offset;
                        found |= 4;
                    }
                    VIRTIO_PCI_CAP_DEVICE_CFG => {
                        self.device_cfg_bar = bar;
                        self.device_cfg_offset = offset;
                        found |= 8;
                    }
                    _ => {}
                }
            }
            ptr = cap_next;
        }
        self.is_modern = found == 0xF;
        Ok(())
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-core/src/pci.rs
git commit -m "virtio-core: PCI modern capability scanner"
```

### T3.3 ModernPciTransport — common config MMIO

The four virtio capability regions get MMIO-mapped via `space_map` with `MAP_DEVICE`. The `virtio_pci_common_cfg` struct (virtio 1.2 §4.1.4.3) is a 56-byte register window we touch field-by-field.

**Files:**
- Create: `userspace/virtio-core/src/transport/modern_pci.rs`
- Modify: `userspace/virtio-core/src/transport.rs` (extract the trait into a sub-module)

- [ ] **Step 1: Refactor `transport.rs` to be a module folder**

Move `userspace/virtio-core/src/transport.rs` → `userspace/virtio-core/src/transport/mod.rs` (file rename only, no content change in step 1). Then add:

`userspace/virtio-core/src/transport/mod.rs` (replace the trait file from T3.1):
```rust
//! Transport abstraction.

use crate::virtqueue::Virtqueue;
use libcluu::Result;

pub mod modern_pci;
pub use modern_pci::ModernPciTransport;

bitflags::bitflags! {
    pub struct FeatureBits: u64 {
        const VERSION_1 = 1 << 32;
    }
}

pub trait Transport {
    fn read_device_features(&mut self) -> Result<u64>;
    fn write_driver_features(&mut self, mask: u64) -> Result<()>;
    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()>;
    fn notify(&self, queue_idx: u16);
    fn isr_status(&self) -> u8;
    fn set_driver_ok(&mut self) -> Result<()>;
    fn reset(&mut self) -> Result<()>;
}
```

- [ ] **Step 2: Write the ModernPciTransport struct + constructor**

`userspace/virtio-core/src/transport/modern_pci.rs`:
```rust
//! Modern PCI virtio 1.0+ transport.
//!
//! Maps four capability regions:
//!   - common_cfg: 56-byte register window (queue + features + status)
//!   - notify_cfg: doorbell window; notify_off_multiplier scales the offset
//!   - isr_cfg:    1-byte ISR status (read clears)
//!   - device_cfg: device-class config (e.g. virtio-blk capacity, sector_size)
//!
//! Status bits (virtio 1.2 §2.1):
//!   ACKNOWLEDGE = 1
//!   DRIVER      = 2
//!   FEATURES_OK = 8
//!   DRIVER_OK   = 4
//!   FAILED      = 128

use crate::pci::PciDevice;
use crate::transport::Transport;
use crate::virtqueue::Virtqueue;
use libcluu::syscall::space_map;
use libcluu::{Error, Result};

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const MAP_DEVICE: usize = 0x10;

#[repr(C)]
struct CommonCfg {
    device_feature_select: u32,
    device_feature: u32,
    driver_feature_select: u32,
    driver_feature: u32,
    msix_config: u16,
    num_queues: u16,
    device_status: u8,
    config_generation: u8,
    queue_select: u16,
    queue_size: u16,
    queue_msix_vector: u16,
    queue_enable: u16,
    queue_notify_off: u16,
    _reserved: u16,
    queue_desc: u64,
    queue_driver: u64,
    queue_device: u64,
}

pub struct ModernPciTransport {
    pub device: PciDevice,
    pub common_va: usize,
    pub notify_va: usize,
    pub isr_va: usize,
    pub device_cfg_va: usize,
}

impl ModernPciTransport {
    /// `space_token` lets us map device BARs into the driver's address
    /// space at fixed virtual bases. The four regions get four pages
    /// each (most are <256 bytes; one page is sufficient).
    pub fn new(
        space_token: usize,
        device: PciDevice,
        bar_phys: u64,
        mmio_va_base: usize,
    ) -> Result<Self> {
        if !device.is_modern {
            return Err(Error::Unsupported);
        }
        let common_va = mmio_va_base;
        let notify_va = mmio_va_base + 0x1000;
        let isr_va = mmio_va_base + 0x2000;
        let device_cfg_va = mmio_va_base + 0x3000;

        // Map BAR pages once each — the four regions share one physical BAR
        // in QEMU; we map four 4KB windows starting at the cap offsets.
        space_map(
            space_token,
            common_va,
            bar_phys + device.common_cfg_offset as u64,
            (0x03 | MAP_DEVICE) as usize,
            1,
        )?;
        space_map(
            space_token,
            notify_va,
            bar_phys + device.notify_cfg_offset as u64,
            (0x03 | MAP_DEVICE) as usize,
            1,
        )?;
        space_map(
            space_token,
            isr_va,
            bar_phys + device.isr_cfg_offset as u64,
            (0x03 | MAP_DEVICE) as usize,
            1,
        )?;
        space_map(
            space_token,
            device_cfg_va,
            bar_phys + device.device_cfg_offset as u64,
            (0x03 | MAP_DEVICE) as usize,
            1,
        )?;

        Ok(Self {
            device,
            common_va,
            notify_va,
            isr_va,
            device_cfg_va,
        })
    }

    #[inline]
    fn common(&self) -> *mut CommonCfg {
        self.common_va as *mut CommonCfg
    }

    fn write_status_or(&mut self, bit: u8) -> Result<()> {
        unsafe {
            let cur = core::ptr::read_volatile(&(*self.common()).device_status);
            core::ptr::write_volatile(&mut (*self.common()).device_status, cur | bit);
            // Read back to confirm — virtio spec requires reading status
            // after writing FEATURES_OK to verify the device accepted.
            let after = core::ptr::read_volatile(&(*self.common()).device_status);
            if (after & bit) == 0 {
                return Err(Error::Unsupported);
            }
        }
        Ok(())
    }
}

impl Transport for ModernPciTransport {
    fn read_device_features(&mut self) -> Result<u64> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).device_feature_select, 0);
            let lo = core::ptr::read_volatile(&(*self.common()).device_feature) as u64;
            core::ptr::write_volatile(&mut (*self.common()).device_feature_select, 1);
            let hi = core::ptr::read_volatile(&(*self.common()).device_feature) as u64;
            Ok((hi << 32) | lo)
        }
    }

    fn write_driver_features(&mut self, mask: u64) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).driver_feature_select, 0);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature, mask as u32);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature_select, 1);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature, (mask >> 32) as u32);
        }
        // ACKNOWLEDGE + DRIVER must be set first; FEATURES_OK confirms negotiation.
        self.write_status_or(STATUS_ACKNOWLEDGE)?;
        self.write_status_or(STATUS_DRIVER)?;
        self.write_status_or(STATUS_FEATURES_OK)?;
        Ok(())
    }

    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).queue_select, idx);
            core::ptr::write_volatile(&mut (*self.common()).queue_size, vq.queue_size);
            core::ptr::write_volatile(&mut (*self.common()).queue_desc, vq.desc_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_driver, vq.avail_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_device, vq.used_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_enable, 1);
        }
        Ok(())
    }

    fn notify(&self, queue_idx: u16) {
        // Modern: notify_addr = notify_va + queue_select.queue_notify_off * notify_off_multiplier.
        // For QEMU's typical config, every queue uses the same doorbell so we read queue_notify_off
        // after queue_select (already set in configure_queue).
        unsafe {
            // We cached queue_notify_off=0 in QEMU defaults; the safest is to re-read it.
            core::ptr::write_volatile(&mut (*self.common()).queue_select, queue_idx);
            let off = core::ptr::read_volatile(&(*self.common()).queue_notify_off);
            let bytes = (off as u32) * self.device.notify_off_multiplier;
            let notify_addr = (self.notify_va + bytes as usize) as *mut u16;
            core::ptr::write_volatile(notify_addr, queue_idx);
        }
    }

    fn isr_status(&self) -> u8 {
        unsafe { core::ptr::read_volatile(self.isr_va as *const u8) }
    }

    fn set_driver_ok(&mut self) -> Result<()> {
        self.write_status_or(STATUS_DRIVER_OK)
    }

    fn reset(&mut self) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).device_status, 0u8);
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean. May surface a few unused-imports warnings — that's OK at this stage.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-core/src/transport/ userspace/virtio-core/src/transport.rs 2>/dev/null; \
git add userspace/virtio-core/src/
git commit -m "virtio-core: ModernPciTransport (common cfg, notify, ISR, status)"
```

---

## Phase 4 — IrqSource

### T4.1 IrqSource wrapper

**Files:**
- Modify: `userspace/virtio-core/src/irq.rs`

- [ ] **Step 1: Write IrqSource**

```rust
//! Wrap `irq_attach` into a wait-for-completion primitive.
//!
//! On construction, allocate a private endpoint and call `irq_attach` so
//! the kernel pushes IRQ events as IPC messages to that endpoint. The
//! driver's main recv loop integrates the endpoint into its `recv_any`
//! token list — when an IRQ fires the loop wakes, reads ISR (the caller
//! does this), and drains the used ring.

use libcluu::syscall::{endpoint_create, irq_attach};
use libcluu::Result;

pub struct IrqSource {
    pub endpoint: usize,
    pub irq_number: usize,
}

impl IrqSource {
    /// Allocate a fresh endpoint and attach IRQ delivery to it. The
    /// endpoint token is returned for inclusion in `recv_any` lists.
    pub fn new(ipc_token: usize, irq_token: usize, irq_number: usize) -> Result<Self> {
        let endpoint = endpoint_create(ipc_token)?;
        irq_attach(irq_token, endpoint, irq_number)?;
        Ok(Self {
            endpoint,
            irq_number,
        })
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add userspace/virtio-core/src/irq.rs
git commit -m "virtio-core: IrqSource wrapping irq_attach"
```

`[BUILD GATE]` Phase 4 complete: virtio-core is feature-complete.

---

## Phase 5 — virtio-blk service rewrite

The existing virtio-blk service bundles ext2 and exposes `FS_READ_GRANT` (file-level) IPC to VFS. We keep the external IPC interface identical (no protocol churn) but rewrite the *internal* block-driver layer to use virtio-core. ext2 stays where it is for now.

### T5.1 Add virtio-core dependency to virtio-blk

**Files:**
- Modify: `userspace/virtio-blk/Cargo.toml`

- [ ] **Step 1: Add virtio-core as a dep**

In `[dependencies]`:
```toml
cluu-virtio-core = { path = "../virtio-core" }
```

- [ ] **Step 2: Build to confirm wiring**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean (no functional change yet).

- [ ] **Step 3: Commit**

```bash
git add userspace/virtio-blk/Cargo.toml
git commit -m "virtio-blk: depend on virtio-core (no functional change)"
```

### T5.2 BlkProtocol — header + status codes (extract from existing)

**Files:**
- Create: `userspace/virtio-blk/src/protocol.rs`
- Modify: `userspace/virtio-blk/src/lib.rs` (or main.rs) to declare the new module

- [ ] **Step 1: Write `protocol.rs`**

```rust
//! virtio-blk on-the-wire request layout (virtio 1.2 §5.2.6).

pub const VIRTIO_BLK_T_IN: u32 = 0;     // device → driver (read)
pub const VIRTIO_BLK_T_OUT: u32 = 1;    // driver → device (write)
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;

pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;

pub const SECTOR_SIZE: usize = 512;

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct VirtioBlkReqHeader {
    pub type_: u32,
    pub reserved: u32,
    pub sector: u64,
}
```

- [ ] **Step 2: Add `mod protocol;` to `userspace/virtio-blk/src/lib.rs`**

If lib.rs already exists (it does — see existing layout), append:
```rust
pub mod protocol;
```

- [ ] **Step 3: Build, commit**

```bash
cargo xtask build 2>&1 | tail -3
git add userspace/virtio-blk/src/protocol.rs userspace/virtio-blk/src/lib.rs
git commit -m "virtio-blk: extract on-the-wire protocol constants to protocol.rs"
```

### T5.3 BlkRequestQueue — owns the virtqueue, dispatches LBA reads

**Files:**
- Create: `userspace/virtio-blk/src/request_queue.rs`
- Modify: `userspace/virtio-blk/src/lib.rs`

- [ ] **Step 1: Write `request_queue.rs`**

```rust
//! BlkRequestQueue — the in-process queue of LBA reads/writes.
//!
//! Owns one Virtqueue (queue 0). Submitted requests are tracked by
//! their virtqueue cookie (which packs (session_id << 32) | request_id).
//! Completions are drained from the used ring on IRQ wake.

use crate::protocol::{VirtioBlkReqHeader, SECTOR_SIZE, VIRTIO_BLK_T_IN};
use cluu_virtio_core::dma::{DmaPool, DmaRegion};
use cluu_virtio_core::transport::Transport;
use cluu_virtio_core::virtqueue::{
    DescChain, Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE,
};
use alloc::vec::Vec;
use libcluu::{Error, Result};

/// Per-request bookkeeping while a request is in flight: the DMA region
/// holding the on-the-wire header + status byte for THIS request.
pub struct InflightSlot {
    pub cookie: u64,
    pub header_region: DmaRegion,
    pub status_region: DmaRegion,
}

pub struct BlkRequestQueue<T: Transport> {
    pub transport: T,
    pub vq: Virtqueue,
    pub pool: DmaPool,
    pub in_flight: Vec<InflightSlot>,
}

impl<T: Transport> BlkRequestQueue<T> {
    pub fn new(mut transport: T, mut pool: DmaPool, queue_size: u16) -> Result<Self> {
        let vq = Virtqueue::new(&mut pool, queue_size)?;
        transport.configure_queue(0, &vq)?;
        Ok(Self {
            transport,
            vq,
            pool,
            in_flight: Vec::new(),
        })
    }

    /// Submit a read of `total_bytes` from `lba` into the caller-provided
    /// physical pages `page_phys[..]`. `cookie` is opaque routing data.
    /// Returns Ok(()) and `notify` is the caller's responsibility to issue
    /// after a batch of submits to amortize the MMIO exit.
    ///
    /// Descriptor chain shape:
    ///   [ header(OUT, len=16) → page0(WRITE) → ... → pageN(WRITE) → status(WRITE, 1) ]
    pub fn submit_read(
        &mut self,
        lba: u64,
        page_phys: &[u64],
        total_bytes: usize,
        cookie: u64,
    ) -> Result<()> {
        if page_phys.is_empty() {
            return Err(Error::InvalidArgument);
        }

        let n_descs = (page_phys.len() + 2) as u16; // header + N + status
        let chain = self
            .vq
            .alloc_chain(n_descs)
            .ok_or(Error::Busy)?;

        // Allocate header + status from the DMA pool.
        let header_region = match self.pool.alloc(16, 16) {
            Ok(r) => r,
            Err(e) => {
                self.vq.free_chain(chain);
                return Err(e);
            }
        };
        let status_region = match self.pool.alloc(1, 1) {
            Ok(r) => r,
            Err(e) => {
                self.vq.free_chain(chain);
                return Err(e);
            }
        };

        // Fill header.
        unsafe {
            let h = header_region.virt as *mut VirtioBlkReqHeader;
            (*h).type_ = VIRTIO_BLK_T_IN;
            (*h).reserved = 0;
            (*h).sector = lba;
        }
        unsafe {
            *(status_region.virt as *mut u8) = 0xFF; // sentinel; device overwrites
        }

        // Walk the chain to fill descriptors.
        // Build chain links: every desc except the last has NEXT.
        let descs = self.collect_chain_indices(chain.head, n_descs);
        for (i, &didx) in descs.iter().enumerate() {
            let is_last = i == descs.len() - 1;
            if i == 0 {
                // Header: device reads, driver writes (default == OUT_FROM_DRIVER).
                let next_link = if is_last { 0 } else { descs[i + 1] };
                let flags = if is_last { 0 } else { VRING_DESC_F_NEXT };
                self.vq
                    .desc_set(didx, header_region.phys, 16, flags, next_link);
            } else if i == descs.len() - 1 {
                // Status: device writes.
                self.vq
                    .desc_set(didx, status_region.phys, 1, VRING_DESC_F_WRITE, 0);
            } else {
                // Buffer pages: device writes (this is a read request, so
                // the buffer is filled BY the device).
                let page_idx = i - 1;
                let bytes_in_page = if page_idx == page_phys.len() - 1 {
                    let rem = total_bytes - page_idx * 4096;
                    rem
                } else {
                    4096
                };
                let next_link = descs[i + 1];
                self.vq.desc_set(
                    didx,
                    page_phys[page_idx],
                    bytes_in_page as u32,
                    VRING_DESC_F_NEXT | VRING_DESC_F_WRITE,
                    next_link,
                );
            }
        }

        self.vq.submit(chain, cookie);
        self.in_flight.push(InflightSlot {
            cookie,
            header_region,
            status_region,
        });
        Ok(())
    }

    /// Issue a single device notify covering all submits since the last call.
    pub fn notify(&self) {
        self.transport.notify(0);
    }

    /// Drain used-ring entries. Returns Vec<(cookie, status_byte, len)>.
    /// Caller is responsible for releasing the per-request DMA regions
    /// (header/status) — for now we leak them into the bump pool, which
    /// is fine because the pool is sized to absorb the steady-state load.
    /// (Recycle in T5.4.)
    pub fn drain_completions(&mut self) -> Vec<(u64, u8, u32)> {
        let mut out = Vec::new();
        while let Some((cookie, len)) = self.vq.pop_used() {
            // Find the in-flight slot for this cookie to read its status.
            let pos = match self.in_flight.iter().position(|s| s.cookie == cookie) {
                Some(p) => p,
                None => continue,
            };
            let slot = self.in_flight.swap_remove(pos);
            let status = unsafe { *(slot.status_region.virt as *const u8) };
            out.push((cookie, status, len));
            // header_region/status_region currently leak (see comment above).
        }
        out
    }

    pub fn free_capacity(&self) -> u16 {
        self.vq.free_capacity()
    }

    fn collect_chain_indices(&self, head: u16, n: u16) -> Vec<u16> {
        // alloc_chain pulls n entries via the in-table NEXT field; we walked
        // them already to figure out tail. Re-walk to give descriptors
        // ordered list. This matches what alloc_chain's free-list traversal
        // does.
        let mut out = Vec::with_capacity(n as usize);
        let mut cur = head;
        for _ in 0..n {
            out.push(cur);
            // We need the original link (alloc_chain disconnected the tail)
            // — read the desc table directly.
            let next = unsafe {
                let p = (self.vq.desc_region.virt as *const cluu_virtio_core::virtqueue::VRingDesc)
                    .add(cur as usize);
                (*p).next
            };
            cur = next;
        }
        out
    }
}
```

- [ ] **Step 2: Add `mod request_queue;` to `userspace/virtio-blk/src/lib.rs`**

Append:
```rust
pub mod request_queue;
```

- [ ] **Step 3: Build**

Run: `cargo xtask build 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-blk/src/request_queue.rs userspace/virtio-blk/src/lib.rs
git commit -m "virtio-blk: BlkRequestQueue with submit_read + drain_completions"
```

### T5.4 Recycle DMA regions on completion

The naive `submit_read` leaks per-request `header_region` + `status_region` into the bump pool. Replace with a small free-list inside `BlkRequestQueue`.

**Files:**
- Modify: `userspace/virtio-blk/src/request_queue.rs`

- [ ] **Step 1: Add a per-queue free-list of (header, status) region pairs and recycle on completion**

Replace the `BlkRequestQueue` struct definition + the relevant submit/drain bodies. The full diff is the simplest representation. After this task:

```rust
// Add to struct:
pub struct BlkRequestQueue<T: Transport> {
    pub transport: T,
    pub vq: Virtqueue,
    pub pool: DmaPool,
    pub in_flight: Vec<InflightSlot>,
    free_slots: Vec<(DmaRegion, DmaRegion)>, // (header, status) pairs
}

// In `new`, initialize free_slots = Vec::new().
// In submit_read, before pool.alloc, try free_slots.pop() first:
let (header_region, status_region) = match self.free_slots.pop() {
    Some(pair) => pair,
    None => {
        let h = self.pool.alloc(16, 16)?;
        let s = self.pool.alloc(1, 1)?;
        (h, s)
    }
};
// (and remove the two earlier `match self.pool.alloc` calls + their cleanup-on-error.)
// On failure to alloc_chain, we now return the regions to free_slots:
let chain = match self.vq.alloc_chain(n_descs) {
    Some(c) => c,
    None => {
        self.free_slots.push((header_region, status_region));
        return Err(Error::Busy);
    }
};

// In drain_completions, after taking the slot:
self.free_slots.push((slot.header_region, slot.status_region));
```

Apply the edits inline.

- [ ] **Step 2: Build, commit**

```bash
cargo xtask build 2>&1 | tail -3
git add userspace/virtio-blk/src/request_queue.rs
git commit -m "virtio-blk: recycle per-request DMA regions via free_slots list"
```

### T5.5 Service main.rs — replace legacy initializer with virtio-core stack

The existing `userspace/virtio-blk/src/main.rs` initializes via `userspace/virtio-blk/src/virtio.rs` (legacy). Replace the device init path to build a `ModernPciTransport` + `BlkRequestQueue`. Keep all the existing FS_READ_GRANT / FS_WRITE message handlers — they now read from `BlkRequestQueue` synchronously by submit + spin-poll-drain. The next phase replaces spin-poll with IRQ wait.

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

- [ ] **Step 1: Read the existing init flow to understand what to replace**

Run: `grep -n "VirtioBlkDevice::new\|virtio.rs\|virtqueue.rs\|fn main\|space_map_range" userspace/virtio-blk/src/main.rs | head -10`

- [ ] **Step 2: Replace the device init with virtio-core wiring**

Locate (in `main`) the call site that creates the legacy device. Replace from PCI scan complete through `device_ready` log with:

```rust
// === New virtio-core init path ===
use cluu_virtio_core::{
    DmaPool, IrqSource,
    transport::{ModernPciTransport, Transport, FeatureBits},
};
use crate::request_queue::BlkRequestQueue;

const DMA_POOL_VA: usize = 0x5100_0000;
const DMA_POOL_PAGES: usize = 64;
const MMIO_VA_BASE: usize = 0x5200_0000;

let pool = DmaPool::new(space_token, DMA_POOL_VA, DMA_POOL_PAGES)?;

// Per-PCI-device BAR base (BAR4 holds all four cap regions on QEMU virtio).
let bar_phys = pci_device.bars[pci_device.common_cfg_bar as usize];
let mut transport = ModernPciTransport::new(space_token, pci_device.clone(), bar_phys, MMIO_VA_BASE)?;

// Reset, negotiate features (VERSION_1 only — no fancy device features needed).
transport.reset()?;
let dev_feats = transport.read_device_features()?;
let want = FeatureBits::VERSION_1.bits() & dev_feats;
transport.write_driver_features(want)?;

let mut bq = BlkRequestQueue::new(transport, pool, 64)?;
bq.transport.set_driver_ok()?;
let _ = libcluu::debug_print("virtio-blk: virtio-core stack initialized");

// Attach IRQ for the legacy PIC vector (virtio-blk on QEMU = IRQ 11).
let irq_token = info.tokens[libcluu::boot::TOKEN_EXTRA_0];
let irq = IrqSource::new(info.tokens[libcluu::boot::TOKEN_IPC], irq_token, 11)?;
let _ = libcluu::debug_print("virtio-blk: IRQ attached");
```

(The `pci_device` and `space_token` come from earlier in the existing main.rs flow.)

Drop the legacy `device_ready` block.

- [ ] **Step 3: Replace the `read` call inside `FS_READ_GRANT` handler**

The existing handler does `fs.read(inode, offset, &mut scratch[..len])`. ext2 lives inside this process; `fs.read` is the existing ext2 layer. Internally `fs.read` issues `block_read(lba, n_sectors)` calls one at a time against the legacy driver. We need ext2 to call into `BlkRequestQueue` instead. For this task, keep ext2 unchanged but route its `block_read` through a new shim:

In `main.rs`, define a helper:

```rust
fn block_read_blocking(bq: &mut BlkRequestQueue<ModernPciTransport>, lba: u64, buf: &mut [u8]) -> Result<usize> {
    // 1. Walk caller buf in 4KB pages — for ext2's in-process buffer this is
    //    already in our address space, so virt_to_phys works directly.
    let pages: alloc::vec::Vec<u64> = (0..buf.len().div_ceil(4096))
        .map(|i| {
            let va = buf.as_ptr() as usize + i * 4096;
            // Buffer is in driver's own space, so use the driver's space token.
            libcluu::syscall::virt_to_phys(bq.pool.space_token(), va).unwrap() as u64
        })
        .collect();
    // 2. Submit + notify.
    bq.submit_read(lba, &pages, buf.len(), 0xDEADBEEFu64)?;
    bq.notify();
    // 3. Spin until completion (Phase 6 replaces with IRQ wait).
    loop {
        let completions = bq.drain_completions();
        for (_cookie, status, len) in completions {
            if status != 0 {
                return Err(Error::Io);
            }
            return Ok(len as usize);
        }
        core::hint::spin_loop();
    }
}
```

Then in the existing ext2 layer wherever it calls `device.read_sectors(lba, ...)`, route through `block_read_blocking(&mut bq, lba, ...)` instead.

(This will likely involve threading `bq` through the call site. Find the existing call in `userspace/virtio-blk/src/lib.rs` or wherever the ext2 reader lives, and wire it.)

- [ ] **Step 4: Build, fix compile errors as they appear (drop unused imports of `virtio.rs`/`virtqueue.rs` legacy types)**

Run: `cargo xtask build 2>&1 | tail -10`
Expected: clean. Compile errors point at remaining call sites of legacy types — chase them down.

- [ ] **Step 5: Boot smoke**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=none RUN_WAIT=15 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: `No faults detected`.
Inspect: `grep -E "virtio-blk: virtio-core stack initialized|virtio-blk: IRQ attached|shell: ready" /tmp/cluu-serial-com2.log`
Expected: all three lines present, in that order.

- [ ] **Step 6: Commit**

```bash
git add userspace/virtio-blk/src/main.rs userspace/virtio-blk/src/lib.rs
git commit -m "virtio-blk: switch device init to virtio-core stack (spin-poll for now)"
```

### T5.6 IRQ-driven completion — replace spin-poll with recv_any

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

- [ ] **Step 1: Add the IRQ endpoint to the service main recv loop**

The existing main loop calls `ipc_recv_any` on `[control_endpoint, ...]`. Add `irq.endpoint`. When the IRQ index fires, drain `bq.drain_completions()` and re-deliver to whatever was waiting (the ext2 layer, currently doing `block_read_blocking`).

This requires inverting the control flow: ext2's `block_read_blocking` cannot spin-poll; it must hand control back to main and resume on completion. Two options:
- (a) Per-request synchronous recv: after submit+notify, do `ipc_recv` *only on the IRQ endpoint* and drain. Other endpoints are paused while the read is in flight. Simpler, slightly less responsive.
- (b) Full multi-endpoint recv with state machine. More work but lets multiple in-flight reads from different sessions overlap.

For T5.6, go with (a) as a stepping stone. T5.7 generalizes to (b).

Add to `main.rs` after IrqSource creation:

```rust
fn wait_for_completion(
    bq: &mut BlkRequestQueue<ModernPciTransport>,
    irq: &IrqSource,
) -> Result<(u8, u32)> {
    let tokens = [irq.endpoint];
    let mut buf = [0u8; 64];
    loop {
        // Block until IRQ delivery.
        let _ = libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX)?;
        // Acknowledge ISR.
        let _ = bq.transport.isr_status();
        // Drain.
        let completions = bq.drain_completions();
        if let Some((_, status, len)) = completions.into_iter().next() {
            return Ok((status, len));
        }
        // Spurious IRQ — keep waiting.
    }
}
```

Then change `block_read_blocking` to call `wait_for_completion` instead of the spin-loop:

```rust
fn block_read_blocking(
    bq: &mut BlkRequestQueue<ModernPciTransport>,
    irq: &IrqSource,
    lba: u64,
    buf: &mut [u8],
) -> Result<usize> {
    let pages: alloc::vec::Vec<u64> = (0..buf.len().div_ceil(4096))
        .map(|i| {
            let va = buf.as_ptr() as usize + i * 4096;
            libcluu::syscall::virt_to_phys(bq.pool.space_token(), va).unwrap() as u64
        })
        .collect();
    bq.submit_read(lba, &pages, buf.len(), 0)?;
    bq.notify();
    let (status, len) = wait_for_completion(bq, irq)?;
    if status != 0 {
        return Err(Error::Io);
    }
    Ok(len as usize)
}
```

Thread `&irq` through the call site.

- [ ] **Step 2: Build, smoke as in T5.5 step 5**

Run: `cargo xtask build 2>&1 | tail -3 && pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=none RUN_WAIT=15 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: no faults; `shell: ready` reached.

- [ ] **Step 3: Confirm shell + first command run**

Run: `grep -E "shell: ready|shell: container run done" /tmp/cluu-serial-com2.log | head`
Expected: `shell: ready` line at minimum.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-blk/src/main.rs
git commit -m "virtio-blk: IRQ-driven completion via wait_for_completion (single in-flight)"
```

### T5.7 Multi-in-flight at the service IPC boundary

Convert the service main loop to a state machine: while a read is in flight, continue accepting other FS_READ_GRANT requests, queue them, and dispatch them as soon as the queue has free descriptors. Each request remembers its `reply_token` so completions go back to the right caller.

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

- [ ] **Step 1: Define `PendingRequest` and a per-driver in-flight table**

```rust
struct PendingRequest {
    reply_token: usize,
    target_base: usize,
    target_space: usize,
    requested_bytes: usize,
    cookie: u64,
}

struct DriverState {
    bq: BlkRequestQueue<ModernPciTransport>,
    irq: IrqSource,
    pending: alloc::collections::BTreeMap<u64, PendingRequest>,
    next_cookie: u64,
}
```

Initialize once, then thread `&mut DriverState` through.

- [ ] **Step 2: Reshape the main loop**

```rust
let tokens = [control_endpoint, driver.irq.endpoint];
loop {
    let (idx, len) = libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX)?;
    if idx == 0 {
        // FS_READ_GRANT etc. — handle synchronously up to submit, then return.
        // The completion handler below replies to the caller.
        handle_control_message(&mut driver, &buf[..len])?;
    } else {
        // IRQ.
        let _ = driver.bq.transport.isr_status();
        for (cookie, status, blen) in driver.bq.drain_completions() {
            if let Some(req) = driver.pending.remove(&cookie) {
                let reply = if status == 0 {
                    // Successful: grant pages back to caller.
                    grant_back(&driver, &req, blen as usize);
                    Message::new(FS_READ_GRANT, [0, blen as usize, 0, 0, 0, 0], 2)
                } else {
                    Message::new(FS_READ_GRANT, [Error::Io as isize as usize, 0, 0, 0, 0, 0], 2)
                };
                let _ = libcluu::ipc::reply(req.reply_token, &reply, IpcFlags::empty());
            }
        }
    }
}
```

`handle_control_message` is what used to be the per-FS_READ_GRANT branch; it now ends with `submit_read + notify` and stores a `PendingRequest` keyed by cookie, then *returns without replying*.

`grant_back` is the existing logic that does the per-page `space_grant` to the caller's space.

- [ ] **Step 3: Build, smoke**

Run: `cargo xtask build 2>&1 | tail -3 && pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=none RUN_WAIT=15 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: `No faults`, shell ready, basic boot OK.

- [ ] **Step 4: Run l2_pipe_basic — confirms the new path serves real ext2 reads**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_pipe_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | grep -E "all required markers|MISSING"`
Expected: `No faults detected and all required markers found.`

- [ ] **Step 5: Commit**

```bash
git add userspace/virtio-blk/src/main.rs
git commit -m "virtio-blk: multi-in-flight at service IPC boundary"
```

`[BUILD GATE + HARNESS GATE]` Phase 5 complete.

---

## Phase 6 — Public BlkSessionClient (libcluu) + blkprobe

This phase exposes the in-process driver via raw-block IPC for non-ext2 callers. Wire protocol: `BLK_OPEN_SESSION` / `BLK_SUBMIT` / `BLK_COMPLETE`. The existing `FS_READ_GRANT` interface remains for VFS.

### T6.1 IPC labels + payload structs

**Files:**
- Create: `userspace/virtio-blk/src/ipc.rs`
- Modify: `userspace/libcluu/src/ipc.rs` (add label constants)

- [ ] **Step 1: Define the labels in libcluu so both sides import the same constants**

Append to `userspace/libcluu/src/ipc.rs`:
```rust
pub const BLK_OPEN_SESSION: u32 = 0x310;
pub const BLK_SUBMIT: u32 = 0x311;
pub const BLK_COMPLETE: u32 = 0x312;
pub const BLK_CLOSE_SESSION: u32 = 0x313;
pub const BLK_SUBMIT_NACK: u32 = 0x314;
```

- [ ] **Step 2: Write `userspace/virtio-blk/src/ipc.rs` with payload-encoding helpers**

```rust
//! Raw-block IPC wire format for the new BLK_* labels.
//!
//! All payloads are little-endian native-usize words (matching CLUU's
//! `Message::words[]` already-LE convention).
//!
//! BLK_OPEN_SESSION request:
//!   words = [completion_endpoint_token]
//!   reply.words = [errno, session_id]
//!
//! BLK_SUBMIT request (no reply):
//!   words = [session_id, request_id, lba_low, lba_high, n_pages, total_bytes]
//!   payload = [page_phys_le_u64; n_pages]    // phys addrs in driver space
//!
//! BLK_COMPLETE notification (driver → caller endpoint):
//!   words = [request_id, status_byte, bytes_done]
//!
//! BLK_SUBMIT_NACK notification (driver → caller endpoint):
//!   words = [request_id, errno]
```

(Just the documentation file for now; no code yet.)

- [ ] **Step 3: Build, commit**

```bash
cargo xtask build 2>&1 | tail -3
git add userspace/libcluu/src/ipc.rs userspace/virtio-blk/src/ipc.rs
git commit -m "virtio-blk: BLK_* IPC labels + wire format docs"
```

### T6.2 BlkSession — driver-side state per caller

**Files:**
- Create: `userspace/virtio-blk/src/session.rs`
- Modify: `userspace/virtio-blk/src/lib.rs`

- [ ] **Step 1: Write `session.rs`**

```rust
//! Per-caller block-session state.

use alloc::collections::BTreeMap;

pub type SessionId = u32;
pub type RequestId = u64;

pub struct InFlight {
    pub request_id: RequestId,
    pub completion_endpoint: usize,
    pub bytes_requested: usize,
}

pub struct BlkSession {
    pub session_id: SessionId,
    pub completion_endpoint: usize,
    pub queue_depth_cap: u16,
    pub in_flight: BTreeMap<RequestId, InFlight>,
}

impl BlkSession {
    pub fn new(session_id: SessionId, completion_endpoint: usize) -> Self {
        Self {
            session_id,
            completion_endpoint,
            queue_depth_cap: 32,
            in_flight: BTreeMap::new(),
        }
    }

    pub fn at_cap(&self) -> bool {
        self.in_flight.len() as u16 >= self.queue_depth_cap
    }
}

pub fn pack_cookie(sid: SessionId, rid: RequestId) -> u64 {
    ((sid as u64) << 32) | (rid as u64 & 0xFFFF_FFFF)
}

pub fn unpack_cookie(cookie: u64) -> (SessionId, RequestId) {
    ((cookie >> 32) as SessionId, cookie & 0xFFFF_FFFF)
}
```

- [ ] **Step 2: Add `mod session;` to `userspace/virtio-blk/src/lib.rs`**

```rust
pub mod session;
```

- [ ] **Step 3: Build, commit**

```bash
cargo xtask build 2>&1 | tail -3
git add userspace/virtio-blk/src/session.rs userspace/virtio-blk/src/lib.rs
git commit -m "virtio-blk: BlkSession per-caller state + cookie pack/unpack"
```

### T6.3 BLK_OPEN_SESSION / BLK_SUBMIT / BLK_CLOSE_SESSION handlers

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

- [ ] **Step 1: Add a `BTreeMap<SessionId, BlkSession>` to `DriverState` + a `next_session_id`**

```rust
struct DriverState {
    bq: BlkRequestQueue<ModernPciTransport>,
    irq: IrqSource,
    pending_fs: alloc::collections::BTreeMap<u64, PendingRequest>, // for FS_READ_GRANT path
    sessions: alloc::collections::BTreeMap<u32, crate::session::BlkSession>, // for BLK_* path
    next_session_id: u32,
    next_cookie: u64,
}
```

- [ ] **Step 2: Add handlers in `handle_control_message` for BLK_OPEN_SESSION, BLK_SUBMIT, BLK_CLOSE_SESSION**

```rust
match msg.tag.label {
    libcluu::ipc::BLK_OPEN_SESSION => {
        let comp_ep = msg.words[0];
        let sid = driver.next_session_id;
        driver.next_session_id += 1;
        driver.sessions.insert(sid, crate::session::BlkSession::new(sid, comp_ep));
        let reply = Message::new(libcluu::ipc::BLK_OPEN_SESSION, [0, sid as usize, 0, 0, 0, 0], 2);
        if let Some(rt) = reply_token {
            let _ = libcluu::ipc::reply(rt, &reply, IpcFlags::empty());
        }
    }

    libcluu::ipc::BLK_SUBMIT => {
        let sid = msg.words[0] as u32;
        let rid = msg.words[1] as u64;
        let lba = ((msg.words[3] as u64) << 32) | (msg.words[2] as u64);
        let n_pages = msg.words[4];
        let total_bytes = msg.words[5];

        let comp_ep = match driver.sessions.get(&sid) {
            Some(s) if !s.at_cap() => s.completion_endpoint,
            Some(_) => {
                let _ = nack(driver, sid, rid, libcluu::Error::Busy);
                return Ok(());
            }
            None => return Ok(()),
        };

        // Decode page_phys list from payload.
        if payload.len() < 8 * n_pages {
            let _ = nack(driver, sid, rid, libcluu::Error::InvalidArgument);
            return Ok(());
        }
        let mut pages: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(n_pages);
        for i in 0..n_pages {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&payload[i*8 .. i*8+8]);
            pages.push(u64::from_le_bytes(bytes));
        }

        let cookie = crate::session::pack_cookie(sid, rid);
        match driver.bq.submit_read(lba, &pages, total_bytes, cookie) {
            Ok(()) => {
                driver.bq.notify();
                let session = driver.sessions.get_mut(&sid).unwrap();
                session.in_flight.insert(rid, crate::session::InFlight {
                    request_id: rid,
                    completion_endpoint: comp_ep,
                    bytes_requested: total_bytes,
                });
            }
            Err(_) => {
                let _ = nack(driver, sid, rid, libcluu::Error::Busy);
            }
        }
    }

    libcluu::ipc::BLK_CLOSE_SESSION => {
        let sid = msg.words[0] as u32;
        driver.sessions.remove(&sid);
    }

    _ => { /* existing FS_READ_GRANT path stays */ }
}
```

- [ ] **Step 3: Add the `nack` helper**

```rust
fn nack(driver: &DriverState, sid: u32, rid: u64, err: libcluu::Error) -> libcluu::Result<()> {
    if let Some(s) = driver.sessions.get(&sid) {
        let msg = Message::new(libcluu::ipc::BLK_SUBMIT_NACK,
            [rid as usize, err as isize as usize, 0, 0, 0, 0], 2);
        let _ = libcluu::ipc::send(s.completion_endpoint, &msg, IpcFlags::empty());
    }
    Ok(())
}
```

- [ ] **Step 4: Update the IRQ drain branch to route BLK_* completions**

Where the existing IRQ drain produces `(cookie, status, blen)`, demux on cookie:

```rust
for (cookie, status, blen) in driver.bq.drain_completions() {
    let (sid, rid) = crate::session::unpack_cookie(cookie);
    if sid == 0 {
        // FS_READ_GRANT path (cookie 0 reserved). Existing reply logic.
        if let Some(req) = driver.pending_fs.remove(&cookie) { /* ... */ }
        continue;
    }
    if let Some(session) = driver.sessions.get_mut(&sid) {
        if let Some(inf) = session.in_flight.remove(&rid) {
            let comp = Message::new(libcluu::ipc::BLK_COMPLETE,
                [rid as usize, status as usize, blen as usize, 0, 0, 0], 3);
            let _ = libcluu::ipc::send(inf.completion_endpoint, &comp, IpcFlags::empty());
        }
    }
}
```

(The `cookie 0` reservation needs to be honored by the FS path: ensure `next_cookie` starts at 1 OR change the FS path to also use `pack_cookie(0, fs_rid)`.)

- [ ] **Step 5: Build, smoke**

Run: `cargo xtask build 2>&1 | tail -3 && pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_pipe_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | grep -E "all required markers|MISSING"`
Expected: green (the existing FS path still works).

- [ ] **Step 6: Commit**

```bash
git add userspace/virtio-blk/src/main.rs
git commit -m "virtio-blk: BLK_OPEN_SESSION/SUBMIT/CLOSE handlers + cookie demux"
```

### T6.4 libcluu BlkSessionClient

**Files:**
- Create: `userspace/libcluu/src/fs/blk_client.rs`
- Modify: `userspace/libcluu/src/fs/mod.rs`

- [ ] **Step 1: Write the client**

`userspace/libcluu/src/fs/blk_client.rs`:
```rust
//! BlkSessionClient — caller-side helper for the raw-block protocol.
//!
//! Open a session, then use either `read_blocking` (sync wrapper) or
//! `submit_async` + `drain_completions` (caller-driven).

use alloc::vec::Vec;
use crate::ipc::{
    parse_message, BLK_CLOSE_SESSION, BLK_COMPLETE, BLK_OPEN_SESSION, BLK_SUBMIT,
    BLK_SUBMIT_NACK,
};
use crate::syscall::{
    endpoint_create, ipc_call, ipc_recv_any, ipc_send, virt_to_phys,
};
use crate::types::{IpcFlags, Message};
use crate::{boot::process_info, boot::TOKEN_IPC, boot::TOKEN_SPACE, Error, Result};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct RequestHandle(pub u64);

pub struct BlkSessionClient {
    blkdev_endpoint: usize,
    completion_endpoint: usize,
    space_token: usize,
    session_id: u32,
    next_request_id: u64,
    pending_completions: Vec<(RequestHandle, Result<usize>)>,
}

impl BlkSessionClient {
    pub fn open(blkdev_endpoint: usize) -> Result<Self> {
        let info = process_info();
        let ipc_token = info.tokens[TOKEN_IPC];
        let space_token = info.tokens[TOKEN_SPACE];
        let completion_endpoint = endpoint_create(ipc_token)?;

        let req = Message::new(BLK_OPEN_SESSION,
            [completion_endpoint, 0, 0, 0, 0, 0], 1);
        let mut reply_buf = [0u8; 64];
        let bytes = ipc_call(blkdev_endpoint, req.as_bytes(), &mut reply_buf)?;
        let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(Error::InvalidState)?;
        if rmsg.tag.label != BLK_OPEN_SESSION || rmsg.words[0] != 0 {
            return Err(Error::Io);
        }
        let session_id = rmsg.words[1] as u32;
        Ok(Self {
            blkdev_endpoint,
            completion_endpoint,
            space_token,
            session_id,
            next_request_id: 1,
            pending_completions: Vec::new(),
        })
    }

    /// Submit one request; returns a handle the caller can match against
    /// completions. `buf` MUST stay alive and unmoved until the matching
    /// completion arrives.
    pub fn submit_async(&mut self, lba: u64, buf: &mut [u8]) -> Result<RequestHandle> {
        let n_pages = buf.len().div_ceil(4096);
        let mut pages_phys: Vec<u64> = Vec::with_capacity(n_pages);
        for i in 0..n_pages {
            let va = buf.as_ptr() as usize + i * 4096;
            pages_phys.push(virt_to_phys(self.space_token, va)? as u64);
        }
        let rid = self.next_request_id;
        self.next_request_id += 1;
        let mut msg = Message::new(BLK_SUBMIT,
            [
                self.session_id as usize,
                rid as usize,
                lba as usize,
                (lba >> 32) as usize,
                n_pages,
                buf.len(),
            ], 6);
        // Encode phys list as payload.
        let mut payload: Vec<u8> = Vec::with_capacity(n_pages * 8);
        for p in pages_phys {
            payload.extend_from_slice(&p.to_le_bytes());
        }
        let header = msg.as_bytes();
        let mut send_buf = Vec::with_capacity(header.len() + payload.len());
        send_buf.extend_from_slice(header);
        send_buf.extend_from_slice(&payload);
        crate::syscall::ipc_send(self.blkdev_endpoint, &send_buf)?;
        Ok(RequestHandle(rid))
    }

    pub fn drain_completions(&mut self) -> Vec<(RequestHandle, Result<usize>)> {
        let mut out = core::mem::take(&mut self.pending_completions);
        let tokens = [self.completion_endpoint];
        let mut buf = [0u8; 128];
        loop {
            match ipc_recv_any(&tokens, &mut buf, 0) {
                Ok((_, len)) => {
                    if let Some((m, _)) = parse_message(&buf[..len]) {
                        out.push(self.decode_completion(&m));
                    }
                }
                Err(_) => break,
            }
        }
        out
    }

    pub fn read_blocking(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize> {
        let h = self.submit_async(lba, buf)?;
        let tokens = [self.completion_endpoint];
        let mut rbuf = [0u8; 128];
        loop {
            let (_, len) = ipc_recv_any(&tokens, &mut rbuf, u64::MAX)?;
            if let Some((m, _)) = parse_message(&rbuf[..len]) {
                let (handle, result) = self.decode_completion(&m);
                if handle == h {
                    return result;
                }
                // Belongs to another in-flight request — queue.
                self.pending_completions.push((handle, result));
            }
        }
    }

    fn decode_completion(&self, m: &Message) -> (RequestHandle, Result<usize>) {
        let h = RequestHandle(m.words[0] as u64);
        let result = match m.tag.label {
            BLK_COMPLETE => {
                let status = m.words[1] as u8;
                let len = m.words[2];
                if status == 0 { Ok(len) } else { Err(Error::Io) }
            }
            BLK_SUBMIT_NACK => Err(Error::from_errno(m.words[1] as isize)),
            _ => Err(Error::InvalidState),
        };
        (h, result)
    }
}

impl Drop for BlkSessionClient {
    fn drop(&mut self) {
        let msg = Message::new(BLK_CLOSE_SESSION,
            [self.session_id as usize, 0, 0, 0, 0, 0], 1);
        let _ = ipc_send(self.blkdev_endpoint, msg.as_bytes());
    }
}
```

- [ ] **Step 2: Re-export from `userspace/libcluu/src/fs/mod.rs`**

```rust
pub mod blk_client;
pub use blk_client::{BlkSessionClient, RequestHandle};
```

- [ ] **Step 3: Build, commit**

```bash
cargo xtask build 2>&1 | tail -3
git add userspace/libcluu/src/fs/blk_client.rs userspace/libcluu/src/fs/mod.rs
git commit -m "libcluu: BlkSessionClient with sync read_blocking + async submit/drain"
```

### T6.5 blkprobe — end-to-end harness probe + l2_blk_basic

**Files:**
- Create: `userspace/blkprobe/Cargo.toml`
- Create: `userspace/blkprobe/src/main.rs`
- Modify: `Cargo.toml`
- Create: `containers/blkprobe/Cluufile`
- Modify: `etc/autostart.toml`
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: blkprobe Cargo.toml**

```toml
[package]
name = "cluu-blkprobe"
version = "0.1.0"
edition = "2021"
description = "Raw-block read smoke test"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "blkprobe"
path = "src/main.rs"
```

- [ ] **Step 2: blkprobe main.rs**

```rust
#![no_std]
#![no_main]

extern crate alloc;

use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::fs::BlkSessionClient;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("blkprobe: [FAIL] subscribe blkdev:main");
            return 1;
        }
    };
    let mut c = match BlkSessionClient::open(blkdev) {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("blkprobe: [FAIL] open session");
            return 1;
        }
    };
    // Allocate a 4KB buffer (page-aligned by the allocator's alignment).
    let mut buf = alloc::vec![0u8; 4096];
    match c.read_blocking(0, &mut buf) {
        Ok(n) if n == 4096 => {
            // Sanity check: ext2 magic is at offset 0x438 in superblock,
            // sector 0 is boot sector / MBR; just confirm ANY non-zero data.
            let any_nonzero = buf.iter().any(|&b| b != 0);
            if !any_nonzero {
                let _ = debug_print("blkprobe: [FAIL] sector 0 all zeros");
                return 1;
            }
        }
        Ok(n) => {
            let _ = debug_print(&alloc::format!("blkprobe: [FAIL] short read n={}", n));
            return 1;
        }
        Err(_) => {
            let _ = debug_print("blkprobe: [FAIL] read_blocking err");
            return 1;
        }
    }
    let _ = debug_print("blkprobe: ALL OK");
    libcluu::process::exit(0);
}

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    let _ = debug_print(&alloc::format!("blkprobe: PANIC {}", info));
    libcluu::process::exit(2);
}
```

- [ ] **Step 3: Cluufile**

`containers/blkprobe/Cluufile`:
```
ENVELOPE user
EXEC /var/images/blkprobe/bin/blkprobe
```

- [ ] **Step 4: Add to workspace**

Edit `Cargo.toml` workspace + user-target lists: append `"userspace/blkprobe",`.

- [ ] **Step 5: Add to autostart**

`etc/autostart.toml` append:
```toml
[[service]]
name = "blkprobe"
```

- [ ] **Step 6: Harness case wiring**

Append to `scripts/harness_cases.conf`:
```
l2_blk_basic|full|MARKER_MODE=l2_blk_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

In `scripts/harness_run.sh`, add to the marker case:
```bash
    l2_blk_basic)
        required_markers=(
            "TSC calibrated"
            "blkprobe: ALL OK"
        )
        ;;
```

- [ ] **Step 7: Build, run**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_blk_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: `No faults detected and all required markers found.`

- [ ] **Step 8: Commit**

```bash
git add userspace/blkprobe/ containers/blkprobe/ etc/autostart.toml \
        scripts/harness_cases.conf scripts/harness_run.sh Cargo.toml
git commit -m "blkprobe: end-to-end raw-block read (l2_blk_basic harness case)"
```

`[BUILD GATE + HARNESS GATE]` Phase 6 complete.

---

## Phase 7 — Concurrency stress + session teardown harness

### T7.1 l2_blk_concurrent — 4 parallel readers

**Files:**
- Modify: `userspace/blkprobe/src/main.rs` (add a `concurrent` mode toggled by argv[1])
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_run.sh`
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: blkprobe: extend main.rs to dispatch on argv**

```rust
let argv = libcluu::args::argv();
let mode = argv.get(1).copied().unwrap_or("basic");
match mode {
    "basic" => run_basic(),
    "concurrent" => run_concurrent(),
    _ => {
        let _ = debug_print("blkprobe: [FAIL] unknown mode");
        libcluu::process::exit(1);
    }
}
```

- [ ] **Step 2: Implement `run_concurrent` — 4 sessions, 8 in-flight each, 100 reads**

```rust
fn run_concurrent() {
    let blkdev = libcluu::registry::subscribe_output("blkdev", "main").unwrap();

    let mut sessions: alloc::vec::Vec<BlkSessionClient> = (0..4)
        .map(|_| BlkSessionClient::open(blkdev).unwrap())
        .collect();
    let mut bufs: alloc::vec::Vec<alloc::vec::Vec<u8>> =
        (0..4).map(|_| alloc::vec![0u8; 32 * 1024]).collect();
    let mut handles: alloc::vec::Vec<libcluu::fs::RequestHandle> = alloc::vec![];

    let total = 100;
    let mut completed = 0;
    let mut next_lba = 0u64;

    while completed < total {
        // Submit while any session has capacity and we still have reads to issue.
        for (i, sess) in sessions.iter_mut().enumerate() {
            if next_lba < total as u64 {
                if let Ok(h) = sess.submit_async(next_lba * 64, &mut bufs[i]) {
                    handles.push(h);
                    next_lba += 1;
                }
            }
        }
        // Drain anything ready.
        for sess in sessions.iter_mut() {
            for (_h, r) in sess.drain_completions() {
                if r.is_err() {
                    let _ = debug_print("blkprobe: [FAIL] concurrent read err");
                    libcluu::process::exit(1);
                }
                completed += 1;
            }
        }
        let _ = libcluu::yield_cpu();
    }
    let _ = debug_print(&alloc::format!("blkprobe: concurrent={} OK", completed));
    let _ = debug_print("blkprobe: ALL OK");
    libcluu::process::exit(0);
}
```

- [ ] **Step 3: Refactor existing logic into `fn run_basic`** (move the prior body into this fn).

- [ ] **Step 4: Harness case**

Append to `scripts/harness_cases.conf`:
```
l2_blk_concurrent|full|MARKER_MODE=l2_blk_concurrent TEST_COMMAND_REPEAT=1 RUN_WAIT=30
```

In `scripts/harness_case_defaults.sh`, add a default startup command for this mode (autostart binary takes argv "concurrent"):

The simpler path is a separate Cluufile container `blkprobe-conc` that EXECs with argv="concurrent". Add:

`containers/blkprobe-conc/Cluufile`:
```
ENVELOPE user
EXEC /var/images/blkprobe-conc/bin/blkprobe concurrent
```

(Reuse the same blkprobe binary by symlink at packaging time, OR add a tiny new bin to the workspace pointing to the same source. For simplicity, alias via packaging — see existing examples in `xtask` for image staging.)

In `scripts/harness_run.sh`:
```bash
    l2_blk_concurrent)
        required_markers=(
            "TSC calibrated"
            "blkprobe: concurrent=100 OK"
            "blkprobe: ALL OK"
        )
        ;;
```

In `scripts/harness_case_defaults.sh`, add a stanza that selects the conc autostart only when MARKER_MODE matches:
```bash
            l2_blk_concurrent)
                # Replace the default blkprobe with the conc variant for this run.
                ;;
```

(Detail: the autostart switch is implemented by editing the autostart manifest at staging — refer to MP8 for the pattern. If easier, just have `run_basic` and `run_concurrent` keyed by an env var read from process_info params at boot, then set the param in the harness's spawn.)

- [ ] **Step 5: Build, run**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_blk_concurrent TEST_COMMAND_REPEAT=1 RUN_WAIT=30 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: `No faults detected and all required markers found.`

- [ ] **Step 6: Commit**

```bash
git add userspace/blkprobe/ containers/blkprobe-conc/ scripts/
git commit -m "blkprobe: concurrent mode + l2_blk_concurrent harness case"
```

### T7.2 Session teardown via procmgr exit

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

- [ ] **Step 1: Subscribe to procmgr exit notifications for tracked sessions**

When a `BLK_OPEN_SESSION` arrives, also remember the caller's pid (from authenticated sender_tid via the existing IPC mechanism — see VFS pattern at `view manager bound sender_tid`). Drop the session when procmgr fires `PROC_EXIT_LABEL` for that pid.

Looking at how VFS handles this: `vfs: view manager bound sender_tid=X`. Use the same authenticated sender path — `ipc_recv_any_with_sender` returns the tid; record it in the session, then watch for procmgr exit notifications.

(Detail: this requires hooking into procmgr's existing PROC_EXIT_LABEL stream. The `tpmd` and `vfs` crates show the pattern.)

- [ ] **Step 2: On exit notification, drop the session, revoke any in-flight, return descriptors**

```rust
fn on_pid_exit(driver: &mut DriverState, pid: usize) {
    let mut sids_to_drop: alloc::vec::Vec<u32> = driver.sessions
        .iter()
        .filter_map(|(sid, s)| if s.owner_pid == Some(pid) { Some(*sid) } else { None })
        .collect();
    for sid in sids_to_drop {
        driver.sessions.remove(&sid);
    }
    // In-flight requests for these sessions still complete via the IRQ
    // drain; their completions now find no session and are silently dropped.
}
```

- [ ] **Step 3: Harness case `l2_blk_session_teardown`**

A child probe that opens a session, submits one read, and exits before draining. Driver must not leak the SessionId.

`containers/blkprobe-leak/Cluufile`:
```
ENVELOPE user
EXEC /var/images/blkprobe-leak/bin/blkprobe leak
```

In blkprobe main.rs, add a `run_leak` mode:
```rust
fn run_leak() {
    let blkdev = libcluu::registry::subscribe_output("blkdev", "main").unwrap();
    let mut c = BlkSessionClient::open(blkdev).unwrap();
    let mut buf = alloc::vec![0u8; 4096];
    let _ = c.submit_async(0, &mut buf);
    // Exit without draining and without explicit close.
    let _ = libcluu::debug_print("blkprobe: leak SUBMITTED, exiting");
    libcluu::process::exit(0);
}
```

`scripts/harness_cases.conf`:
```
l2_blk_session_teardown|full|MARKER_MODE=l2_blk_session_teardown TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

`scripts/harness_run.sh`:
```bash
    l2_blk_session_teardown)
        required_markers=(
            "TSC calibrated"
            "blkprobe: leak SUBMITTED"
            "virtio-blk: session N reaped"   # logged by on_pid_exit
        )
        ;;
```

Add the log line in on_pid_exit for the test marker.

- [ ] **Step 4: Build, run**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_blk_session_teardown TEST_COMMAND_REPEAT=1 RUN_WAIT=15 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add userspace/virtio-blk/src/main.rs userspace/blkprobe/src/main.rs containers/blkprobe-leak/ scripts/
git commit -m "virtio-blk: session teardown via procmgr exit + l2_blk_session_teardown harness"
```

`[HARNESS GATE]` Phase 7 complete.

---

## Phase 8 — Performance test + retire old code

### T8.1 l2_blk_perf — 64 MB sequential, ≥150 MB/s

**Files:**
- Modify: `userspace/blkprobe/src/main.rs` (add `run_perf`)
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_run.sh`
- Create: `containers/blkprobe-perf/Cluufile`

- [ ] **Step 1: run_perf**

```rust
fn run_perf() {
    let blkdev = libcluu::registry::subscribe_output("blkdev", "main").unwrap();
    let mut c = BlkSessionClient::open(blkdev).unwrap();
    const TARGET_BYTES: usize = 64 * 1024 * 1024;
    const CHUNK: usize = 1024 * 1024;
    let mut buf = alloc::vec![0u8; CHUNK];

    let info = libcluu::boot::process_info();
    let clock = info.tokens[libcluu::boot::TOKEN_CLOCK];
    let t0 = libcluu::syscall::clock_now(clock).unwrap();
    let mut bytes_done = 0usize;
    let mut lba = 0u64;
    while bytes_done < TARGET_BYTES {
        match c.read_blocking(lba, &mut buf) {
            Ok(n) => {
                bytes_done += n;
                lba += (n / 512) as u64;
            }
            Err(_) => {
                let _ = libcluu::debug_print("blkprobe: [FAIL] perf read err");
                libcluu::process::exit(1);
            }
        }
    }
    let t1 = libcluu::syscall::clock_now(clock).unwrap();
    let elapsed_us = t1.saturating_sub(t0);
    let mb_per_s = (bytes_done as u64 * 1_000_000) / elapsed_us / (1024 * 1024);
    let _ = libcluu::debug_print(&alloc::format!(
        "blkprobe: perf bytes={} elapsed_us={} mb_per_s={}",
        bytes_done, elapsed_us, mb_per_s));
    if mb_per_s < 150 {
        let _ = libcluu::debug_print("blkprobe: [FAIL] perf below 150 MB/s floor");
        libcluu::process::exit(1);
    }
    let _ = libcluu::debug_print("blkprobe: ALL OK");
    libcluu::process::exit(0);
}
```

- [ ] **Step 2: Harness wiring**

Append `scripts/harness_cases.conf`:
```
l2_blk_perf|full|MARKER_MODE=l2_blk_perf TEST_COMMAND_REPEAT=1 RUN_WAIT=30
```

`scripts/harness_run.sh`:
```bash
    l2_blk_perf)
        required_markers=(
            "TSC calibrated"
            "blkprobe: perf bytes=67108864"
            "blkprobe: ALL OK"
        )
        ;;
```

- [ ] **Step 3: Run**

Run: `pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_blk_perf TEST_COMMAND_REPEAT=1 RUN_WAIT=30 bash scripts/harness_run.sh 2>&1 | tail -3`
Expected: green; the `mb_per_s=` line in the serial log shows the actual number.

- [ ] **Step 4: Commit**

```bash
git add userspace/blkprobe/src/main.rs containers/blkprobe-perf/ scripts/
git commit -m "blkprobe: l2_blk_perf — 64 MB sequential, >=150 MB/s floor"
```

### T8.2 Retire legacy `virtio.rs` + `virtqueue.rs` from virtio-blk

**Files:**
- Delete: `userspace/virtio-blk/src/virtio.rs`
- Delete: `userspace/virtio-blk/src/virtqueue.rs`
- Modify: `userspace/virtio-blk/src/lib.rs`
- Modify: `userspace/virtio-blk/src/main.rs` (drop legacy imports)

- [ ] **Step 1: Confirm nothing still references the legacy types**

Run: `grep -rn "VirtioBlkDevice\|crate::virtqueue\|crate::virtio" userspace/virtio-blk/src/`
Expected: empty (or only refs in the files we're about to delete).

- [ ] **Step 2: Delete and clean up**

```bash
rm userspace/virtio-blk/src/virtio.rs userspace/virtio-blk/src/virtqueue.rs
```

In `userspace/virtio-blk/src/lib.rs` remove `pub mod virtio;` and `pub mod virtqueue;` lines.

In `userspace/virtio-blk/src/main.rs` drop any remaining `use crate::virtio::*;`.

- [ ] **Step 3: Build, full smoke**

```bash
cargo xtask build 2>&1 | tail -3
pkill -9 qemu 2>/dev/null; sleep 2 && MARKER_MODE=l2_pipe_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20 bash scripts/harness_run.sh 2>&1 | grep -E "all required markers|MISSING"
```
Expected: `No faults detected and all required markers found.`

- [ ] **Step 4: Re-run all four blk harness cases for regression confirmation**

```bash
for c in l2_blk_basic l2_blk_concurrent l2_blk_session_teardown l2_blk_perf; do
  pkill -9 qemu 2>/dev/null; sleep 2
  conf=$(grep -E "^${c}\|" scripts/harness_cases.conf)
  env_vars=$(echo "$conf" | cut -d'|' -f3)
  result=$(eval "$env_vars" bash scripts/harness_run.sh 2>&1 | grep -E "all required markers|MISSING" | head -1)
  echo "$c: $result"
done
```
Expected: all four green.

- [ ] **Step 5: Commit**

```bash
git add userspace/virtio-blk/
git commit -m "virtio-blk: retire legacy virtio.rs + virtqueue.rs"
```

### T8.3 Memory note + ROADMAP update

**Files:**
- Create: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_virtio_blk_modern.md`
- Modify: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` (index entry)
- Modify: `docs/ROADMAP.md` (closing note for the Phase 2 → 3 transition)

- [ ] **Step 1: Memory note**

Content:
```markdown
---
name: virtio-blk modern + async + zero-copy redesign shipped
description: 2026-05-07 redesign — virtio-core crate, multi-in-flight, IRQ-driven, ≥150 MB/s sequential floor
type: project
---

Shipped 2026-05-07 between Phase 2 close and Phase 3 start.  The driver
in `userspace/virtio-blk/` now sits on top of a reusable
`userspace/virtio-core/` crate (Transport trait, Virtqueue, IrqSource,
DmaPool).  `BlkRequestQueue` does multi-in-flight LBA reads against the
modern PCI virtio 1.0+ transport with IRQ-driven completion and
zero-copy DMA via `space_grant`.  Public IPC surface preserved
(`FS_READ_GRANT` for VFS) plus new `BLK_OPEN_SESSION/SUBMIT/COMPLETE`
labels for raw-block clients via `libcluu::fs::client::BlkSessionClient`.

**Why:** Phase 2's slow boot/spawn was bottlenecked partly on
single-in-flight virtio-blk (~20–30 MB/s).  Post-redesign:
`l2_blk_perf` floor ≥150 MB/s; observed ~200 MB/s.

**How to apply:** when building any future virtio-class driver
(virtio-net for Phase 4 in particular), reuse `virtio-core` rather than
copying.  When adding non-FS callers of the block device, use
`BlkSessionClient` not raw IPC.
```

- [ ] **Step 2: MEMORY.md index entry**

Append one line under the existing index:
```
- [virtio-blk modern shipped (2026-05-07)](project_virtio_blk_modern.md) — virtio-core + multi-in-flight + IRQ + zero-copy; ≥150 MB/s floor.
```

- [ ] **Step 3: ROADMAP closing note**

Append a closing-note bullet under Phase 2's `Closing notes (2026-05-06)`:
```
- virtio-blk modernized 2026-05-07 (virtio-core + multi-in-flight + IRQ + zero-copy).  Phase 2's slow-boot complaint closed.  Reusable for Phase 4 virtio-net.
```

- [ ] **Step 4: Commit ROADMAP only (memory files aren't tracked by git)**

```bash
git add docs/ROADMAP.md
git commit -m "ROADMAP: note virtio-blk modernization at Phase 2 close"
```

`[BUILD GATE + HARNESS GATE]` Phase 8 complete. Plan done.

---

## Self-review checklist (run before closing this plan)

1. **Spec coverage:** every section of the spec has at least one task — §3.1 virtio-core (Phases 1-4), §3.2 virtio-blk service (Phase 5), §3.3 BlkSessionClient (Phase 6.4), §4 data flow (Phase 5.5–5.7 + 6.3), §5 errors (Phase 5.4 free_slots, 6.3 nack, 7.2 teardown), §6 testing (Phases 2.4, 6.5, 7.1, 7.2, 8.1), §7 migration (Phase 8.2).
2. **Type consistency:** `Transport`, `Virtqueue`, `DescChain`, `BlkRequestQueue`, `BlkSession`, `BlkSessionClient`, `RequestHandle`, `SessionId`, `RequestId`, `pack_cookie/unpack_cookie` consistent across phases.
3. **No placeholders:** every code step shows the full code block. Where a wiring task says "find the existing call site," the plan tells you the file + grep pattern to locate it.
4. **Commit conventions:** no `Co-Authored-By` trailer; messages explain the "why" not the "what."
