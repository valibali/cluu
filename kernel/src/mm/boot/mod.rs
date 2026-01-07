//! Bootloader Abstraction Layer
//!
//! This module provides a bootloader-agnostic interface for the memory
//! management subsystem. Different bootloaders implement the BootInfoProvider
//! trait to provide boot-time information.
//!
//! # Supported Bootloaders
//!
//! - BOOTBOOT (bootboot.rs)
//!
//! # Future Support
//!
//! - Multiboot2
//! - UEFI
//! - Limine
//! - Stivale2

// Boot information abstraction
pub mod info;

// Bootloader-specific adapters
pub mod bootboot;

// Re-export commonly used types
pub use bootboot::BootbootAdapter;
pub use info::{BootInfoProvider, BootMemoryRegion, MemoryRegionType};
