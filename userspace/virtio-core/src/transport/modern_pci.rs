//! Modern PCI virtio 1.0+ transport.
//!
//! Strategy:
//!   - Map the whole BAR0 once (page-rounded) at `mmio_va_base` via space_map
//!     with MAP_DEVICE.
//!   - Cap region VAs are `mmio_va_base + cap_offset` (offsets discovered by
//!     `pci::parse_capabilities`).
//!
//! Status bits (virtio 1.2 §2.1):
//!   ACKNOWLEDGE = 1
//!   DRIVER      = 2
//!   FEATURES_OK = 8
//!   DRIVER_OK   = 4
//!   FAILED      = 128

use crate::pci::PciDevice;
use crate::transport::Transport;
use crate::virtqueue::Virtqueue;
use libcluu::syscall::{space_map, MAP_DEVICE};
use libcluu::{Error, Result};

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

#[repr(C)]
struct CommonCfg {
    device_feature_select: u32,
    device_feature: u32,
    driver_feature_select: u32,
    driver_feature: u32,
    msix_config: u16,
    num_queues: u16,
    device_status: u8,
    config_generation: u8,
    queue_select: u16,
    queue_size: u16,
    queue_msix_vector: u16,
    queue_enable: u16,
    queue_notify_off: u16,
    _reserved: u16,
    queue_desc: u64,
    queue_driver: u64,
    queue_device: u64,
}

pub struct ModernPciTransport {
    pub device: PciDevice,
    pub mmio_va_base: usize,
    pub common_va: usize,
    pub notify_va: usize,
    pub isr_va: usize,
    pub device_cfg_va: usize,
}

impl ModernPciTransport {
    /// Map the device's BAR0 into the driver's address space starting at
    /// `mmio_va_base` (must be page-aligned and free), then resolve the
    /// virt addresses of the four virtio cap regions.
    ///
    /// `bar_phys` is the BAR0 physical base (from `device.bar0`); `bar_size`
    /// is `device.bar0_size` rounded up to whole pages.
    pub fn new(
        space_token: usize,
        device: PciDevice,
        bar_phys: u64,
        bar_size: u32,
        mmio_va_base: usize,
    ) -> Result<Self> {
        if !device.is_modern {
            return Err(Error::NotImplemented);
        }
        let pages = ((bar_size as usize) + 4095) / 4096;
        for i in 0..pages {
            let virt = mmio_va_base + i * 4096;
            let phys = bar_phys + (i as u64) * 4096;
            space_map(
                space_token,
                virt,
                phys as usize,
                MAP_DEVICE | 0x03, // R+W + device-MMIO
                0,                  // data_len=0; MAP_DEVICE skips alloc/zero/copy
            )?;
        }

        let common_va = mmio_va_base + device.common_cfg_offset as usize;
        let notify_va = mmio_va_base + device.notify_cfg_offset as usize;
        let isr_va = mmio_va_base + device.isr_cfg_offset as usize;
        let device_cfg_va = mmio_va_base + device.device_cfg_offset as usize;

        Ok(Self {
            device,
            mmio_va_base,
            common_va,
            notify_va,
            isr_va,
            device_cfg_va,
        })
    }

    #[inline]
    fn common(&self) -> *mut CommonCfg {
        self.common_va as *mut CommonCfg
    }

    fn write_status_or(&mut self, bit: u8) -> Result<()> {
        unsafe {
            let cur = core::ptr::read_volatile(&(*self.common()).device_status);
            core::ptr::write_volatile(&mut (*self.common()).device_status, cur | bit);
            // Read back to confirm — virtio spec requires reading status
            // after writing FEATURES_OK to verify the device accepted.
            let after = core::ptr::read_volatile(&(*self.common()).device_status);
            if (after & bit) == 0 {
                return Err(Error::InvalidState);
            }
        }
        Ok(())
    }
}

impl Transport for ModernPciTransport {
    fn read_device_features(&mut self) -> Result<u64> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).device_feature_select, 0);
            let lo = core::ptr::read_volatile(&(*self.common()).device_feature) as u64;
            core::ptr::write_volatile(&mut (*self.common()).device_feature_select, 1);
            let hi = core::ptr::read_volatile(&(*self.common()).device_feature) as u64;
            Ok((hi << 32) | lo)
        }
    }

    fn write_driver_features(&mut self, mask: u64) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).driver_feature_select, 0);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature, mask as u32);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature_select, 1);
            core::ptr::write_volatile(&mut (*self.common()).driver_feature, (mask >> 32) as u32);
        }
        // ACKNOWLEDGE + DRIVER must be set first; FEATURES_OK confirms negotiation.
        self.write_status_or(STATUS_ACKNOWLEDGE)?;
        self.write_status_or(STATUS_DRIVER)?;
        self.write_status_or(STATUS_FEATURES_OK)?;
        Ok(())
    }

    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).queue_select, idx);
            core::ptr::write_volatile(&mut (*self.common()).queue_size, vq.queue_size);
            core::ptr::write_volatile(&mut (*self.common()).queue_desc, vq.desc_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_driver, vq.avail_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_device, vq.used_region.phys);
            core::ptr::write_volatile(&mut (*self.common()).queue_enable, 1);
        }
        Ok(())
    }

    fn notify(&self, queue_idx: u16) {
        // Modern: notify_addr = notify_va + queue_select.queue_notify_off * notify_off_multiplier.
        // queue_select must be set to read queue_notify_off; we re-set it here defensively.
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).queue_select, queue_idx);
            let off = core::ptr::read_volatile(&(*self.common()).queue_notify_off);
            let bytes = (off as u32) * self.device.notify_off_multiplier;
            let notify_addr = (self.notify_va + bytes as usize) as *mut u16;
            core::ptr::write_volatile(notify_addr, queue_idx);
        }
    }

    fn isr_status(&self) -> u8 {
        unsafe { core::ptr::read_volatile(self.isr_va as *const u8) }
    }

    fn set_driver_ok(&mut self) -> Result<()> {
        self.write_status_or(STATUS_DRIVER_OK)
    }

    fn reset(&mut self) -> Result<()> {
        unsafe {
            core::ptr::write_volatile(&mut (*self.common()).device_status, 0u8);
        }
        Ok(())
    }
}
