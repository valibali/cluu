use cluu_driver_framework::mmio::MmioRegion;

pub struct EhciRegs {
    pub mmio: MmioRegion,
    pub cap_off: usize,
}

impl EhciRegs {
    pub fn new(mmio: MmioRegion) -> Self {
        let cap_off = mmio.read32_safe(0x00) as u8 as usize;
        Self { mmio, cap_off }
    }

    pub fn hciversion(&self) -> u16 {
        ((self.mmio.read32_safe(0x00) >> 16) & 0xFFFF) as u16
    }

    pub fn hcsparams(&self) -> u32 {
        unsafe { self.mmio.read32(0x04) }
    }

    pub fn n_ports(&self) -> u8 {
        (self.hcsparams() & 0xF) as u8
    }

    pub fn hccparams(&self) -> u32 {
        unsafe { self.mmio.read32(0x08) }
    }

    pub fn ext_cap_ptr(&self) -> u8 {
        ((self.hccparams() >> 8) & 0xFF) as u8
    }

    // --- Operational registers (at cap_off) ---

    pub fn usbcmd(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x00) }
    }

    pub fn set_usbcmd(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x00, val) }
    }

    pub fn usbsts(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x04) }
    }

    pub fn set_usbsts(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x04, val) }
    }

    pub fn usbintr(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x08) }
    }

    pub fn set_usbintr(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x08, val) }
    }

    pub fn frindex(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x0C) }
    }

    pub fn set_frindex(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x0C, val) }
    }

    pub fn set_periodic_list_base(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x14, val) }
    }

    pub fn set_async_list_base(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x18, val) }
    }

    pub fn async_list_base(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x18) }
    }

    pub fn set_config_flag(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x40, val) }
    }

    pub fn portsc(&self, port: u8) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x44 + (port as usize) * 4) }
    }

    pub fn set_portsc(&self, port: u8, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x44 + (port as usize) * 4, val) }
    }
}

// USBCMD bits
pub const USBCMD_RUN: u32 = 1 << 0;
pub const USBCMD_HCREST: u32 = 1 << 1;
pub const USBCMD_ASEN: u32 = 1 << 5;
pub const USBCMD_PSEN: u32 = 1 << 4;
pub const USBCMD_FLSIZE_SHIFT: u32 = 2;
pub const USBCMD_INT_THR_CTRL: u32 = 1 << 16;

// USBSTS bits
pub const USBSTS_INT: u32 = 1 << 0;
pub const USBSTS_ERR: u32 = 1 << 1;
pub const USBSTS_PCD: u32 = 1 << 2;
pub const USBSTS_FLR: u32 = 1 << 3;
pub const USBSTS_HCH: u32 = 1 << 12;
pub const USBSTS_ASI: u32 = 1 << 5;
pub const USBSTS_AADV: u32 = 1 << 6;

// PORTSC bits
pub const PORTSC_CCS: u32 = 1 << 0;
pub const PORTSC_CSC: u32 = 1 << 1;
pub const PORTSC_PED: u32 = 1 << 2;
pub const PORTSC_PEC: u32 = 1 << 3;
pub const PORTSC_OSC: u32 = 1 << 5;
pub const PORTSC_RESET: u32 = 1 << 8;
pub const PORTSC_LINE_K: u32 = 1 << 11;
pub const PORTSC_LINE_J: u32 = 1 << 12;
pub const PORTSC_PP: u32 = 1 << 12;
pub const PORTSC_OWNER: u32 = 1 << 13;
pub const PORTSC_FPR: u32 = 1 << 8;
