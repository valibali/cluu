/*
 * Shared Memory Management
 *
 * This module implements shared memory regions that can be mapped into
 * multiple address spaces for efficient inter-process communication.
 *
 * Design:
 * - Shared memory regions are backed by physical pages
 * - Multiple processes can map the same physical pages
 * - Reference counted for automatic cleanup
 * - Permissions enforced per-mapping (read/write)
 *
 * Use Cases:
 * - Bulk data transfer between processes (e.g., VFS server)
 * - Shared buffers for IPC
 * - Memory-mapped files (future)
 */

use crate::memory::{PhysFrame, phys};
use crate::scheduler::process::ProcessId;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::PhysAddr;

/// Shared memory region identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShmemId(pub usize);

impl core::fmt::Display for ShmemId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Shmem({})", self.0)
    }
}

/// Shared memory permissions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShmemPermissions {
    pub read: bool,
    pub write: bool,
}

impl ShmemPermissions {
    pub const READ: u32 = 0x1;
    pub const WRITE: u32 = 0x2;
    pub const READ_WRITE: u32 = Self::READ | Self::WRITE;

    pub fn from_flags(flags: u32) -> Self {
        Self {
            read: (flags & Self::READ) != 0,
            write: (flags & Self::WRITE) != 0,
        }
    }

    pub fn to_page_flags(&self) -> x86_64::structures::paging::PageTableFlags {
        use x86_64::structures::paging::PageTableFlags;

        let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if self.write {
            flags |= PageTableFlags::WRITABLE;
        }
        flags
    }
}

/// Shared memory region
///
/// Represents a region of physical memory that can be mapped into
/// multiple address spaces.
pub struct SharedMemoryRegion {
    pub id: ShmemId,
    pub size: usize,
    pub physical_frames: Vec<PhysFrame>,
    pub owner: ProcessId,
    pub permissions: ShmemPermissions,
    pub ref_count: usize,
    pub marked_for_deletion: bool,
    pub owned: bool,  // If true, free frames on drop; if false, don't free (e.g., for initrd)
}

impl SharedMemoryRegion {
    /// Create a new shared memory region
    ///
    /// Allocates physical frames to back the region.
    ///
    /// # Arguments
    /// * `id` - Unique identifier for this region
    /// * `size` - Size in bytes (will be rounded up to page boundary)
    /// * `owner` - Process that created this region
    /// * `permissions` - Default permissions for the region
    ///
    /// # Returns
    /// SharedMemoryRegion or error if allocation fails
    pub fn new(
        id: ShmemId,
        size: usize,
        owner: ProcessId,
        permissions: ShmemPermissions,
    ) -> Result<Self, &'static str> {
        // Round size up to page boundary
        let size_pages = (size + 4095) / 4096;
        let actual_size = size_pages * 4096;

        // Allocate physical frames
        let mut physical_frames = Vec::new();
        for _ in 0..size_pages {
            match phys::alloc_frame() {
                Some(frame) => physical_frames.push(frame),
                None => {
                    // Allocation failed - free already allocated frames
                    for frame in physical_frames {
                        phys::free_frame(frame);
                    }
                    return Err("Out of physical memory");
                }
            }
        }

        Ok(Self {
            id,
            size: actual_size,
            physical_frames,
            owner,
            permissions,
            ref_count: 0,
            marked_for_deletion: false,
            owned: true,  // Frames allocated by us, we own them
        })
    }

    /// Increment reference count
    pub fn add_ref(&mut self) {
        self.ref_count += 1;
    }

    /// Decrement reference count
    ///
    /// Returns true if ref_count reached zero and region should be freed
    pub fn remove_ref(&mut self) -> bool {
        if self.ref_count > 0 {
            self.ref_count -= 1;
        }
        self.ref_count == 0 && self.marked_for_deletion
    }
}

impl Drop for SharedMemoryRegion {
    fn drop(&mut self) {
        // Only free frames if we own them (not for wrapped existing memory like initrd)
        if self.owned {
            for frame in &self.physical_frames {
                phys::free_frame(*frame);
            }
            log::debug!("Freed {} frames for shared memory region {}",
                       self.physical_frames.len(), self.id.0);
        } else {
            log::debug!("Dropped non-owned shared memory region {} ({} frames)",
                       self.id.0, self.physical_frames.len());
        }
    }
}

/// Global shared memory registry
static SHMEM_REGISTRY: Mutex<Option<BTreeMap<ShmemId, SharedMemoryRegion>>> = Mutex::new(None);

/// Next shared memory ID to allocate
static NEXT_SHMEM_ID: AtomicUsize = AtomicUsize::new(1);

/// Shared memory errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmemError {
    NotInitialized,
    OutOfMemory,
    InvalidId,
    InvalidSize,
    InvalidPermissions,
    NotOwner,
    AlreadyMapped,
    NotMapped,
}

