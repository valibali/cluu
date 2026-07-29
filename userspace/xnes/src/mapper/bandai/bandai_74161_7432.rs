use super::super::MapperImpl;
use alloc::vec::Vec;

/// Bandai 74161/7432 (mapper 70 / 152)
///
/// Simple mapper used by some Bandai games.
///
/// PRG: 16 KB banks
///   - $8000-$BFFF: switchable 16 KB bank
///   - $C000-$FFFF: fixed to last 16 KB bank
///
/// CHR: 8 KB banks (selectable as a whole)
///
/// Register write at $8000-$FFFF:
///   - Bits 0-3: PRG bank select (16 KB bank for $8000)
///   - Bits 4-6: CHR bank select (8 KB bank for PPU $0000)
///   - Bit 7: Mirroring control (0 = vertical, 1 = horizontal) - for mapper 152
///
/// Mapper 70: Basic 74161, no mirroring control
/// Mapper 152: 7432 variant, adds mirroring control via bit 7
pub struct Bandai74161_7432 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// PRG 16 KB bank select for $8000-$BFFF
    prg_bank: u8,
    /// CHR 8 KB bank select for PPU $0000-$1FFF
    chr_bank: u8,
    /// Whether this is mapper 152 (has mirroring control)
    has_mirror_control: bool,
}

impl Bandai74161_7432 {
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
            has_mirror_control: false,
        }
    }

    /// Create a mapper 152 variant instance (with mirroring control).
    pub fn new_mapper152(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.has_mirror_control = true;
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

impl MapperImpl for Bandai74161_7432 {
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
            // Bits 0-3: PRG bank select
            self.prg_bank = val & 0x0F;
            // Bits 4-6: CHR bank select
            self.chr_bank = (val >> 4) & 0x07;
            // Bit 7: Mirroring (mapper 152 only)
            if self.has_mirror_control {
                self.mirror = (val >> 7) & 1;
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
