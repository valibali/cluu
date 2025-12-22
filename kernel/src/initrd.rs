/*
 * Initrd Access Module
 *
 * This module provides access to the initial ramdisk provided by BOOTBOOT.
 * The initrd is a TAR archive containing userspace binaries and data files.
 *
 * Usage:
 *   initrd::init();  // Initialize from BOOTBOOT
 *   let data = initrd::read_file("bin/hello")?;  // Read a file
 */

use crate::bootboot::bootboot;
use crate::fs::TarReader;
use x86_64::PhysAddr;
use spin::Mutex;

/// Global initrd instance
static INITRD: Mutex<Option<TarReader>> = Mutex::new(None);

/// Initialize the initrd from BOOTBOOT
///
/// This must be called early in kernel initialization, after memory management
/// is set up but before trying to access any initrd files.
pub fn init() {
    log::info!("Initializing initrd...");

    unsafe {
        let initrd_ptr = bootboot.initrd_ptr;
        let initrd_size = bootboot.initrd_size as usize;

        if initrd_ptr == 0 || initrd_size == 0 {
            log::warn!("No initrd provided by bootloader");
            return;
        }

        log::info!("Initrd at physical address: 0x{:x}", initrd_ptr);
        log::info!("Initrd size: {} bytes ({} KiB)", initrd_size, initrd_size / 1024);

        // Create slice from physical address
        // BOOTBOOT maps the entire physical memory to the higher half
        // So we can access it directly
        let initrd_data = core::slice::from_raw_parts(initrd_ptr as *const u8, initrd_size);

        // Create TAR reader
        let tar = TarReader::new(initrd_data);

        // List contents for debugging
        if let Err(e) = tar.list() {
            log::error!("Failed to list initrd contents: {}", e);
        }

        // Store globally
        *INITRD.lock() = Some(tar);
        log::info!("Initrd initialized successfully");
    }
}

/// Read a file from the initrd
///
/// # Arguments
/// * `path` - Path to the file (e.g., "bin/hello")
///
/// # Returns
/// A slice containing the file data, or an error
pub fn read_file(path: &str) -> Result<&'static [u8], &'static str> {
    let initrd = INITRD.lock();
    let tar = initrd.as_ref().ok_or("Initrd not initialized")?;

    // Find the file
    let entry = tar.find(path)?.ok_or("File not found in initrd")?;

    // Read file data
    tar.read_file(&entry)
}

/// Check if a file exists in the initrd
///
/// # Arguments
/// * `path` - Path to check
pub fn exists(path: &str) -> bool {
    let initrd = INITRD.lock();
    if let Some(tar) = initrd.as_ref() {
        if let Ok(Some(_)) = tar.find(path) {
            return true;
        }
    }
    false
}

/// List all files in the initrd (for debugging)
pub fn list() -> Result<(), &'static str> {
    let initrd = INITRD.lock();
    let tar = initrd.as_ref().ok_or("Initrd not initialized")?;
    tar.list()
}

/// Get initrd location and size
///
/// Returns the physical address and size of the initrd in memory.
/// This is useful for mapping the initrd into userspace processes
/// (e.g., the VFS server).
///
/// # Returns
/// (physical_address, size_in_bytes)
pub fn get_info() -> (PhysAddr, usize) {
    unsafe {
        let phys_addr = PhysAddr::new(bootboot.initrd_ptr);
        let size = bootboot.initrd_size as usize;
        (phys_addr, size)
    }
}
