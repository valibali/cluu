//! Virtqueue implementation for virtio devices.
//!
//! A virtqueue is a ring buffer shared between driver and device for
//! passing buffers back and forth.

use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

/// Wrapper for raw pointers to make them Send/Sync.
/// Safety: The driver ensures single-threaded access to these pointers.
#[derive(Clone, Copy)]
struct SendPtr<T>(*mut T);

unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

impl<T> SendPtr<T> {
    fn new(ptr: *mut T) -> Self {
        Self(ptr)
    }

    fn as_ptr(&self) -> *mut T {
        self.0
    }
}

/// Virtqueue descriptor flags
pub const VIRTQ_DESC_F_NEXT: u16 = 1;     // Buffer continues via 'next' field
pub const VIRTQ_DESC_F_WRITE: u16 = 2;    // Buffer is device-writable (vs read-only)
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4; // Buffer contains a list of descriptors

/// Virtqueue descriptor - describes a buffer
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqDesc {
    /// Physical address of the buffer
    pub addr: u64,
    /// Length of the buffer
    pub len: u32,
    /// Descriptor flags
    pub flags: u16,
    /// Index of next descriptor in chain
    pub next: u16,
}

/// Virtqueue available ring header
#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    // Followed by ring[queue_size] of u16 descriptor indices
}

/// Virtqueue used ring element
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtqUsedElem {
    /// Index of the start of the used descriptor chain
    pub id: u32,
    /// Total bytes written to the descriptor chain
    pub len: u32,
}

/// Virtqueue used ring header
#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    // Followed by ring[queue_size] of VirtqUsedElem
}

/// A virtqueue with all its ring structures.
pub struct Virtqueue {
    /// Queue size (number of descriptors)
    pub size: u16,
    /// Index of next descriptor to allocate
    pub free_head: u16,
    /// Number of free descriptors
    pub num_free: u16,
    /// Last used index we've processed
    pub last_used_idx: u16,

    /// Descriptor table base (physical address)
    pub desc_phys: u64,
    /// Available ring base (physical address)
    pub avail_phys: u64,
    /// Used ring base (physical address)
    pub used_phys: u64,

    /// Descriptor table (virtual pointer)
    desc: SendPtr<VirtqDesc>,
    /// Available ring (virtual pointer)
    avail: SendPtr<VirtqAvail>,
    /// Used ring (virtual pointer)
    used: SendPtr<VirtqUsed>,

    /// Track which descriptors are in use
    pub desc_in_use: Vec<bool>,
}

impl Virtqueue {
    /// Calculate the total size needed for a virtqueue with given queue_size.
    pub fn size_bytes(queue_size: u16) -> usize {
        let desc_size = (queue_size as usize) * core::mem::size_of::<VirtqDesc>();
        let avail_size = 4 + (queue_size as usize) * 2 + 2; // flags, idx, ring[], used_event
        let used_size = 4 + (queue_size as usize) * 8 + 2;  // flags, idx, ring[], avail_event

        // Align avail to 2, used to 4096
        let desc_avail = desc_size + avail_size;
        let aligned = (desc_avail + 4095) & !4095;
        aligned + used_size
    }

    /// Create a new virtqueue from pre-allocated memory.
    ///
    /// # Safety
    ///
    /// The provided memory must be:
    /// - Page-aligned
    /// - At least `size_bytes(queue_size)` bytes
    /// - Zeroed
    pub unsafe fn new(
        queue_size: u16,
        virt_base: usize,
        phys_base: u64,
    ) -> Self {
        let desc_size = (queue_size as usize) * core::mem::size_of::<VirtqDesc>();
        let avail_size = 4 + (queue_size as usize) * 2 + 2;
        let desc_avail = desc_size + avail_size;
        let used_offset = (desc_avail + 4095) & !4095;

        let desc = virt_base as *mut VirtqDesc;
        let avail = (virt_base + desc_size) as *mut VirtqAvail;
        let used = (virt_base + used_offset) as *mut VirtqUsed;

        // Initialize free descriptor chain
        let desc_slice = core::slice::from_raw_parts_mut(desc, queue_size as usize);
        for i in 0..queue_size {
            desc_slice[i as usize].next = if i + 1 < queue_size { i + 1 } else { 0 };
        }

        Self {
            size: queue_size,
            free_head: 0,
            num_free: queue_size,
            last_used_idx: 0,
            desc_phys: phys_base,
            avail_phys: phys_base + desc_size as u64,
            used_phys: phys_base + used_offset as u64,
            desc: SendPtr::new(desc),
            avail: SendPtr::new(avail),
            used: SendPtr::new(used),
            desc_in_use: alloc::vec![false; queue_size as usize],
        }
    }

    /// Get descriptor pointer for direct access.
    pub fn desc_ptr(&self) -> *mut VirtqDesc {
        self.desc.as_ptr()
    }

    /// Allocate a descriptor from the free list.
    pub fn alloc_desc(&mut self) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }

        let idx = self.free_head;
        self.desc_in_use[idx as usize] = true;

        unsafe {
            let desc = &*self.desc.as_ptr().add(idx as usize);
            self.free_head = desc.next;
        }

        self.num_free -= 1;
        Some(idx)
    }

    /// Free a descriptor back to the free list.
    pub fn free_desc(&mut self, idx: u16) {
        self.desc_in_use[idx as usize] = false;

        unsafe {
            let desc = &mut *self.desc.as_ptr().add(idx as usize);
            desc.next = self.free_head;
        }

        self.free_head = idx;
        self.num_free += 1;
    }

    /// Free a chain of descriptors.
    pub fn free_chain(&mut self, head: u16) {
        let mut idx = head;
        loop {
            let desc = unsafe { &*self.desc.as_ptr().add(idx as usize) };
            let flags = desc.flags;
            let next = desc.next;

            self.free_desc(idx);

            if flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            idx = next;
        }
    }

    /// Add a buffer chain to the available ring.
    pub fn add_to_avail(&mut self, head: u16) {
        fence(Ordering::SeqCst);

        unsafe {
            let avail = &mut *self.avail.as_ptr();
            let idx = avail.idx;
            let ring_ptr = (self.avail.as_ptr() as *mut u8).add(4) as *mut u16;
            let ring_idx = (idx % self.size) as usize;
            ring_ptr.add(ring_idx).write_volatile(head);

            fence(Ordering::SeqCst);
            avail.idx = idx.wrapping_add(1);
        }

        fence(Ordering::SeqCst);
    }

    /// Check if there are used buffers to process.
    pub fn has_used(&self) -> bool {
        fence(Ordering::SeqCst);
        let used_idx = unsafe { (*self.used.as_ptr()).idx };
        used_idx != self.last_used_idx
    }

    /// Pop a used buffer from the used ring.
    ///
    /// Returns (descriptor head index, bytes written) or None if empty.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);

        let used_idx = unsafe { (*self.used.as_ptr()).idx };
        if used_idx == self.last_used_idx {
            return None;
        }

        let ring_idx = (self.last_used_idx % self.size) as usize;
        let elem_ptr = unsafe {
            (self.used.as_ptr() as *const u8).add(4) as *const VirtqUsedElem
        };
        let elem = unsafe { elem_ptr.add(ring_idx).read_volatile() };

        self.last_used_idx = self.last_used_idx.wrapping_add(1);

        fence(Ordering::SeqCst);

        Some((elem.id as u16, elem.len))
    }
}
