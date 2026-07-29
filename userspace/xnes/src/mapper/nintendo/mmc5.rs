use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mmc5 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,

    // $5100 PRG mode (0-3)
    pub prg_mode: u8,
    // $5101 CHR mode (0-3)
    pub chr_mode: u8,
    // $5102 PRG RAM protect 1
    pub prg_ram_protect1: u8,
    // $5103 PRG RAM protect 2
    pub prg_ram_protect2: u8,
    // $5104 ExRAM mode (0-3)
    pub ex_ram_mode: u8,
    // $5105 Nametable mapping
    pub nt_mapping_reg: u8,
    // $5106 Fill mode tile
    pub fill_tile: u8,
    // $5107 Fill mode attribute
    pub fill_attr: u8,

    // $5113 PRG RAM bank select
    pub prg_ram_bank: u8,
    // $5114-$5117 PRG bank registers (8KB each)
    pub prg_reg: [u8; 4],

    // $5120-$5127 CHR sprite registers (1KB each)
    pub chr_sprite_reg: [u8; 8],
    // $5128-$512B CHR background registers (1KB each)
    pub chr_bg_reg: [u8; 4],
    // $5130 CHR upper bits
    pub chr_upper_bits: u8,

    // ExRAM (1024 bytes)
    pub ex_ram: [u8; 1024],

    // $5205/$5206 Multiplier
    pub mul_a: u8,
    pub mul_b: u8,
    pub mul_result: u16,

    // $5203/$5204 IRQ
    pub irq_scanline: u8,
    pub irq_status: u8,
    pub irq_enable: bool,
    pub irq_pending_flag: bool,

    // PRG RAM (8KB)
    prg_ram: [u8; 0x2000],

    // CHR fetch mode (for ExRAM mode 1)
    chr_fetch_bg: bool,
    // Extended CHR bank from ExRAM (for mode 1)
    extended_chr_bank: u8,

    has_chr_ram: bool,
}

