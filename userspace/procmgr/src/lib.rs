//! Process Manager Library
//!
//! This library provides the core functionality for process management,
//! including ELF parsing and loading.

#![no_std]

pub mod elf;

// Re-export main types
pub use elf::{ElfFile, LoadableSegment};