impl core::fmt::Display for ShmemError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShmemError::NotInitialized => write!(f, "Shared memory not initialized"),
            ShmemError::OutOfMemory => write!(f, "Out of memory"),
            ShmemError::InvalidId => write!(f, "Invalid shared memory ID"),
            ShmemError::InvalidSize => write!(f, "Invalid size"),
            ShmemError::InvalidPermissions => write!(f, "Invalid permissions"),
            ShmemError::NotOwner => write!(f, "Not owner of shared memory"),
            ShmemError::AlreadyMapped => write!(f, "Already mapped"),
            ShmemError::NotMapped => write!(f, "Not mapped"),
        }
    }
}

/// Initialize shared memory subsystem
pub fn init() {
    *SHMEM_REGISTRY.lock() = Some(BTreeMap::new());
    log::info!("Shared memory subsystem initialized");
}

/// Create a new shared memory region
///
/// # Arguments
/// * `size` - Size in bytes (rounded up to page boundary)
/// * `owner` - Process creating the region
/// * `permissions` - Access permissions
///
/// # Returns
/// ShmemId on success, error otherwise
pub fn shmem_create(
    size: usize,
    owner: ProcessId,
    permissions: ShmemPermissions,
) -> Result<ShmemId, ShmemError> {
    if size == 0 || size > 16 * 1024 * 1024 {
        // Max 16MB shared memory regions
        return Err(ShmemError::InvalidSize);
    }

    // Allocate ID
    let id = ShmemId(NEXT_SHMEM_ID.fetch_add(1, Ordering::SeqCst));

    // Create region
    let region = SharedMemoryRegion::new(id, size, owner, permissions)
        .map_err(|_| ShmemError::OutOfMemory)?;

    // Store in registry
    let mut registry = SHMEM_REGISTRY.lock();
    let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;

    log::debug!("Created shared memory region {} ({} bytes, {} pages)",
               id.0, region.size, region.physical_frames.len());

    map.insert(id, region);

    Ok(id)
}

/// Create shared memory region from existing physical memory
///
/// This wraps an existing physical memory range (like initrd) as a shared memory region
/// without allocating new frames. Used to give userspace servers access to kernel-provided data.
///
/// The frames will NOT be freed when the region is destroyed (owned=false).
///
/// # Arguments
/// * `phys_addr` - Starting physical address (must be page-aligned)
/// * `size` - Size in bytes (will be rounded up to page boundary)
/// * `owner` - Process that will own this region (usually kernel PID 0)
/// * `permissions` - Default permissions for the region
///
/// # Returns
/// Shared memory ID on success, or error
pub fn shmem_create_from_phys(
    phys_addr: PhysAddr,
    size: usize,
    owner: ProcessId,
    permissions: ShmemPermissions,
) -> Result<ShmemId, ShmemError> {
    if size == 0 {
        return Err(ShmemError::InvalidSize);
    }

    // Check that physical address is page-aligned
    if phys_addr.as_u64() % 4096 != 0 {
        return Err(ShmemError::InvalidSize);
    }

    // Round size up to page boundary
    let size_pages = (size + 4095) / 4096;
    let actual_size = size_pages * 4096;

    // Create frame list from physical address range
    let mut physical_frames = Vec::new();
    for i in 0..size_pages {
        let frame_phys_addr = phys_addr.as_u64() + (i * 4096) as u64;
        let frame = PhysFrame::containing_address(frame_phys_addr);
        physical_frames.push(frame);
    }

    // Allocate ID
    let id = ShmemId(NEXT_SHMEM_ID.fetch_add(1, Ordering::SeqCst));

    // Create region (non-owned, won't free frames on drop)
    let region = SharedMemoryRegion {
        id,
        size: actual_size,
        physical_frames,
        owner,
        permissions,
        ref_count: 0,
        marked_for_deletion: false,
        owned: false,  // Don't free these frames!
    };

    // Store in registry
    let mut registry = SHMEM_REGISTRY.lock();
    let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;

    log::info!("Created non-owned shared memory region {} from phys 0x{:x} ({} bytes, {} pages)",
               id.0, phys_addr.as_u64(), region.size, region.physical_frames.len());

    map.insert(id, region);

    Ok(id)
}

