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
