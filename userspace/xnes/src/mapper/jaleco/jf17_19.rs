use super::super::MapperImpl;
use alloc::vec::Vec;

/// Jaleco JF-17/19 (mapper 72 / 92)
///
/// Simple PRG/CHR banking used by some Jaleco games.
///
/// Mapper 72:
///   - PRG: 16 KB bank at $8000 (switchable), fixed 16 KB at $C000 (last bank)
///   - CHR: 8 KB bank at PPU $0000 (switchable, but no individual banking)
///   - Register write at $8000-$FFFF:
///     - Bits 0-3: PRG bank select
///     - Bit 4: CHR bank select (bit 0)
///     - Bit 5: Mirroring (0 = vertical, 1 = horizontal)
///
/// Mapper 92:
///   - Similar to mapper 72 but with different register layout
///   - PRG: 16 KB bank at $8000 (switchable), fixed 16 KB at $C000
///   - CHR: 8 KB bank at PPU $0000 (switchable)
///   - Register write at $8000-$FFFF:
///     - Bits 0-3: PRG bank select
///     - Bits 4-5: CHR bank select
///     - Bit 6: Mirroring
pub struct Jf17_19 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// PRG 16 KB bank select for $8000-$BFFF
    prg_bank: u8,
    /// CHR bank select (for entire 8 KB CHR ROM)
    chr_bank: u8,
    /// Variant: false = mapper 72, true = mapper 92
    is_mapper92: bool,
}

impl Jf17_19 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        Self {
            prg: prg.to_vec(),
            chr: if chr_ram {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            chr_ram,
            mirror,
            prg_bank: 0,
            chr_bank: 0,
            is_mapper92: false,
        }
    }

    /// Create a mapper 92 variant instance.
    pub fn new_mapper92(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.is_mapper92 = true;
        m
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x4000).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        if self.chr.is_empty() {
            1
        } else {
            (self.chr.len() / 0x2000).max(1)
        }
    }

    fn read_prg_16k(&self, bank: usize, offset: usize) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let banks = self.prg_bank_count();
        let idx = (bank % banks) * 0x4000 + (offset % 0x4000);
        self.prg[idx % self.prg.len()]
    }
}

impl MapperImpl for Jf17_19 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => {
                let off = (addr & 0x3FFF) as usize;
                self.read_prg_16k(self.prg_bank as usize, off)
            }
            0xC000..=0xFFFF => {
                let off = (addr & 0x3FFF) as usize;
                let banks = self.prg_bank_count();
                let bank = banks.saturating_sub(1);
                self.read_prg_16k(bank, off)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            if self.is_mapper92 {
                // Mapper 92 layout
                self.prg_bank = val & 0x0F;
                self.chr_bank = (val >> 4) & 0x03;
                self.mirror = (val >> 6) & 1;
            } else {
                // Mapper 72 layout
                self.prg_bank = val & 0x0F;
                self.chr_bank = (val >> 4) & 0x01;
                self.mirror = ((val >> 5) & 1) ^ 1; // 0 = vertical, 1 = horizontal
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.chr.is_empty() {
            return 0;
        }

        let banks = self.chr_bank_count();
        let bank = (self.chr_bank as usize) % banks;
        let off = a as usize % 0x2000;
        self.chr[bank * 0x2000 + off]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x1FFF;
            if (a as usize) < self.chr.len() {
                self.chr[a as usize] = val;
            }
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }

    fn irq_pending(&self) -> bool {
        false
    }

    fn ack_irq(&mut self) {}

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
