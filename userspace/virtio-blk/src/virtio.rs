#![allow(unused)]
//! Virtio-blk device implementation.
//!
//! This module implements the virtio device initialization sequence and
//! block I/O operations.

use crate::pci::{self, PciDevice};
use crate::virtqueue::{VirtqDesc, Virtqueue, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use alloc::boxed::Box;
use alloc::vec;
use core::sync::atomic::{fence, Ordering};
use libcluu::device_io::{DeviceIo, MmioRegion, PortIoRegion};
use libcluu::syscall::{space_map, MAP_DEVICE};
use libcluu::{Error, Result};

const IO_TRACE: bool = false;

macro_rules! io_trace {
    ($($arg:tt)*) => {
        if IO_TRACE {
            let _ = libcluu::debug_print(&alloc::format!($($arg)*));
        }
    };
}

/// Virtio device status bits
const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_STATUS_FAILED: u8 = 128;

/// Virtio-blk feature bits
const VIRTIO_BLK_F_SIZE_MAX: u64 = 1 << 1;
const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
const VIRTIO_BLK_F_GEOMETRY: u64 = 1 << 4;
const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Virtio-blk request types
const VIRTIO_BLK_T_IN: u32 = 0; // Read
const VIRTIO_BLK_T_OUT: u32 = 1; // Write

/// Virtio-blk status values
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

/// Virtio legacy I/O port offsets (for transitional devices using BAR0 I/O)
mod legacy {
    pub const DEVICE_FEATURES: u32 = 0x00;
    pub const GUEST_FEATURES: u32 = 0x04;
    pub const QUEUE_ADDRESS: u32 = 0x08;
    pub const QUEUE_SIZE: u32 = 0x0C;
    pub const QUEUE_SELECT: u32 = 0x0E;
    pub const QUEUE_NOTIFY: u32 = 0x10;
    pub const DEVICE_STATUS: u32 = 0x12;
    pub const ISR_STATUS: u32 = 0x13;
    pub const CONFIG_OFFSET: u32 = 0x14;
}

/// Virtio modern MMIO common config offsets
mod modern {
    pub const DEVICE_FEATURE_SELECT: u32 = 0x00;
    pub const DEVICE_FEATURE: u32 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u32 = 0x08;
    pub const DRIVER_FEATURE: u32 = 0x0C;
    pub const MSIX_CONFIG: u32 = 0x10;
    pub const NUM_QUEUES: u32 = 0x12;
    pub const DEVICE_STATUS: u32 = 0x14;
    pub const CONFIG_GENERATION: u32 = 0x15;
    pub const QUEUE_SELECT: u32 = 0x16;
    pub const QUEUE_SIZE: u32 = 0x18;
    pub const QUEUE_MSIX_VECTOR: u32 = 0x1A;
    pub const QUEUE_ENABLE: u32 = 0x1C;
    pub const QUEUE_NOTIFY_OFF: u32 = 0x1E;
    pub const QUEUE_DESC_LO: u32 = 0x20;
    pub const QUEUE_DESC_HI: u32 = 0x24;
    pub const QUEUE_AVAIL_LO: u32 = 0x28;
    pub const QUEUE_AVAIL_HI: u32 = 0x2C;
    pub const QUEUE_USED_LO: u32 = 0x30;
    pub const QUEUE_USED_HI: u32 = 0x34;
}

/// Virtio-blk request header
#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

/// Base address for virtqueue memory allocation
const VIRTQUEUE_BASE: usize = 0x50000000;
const DATA_BUFFER_OFFSET: usize = 0x10000 + core::mem::size_of::<VirtioBlkReqHeader>() + 8;
const DATA_BUFFER_MAX: usize = 64 * 1024;

/// Virtio-blk device
pub struct VirtioBlkDevice {
    pci_token: usize,
    space_token: usize,
    pci_device: PciDevice,

    /// Device register I/O (MMIO or I/O ports)
    device_io: Box<dyn DeviceIo>,
    /// MMIO base address (used for modern virtio direct access and I/O port base)
    mmio_base: usize,
    /// Common config MMIO base (for modern devices)
    common_cfg: usize,
    /// Notify MMIO base (for modern devices)
    notify_base: usize,
    /// Device config MMIO base
    device_cfg: usize,

    /// The request virtqueue
    vq: Option<Virtqueue>,
    /// Virtqueue memory base (virtual)
    vq_mem: usize,

    /// Number of sectors on the device
    capacity: u64,
    /// Sector size (typically 512)
    sector_size: usize,
    /// Is device read-only?
    read_only: bool,
    /// Using modern virtio (1.0+)?
    is_modern: bool,
}

impl VirtioBlkDevice {
    /// Create and initialize a new virtio-blk device.
    pub fn new(pci_token: usize, space_token: usize, pci_device: PciDevice) -> Result<Self> {
        // Create temporary DeviceIo - will be replaced in map_mmio()
        let temp_io: Box<dyn DeviceIo> = Box::new(MmioRegion::new(0));

        let mut dev = Self {
            pci_token,
            space_token,
            pci_device: pci_device.clone(),
            device_io: temp_io,
            mmio_base: 0,
            common_cfg: 0,
            notify_base: 0,
            device_cfg: 0,
            vq: None,
            vq_mem: 0,
            capacity: 0,
            sector_size: 512,
            read_only: false,
            is_modern: pci_device.is_modern,
        };

        dev.init()?;
        Ok(dev)
    }

    /// Initialize the device.
    fn init(&mut self) -> Result<()> {
        // Enable PCI bus master and memory access
        pci::enable_device(self.pci_token, &self.pci_device)?;

        // Map BAR0 MMIO region
        self.map_mmio()?;

        // Reset device
        self.write_status(0);
        libcluu::debug_print("virtio-blk: device reset")?;

        // Set ACKNOWLEDGE status
        self.write_status(VIRTIO_STATUS_ACKNOWLEDGE);
        let status_ack = self.read_status();
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: ACKNOWLEDGE status={:#x}",
            status_ack
        ))?;

        // Set DRIVER status
        self.write_status(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
        let status_drv = self.read_status();
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: DRIVER status={:#x}",
            status_drv
        ))?;

        // Negotiate features
        let features = self.negotiate_features()?;
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: negotiated features={:#x}",
            features
        ))?;

        // Set FEATURES_OK
        self.write_status(
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
        );

        // Verify FEATURES_OK was accepted
        let status = self.read_status();
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: FEATURES_OK status={:#x}",
            status
        ))?;
        if status & VIRTIO_STATUS_FEATURES_OK == 0 {
            libcluu::debug_print("virtio-blk: device rejected FEATURES_OK")?;
            self.write_status(VIRTIO_STATUS_FAILED);
            return Err(Error::InvalidState);
        }

        // Read device configuration
        self.read_config()?;

        // Setup virtqueue
        self.setup_virtqueue()?;

        // Set DRIVER_OK status
        self.write_status(
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        );

        let status_ok = self.read_status();
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: DRIVER_OK status={:#x}",
            status_ok
        ))?;

        libcluu::debug_print(&alloc::format!(
            "virtio-blk: initialized, capacity={} sectors, sector_size={}",
            self.capacity,
            self.sector_size
        ))?;

        Ok(())
    }

    /// Map the device MMIO regions (or just store I/O port base for legacy).
    fn map_mmio(&mut self) -> Result<()> {
        let bar0_phys = self.pci_device.bar0 as u64;
        let bar0_size = self.pci_device.bar0_size as usize;

        // For I/O port BARs, create PortIoRegion
        if self.pci_device.is_io_bar {
            let port_base = (bar0_phys & 0xFFFC) as u16;
            self.mmio_base = port_base as usize;
            self.device_io = Box::new(PortIoRegion::new(port_base, self.pci_token));
            libcluu::debug_print(&alloc::format!(
                "virtio-blk: using I/O ports at {:#x}",
                port_base
            ))?;
            return Ok(());
        }

        // Use a fixed virtual address for device MMIO
        let mmio_virt = 0x40000000usize;

        // Map pages for the BAR
        let pages = bar0_size.div_ceil(4096);
        for i in 0..pages {
            let virt = mmio_virt + i * 4096;
            let phys = bar0_phys + (i * 4096) as u64;
            space_map(
                self.space_token,
                virt,
                phys as usize,     // For MAP_DEVICE, this is the physical address
                MAP_DEVICE | 0x03, // read+write+device
                0,
            )?;
        }

        self.mmio_base = mmio_virt;
        self.device_io = Box::new(MmioRegion::new(mmio_virt));

        if self.is_modern {
            self.common_cfg = mmio_virt + self.pci_device.common_cfg_offset as usize;
            self.notify_base = mmio_virt + self.pci_device.notify_cfg_offset as usize;
            self.device_cfg = mmio_virt + self.pci_device.device_cfg_offset as usize;
        }

        Ok(())
    }

    /// Negotiate device features.
    fn negotiate_features(&mut self) -> Result<u64> {
        let device_features = self.read_device_features();
        libcluu::debug_print(&alloc::format!(
            "virtio-blk: device features={:#x}",
            device_features
        ))?;

        // Select features we support
        let mut driver_features = 0u64;

        if device_features & VIRTIO_BLK_F_BLK_SIZE != 0 {
            driver_features |= VIRTIO_BLK_F_BLK_SIZE;
        }
        if device_features & VIRTIO_F_VERSION_1 != 0 {
            driver_features |= VIRTIO_F_VERSION_1;
            self.is_modern = true;
        }
        if device_features & VIRTIO_BLK_F_RO != 0 {
            driver_features |= VIRTIO_BLK_F_RO;
            self.read_only = true;
        }

        libcluu::debug_print(&alloc::format!(
            "virtio-blk: driver features={:#x}",
            driver_features
        ))?;
        self.write_driver_features(driver_features);

        Ok(driver_features)
    }

    /// Read device configuration.
    fn read_config(&mut self) -> Result<()> {
        // Read capacity (8 bytes at offset 0 of device config)
        let cap_lo = self.read_device_config_u32(0);
        let cap_hi = self.read_device_config_u32(4);
        self.capacity = (cap_hi as u64) << 32 | (cap_lo as u64);

        // Read block size if feature is set (4 bytes at offset 20)
        // For simplicity, use default 512
        self.sector_size = 512;

        Ok(())
    }

    /// Setup the request virtqueue.
    fn setup_virtqueue(&mut self) -> Result<()> {
        let queue_num = 0u16;
        self.select_queue(queue_num);

        let queue_size = self.read_queue_size();
        if queue_size == 0 {
            return Err(Error::InvalidState);
        }

        let vq_bytes = Virtqueue::size_bytes(queue_size);
        let vq_pages = vq_bytes.div_ceil(4096);

        // Ensure the request data buffer fits in the mapped region.
        let data_pages = (DATA_BUFFER_OFFSET + DATA_BUFFER_MAX).div_ceil(4096);
        let total_pages = vq_pages.max(data_pages).max(32);

        // Allocate one 2MB physically-contiguous block from the kernel.
        // This gives you 512 pages; we use only total_pages of them.
        let vq_phys_base = libcluu::syscall::pmm_alloc_large(self.space_token)?;
        let _ = libcluu::debug_print(&alloc::format!(
            "virtio-blk: vq phys base={:#x}",
            vq_phys_base
        ));

        // Map the required pages into our address space at VIRTQUEUE_BASE.
        // We intentionally use MAP_DEVICE here to “map a caller-provided physical frame”.
        // (Your kernel’s invoke_space_map treats MAP_DEVICE as: do not alloc_frame, use data_ptr as phys.)
        for i in 0..total_pages {
            let virt = VIRTQUEUE_BASE + i * 4096;
            let phys = vq_phys_base + (i as u64) * 4096;

            // perms: RW + MAP_DEVICE; copy_len=0
            if let Err(err) = space_map(self.space_token, virt, phys as usize, MAP_DEVICE | 0x03, 0)
            {
                let _ = libcluu::debug_print(&alloc::format!(
                    "virtio-blk: space_map failed virt={:#x} phys={:#x} err={:?}",
                    virt,
                    phys,
                    err
                ));
                return Err(err);
            }
        }

        // Zero the region explicitly (MAP_DEVICE path does not zero-fill).
        unsafe {
            core::ptr::write_bytes(VIRTQUEUE_BASE as *mut u8, 0, total_pages * 4096);
        }

        self.vq_mem = VIRTQUEUE_BASE;

        libcluu::debug_print(&alloc::format!(
            "virtio-blk: virtqueue virt={:#x} phys={:#x} pages={}",
            VIRTQUEUE_BASE,
            vq_phys_base,
            total_pages
        ))?;

        // Now the phys base is guaranteed contiguous by construction.
        let vq = unsafe { Virtqueue::new(queue_size, VIRTQUEUE_BASE, vq_phys_base) };
        self.configure_queue(&vq)?;
        self.vq = Some(vq);

        Ok(())
    }

    /// Configure virtqueue addresses in the device.
    fn configure_queue(&self, vq: &Virtqueue) -> Result<()> {
        if self.is_modern {
            // Modern virtio: set addresses directly
            unsafe {
                let common = self.common_cfg as *mut u8;
                // Queue desc address
                (common.add(modern::QUEUE_DESC_LO as usize) as *mut u32)
                    .write_volatile(vq.desc_phys as u32);
                (common.add(modern::QUEUE_DESC_HI as usize) as *mut u32)
                    .write_volatile((vq.desc_phys >> 32) as u32);
                // Queue avail address
                (common.add(modern::QUEUE_AVAIL_LO as usize) as *mut u32)
                    .write_volatile(vq.avail_phys as u32);
                (common.add(modern::QUEUE_AVAIL_HI as usize) as *mut u32)
                    .write_volatile((vq.avail_phys >> 32) as u32);
                // Queue used address
                (common.add(modern::QUEUE_USED_LO as usize) as *mut u32)
                    .write_volatile(vq.used_phys as u32);
                (common.add(modern::QUEUE_USED_HI as usize) as *mut u32)
                    .write_volatile((vq.used_phys >> 32) as u32);
                // Enable queue
                (common.add(modern::QUEUE_ENABLE as usize) as *mut u16).write_volatile(1);
            }
        } else {
            // Legacy virtio: set PFN (page frame number)
            let pfn = (vq.desc_phys / 4096) as u32;
            let _ = libcluu::debug_print(&alloc::format!(
                "virtio-blk: configure queue - writing PFN {:#x} to QUEUE_ADDRESS",
                pfn
            ));
            self.device_io.write_u32(legacy::QUEUE_ADDRESS as u16, pfn);
        }

        Ok(())
    }

    // =========================================================================
    // Device register access
    // =========================================================================

    fn read_status(&self) -> u8 {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *const u8;
                common.add(modern::DEVICE_STATUS as usize).read_volatile()
            }
        } else {
            self.device_io.read_u8(legacy::DEVICE_STATUS as u16)
        }
    }

    fn write_status(&self, status: u8) {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *mut u8;
                common
                    .add(modern::DEVICE_STATUS as usize)
                    .write_volatile(status);
            }
            fence(Ordering::SeqCst);
        } else {
            self.device_io
                .write_u8(legacy::DEVICE_STATUS as u16, status);
        }
    }

    fn read_device_features(&self) -> u64 {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *mut u8;
                // Select feature set 0 (bits 0-31)
                (common.add(modern::DEVICE_FEATURE_SELECT as usize) as *mut u32).write_volatile(0);
                fence(Ordering::SeqCst);
                let lo =
                    (common.add(modern::DEVICE_FEATURE as usize) as *const u32).read_volatile();
                // Select feature set 1 (bits 32-63)
                (common.add(modern::DEVICE_FEATURE_SELECT as usize) as *mut u32).write_volatile(1);
                fence(Ordering::SeqCst);
                let hi =
                    (common.add(modern::DEVICE_FEATURE as usize) as *const u32).read_volatile();
                (hi as u64) << 32 | (lo as u64)
            }
        } else {
            self.device_io.read_u32(legacy::DEVICE_FEATURES as u16) as u64
        }
    }

    fn write_driver_features(&self, features: u64) {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *mut u8;
                // Select feature set 0 (bits 0-31)
                (common.add(modern::DRIVER_FEATURE_SELECT as usize) as *mut u32).write_volatile(0);
                fence(Ordering::SeqCst);
                (common.add(modern::DRIVER_FEATURE as usize) as *mut u32)
                    .write_volatile(features as u32);
                // Select feature set 1 (bits 32-63)
                (common.add(modern::DRIVER_FEATURE_SELECT as usize) as *mut u32).write_volatile(1);
                fence(Ordering::SeqCst);
                (common.add(modern::DRIVER_FEATURE as usize) as *mut u32)
                    .write_volatile((features >> 32) as u32);
            }
            fence(Ordering::SeqCst);
        } else {
            self.device_io
                .write_u32(legacy::GUEST_FEATURES as u16, features as u32);
        }
    }

    fn select_queue(&self, queue_num: u16) {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *mut u8;
                (common.add(modern::QUEUE_SELECT as usize) as *mut u16).write_volatile(queue_num);
            }
            fence(Ordering::SeqCst);
        } else {
            self.device_io
                .write_u16(legacy::QUEUE_SELECT as u16, queue_num);
        }
    }

    fn read_queue_size(&self) -> u16 {
        if self.is_modern {
            unsafe {
                let common = self.common_cfg as *const u8;
                (common.add(modern::QUEUE_SIZE as usize) as *const u16).read_volatile()
            }
        } else {
            self.device_io.read_u16(legacy::QUEUE_SIZE as u16)
        }
    }

    fn read_device_config_u32(&self, offset: usize) -> u32 {
        if self.is_modern {
            unsafe {
                let device = self.device_cfg as *const u8;
                (device.add(offset) as *const u32).read_volatile()
            }
        } else {
            self.device_io
                .read_u32((legacy::CONFIG_OFFSET + offset as u32) as u16)
        }
    }

    fn notify_queue(&self, queue_num: u16) {
        fence(Ordering::SeqCst);
        if self.is_modern {
            unsafe {
                // Modern: notify via notify_base + queue_notify_off * notify_off_multiplier
                let notify_off = {
                    let common = self.common_cfg as *const u8;
                    (common.add(modern::QUEUE_NOTIFY_OFF as usize) as *const u16).read_volatile()
                };
                let offset = notify_off as u32 * self.pci_device.notify_off_multiplier;
                let notify_addr = self.notify_base + offset as usize;
                (notify_addr as *mut u16).write_volatile(queue_num);
            }
        } else {
            io_trace!(
                "virtio-blk: notify queue {} via I/O port {:#x}",
                queue_num,
                legacy::QUEUE_NOTIFY
            );
            self.device_io
                .write_u16(legacy::QUEUE_NOTIFY as u16, queue_num);
        }
        fence(Ordering::SeqCst);
    }

    // =========================================================================
    // Block I/O operations
    // =========================================================================

    /// Perform a block I/O request.
    fn do_request(&mut self, req_type: u32, sector: u64, buf: &mut [u8]) -> Result<usize> {
        let desc_head;
        let desc_data;
        let desc_status;

        if buf.is_empty() {
            return Err(Error::InvalidArgument);
        }
        // Allocate descriptors and setup the request
        {
            let vq = self.vq.as_mut().ok_or(Error::InvalidState)?;

            // We need 3 descriptors: header, data, status
            desc_head = vq.alloc_desc().ok_or(Error::OutOfMemory)?;
            desc_data = vq.alloc_desc().ok_or(Error::OutOfMemory)?;
            desc_status = vq.alloc_desc().ok_or(Error::OutOfMemory)?;

            // Build request header
            let header = VirtioBlkReqHeader {
                req_type,
                reserved: 0,
                sector,
            };

            // We need to place the header in memory accessible to the device
            // For simplicity, use a fixed location after virtqueue memory
            let header_addr = self.vq_mem + 0x10000;
            let status_addr = header_addr + core::mem::size_of::<VirtioBlkReqHeader>();
            let data_addr = status_addr + 8; // Leave room for status

            if buf.len() > DATA_BUFFER_MAX {
                return Err(Error::BufferTooSmall);
            }

            // Translate virtual addresses to physical for DMA
            let header_phys = libcluu::syscall::virt_to_phys(self.space_token, header_addr)?;
            let data_phys = libcluu::syscall::virt_to_phys(self.space_token, data_addr)?;
            let status_phys = libcluu::syscall::virt_to_phys(self.space_token, status_addr)?;

            if header_phys == 0 || data_phys == 0 || status_phys == 0 {
                io_trace!(
                    "virtio-blk: virt_to_phys failed: header={:#x} data={:#x} status={:#x}",
                    header_phys,
                    data_phys,
                    status_phys
                );
                vq.free_desc(desc_head);
                vq.free_desc(desc_data);
                vq.free_desc(desc_status);
                return Err(Error::InvalidState);
            }

            io_trace!(
                "virtio-blk: I/O req sector={} header_phys={:#x} data_phys={:#x} status_phys={:#x}",
                sector,
                header_phys,
                data_phys,
                status_phys
            );

            // Copy header to device-accessible memory
            unsafe {
                (header_addr as *mut VirtioBlkReqHeader).write(header);
            }

            // Setup descriptor chain with physical addresses
            unsafe {
                let descs = vq.desc_ptr();

                // Descriptor 0: header (device reads)
                (*descs.add(desc_head as usize)) = VirtqDesc {
                    addr: header_phys,
                    len: core::mem::size_of::<VirtioBlkReqHeader>() as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: desc_data,
                };

                // Descriptor 1: data buffer
                let data_flags = if req_type == VIRTIO_BLK_T_IN {
                    VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE // Device writes (read request)
                } else {
                    VIRTQ_DESC_F_NEXT // Device reads (write request)
                };

                // For reads, device writes to buffer. For writes, copy data first.
                if req_type == VIRTIO_BLK_T_OUT {
                    core::ptr::copy_nonoverlapping(buf.as_ptr(), data_addr as *mut u8, buf.len());
                }

                (*descs.add(desc_data as usize)) = VirtqDesc {
                    addr: data_phys,
                    len: buf.len() as u32,
                    flags: data_flags,
                    next: desc_status,
                };

                // Descriptor 2: status (device writes)
                *(status_addr as *mut u8) = 0xFF; // Initialize to invalid status
                (*descs.add(desc_status as usize)) = VirtqDesc {
                    addr: status_phys,
                    len: 1,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                };
            }

            // Debug: confirm we did not accidentally submit a zero-length descriptor.
            unsafe {
                let d0 = *vq.desc_ptr().add(desc_head as usize);
                let d1 = *vq.desc_ptr().add(desc_data as usize);
                let d2 = *vq.desc_ptr().add(desc_status as usize);
                io_trace!(
                    "virtio-blk: desc lens head={} data={} status={} flags {:x} {:x} {:x}",
                    d0.len,
                    d1.len,
                    d2.len,
                    d0.flags,
                    d1.flags,
                    d2.flags
                );
            }

            // Add to available ring
            vq.add_to_avail(desc_head);

            io_trace!(
                "virtio-blk: added to avail ring, desc_head={} buf_len={}",
                desc_head,
                buf.len()
            );
            io_trace!(
                "virtio-blk: ring idx avail={} used={}",
                vq.avail_idx(),
                vq.used_idx()
            );
        } // Release vq borrow

        // Notify device
        self.notify_queue(0);
        io_trace!("virtio-blk: notified device");

        // Wait for completion and process result
        let vq = self.vq.as_mut().ok_or(Error::InvalidState)?;
        io_trace!(
            "virtio-blk: wait start avail={} used={} last_used={}",
            vq.avail_idx(),
            vq.used_idx(),
            vq.last_used_idx()
        );
        let mut timeout = 1_000_000u32;
        while !vq.has_used() && timeout > 0 {
            timeout -= 1;
            core::hint::spin_loop();
        }

        if timeout == 0 {
            io_trace!(
                "virtio-blk: timeout waiting (avail={}, used={}, last_used={})",
                vq.avail_idx(),
                vq.used_idx(),
                vq.last_used_idx()
            );
            vq.free_chain(desc_head);
            return Err(Error::Timeout);
        }

        io_trace!(
            "virtio-blk: wait done avail={} used={} last_used={}",
            vq.avail_idx(),
            vq.used_idx(),
            vq.last_used_idx()
        );

        // Get result from used ring
        let (used_head, _used_len) = vq.pop_used().ok_or(Error::InvalidState)?;
        io_trace!(
            "virtio-blk: pop_used head={} now used_idx={} last_used={}",
            used_head,
            vq.used_idx(),
            vq.last_used_idx()
        );

        // Check status
        let header_addr = self.vq_mem + 0x10000;
        let status_addr = header_addr + core::mem::size_of::<VirtioBlkReqHeader>();
        let data_addr = status_addr + 8;
        let status = unsafe { *(status_addr as *const u8) };
        let result = if status == VIRTIO_BLK_S_OK {
            // For reads, copy data from device buffer
            if req_type == VIRTIO_BLK_T_IN {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        data_addr as *const u8,
                        buf.as_mut_ptr(),
                        buf.len(),
                    );
                }
            }
            Ok(buf.len())
        } else {
            Err(Error::InvalidState)
        };

        // Free descriptors
        vq.free_chain(used_head);

        result
    }
}

impl VirtioBlkDevice {
    /// Read sectors from the device.
    pub fn read_sectors(&mut self, start: u64, buf: &mut [u8]) -> Result<usize> {
        self.do_request(VIRTIO_BLK_T_IN, start, buf)
    }

    /// Write sectors to the device.
    pub fn write_sectors(&mut self, start: u64, buf: &[u8]) -> Result<usize> {
        if self.read_only {
            return Err(Error::PermissionDenied);
        }
        // Need a mutable buffer for the request API
        let mut write_buf = vec![0u8; buf.len()];
        write_buf.copy_from_slice(buf);
        self.do_request(VIRTIO_BLK_T_OUT, start, &mut write_buf)
    }

    /// Get the sector size in bytes.
    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    /// Get the total number of sectors.
    pub fn sector_count(&self) -> u64 {
        self.capacity
    }
}
