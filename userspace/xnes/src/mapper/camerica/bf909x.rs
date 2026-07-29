use super::super::MapperImpl;
use alloc::vec::Vec;

/// Camerica BF909x (mapper 71)
///
/// UNROM-like mapper with 16 KB PRG banks and mirroring control.
/// Used by Camerica (Aladdin, Bee 52, etc.) and Codemasters games.
///
/// PRG: 16 KB banks
///   - $8000-$BFFF: switchable 16 KB bank
///   - $C000-$FFFF: fixed to last 16 KB bank
///
/// CHR: No CHR banking - CHR is typically fixed RAM or ROM
///   - The PPU sees the entire CHR space directly
///
/// Mirroring: Controlled by register write
///   - Writing to $8000-$FFFF:
///     - Bit 0: Mirroring (0 = vertical, 1 = horizontal)
///     - Bits 4-7: PRG bank select (16 KB bank for $8000)
///     - Some variants use more bits for bank select
pub struct Bf909x {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// PRG 16 KB bank select for $8000-$BFFF
    prg_bank: u8,
}

impl Bf909x {
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
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x4000).max(1)
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

impl MapperImpl for Bf909x {
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
            // Bit 0: Mirroring (0 = vertical, 1 = horizontal)
            self.mirror = val & 1;
            // Bits 4-7: PRG bank select
            self.prg_bank = (val >> 4) & 0x0F;
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
        // CHR is fixed - map directly
        self.chr[a as usize % self.chr.len()]
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
