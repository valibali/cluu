//! Transport abstraction over the device transport (modern PCI today, MMIO future).

use crate::virtqueue::Virtqueue;
use libcluu::Result;

pub mod modern_pci;
pub use modern_pci::ModernPciTransport;

bitflags::bitflags! {
    pub struct FeatureBits: u64 {
        const VERSION_1 = 1 << 32;       // virtio 1.0 compliance
        const RING_INDIRECT_DESC = 1 << 28; // indirect descriptor tables
        // device-class feature bits live in higher namespaces (e.g. blk uses 0..16)
    }
}

pub trait Transport {
    /// Read what the device claims to support (raw 64-bit feature mask).
    fn read_device_features(&mut self) -> Result<u64>;

    /// Tell the device which features the driver wants (subset of device's).
    fn write_driver_features(&mut self, mask: u64) -> Result<()>;

    /// Configure a queue: tell the device the desc/avail/used phys addresses.
    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()>;

    /// Kick the device — tell it to look at the avail ring of `queue_idx`.
    fn notify(&self, queue_idx: u16);

    /// Read the ISR status byte; clears interrupt as side effect.
    fn isr_status(&self) -> u8;

    /// Set DRIVER_OK status bit; device may now process requests.
    fn set_driver_ok(&mut self) -> Result<()>;

    /// Reset the device (status = 0).
    fn reset(&mut self) -> Result<()>;
}
