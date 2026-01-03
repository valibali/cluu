//! CLUU Microkernel Library
//!
//! This module exists to allow unit testing of kernel components

#![no_std]
#![cfg_attr(test, allow(unused))]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};

// Dummy allocator for Phase 7b (allocations will panic)
// TODO Phase 8: Replace with proper bump/slab allocator
struct DummyAllocator;

unsafe impl GlobalAlloc for DummyAllocator {
    unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
        // For now, panic on allocation attempts
        // This is okay for Phase 7b since we're not using heap yet
        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Nothing to do for dummy allocator
    }
}

#[global_allocator]
static ALLOCATOR: DummyAllocator = DummyAllocator;

// Module structure will be built out incrementally
pub mod error;
pub mod mm;
pub mod sched;
pub mod ipc;
pub mod cap;
pub mod syscall;

// Re-exports
pub use error::Error;
