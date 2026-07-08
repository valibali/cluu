#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    Block,
    Char,
    Input,
    Network,
    Display,
    Usb,
    Tpm,
}

#[derive(Debug)]
pub enum DriverError {
    NotFound,
    PermissionDenied,
    IoError,
    InvalidState,
    OutOfMemory,
    Timeout,
}

pub type DriverResult<T> = core::result::Result<T, DriverError>;

pub struct ProbeContext<'a> {
    pub pci_token: usize,
    pub space_token: usize,
    pub dma_pool: &'a mut cluu_dma_core::DmaPool,
}

pub trait DriverProbe {
    fn matches(&self, dev: &super::pci::PciDeviceInfo) -> bool;
    fn name(&self) -> &str;
    fn device_class(&self) -> DeviceClass;
    fn probe(&mut self, dev: &super::pci::PciDeviceInfo, ctx: &mut ProbeContext<'_>) -> DriverResult<()>;
}
