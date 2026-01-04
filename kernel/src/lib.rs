//! CLUU Microkernel Library
//!
//! This module exists to allow unit testing of kernel components

#![no_std]
#![cfg_attr(test, allow(unused))]
#![feature(abi_x86_interrupt)]

extern crate alloc;

// Bootstrap allocator for early kernel initialization
// This is a simple bump allocator that never frees memory
// TODO Phase 8: Replace with proper heap allocator after PMM is initialized
#[global_allocator]
static ALLOCATOR: mm::bump::BumpAllocator = mm::bump::BumpAllocator;

// Module structure will be built out incrementally
pub mod bootboot;
pub mod error;
pub mod mm;
pub mod sched;
pub mod ipc;
pub mod cap;
pub mod syscall;

// Re-exports
pub use error::Error;
