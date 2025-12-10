/*
 * Memory Management
 *
 * High-level module that ties together:
 *  - Physical frame allocator (phys)
 *  - Paging / virtual memory manager (paging)
 *  - Kernel heap (heap)
 */

pub mod heap;
pub mod paging;
pub mod phys;

use crate::bootboot::BOOTBOOT;

/// Physical frame representation (4 KiB)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PhysFrame(u64);

impl PhysFrame {
    pub const SIZE: u64 = 4096;

    pub fn containing_address(addr: u64) -> Self {
        Self(addr & !0xfff)
    }

    pub fn start_address(&self) -> u64 {
        self.0
    }

    pub fn end_address(&self) -> u64 {
        self.0 + Self::SIZE - 1
    }
}

/// Top-level memory initialization:
///  1. Physical frame allocator from BOOTBOOT memory map
///  2. Paging mapper
///  3. Kernel heap
pub fn init(bootboot_ptr: *const BOOTBOOT) {
    log::info!("Initializing memory management...");

    // 1) Physical frames
    phys::init_from_bootboot(bootboot_ptr);

    // 2) Paging
    paging::init();

    // 3) Heap
    heap::init().expect("Failed to initialize kernel heap");

    let (used, total) = phys::get_stats();
    log::info!(
        "Physical memory: used frames = {}, total frames = {}",
        used,
        total
    );
}
