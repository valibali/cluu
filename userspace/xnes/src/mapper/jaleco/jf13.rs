use super::super::MapperImpl;
use alloc::vec::Vec;

/// Jaleco JF-13 (mapper 86)
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable
///   - $A000-$BFFF: switchable
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots)
///   - 4 CHR banks are selectable via registers
///   - CHR banks 4-7 ($1000-$1FFF) are fixed to last 4 KB of CHR ROM
///
/// Register layout (write at $8000-$FFFF):
///   - Register index derived from address bits A0-A1 or A13-A14
///   - Each write controls CHR bank select and PRG bank select
///
/// No IRQ.
pub struct Jf13 {
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

impl Jf13 {
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

impl MapperImpl for Jf13 {
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
            // Register index from A0-A1 (or A14 depending on variant)
            let reg = ((addr >> 14) & 0x03) as usize;

            match reg {
                0 => {
                    // $8000-$BFFF: PRG bank for $8000 + CHR slot 0
                    self.prg_bank_8000 = val & 0x07;
                    self.chr_banks[0] = (val >> 4) & 0x0F;
                }
                1 => {
                    // $C000-$FFFF (with A14=1): PRG bank for $A000 + CHR slot 1
                    self.prg_bank_a000 = val & 0x07;
                    self.chr_banks[1] = (val >> 4) & 0x0F;
                }
                2 => {
                    // CHR slots 2-3
                    self.chr_banks[2] = val & 0x0F;
                    self.chr_banks[3] = (val >> 4) & 0x0F;
                }
                _ => {
                    // Additional control
                    self.chr_banks[reg - 1] = val & 0x0F;
                }
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
            // PPU $1000-$1FFF: fixed to last 4 KB of CHR ROM
            let slot = ((a - 0x1000) >> 10) as usize; // 0-3
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
