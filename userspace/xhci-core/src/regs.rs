use cluu_driver_framework::mmio::MmioRegion;

pub struct XhciRegs {
    pub mmio: MmioRegion,
    pub cap_off: usize,
    pub rtsoff: usize,
    pub dboff: usize,
}

impl XhciRegs {
    pub fn new(mmio: MmioRegion) -> Self {
        let cap_off = (mmio.read32_safe(0x00) & 0xFF) as usize;
        let dboff = mmio.read32_safe(0x14) as usize;
        let rtsoff = mmio.read32_safe(0x18) as usize;
        Self { mmio, cap_off, rtsoff, dboff }
    }

    pub fn hciversion(&self) -> u16 {
        ((self.cap_full() >> 16) & 0xFFFF) as u16
    }

    pub fn caplength(&self) -> u8 {
        (self.cap_full() & 0xFF) as u8
    }

    pub fn cap_full(&self) -> u32 {
        unsafe { self.mmio.read32(0x00) }
    }

    pub fn hcsparams1(&self) -> u32 {
        unsafe { self.mmio.read32(0x04) }
    }

    pub fn hcsparams2(&self) -> u32 {
        unsafe { self.mmio.read32(0x08) }
    }

    pub fn max_ports(&self) -> u8 {
        ((self.hcsparams1() >> 24) & 0xFF) as u8
    }

    pub fn usbcmd(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x00) }
    }

    pub fn set_usbcmd(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x00, val) }
    }

    pub fn usbsts(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x04) }
    }

    pub fn set_crcr_lo(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x08, val) }
    }

    pub fn set_crcr_hi(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x0C, val) }
    }

    pub fn set_dcbaap_lo(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x10, val) }
    }

    pub fn set_dcbaap_hi(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x14, val) }
    }

    pub fn config(&self) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x18) }
    }

    pub fn set_config(&self, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x18, val) }
    }

    pub fn max_slots(&self) -> u8 {
        ((self.hcsparams1() >> 0) & 0xFF) as u8
    }

    // RTS interrupter 0: IMAN, IMOD, ERSTSZ, ERSTBA, ERDP (offsets from RTSOFF)
    pub fn set_iman(&self, val: u32) {
        unsafe { self.mmio.write32(self.rtsoff + 0x00, val) }
    }

    pub fn iman(&self) -> u32 {
        unsafe { self.mmio.read32(self.rtsoff + 0x00) }
    }

    pub fn set_erstsz(&self, val: u16) {
        unsafe { self.mmio.write32(self.rtsoff + 0x08, val as u32) }
    }

    pub fn set_erstba_lo(&self, val: u32) {
        unsafe { self.mmio.write32(self.rtsoff + 0x0C, val) }
    }

    pub fn set_erstba_hi(&self, val: u32) {
        unsafe { self.mmio.write32(self.rtsoff + 0x10, val) }
    }

    pub fn set_erdp_lo(&self, val: u32) {
        unsafe { self.mmio.write32(self.rtsoff + 0x14, val) }
    }

    pub fn set_erdp_hi(&self, val: u32) {
        unsafe { self.mmio.write32(self.rtsoff + 0x18, val) }
    }

    // Port registers: PORTSC at cap_off + 0x400 + port * 0x10
    pub fn portsc(&self, port: u8) -> u32 {
        unsafe { self.mmio.read32(self.cap_off + 0x400 + (port as usize) * 0x10) }
    }

    pub fn set_portsc(&self, port: u8, val: u32) {
        unsafe { self.mmio.write32(self.cap_off + 0x400 + (port as usize) * 0x10, val) }
    }

    // Doorbell register: at dboff + slot * 0x20 (offset from MMIO base, not cap_off)
    pub fn ring_doorbell(&self, slot: u8, target: u8) {
        unsafe { self.mmio.write32(self.dboff + (slot as usize) * 0x20, target as u32) }
    }
}

pub const USBCMD_RS: u32 = 1;
pub const USBCMD_HCRST: u32 = 2;
pub const USBSTS_HCH: u32 = 1 << 0;
pub const USBSTS_CNR: u32 = 1 << 11;
pub const USBSTS_EINT: u32 = 1 << 3;

pub const PORTSC_CCS: u32 = 1 << 0;
pub const PORTSC_PED: u32 = 1 << 1;
pub const PORTSC_PR: u32 = 1 << 4;
pub const PORTSC_PLS_MASK: u32 = 0xF << 5;
pub const PORTSC_SPEED_MASK: u32 = 0xF << 10;
pub const PORTSC_SPEED_FULL: u32 = 2 << 10;
pub const PORTSC_SPEED_LOW: u32 = 1 << 10;
pub const PORTSC_SPEED_HIGH: u32 = 3 << 10;
pub const PORTSC_CSC: u32 = 1 << 17;
pub const PORTSC_PRC: u32 = 1 << 21;