impl Mmc5 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let prg8_count = if prg.is_empty() {
            1u8
        } else {
            (prg.len() / 0x2000).max(1) as u8
        };

        let mut chr_sprite_reg = [0u8; 8];
        let mut chr_bg_reg = [0u8; 4];
        for (i, reg) in chr_sprite_reg.iter_mut().enumerate() {
            *reg = i as u8;
        }
        for (i, reg) in chr_bg_reg.iter_mut().enumerate() {
            *reg = i as u8;
        }

        let prg_reg = [
            0,
            1,
            prg8_count.saturating_sub(2),
            prg8_count.saturating_sub(1),
        ];

        Self {
            prg: prg.to_vec(),
            chr: if chr_ram {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            chr_ram,
            mirror,
            prg_mode: 3,
            chr_mode: 3,
            prg_ram_protect1: 0,
            prg_ram_protect2: 0,
            ex_ram_mode: 0,
            nt_mapping_reg: 0x44,
            fill_tile: 0,
            fill_attr: 0,
            prg_ram_bank: 0,
            prg_reg,
            chr_sprite_reg,
            chr_bg_reg,
            chr_upper_bits: 0,
            ex_ram: [0; 1024],
            mul_a: 0,
            mul_b: 0,
            mul_result: 0,
            irq_scanline: 0,
            irq_status: 0,
            irq_enable: false,
            irq_pending_flag: false,
            prg_ram: [0; 0x2000],
            chr_fetch_bg: true,
            extended_chr_bank: 0,
            has_chr_ram: chr_ram,
        }
    }

    fn prg_8k_count(&self) -> u8 {
        if self.prg.is_empty() {
            1
        } else {
            (self.prg.len() / 0x2000).max(1) as u8
        }
    }

    fn chr_1k_count(&self) -> u16 {
        if self.chr.is_empty() {
            8
        } else {
            (self.chr.len() / 0x400).max(1) as u16
        }
    }

    fn get_prg_bank_reg(&self, index: usize) -> u8 {
        self.prg_reg[index & 3] & 0x7F
    }

    fn map_prg_8k(&self, bank: u8, addr: u16) -> usize {
        let prg8 = self.prg_8k_count() as usize;
        let b = (bank as usize) % prg8;
        (b * 0x2000 + (addr as usize & 0x1FFF)) % self.prg.len()
    }

    fn map_chr_1k(&self, bank: u16, addr: u16) -> usize {
        let chr1k = self.chr_1k_count() as usize;
        let b = (bank as usize) % chr1k;
        (b * 0x400 + (addr as usize & 0x3FF)) % self.chr.len()
    }

    fn prg_ram_is_protected(&self) -> bool {
        // PRG RAM is write-protected when protect1 == 0x02 and protect2 == 0x01
        // (Castlevania III uses these values for write protection)
        // Other value pairs (e.g., 0x03/0x00) allow writes.
        self.prg_ram_protect1 == 0x02 && self.prg_ram_protect2 == 0x01
    }

    // Helper to resolve PRG ROM address for 0x8000..=0xFFFF reads
    fn read_prg_rom(&self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg8 = self.prg_8k_count() as usize;
        let bank = match self.prg_mode & 0x03 {
            0 => {
                // 32KB mode: uses $5117 (bank reg 3), bits 0-5
                let b = (self.get_prg_bank_reg(3) & 0x7C) as usize;
                (b % prg8) * 0x2000 + ((addr & 0x7FFF) as usize)
            }
            1 => {
                // 2x16KB: $8000-$BFFF from $5115, $C000-$FFFF from $5117
                if addr < 0xC000 {
                    let b = (self.get_prg_bank_reg(1) & 0x7E) as usize;
                    (b % prg8) * 0x2000 + ((addr & 0x3FFF) as usize)
                } else {
                    let b = (self.get_prg_bank_reg(3) & 0x7E) as usize;
                    (b % prg8) * 0x2000 + ((addr & 0x3FFF) as usize)
                }
            }
            2 => {
                // 16KB + 8KB + 8KB
                if addr < 0xC000 {
                    let b = (self.get_prg_bank_reg(1) & 0x7E) as usize;
                    (b % prg8) * 0x2000 + ((addr & 0x3FFF) as usize)
                } else if addr < 0xE000 {
                    self.map_prg_8k(self.get_prg_bank_reg(2), addr)
                } else {
                    self.map_prg_8k(self.get_prg_bank_reg(3), addr)
                }
            }
            _ => {
                // 4x8KB mode
                if addr < 0xA000 {
                    self.map_prg_8k(self.get_prg_bank_reg(0), addr)
                } else if addr < 0xC000 {
                    self.map_prg_8k(self.get_prg_bank_reg(1), addr)
                } else if addr < 0xE000 {
                    self.map_prg_8k(self.get_prg_bank_reg(2), addr)
                } else {
                    self.map_prg_8k(self.get_prg_bank_reg(3), addr)
                }
            }
        };
        self.prg[bank % self.prg.len()]
    }

    // Helper to resolve CHR bank address for ppu_read
    fn resolve_chr_bank(&self, slot: u16, bg_slot: u16, bg_fetch: bool, upper: u16) -> u16 {
        match self.chr_mode & 0x03 {
            0 => {
                // 8KB mode
                if bg_fetch {
                    let base = upper | (u16::from(self.chr_bg_reg[3]) & 0xFC);
                    base + bg_slot
                } else {
                    let base = upper | (u16::from(self.chr_sprite_reg[7]) & 0xF8);
                    base + slot
                }
            }
            1 => {
                // 4KB mode
                if bg_fetch {
                    let base = upper | (u16::from(self.chr_bg_reg[3]) & 0xFC);
                    base + bg_slot
                } else {
                    let base = if slot < 4 {
                        upper | (u16::from(self.chr_sprite_reg[3]) & 0xFC)
                    } else {
                        upper | (u16::from(self.chr_sprite_reg[7]) & 0xFC)
                    };
                    base + (slot & 0x03)
                }
            }
            2 => {
                // 2KB mode
                if bg_fetch {
                    let pair = bg_slot >> 1;
                    let base = upper | (u16::from(self.chr_bg_reg[(pair * 2 + 1) as usize]) & 0xFE);
                    base + (bg_slot & 0x01)
                } else {
                    let pair = slot >> 1;
                    let base =
                        upper | (u16::from(self.chr_sprite_reg[(pair * 2 + 1) as usize]) & 0xFE);
                    base + (slot & 0x01)
                }
            }
            _ => {
                // 1KB mode
                if bg_fetch {
                    upper | u16::from(self.chr_bg_reg[bg_slot as usize])
                } else {
                    upper | u16::from(self.chr_sprite_reg[slot as usize])
                }
            }
        }
    }
}

