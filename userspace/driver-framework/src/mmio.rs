use libcluu::Result as CluuResult;

pub struct MmioRegion {
    pub virt_base: usize,
    pub phys_base: u64,
    pub size: usize,
}

impl MmioRegion {
    pub fn map(space_token: usize, phys_base: u64, size: usize, map_device_flag: usize) -> CluuResult<Self> {
        let pages = (size + 0xFFF) / 0x1000;
        let virt_base = 0x5000_0000usize;
        libcluu::syscall::space_map_range(space_token, virt_base, phys_base as usize, map_device_flag, pages, 0)?;
        Ok(Self { virt_base, phys_base, size: pages * 0x1000 })
    }

    pub unsafe fn read8(&self, offset: usize) -> u8 {
        core::ptr::read_volatile((self.virt_base + offset) as *const u8)
    }

    pub unsafe fn read16(&self, offset: usize) -> u16 {
        core::ptr::read_volatile((self.virt_base + offset) as *const u16)
    }

    pub unsafe fn read32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile((self.virt_base + offset) as *const u32)
    }

    pub fn read32_safe(&self, offset: usize) -> u32 {
        unsafe { self.read32(offset) }
    }

    pub unsafe fn read64(&self, offset: usize) -> u64 {
        core::ptr::read_volatile((self.virt_base + offset) as *const u64)
    }

    pub unsafe fn write8(&self, offset: usize, val: u8) {
        core::ptr::write_volatile((self.virt_base + offset) as *mut u8, val);
    }

    pub unsafe fn write16(&self, offset: usize, val: u16) {
        core::ptr::write_volatile((self.virt_base + offset) as *mut u16, val);
    }

    pub unsafe fn write32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile((self.virt_base + offset) as *mut u32, val);
    }

    pub unsafe fn write64(&self, offset: usize, val: u64) {
        core::ptr::write_volatile((self.virt_base + offset) as *mut u64, val);
    }
}

impl MmioRegion {
    pub fn write64_safe(&self, offset: usize, val: u64) {
        unsafe { self.write64(offset, val) }
    }
}

pub struct MmioAccess;

impl MmioAccess {
    pub const MAP_DEVICE: usize = 0x103;
    pub const MAP_DEVICE_WC: usize = 0x1003;
}
