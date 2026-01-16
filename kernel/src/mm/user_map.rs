//! Userspace Physical Memory Mapping
//!
//! Provides utilities for mapping physical memory regions into userspace
//! address spaces. Used for initrd mapping and sys_invoke Map operations.

use crate::error::Error;
use crate::mm::AddressSpace;

/// Map a physical memory region into a userspace address space (read-only)
///
/// This function maps consecutive physical frames into a userspace virtual
/// address range with the specified permissions.
///
/// # Arguments
///
/// * `space` - Target address space
/// * `virt_base` - Virtual address to map at (must be page-aligned)
/// * `phys_base` - Physical address to map (must be page-aligned)
/// * `size` - Size in bytes (will be rounded up to page boundary)
/// * `writable` - Whether pages should be writable
///
/// # Returns
///
/// Ok(()) on success, Error on failure
///
/// # Example
///
/// ```rust
/// // Map initrd at 2GB mark (read-only)
/// map_phys_to_userspace(
///     &mut init_space,
///     0x80000000,  // 2GB virtual
///     initrd_phys,
///     initrd_size,
///     false,       // read-only
/// )?;
/// ```
pub fn map_phys_to_userspace(
    space: &mut AddressSpace,
    virt_base: u64,
    phys_base: u64,
    size: u64,
    writable: bool,
) -> Result<(), Error> {
    use klibcluu::util::PAGE_SIZE;

    // Validate alignment
    if virt_base & 0xFFF != 0 {
        klibcluu::error("map_phys_to_userspace: virt_base not page-aligned");
        return Err(Error::InvalidArgument);
    }
    if phys_base & 0xFFF != 0 {
        klibcluu::error("map_phys_to_userspace: phys_base not page-aligned");
        return Err(Error::InvalidArgument);
    }

    // Round size up to page boundary
    let num_pages = size.div_ceil(PAGE_SIZE);
    let page_table_root = space.page_table_root;

    klibcluu::trace("Mapping phys region: 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", phys_base);
    klibcluu::trace(" -> virt 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, " (", virt_base);
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " pages)", num_pages);

    // Map each page
    for i in 0..num_pages {
        let virt_addr = virt_base + (i * PAGE_SIZE);
        let phys_addr = phys_base + (i * PAGE_SIZE);

        // Map page (user-accessible, non-executable, read-only or writable)
        unsafe {
            crate::elf::map_user_page(virt_addr, phys_addr, writable, false, page_table_root)
                .map_err(|_| Error::OutOfMemory)?;
        }
    }

    klibcluu::trace("Physical mapping complete");
    Ok(())
}

/// Unmap a physical memory region from userspace
///
/// # Arguments
///
/// * `space` - Target address space
/// * `virt_base` - Virtual address to unmap (must be page-aligned)
/// * `size` - Size in bytes (will be rounded up to page boundary)
///
/// # Returns
///
/// Ok(()) on success, Error on failure
pub fn unmap_phys_from_userspace(
    _space: &mut AddressSpace,
    virt_base: u64,
    size: u64,
) -> Result<(), Error> {
    use klibcluu::util::PAGE_SIZE;

    // Validate alignment
    if virt_base & 0xFFF != 0 {
        return Err(Error::InvalidArgument);
    }

    let num_pages = size.div_ceil(PAGE_SIZE);

    klibcluu::trace("Unmapping phys region: virt 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, " (", virt_base);
    klibcluu::log_dec(klibcluu::LogLevel::Trace, " pages)", num_pages);

    // TODO: Implement unmapping
    // For now, just log that it's not implemented
    klibcluu::warn("  TODO: unmap_phys_from_userspace not yet implemented");

    Ok(())
}