impl MapperImpl for Mmc5 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x5000..=0x5015 => {
                // MMC5 expansion audio registers — return 0 for reads
                // $5015 returns audio status
                if addr == 0x5015 {
                    return 0;
                }
                0
            }
            0x5204 => {
                // IRQ status: reading clears bit 7 and acknowledges IRQ
                let val = self.irq_status;
                self.irq_status &= !0x80;
                self.irq_pending_flag = false;
                val
            }
            0x5205 => (self.mul_result & 0xFF) as u8,
            0x5206 => ((self.mul_result >> 8) & 0xFF) as u8,
            0x5C00..=0x5FFF => {
                // ExRAM read: only available in modes 2 and 3
                // Mode 0/1 return open bus (but we just return 0)
                if self.ex_ram_mode >= 2 {
                    self.ex_ram[(addr & 0x03FF) as usize]
                } else {
                    0
                }
            }
            0x6000..=0x7FFF => {
                // PRG RAM
                let bank = (self.prg_ram_bank as usize & 0x07) * 0x2000;
                let offset = (addr & 0x1FFF) as usize;
                let ram_size = self.prg_ram.len();
                self.prg_ram[(bank + offset) % ram_size]
            }
            0x8000..=0xFFFF => self.read_prg_rom(addr),
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x5100 => self.prg_mode = val & 0x03,
            0x5101 => self.chr_mode = val & 0x03,
            0x5102 => self.prg_ram_protect1 = val,
            0x5103 => self.prg_ram_protect2 = val,
            0x5104 => self.ex_ram_mode = val & 0x03,
            0x5105 => self.nt_mapping_reg = val,
            0x5106 => self.fill_tile = val,
            0x5107 => self.fill_attr = val & 0x03,
            0x5113 => self.prg_ram_bank = val & 0x07,
            0x5114..=0x5117 => {
                self.prg_reg[(addr - 0x5114) as usize] = val;
            }
            0x5120..=0x5127 => {
                self.chr_sprite_reg[(addr - 0x5120) as usize] = val;
            }
            0x5128..=0x512B => {
                self.chr_bg_reg[(addr - 0x5128) as usize] = val;
            }
            0x5130 => self.chr_upper_bits = val & 0x03,
            0x5203 => self.irq_scanline = val,
            0x5204 => {
                self.irq_enable = (val & 0x80) != 0;
                self.irq_status &= !0x80;
                self.irq_pending_flag = false;
            }
            0x5205 => {
                self.mul_a = val;
                self.mul_result = u16::from(self.mul_a) * u16::from(self.mul_b);
            }
            0x5206 => {
                self.mul_b = val;
                self.mul_result = u16::from(self.mul_a) * u16::from(self.mul_b);
            }
            0x5C00..=0x5FFF => {
                // ExRAM write: available in modes 0, 1, and 2.
                // Mode 3 is read-only; writes are ignored.
                if self.ex_ram_mode < 3 {
                    self.ex_ram[(addr & 0x03FF) as usize] = val;
                }
            }
            0x6000..=0x7FFF if !self.prg_ram_is_protected() => {
                // PRG RAM write (if not protected)
                let bank = (self.prg_ram_bank as usize & 0x07) * 0x2000;
                let offset = (addr & 0x1FFF) as usize;
                let ram_size = self.prg_ram.len();
                self.prg_ram[(bank + offset) % ram_size] = val;
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let addr = addr & 0x3FFF;
        if addr >= 0x2000 {
            return 0; // Nametable reads handled by nt_read_ext
        }
        if self.has_chr_ram {
            return self.chr[addr as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }

        let upper = u16::from(self.chr_upper_bits) << 8;
        let slot = (addr >> 10) & 0x07;
        let bg_slot = (addr >> 10) & 0x03;
        let bg_fetch = self.chr_fetch_bg;

        // ExRAM mode 1: extended attribute mode
        // Each background tile can select its own 4KB CHR page via ExRAM
        if bg_fetch && self.ex_ram_mode == 1 {
            let bank = (u16::from(self.extended_chr_bank & 0x3F) << 2) + bg_slot;
            let idx = self.map_chr_1k(bank, addr);
            return self.chr[idx];
        }

        let bank = self.resolve_chr_bank(slot, bg_slot, bg_fetch, upper);
        let idx = self.map_chr_1k(bank, addr);
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.has_chr_ram {
            let a = addr & 0x1FFF;
            self.chr[a as usize] = val;
        }
    }

    fn mirroring(&self) -> u8 {
        // Return a best-guess mirroring from $5105
        let nt0 = self.nt_mapping_reg & 0x03;
        let nt1 = (self.nt_mapping_reg >> 2) & 0x03;
        let nt2 = (self.nt_mapping_reg >> 4) & 0x03;
        let nt3 = (self.nt_mapping_reg >> 6) & 0x03;

        u8::from(nt0 == 0 && nt1 == 1 && nt2 == 0 && nt3 == 1) // 1 = vertical, 0 = horizontal
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending_flag
    }

    fn ack_irq(&mut self) {
        self.irq_pending_flag = false;
        self.irq_status &= !0x80;
    }

    fn clock_scanline(&mut self) {
        // MMC5 scanline IRQ:
        // - Bit 6 of irq_status = in-frame (scanlines 0-239)
        // - When enabled and scanline matches $5203, fire IRQ
        // This method is called by the PPU for each A12 edge.
        // For MMC5, IRQ is scanline-based, not A12-based like MMC3.
        // We use clock_scanline as a per-scanline notification.
    }

    fn notify_scanline(&mut self, scanline: u16) {
        // Called once per scanline from the PPU.
        // MMC5 only tracks visible scanlines (0-239).
        if scanline < 240 {
            self.irq_status = (self.irq_status & 0xC0) | (scanline as u8 & 0x3F);
            self.irq_status |= 0x40;

            // MMC5 scanline IRQ: fires when counter matches $5203
            if self.irq_enable && scanline as u8 == self.irq_scanline {
                self.irq_status |= 0x80;
                self.irq_pending_flag = true;
            }
        } else {
            self.irq_status &= !0x40;
        }
    }

    fn has_chr_ram(&self) -> bool {
        self.has_chr_ram
    }

    // MMC5 custom NT mapping
    fn nt_mapping(&self) -> u8 {
        self.nt_mapping_reg
    }

    fn read_nt_ext(&mut self, addr: u16, nt_source: u8) -> u8 {
        match nt_source {
            2 => {
                // ExRAM as nametable
                self.ex_ram[(addr & 0x03FF) as usize]
            }
            3 => {
                // Fill mode
                if addr & 0x03C0 == 0x03C0 {
                    // Attribute area: return fill attribute
                    (self.fill_attr & 0x03) * 0x55
                } else {
                    // Tile area: return fill tile
                    self.fill_tile
                }
            }
            _ => 0, // Shouldn't happen for CIRAM sources
        }
    }

    fn write_nt_ext(&mut self, addr: u16, nt_source: u8, val: u8) {
        if nt_source == 2 {
            // Write to ExRAM as nametable
            self.ex_ram[(addr & 0x03FF) as usize] = val;
        }
        // Fill mode writes are ignored
    }

    fn set_chr_fetch_bg(&mut self) {
        self.chr_fetch_bg = true;
    }

    fn set_chr_fetch_sprite(&mut self) {
        self.chr_fetch_bg = false;
    }

    fn set_extended_chr_bank(&mut self, bank: u8) {
        self.extended_chr_bank = bank;
    }

    fn get_extended_chr_bank(&self) -> u8 {
        self.extended_chr_bank
    }

    fn get_ex_ram_mode(&self) -> u8 {
        self.ex_ram_mode
    }

    fn get_fill_tile(&self) -> u8 {
        self.fill_tile
    }

    fn get_fill_attr(&self) -> u8 {
        self.fill_attr
    }

    fn read_ex_ram_byte(&mut self, offset: u16) -> u8 {
        self.ex_ram[(offset & 0x03FF) as usize]
    }
}
