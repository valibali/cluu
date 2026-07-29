use crate::mapper::Mapper;

use super::Ppu;

impl Ppu {
    /// Compute the physical nametable index for a given PPU address.
    /// Uses either the mapper's custom NT mapping (MMC5) or standard mirroring.
    fn nt_index(addr: u16, mirroring: u8, nt_mapping: u8) -> (u8, u16) {
        let a = addr & 0x3FFF;
        if nt_mapping == 0xFF {
            // Standard mirroring
            let (nt, off) = match mirroring {
                0 => (((a >> 11) & 1) as u8, a & 0x03FF),
                _ => (((a >> 10) & 1) as u8, a & 0x03FF),
            };
            (nt, off)
        } else {
            // MMC5-style: 2 bits per NT (NT0 in bits 1-0, NT1 in bits 3-2, etc.)
            let nt = ((a >> 10) & 3) as u8;
            let source = (nt_mapping >> (nt * 2)) & 0x03;
            (source, a & 0x03FF)
        }
    }

    pub fn ppu_read_nt(&mut self, addr: u16, mapper: &mut Mapper) -> u8 {
        let mirroring = mapper.mirroring();
        let nt_mapping = mapper.nt_mapping();
        let a = addr & 0x3FFF;
        if a >= 0x3F00 {
            let i = (a & 0x1F) as usize;
            self.palette[if i & 0x13 == 0x10 { i & 0x0F } else { i }]
        } else if a < 0x2000 {
            0
        } else if a < 0x3000 {
            let (source, off) = Self::nt_index(a, mirroring, nt_mapping);
            match source {
                0 | 1 => self.vram[(source as usize) * 0x400 + off as usize],
                2 | 3 => mapper.read_nt_ext(a & 0x03FF, source),
                _ => 0,
            }
        } else {
            self.ppu_read_nt(a & 0x2FFF, mapper)
        }
    }

    pub fn ppu_write_nt(&mut self, addr: u16, val: u8, mapper: &mut Mapper) {
        let mirroring = mapper.mirroring();
        let nt_mapping = mapper.nt_mapping();
        let a = addr & 0x3FFF;
        if a >= 0x3F00 {
            let i = (a & 0x1F) as usize;
            self.palette[if i & 0x13 == 0x10 { i & 0x0F } else { i }] = val;
        } else if a < 0x2000 {
        } else if a < 0x3000 {
            let (source, off) = Self::nt_index(a, mirroring, nt_mapping);
            match source {
                0 | 1 => self.vram[(source as usize) * 0x400 + off as usize] = val,
                2 => mapper.write_nt_ext(a & 0x03FF, source, val),
                _ => {} // Fill mode: writes are ignored
            }
        } else {
            self.ppu_write_nt(a & 0x2FFF, val, mapper);
        }
    }

    /// Get the PPU open bus value with decay applied.
    /// Bits decay toward 0 over ~1 second (~60 frames) due to capacitance.
    pub fn get_open_bus(&self) -> u8 {
        const DECAY_CYCLES: u64 = 5_000_000; // ~60 frames worth of PPU ticks
        let elapsed = self.tick_count.saturating_sub(self.last_bus_write_tick);
        if elapsed >= DECAY_CYCLES {
            return 0;
        }
        // Gradual decay: bits fade proportionally
        let decay = (elapsed * 255 / DECAY_CYCLES) as u8;
        self.last_bus_value & !(decay | decay >> 1 | decay >> 2)
    }

    /// Set the PPU data bus value and track the write tick for open bus decay.
    #[inline(always)]
    pub(super) fn set_last_bus_value(&mut self, val: u8) {
        self.last_bus_value = val;
        self.last_bus_write_tick = self.tick_count;
    }

    pub fn ppu_read(&mut self, addr: u16, mapper: &mut Mapper) -> u8 {
        if addr < 0x2000 {
            mapper.ppu_read(addr)
        } else {
            self.ppu_read_nt(addr, mapper)
        }
    }

    pub fn ppu_write(&mut self, addr: u16, val: u8, mapper: &mut Mapper) {
        if addr < 0x2000 {
            mapper.ppu_write(addr, val);
        } else {
            self.ppu_write_nt(addr, val, mapper);
        }
    }
}
