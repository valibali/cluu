use super::super::MapperImpl;
use alloc::vec::Vec;

/// Jaleco JF-16 (mapper 78)
///
/// Simple mapper used by "Ufouria: The Saga", "Holy Diver", and other Jaleco titles.
///
/// PRG: 16 KB banks
///   - $8000-$BFFF: switchable 16 KB bank
///   - $C000-$FFFF: fixed to last 16 KB bank
///
/// CHR: 2 KB banks
///   - PPU $0000-$07FF: switchable 2 KB bank
///   - PPU $0800-$0FFF: fixed to second 2 KB bank of CHR ROM
///   - PPU $1000-$1FFF: fixed to last 4 KB of CHR ROM (or mirror of $0800)
///
/// Register write at $8000-$FFFF:
///   - Bits 0-3: CHR bank select (2 KB bank for PPU $0000-$07FF)
///   - Bit 4: PRG bank select (0 = first 16 KB bank, 1 = second 16 KB bank)
///   - Other bits are typically ignored
pub struct Jf16 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// PRG 16 KB bank select for $8000-$BFFF
    prg_bank: u8,
    /// CHR 2 KB bank select for PPU $0000-$07FF
    chr_bank: u8,
}

impl Jf16 {
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
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x4000).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        if self.chr.is_empty() {
            1
        } else {
            (self.chr.len() / 0x0800).max(1)
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

impl MapperImpl for Jf16 {
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
            // CHR bank select: bits 0-3 (2 KB bank for PPU $0000-$07FF)
            self.chr_bank = val & 0x0F;
            // PRG bank select: bit 4
            self.prg_bank = (val >> 4) & 0x01;
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

        if a < 0x0800 {
            // PPU $0000-$07FF: switchable 2 KB bank
            let bank = (self.chr_bank as usize) % banks;
            let off = (a & 0x07FF) as usize;
            self.chr[bank * 0x0800 + off]
        } else if a < 0x1000 {
            // PPU $0800-$0FFF: fixed to second 2 KB bank (bank 1)
            let bank = 1 % banks;
            let off = (a & 0x07FF) as usize;
            self.chr[bank * 0x0800 + off]
        } else {
            // PPU $1000-$1FFF: fixed to last 4 KB of CHR ROM (banks 2-3)
            // or mirror of the second 2 KB bank
            let off = (a & 0x0FFF) as usize;
            // Use banks 2-3 from the end
            let base_bank = banks.saturating_sub(2);
            let chr_off = base_bank * 0x0800 + off;
            self.chr[chr_off % self.chr.len()]
        }
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
