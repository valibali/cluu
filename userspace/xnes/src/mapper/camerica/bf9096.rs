use super::super::MapperImpl;
use alloc::vec::Vec;

/// Camerica BF9096 (mapper 232)
///
/// Similar to BF909x (mapper 71) but with a reversed register layout.
/// Used by some Camerica/Codemasters games.
///
/// PRG: 16 KB banks
///   - $8000-$BFFF: switchable 16 KB bank
///   - $C000-$FFFF: fixed to last 16 KB bank
///
/// CHR: No CHR banking - CHR is fixed
///
/// Register layout (write at $8000-$FFFF):
///   - Bits 0-3: PRG bank select (16 KB bank for $8000)
///   - Bit 4: Mirroring (0 = vertical, 1 = horizontal) - differs from BF909x
///   - vs BF909x which uses bit 0 for mirroring and bits 4-7 for PRG bank
pub struct Bf9096 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// PRG 16 KB bank select for $8000-$BFFF
    prg_bank: u8,
}

impl Bf9096 {
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

impl MapperImpl for Bf9096 {
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
            // Bit 4: Mirroring (0 = vertical, 1 = horizontal)
            self.mirror = (val >> 4) & 1;
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
        // CHR is fixed
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
