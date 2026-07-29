use super::super::MapperImpl;
use alloc::vec::Vec;

/// Taito TC0190 (mapper 33)
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable (selected by bits 4-6 of register at $8000-$9FFF)
///   - $A000-$BFFF: switchable (selected by bits 4-6 of register at $A000-$BFFF)
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots total, 4 selectable)
///   - Registers at $8000-$9FFF through $E000-$FFFF control 4 1 KB CHR banks
///   - CHR banks 4-7 ($1000-$1FFF) are fixed to last 4 KB of CHR ROM
///
/// Mirroring: controlled by bit 7 of register writes (0 = horizontal, 1 = vertical)
/// No IRQ
pub struct Tc0190 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// CHR 1 KB bank selects for PPU $0000-$0FFF (4 selectable slots)
    chr_banks: [u8; 4],
    /// PRG 8 KB bank select for $8000-$9FFF
    prg_bank_8000: u8,
    /// PRG 8 KB bank select for $A000-$BFFF
    prg_bank_a000: u8,
}

impl Tc0190 {
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
            chr_banks: [0; 4],
            prg_bank_8000: 0,
            prg_bank_a000: 0,
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x2000).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        if self.chr.is_empty() {
            1
        } else {
            (self.chr.len() / 0x0400).max(1)
        }
    }

    fn read_prg(&self, bank: usize, offset: usize) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let banks = self.prg_bank_count();
        let idx = (bank % banks) * 0x2000 + (offset % 0x2000);
        self.prg[idx % self.prg.len()]
    }
}

impl MapperImpl for Tc0190 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0x9FFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_bank_8000 as usize, off)
            }
            0xA000..=0xBFFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_bank_a000 as usize, off)
            }
            0xC000..=0xDFFF => {
                let off = (addr & 0x1FFF) as usize;
                let banks = self.prg_bank_count();
                let bank = banks.saturating_sub(2);
                self.read_prg(bank, off)
            }
            0xE000..=0xFFFF => {
                let off = (addr & 0x1FFF) as usize;
                let banks = self.prg_bank_count();
                let bank = banks.saturating_sub(1);
                self.read_prg(bank, off)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            let slot = match addr & 0x6000 {
                0x0000 => 0, // $8000-$9FFF -> CHR slot 0 and PRG $8000
                0x2000 => 1, // $A000-$BFFF -> CHR slot 1 and PRG $A000
                0x4000 => 2, // $C000-$DFFF -> CHR slot 2
                _ => 3,      // $E000-$FFFF -> CHR slot 3
            };

            // Mirroring: bit 7 controls mirroring
            // 0 = horizontal, 1 = vertical
            self.mirror = (val >> 7) & 1;

            // CHR bank select: bits 0-3
            if slot < 4 {
                self.chr_banks[slot] = val & 0x0F;
            }

            // PRG bank select: bits 4-6
            match slot {
                0 => self.prg_bank_8000 = (val >> 4) & 0x07,
                1 => self.prg_bank_a000 = (val >> 4) & 0x07,
                _ => {}
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

        if a < 0x1000 {
            // PPU $0000-$0FFF: 4 selectable 1 KB banks
            let slot = (a >> 10) as usize; // 0-3
            let bank = (self.chr_banks[slot] as usize) % banks;
            let off = (a & 0x03FF) as usize;
            self.chr[bank * 0x0400 + off]
        } else {
            // PPU $1000-$1FFF: fixed to last 4 KB of CHR ROM (4 1 KB banks)
            let slot = ((a - 0x1000) >> 10) as usize; // 0-3 (offset 4-7)
            let bank = (banks.saturating_sub(4) + slot) % banks;
            let off = (a & 0x03FF) as usize;
            self.chr[bank * 0x0400 + off]
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