/// Map shared memory into a process's address space
///
/// # Arguments
/// * `shmem_id` - Shared memory region to map
/// * `process_id` - Process to map into
/// * `hint_addr` - Desired virtual address (0 = kernel chooses)
/// * `permissions` - Mapping permissions (must be subset of region permissions)
///
/// # Returns
/// Virtual address where region was mapped
pub fn shmem_map(
    shmem_id: ShmemId,
    process_id: ProcessId,
    hint_addr: u64,
    permissions: ShmemPermissions,
) -> Result<u64, ShmemError> {
    // Get region from registry
    let (frames, _size, _region_perms) = {
        let mut registry = SHMEM_REGISTRY.lock();
        let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;
        let region = map.get_mut(&shmem_id).ok_or(ShmemError::InvalidId)?;

        // Check permissions are subset of region permissions
        if (permissions.read && !region.permissions.read) ||
           (permissions.write && !region.permissions.write) {
            return Err(ShmemError::InvalidPermissions);
        }

        // Increment reference count
        region.add_ref();

        (region.physical_frames.clone(), region.size, region.permissions)
    };

    // Choose virtual address
    // TODO: Properly manage virtual address space allocation
    // For now, use a simple fixed range for shared memory
    let virt_addr = if hint_addr != 0 && hint_addr >= 0x400000000 {
        hint_addr
    } else {
        // Default shared memory region: 0x400000000 - 0x500000000
        0x400000000 + (shmem_id.0 as u64 * 0x10000000)
    };

    // Get kernel CR3 before entering with_process_mut to avoid deadlock
    let kernel_cr3 = crate::memory::paging::get_kernel_cr3();

    // Map pages into process address space using batch operation
    crate::scheduler::ProcessManager::with_mut(process_id, |process| {
        let page_flags = permissions.to_page_flags();
        let page_table_root = process.address_space.page_table_root;

        // Prepare batch mappings
        let mappings: Vec<_> = frames.iter().enumerate().map(|(i, frame)| {
            let page_virt = virt_addr + (i as u64 * 4096);
            let page_phys = frame.start_address();
            (
                x86_64::VirtAddr::new(page_virt),
                x86_64::PhysAddr::new(page_phys),
                page_flags,
            )
        }).collect();

        // Map all pages in a single batch
        if let Err(e) = crate::memory::paging::map_pages_batch_in_table(
            page_table_root,
            &mappings,
            kernel_cr3,
        ) {
            log::error!("Failed to map shared memory pages: {:?}", e);
            // TODO: Unmap already-mapped pages on error
            return Err(ShmemError::OutOfMemory);
        }

        Ok(virt_addr)
    }).ok_or(ShmemError::InvalidId)?
}

/// Unmap shared memory from a process's address space
///
/// # Arguments
/// * `addr` - Virtual address where region is mapped
/// * `process_id` - Process to unmap from
///
/// # Returns
/// Ok if unmapped successfully
pub fn shmem_unmap(addr: u64, _process_id: ProcessId) -> Result<(), ShmemError> {
    // TODO: Track which shmem regions are mapped at which addresses
    // For now, this is a simplified implementation

    // Find the shmem region that is mapped at this address
    let shmem_id = {
        let registry = SHMEM_REGISTRY.lock();
        let _map = registry.as_ref().ok_or(ShmemError::NotInitialized)?;

        // For now, deduce shmem_id from address
        // addr = 0x400000000 + (shmem_id * 0x10000000)
        if addr < 0x400000000 || addr >= 0x500000000 {
            return Err(ShmemError::NotMapped);
        }

        let id = ((addr - 0x400000000) / 0x10000000) as usize;
        ShmemId(id)
    };

    // Unmap from process address space
    let should_delete = {
        // Get region info and decrement reference count
        let mut registry = SHMEM_REGISTRY.lock();
        let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;
        let region = map.get_mut(&shmem_id).ok_or(ShmemError::InvalidId)?;

        // TODO: Unmap pages from process page table
        // For now, we skip the actual unmapping - pages will be cleaned up
        // when the process exits and its entire address space is destroyed.
        // To implement proper unmapping, we need an unmap_page_in_table function
        // similar to map_page_in_table.

        // Decrement reference count
        region.remove_ref()
    };

    // Delete region if ref count reached zero and marked for deletion
    if should_delete {
        let mut registry = SHMEM_REGISTRY.lock();
        let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;
        map.remove(&shmem_id);
        log::debug!("Deleted shared memory region {} (ref count reached zero)", shmem_id.0);
    }

    Ok(())
}

/// Destroy a shared memory region
///
/// Marks the region for deletion. It will be freed when the last mapping is removed.
///
/// # Arguments
/// * `shmem_id` - Region to destroy
/// * `process_id` - Process requesting deletion (must be owner)
///
/// # Returns
/// Ok if marked for deletion
pub fn shmem_destroy(shmem_id: ShmemId, process_id: ProcessId) -> Result<(), ShmemError> {
    let should_delete_now = {
        let mut registry = SHMEM_REGISTRY.lock();
        let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;
        let region = map.get_mut(&shmem_id).ok_or(ShmemError::InvalidId)?;

        // Check ownership
        if region.owner != process_id {
            return Err(ShmemError::NotOwner);
        }

        // Mark for deletion
        region.marked_for_deletion = true;

        // If ref count is already zero, delete immediately
        region.ref_count == 0
    };

    if should_delete_now {
        let mut registry = SHMEM_REGISTRY.lock();
        let map = registry.as_mut().ok_or(ShmemError::NotInitialized)?;
        map.remove(&shmem_id);
        log::debug!("Deleted shared memory region {} immediately", shmem_id.0);
    }

    Ok(())
}

/// Get information about a shared memory region
pub fn shmem_info(shmem_id: ShmemId) -> Option<(usize, ProcessId, ShmemPermissions)> {
    let registry = SHMEM_REGISTRY.lock();
    let map = registry.as_ref()?;
    let region = map.get(&shmem_id)?;

    Some((region.size, region.owner, region.permissions))
}
