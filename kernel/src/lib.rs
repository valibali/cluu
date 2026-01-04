//! CLUU Microkernel Library
//!
//! This module exists to allow unit testing of kernel components

#![no_std]
#![cfg_attr(test, allow(unused))]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

// Global allocator is defined in mm::heap
// (Previously was bump allocator, now using linked_list_allocator backed by PMM)

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
