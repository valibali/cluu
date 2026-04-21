//! Userspace Pointer Validation
//!
//! This module provides utilities for validating userspace pointers
//! before accessing them from the kernel.
//!
//! # Security
//!
//! All userspace pointers must be validated before use to prevent:
//! - Kernel memory disclosure (reading kernel addresses)
//! - Kernel memory corruption (writing to kernel addresses)
//! - NULL pointer dereferences
//! - Integer overflows in size calculations

use crate::error::Error;
use crate::mm::vmm::pte_flags;
use klibcluu;
use x86_64::{PhysAddr, VirtAddr};

/// Maximum userspace address on x86_64 (canonical address limit)
///
/// On x86_64, userspace uses the lower half of the address space:
/// - Userspace: 0x0000_0000_0000_0000 to 0x0000_7FFF_FFFF_FFFF
/// - Kernel:    0xFFFF_8000_0000_0000 to 0xFFFF_FFFF_FFFF_FFFF
///
/// Any address >= USERSPACE_MAX is kernel space and must be rejected.
pub const USERSPACE_MAX: usize = 0x0000_8000_0000_0000;

/// Maximum size for debug print messages (4KB)
pub const MAX_DEBUG_PRINT_SIZE: usize = 4096;

/// Validate that a pointer points to userspace memory
///
/// # Arguments
///
/// - `ptr`: The pointer to validate
///
/// # Returns
///
/// - `Ok(())`: Pointer is in userspace range
/// - `Err(Error::InvalidAddress)`: Pointer is NULL or in kernel space
///
/// # Security
///
/// This prevents kernel memory disclosure/corruption by rejecting:
/// - NULL pointers (0x0)
/// - Kernel addresses (>= 0x0000_8000_0000_0000)
pub fn validate_user_ptr(ptr: usize) -> Result<(), Error> {
    // Reject NULL pointers
    if ptr == 0 {
        klibcluu::warn("userptr: null pointer");
        return Err(Error::InvalidAddress);
    }

    // Reject kernel addresses
    if ptr >= USERSPACE_MAX {
        klibcluu::warn("userptr: pointer in kernel space");
        return Err(Error::InvalidAddress);
    }

    Ok(())
}

/// Validate that a buffer is entirely in userspace
///
/// # Arguments
///
/// - `ptr`: Start address of the buffer
/// - `len`: Length of the buffer in bytes
///
/// # Returns
///
/// - `Ok(())`: Buffer is entirely in userspace
/// - `Err(Error::InvalidAddress)`: Buffer is NULL, in kernel space, or wraps around
/// - `Err(Error::InvalidParameter)`: Length is zero or too large
///
/// # Security
///
/// This prevents:
/// - Integer overflow in end address calculation
/// - Buffers that span userspace/kernel boundary
/// - Zero-length buffers (often indicates programming error)
pub fn validate_user_buffer(ptr: usize, len: usize) -> Result<(), Error> {
    // Reject zero-length buffers
    if len == 0 {
        klibcluu::warn("userptr: buffer length zero");
        return Err(Error::InvalidParameter);
    }

    // Validate start pointer
    validate_user_ptr(ptr)?;

    // Check for integer overflow in end address calculation
    let end = ptr.checked_add(len).ok_or(Error::InvalidAddress)?;

    // Ensure end address is still in userspace
    // Note: end is one past the last byte, so it can equal USERSPACE_MAX
    if end > USERSPACE_MAX {
        klibcluu::warn("userptr: buffer exceeds userspace boundary");
        return Err(Error::InvalidAddress);
    }

    Ok(())
}

/// Ensure every page covered by the buffer is mapped in the provided page table.
pub fn ensure_pages_mapped(ptr: usize, len: usize, page_table_root: PhysAddr) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }

    let mut offset = 0usize;
    while offset < len {
        let addr = ptr + offset;
        let page_offset = addr & 0xFFF;
        let page_start = VirtAddr::new((addr - page_offset) as u64);

        match crate::elf::translate_vaddr_with_flags(page_table_root, page_start) {
            Some((_, flags)) => {
                if (flags & pte_flags::USER) == 0 {
                    klibcluu::warn("userptr: page not user-accessible");
                    return Err(Error::InvalidAddress);
                }
            }
            None => {
                klibcluu::warn("userptr: page not present");
                return Err(Error::InvalidAddress);
            }
        }

        let remaining_in_page = 4096 - page_offset;
        offset += remaining_in_page.min(len - offset);
    }

    Ok(())
}

/// Copy bytes from user buffer into kernel destination via physmap.
///
/// # Safety
/// Caller must ensure `dst` points to a valid kernel buffer for `len` bytes.
pub unsafe fn copy_from_user(
    dst: *mut u8,
    src: usize,
    len: usize,
    page_table_root: PhysAddr,
) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }

    let mut offset = 0usize;
    while offset < len {
        let addr = src + offset;
        let page_offset = addr & 0xFFF;
        let page_start = VirtAddr::new((addr - page_offset) as u64);

        let (phys, flags) = crate::elf::translate_vaddr_with_flags(page_table_root, page_start)
            .ok_or_else(|| {
                klibcluu::warn("userptr: copy_from_user page not present");
                Error::InvalidAddress
            })?;

        if (flags & pte_flags::USER) == 0 {
            klibcluu::warn("userptr: copy_from_user page not user-accessible");
            return Err(Error::InvalidAddress);
        }

        let bytes_in_page = core::cmp::min(4096 - page_offset, len - offset);
        let phys_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys.as_u64()) };
        let src_ptr = (phys_virt + page_offset as u64) as *const u8;
        core::ptr::copy_nonoverlapping(src_ptr, dst.add(offset), bytes_in_page);

        offset += bytes_in_page;
    }

    Ok(())
}

