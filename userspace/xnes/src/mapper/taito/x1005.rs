use super::super::MapperImpl;
use alloc::vec::Vec;

/// Taito X1005 (mapper 80 / 207)
///
/// Used by games like "Doki Doki Panic" (mapper 80) and some other Taito titles.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable
///   - $A000-$BFFF: switchable
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots)
///   - 4 CHR banks are selectable through registers
///
/// IRQ: Present, scanline-based
///   - Counter reload, enable/disable via registers
///
/// Register layout (write at $8000-$FFFF):
///   - Register index determined by address bits A0-A2 or A13-A14
///   - Mapper 207 variant has slightly different addressing
pub struct X1005 {
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
    /// IRQ counter
    irq_counter: u16,
    /// IRQ reload value
    irq_reload: u16,
    /// IRQ enabled
    irq_enabled: bool,
    /// IRQ pending flag
    irq_pending: bool,
    /// Whether this is mapper 207 variant (different register addressing)
    is_mapper_207: bool,
    /// Previous A12 state for edge detection
    prev_a12: bool,
}

impl X1005 {
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
            irq_counter: 0,
            irq_reload: 0,
            irq_enabled: false,
            irq_pending: false,
            is_mapper_207: false,
            prev_a12: false,
        }
    }

    /// Create a mapper 207 (X1005 variant) instance.
    pub fn new_mapper_207(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.is_mapper_207 = true;
        m
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

    /// Clock the scanline IRQ counter.
    /// Called on each PPU A12 rising edge.
    fn clock_irq(&mut self) {
        if self.irq_counter == 0 {
            self.irq_counter = self.irq_reload;
        } else {
            self.irq_counter -= 1;
            if self.irq_counter == 0 && self.irq_enabled {
                self.irq_pending = true;
            }
        }
    }

    /// Handle register write for mapper 80 (X1005).
    fn write_reg_mapper80(&mut self, addr: u16, val: u8) {
        match addr & 0xE000 {
            0x8000 => {
                // $8000-$9FFF: CHR bank 0 + PRG $8000 bank + mirror
                self.chr_banks[0] = val & 0x0F;
                self.prg_bank_8000 = (val >> 4) & 0x07;
                self.mirror = (val >> 7) & 1;
            }
            0xA000 => {
                // $A000-$BFFF: CHR bank 1 + PRG $A000 bank + mirror
                self.chr_banks[1] = val & 0x0F;
                self.prg_bank_a000 = (val >> 4) & 0x07;
                self.mirror = (val >> 7) & 1;
            }
            0xC000 => {
                // $C000-$DFFF: CHR bank 2 + IRQ reload
                self.chr_banks[2] = val & 0x0F;
                self.irq_reload = (val >> 4) as u16;
            }
            _ => {
                // $E000-$FFFF: CHR bank 3 + IRQ control
                self.chr_banks[3] = val & 0x0F;
                self.irq_enabled = (val >> 4) & 1 != 0;
                self.irq_counter = self.irq_reload;
                self.irq_pending = false;
            }
        }
    }

    /// Handle register write for mapper 207 (X1005 variant).
    fn write_reg_mapper207(&mut self, addr: u16, val: u8) {
        match addr & 0xE000 {
            0x8000 => {
                // $8000-$9FFF: PRG bank for $8000
                self.prg_bank_8000 = val & 0x07;
                self.mirror = (val >> 7) & 1;
            }
            0xA000 => {
                // $A000-$BFFF: CHR bank 0 + PRG $A000 bank
                self.chr_banks[0] = val & 0x0F;
                self.prg_bank_a000 = (val >> 4) & 0x07;
            }
            0xC000 => {
                // $C000-$DFFF: CHR banks 1,2
                self.chr_banks[1] = val & 0x0F;
                // Possibly CHR bank 2 as well depending on implementation
            }
            _ => {
                // $E000-$FFFF: CHR bank 3 + IRQ
                self.chr_banks[3] = val & 0x0F;
                // IRQ control bits
                if val & 0x10 != 0 {
                    self.irq_enabled = true;
                    self.irq_counter = self.irq_reload;
                } else {
                    self.irq_enabled = false;
                    self.irq_pending = false;
                }
            }
        }
    }
}

impl MapperImpl for X1005 {
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
            if self.is_mapper_207 {
                self.write_reg_mapper207(addr, val);
            } else {
                self.write_reg_mapper80(addr, val);
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            // Check for A12 edge detection for IRQ
            // We only clock on A12 rising edge in $2000-$3FFF range
            #[allow(clippy::comparison_chain)]
            if a < 0x3000 {
                let a12 = (a & 0x1000) != 0;
                if a12 && !self.prev_a12 {
                    self.clock_irq();
                }
                self.prev_a12 = a12;
            }
            return 0;
        }

        if self.chr.is_empty() {
            return 0;
        }

        // A12 edge detection on CHR address lines
        let a12 = (a & 0x1000) != 0;
        if a12 && !self.prev_a12 {
            self.clock_irq();
        }
        self.prev_a12 = a12;

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
        self.irq_pending
    }

    fn ack_irq(&mut self) {
        self.irq_pending = false;
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
