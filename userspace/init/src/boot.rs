//! Boot snapshot helpers.
//!
//! This module isolates all direct reads from the boot info page. Keeping
//! these reads in one place makes it obvious which boot-time fields init
//! consumes and keeps other modules testable.

use libcluu::boot::{boot_info, INITRD_USER_BASE};
use libcluu::Result;

/// Minimal subset of boot info used by init.
///
/// Capturing these values early avoids repeated reads and clarifies which
/// kernel-provided parameters are in use.
#[derive(Copy, Clone)]
pub struct BootSnapshot {
    pub root_token: usize,
    pub initrd_size: usize,
    pub fb_phys: u64,
    pub fb_size: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_pitch: u32,
}

/// Read boot info and capture values init depends on.
///
/// This is intentionally narrow so the rest of init stays stable even if
/// the boot info layout grows or changes.
pub fn capture_boot_snapshot() -> Result<BootSnapshot> {
    let info = boot_info();
    Ok(BootSnapshot {
        root_token: info.root_token,
        initrd_size: info.initrd_size as usize,
        fb_phys: info.fb_phys,
        fb_size: info.fb_size,
        fb_width: info.fb_width,
        fb_height: info.fb_height,
        fb_pitch: info.fb_pitch,
    })
}

/// Map the initrd slice already present in the init address space.
///
/// The slice is required to load service binaries from the tar archive, so
/// we expose it as a stable reference for the wiring layer.
///
/// Safety: the kernel maps the initrd to INITRD_USER_BASE before init runs.
pub fn map_initrd_slice(initrd_size: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, initrd_size) }
}