/// Copy bytes from kernel source into a user buffer via physmap.
///
/// # Safety
/// Caller must ensure `src` points to `len` readable bytes.
pub unsafe fn copy_to_user(
    dst: usize,
    src: *const u8,
    len: usize,
    page_table_root: PhysAddr,
) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }

    let mut offset = 0usize;
    while offset < len {
        let addr = dst + offset;
        let page_offset = addr & 0xFFF;
        let page_start = VirtAddr::new((addr - page_offset) as u64);

        let (phys, flags) = crate::elf::translate_vaddr_with_flags(page_table_root, page_start)
            .ok_or_else(|| {
                klibcluu::warn("userptr: copy_to_user page not present");
                Error::InvalidAddress
            })?;

        if (flags & pte_flags::USER) == 0 {
            klibcluu::warn("userptr: copy_to_user page not user-accessible");
            return Err(Error::InvalidAddress);
        }

        let bytes_in_page = core::cmp::min(4096 - page_offset, len - offset);
        let phys_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(phys.as_u64()) };
        let dst_ptr = (phys_virt + page_offset as u64) as *mut u8;
        core::ptr::copy_nonoverlapping(src.add(offset), dst_ptr, bytes_in_page);

        offset += bytes_in_page;
    }

    Ok(())
}

/// Read a byte slice from userspace
///
/// # Arguments
///
/// - `ptr`: Userspace pointer to the data
/// - `len`: Length of the data in bytes
///
/// # Returns
///
/// - `Ok(&[u8])`: Reference to the userspace data
/// - `Err(Error)`: Invalid pointer or length
///
/// # Safety
///
/// This function validates the pointer and length, then creates a slice.
/// However, it assumes the pointer is valid within the userspace range.
/// In a full implementation, this would:
/// - Check that all pages are mapped and accessible
/// - Handle page faults properly
/// - Verify userspace has read permission
///
/// For now, we use an unsafe cast after validation.
#[cfg(test)]
pub fn read_user_buffer(ptr: usize, len: usize) -> Result<&'static [u8], Error> {
    // Validate buffer
    validate_user_buffer(ptr, len)?;

    // TODO: Full implementation should:
    // 1. Check page table entries for all pages in range
    // 2. Verify pages are present and user-accessible
    // 3. Handle page faults if pages are swapped out
    // 4. Ensure pages don't get unmapped during access (pin pages)

    // For now, create slice directly (unsafe but validated)
    // SAFETY: We've validated the address range is in userspace.
    // In a real kernel, we would also need to ensure:
    // - Pages are mapped
    // - Pages have user read permission
    // - Pages won't be unmapped during access
    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };

    Ok(slice)
}

/// Read a string from userspace
///
/// # Arguments
///
/// - `ptr`: Userspace pointer to the string
/// - `len`: Length of the string in bytes
///
/// # Returns
///
/// - `Ok(&str)`: Valid UTF-8 string
/// - `Err(Error)`: Invalid pointer, length, or non-UTF-8 data
///
/// # Security
///
/// Validates that:
/// - Pointer is in userspace
/// - Length doesn't cause overflow
/// - Data is valid UTF-8
#[cfg(test)]
pub fn read_user_string(ptr: usize, len: usize) -> Result<&'static str, Error> {
    // Read bytes from userspace
    let bytes = read_user_buffer(ptr, len)?;

    // Validate UTF-8
    core::str::from_utf8(bytes).map_err(|_| Error::InvalidParameter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_user_ptr_null() {
        assert_eq!(validate_user_ptr(0), Err(Error::InvalidAddress));
    }

    #[test]
    fn test_validate_user_ptr_valid() {
        assert_eq!(validate_user_ptr(0x1000), Ok(()));
        assert_eq!(validate_user_ptr(0x0000_7FFF_FFFF_FFFF), Ok(()));
    }

    #[test]
    fn test_validate_user_ptr_kernel() {
        assert_eq!(validate_user_ptr(USERSPACE_MAX), Err(Error::InvalidAddress));
        assert_eq!(
            validate_user_ptr(0xFFFF_8000_0000_0000),
            Err(Error::InvalidAddress)
        );
        assert_eq!(
            validate_user_ptr(0xFFFF_FFFF_FFFF_FFFF),
            Err(Error::InvalidAddress)
        );
    }

    #[test]
    fn test_validate_user_buffer_zero_len() {
        assert_eq!(
            validate_user_buffer(0x1000, 0),
            Err(Error::InvalidParameter)
        );
    }

    #[test]
    fn test_validate_user_buffer_valid() {
        assert_eq!(validate_user_buffer(0x1000, 100), Ok(()));
        assert_eq!(validate_user_buffer(0x1000, 4096), Ok(()));
    }

    #[test]
    fn test_validate_user_buffer_overflow() {
        // Would overflow past USERSPACE_MAX
        assert_eq!(
            validate_user_buffer(0x0000_7FFF_FFFF_F000, 0x2000),
            Err(Error::InvalidAddress)
        );

        // Would overflow usize
        assert_eq!(
            validate_user_buffer(usize::MAX - 100, 200),
            Err(Error::InvalidAddress)
        );
    }

    #[test]
    fn test_validate_user_buffer_kernel_range() {
        // Start in kernel space
        assert_eq!(
            validate_user_buffer(USERSPACE_MAX, 100),
            Err(Error::InvalidAddress)
        );

        // Start in userspace but end in kernel space
        assert_eq!(
            validate_user_buffer(USERSPACE_MAX - 50, 100),
            Err(Error::InvalidAddress)
        );
    }
}
