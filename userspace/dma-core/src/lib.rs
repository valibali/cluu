#![no_std]

extern crate alloc;

pub mod dma;

pub use dma::{DmaPool, DmaRegion};
