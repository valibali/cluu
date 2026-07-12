//! Modern PCI virtio 1.0+ transport.
//!
//! Strategy:
//!   - Map the whole BAR window (page-rounded) at `mmio_va_base` via
//!     `space_map_range` with `MAP_DEVICE` so PTEs get the PCD bit and
//!     writes bypass the cache.
//!   - Cap region VAs are `mmio_va_base + cap_offset` (offsets discovered
//!     by `pci::parse_capabilities`).
//!
//! All MMIO access is done with `read_volatile` / `write_volatile` against
//! a pointer computed by adding a byte offset to `common_va`. We do NOT
//! use a `#[repr(C)]` struct view here — Rust's default C alignment would
//! insert 6 bytes of padding between `queue_notify_off` (u16 at 0x1e) and
//! `queue_desc` (u64 at 0x20), which would shift `queue_desc` and every
//! later field off-spec. Using `#[repr(C, packed)]` would fix the layout
//! but blocks `&mut field` references on all u64 / u32 fields, which
//! `read_volatile`/`write_volatile` need. Byte-offset access sidesteps
//! both pitfalls.

use crate::pci::PciDevice;
use crate::transport::Transport;
use crate::virtqueue::Virtqueue;
use libcluu::syscall::{space_map_range, MAP_DEVICE};
use libcluu::{Error, Result};
extern crate alloc;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

// virtio 1.1 §4.1.4.3 virtio_pci_common_cfg byte offsets within the cap.
const O_DEVICE_FEATURE_SELECT: usize = 0x00; // u32
const O_DEVICE_FEATURE: usize = 0x04; // u32
const O_DRIVER_FEATURE_SELECT: usize = 0x08; // u32
const O_DRIVER_FEATURE: usize = 0x0c; // u32
const O_MSIX_CONFIG: usize = 0x10; // u16
const O_NUM_QUEUES: usize = 0x12; // u16
const O_DEVICE_STATUS: usize = 0x14; // u8
const O_CONFIG_GENERATION: usize = 0x15; // u8
const O_QUEUE_SELECT: usize = 0x16; // u16
const O_QUEUE_SIZE: usize = 0x18; // u16
const O_QUEUE_MSIX_VECTOR: usize = 0x1a; // u16
const O_QUEUE_ENABLE: usize = 0x1c; // u16
const O_QUEUE_NOTIFY_OFF: usize = 0x1e; // u16
const O_QUEUE_DESC: usize = 0x20; // u64
const O_QUEUE_DRIVER: usize = 0x28; // u64
const O_QUEUE_DEVICE: usize = 0x30; // u64

#[inline]
unsafe fn r8(base: usize, off: usize) -> u8 {
    core::ptr::read_volatile((base + off) as *const u8)
}
#[inline]
unsafe fn r16(base: usize, off: usize) -> u16 {
    core::ptr::read_volatile((base + off) as *const u16)
}
#[inline]
unsafe fn r32(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}
#[inline]
unsafe fn w8(base: usize, off: usize, v: u8) {
    core::ptr::write_volatile((base + off) as *mut u8, v)
}
#[inline]
unsafe fn w16(base: usize, off: usize, v: u16) {
    core::ptr::write_volatile((base + off) as *mut u16, v)
}
#[inline]
unsafe fn w32(base: usize, off: usize, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v)
}
#[inline]
unsafe fn w64(base: usize, off: usize, v: u64) {
    core::ptr::write_volatile((base + off) as *mut u64, v)
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
        space_map_range(
            space_token,
            mmio_va_base,
            bar_phys as usize,
            (MAP_DEVICE | 0x03) as usize,
            pages,
            0,
        )?;

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

    fn write_status_or(&mut self, bit: u8) -> Result<()> {
        unsafe {
            let cur = r8(self.common_va, O_DEVICE_STATUS);
            w8(self.common_va, O_DEVICE_STATUS, cur | bit);
            // Re-read; the device may refuse a state transition (e.g. reject
            // FEATURES_OK if it doesn't like our subset).
            let after = r8(self.common_va, O_DEVICE_STATUS);
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
            w32(self.common_va, O_DEVICE_FEATURE_SELECT, 0);
            let lo = r32(self.common_va, O_DEVICE_FEATURE) as u64;
            w32(self.common_va, O_DEVICE_FEATURE_SELECT, 1);
            let hi = r32(self.common_va, O_DEVICE_FEATURE) as u64;
            Ok((hi << 32) | lo)
        }
    }

    fn write_driver_features(&mut self, mask: u64) -> Result<()> {
        unsafe {
            w32(self.common_va, O_DRIVER_FEATURE_SELECT, 0);
            w32(self.common_va, O_DRIVER_FEATURE, mask as u32);
            w32(self.common_va, O_DRIVER_FEATURE_SELECT, 1);
            w32(self.common_va, O_DRIVER_FEATURE, (mask >> 32) as u32);
        }
        self.write_status_or(STATUS_ACKNOWLEDGE)?;
        self.write_status_or(STATUS_DRIVER)?;
        self.write_status_or(STATUS_FEATURES_OK)?;
        Ok(())
    }

    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()> {
        unsafe {
            w16(self.common_va, O_QUEUE_SELECT, idx);
            let dev_max_size = r16(self.common_va, O_QUEUE_SIZE);
            w16(self.common_va, O_QUEUE_SIZE, vq.queue_size);
            w64(self.common_va, O_QUEUE_DESC, vq.desc_region.phys);
            w64(self.common_va, O_QUEUE_DRIVER, vq.avail_region.phys);
            w64(self.common_va, O_QUEUE_DEVICE, vq.used_region.phys);
            // No MSI-X: write NO_VECTOR explicitly so the device doesn't
            // try to deliver via a vector we never set up.
            w16(self.common_va, O_QUEUE_MSIX_VECTOR, 0xFFFF);
            w16(self.common_va, O_QUEUE_ENABLE, 1);

            let qe = r16(self.common_va, O_QUEUE_ENABLE);
            let qmv = r16(self.common_va, O_QUEUE_MSIX_VECTOR);
            let qno = r16(self.common_va, O_QUEUE_NOTIFY_OFF);
            let qd_back = core::ptr::read_volatile((self.common_va + O_QUEUE_DESC) as *const u64);
            let _ = libcluu::debug_print(&alloc::format!(
                "virtio-core: q{} dev_max={} desc={:#x} (back={:#x}) avail={:#x} used={:#x} qe={} qmv={:#x} qno={}",
                idx,
                dev_max_size,
                vq.desc_region.phys,
                qd_back,
                vq.avail_region.phys,
                vq.used_region.phys,
                qe,
                qmv,
                qno
            ));
        }
        Ok(())
    }

    fn notify(&self, queue_idx: u16) {
        unsafe {
            w16(self.common_va, O_QUEUE_SELECT, queue_idx);
            let off = r16(self.common_va, O_QUEUE_NOTIFY_OFF);
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
            w8(self.common_va, O_DEVICE_STATUS, 0);
        }
        Ok(())
    }
}
