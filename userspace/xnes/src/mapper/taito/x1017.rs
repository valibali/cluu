use super::super::MapperImpl;
use alloc::vec::Vec;

/// Taito X1017 (mapper 82 / 552)
///
/// Used by "Taito Grand Champion" and similar titles.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable
///   - $A000-$BFFF: switchable
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots)
///
/// IRQ: Present, scanline-based
///
/// Register layout:
///   - Registers selected by address bits A0-A2 or A13-A14
///   - Mapper 552 variant has slightly different register mapping
pub struct X1017 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// CHR 1 KB bank selects (8 slots)
    chr_banks: [u8; 8],
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
    /// Whether this is mapper 552 variant
    is_mapper_552: bool,
    /// Previous A12 state for edge detection
    prev_a12: bool,
}

impl X1017 {
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
            chr_banks: [0; 8],
            prg_bank_8000: 0,
            prg_bank_a000: 0,
            irq_counter: 0,
            irq_reload: 0,
            irq_enabled: false,
            irq_pending: false,
            is_mapper_552: false,
            prev_a12: false,
        }
    }

    /// Create a mapper 552 (X1017 variant) instance.
    pub fn new_mapper_552(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.is_mapper_552 = true;
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

    /// Handle register write for mapper 82 (X1017).
    fn write_reg_mapper82(&mut self, addr: u16, val: u8) {
        // For mapper 82, register selection is done by address bits A0-A2
        let reg = (addr & 0x07) as usize;
        match reg {
            0 => {
                // $8000 with A0-A2=0: PRG bank for $8000
                self.prg_bank_8000 = val & 0x07;
            }
            1 => {
                // $8000 with A0-A2=1: PRG bank for $A000
                self.prg_bank_a000 = val & 0x07;
            }
            2..=5 => {
                // $8000 with A0-A2=2-5: CHR bank selects for slots 0-3
                let chr_slot = reg - 2;
                self.chr_banks[chr_slot] = val & 0x0F;
            }
            6 => {
                // $8000 with A0-A2=6: IRQ reload + mirroring
                self.irq_reload = val as u16;
                self.mirror = (val >> 7) & 1;
            }
            _ => {
                // $8000 with A0-A2=7: IRQ control
                self.irq_enabled = (val >> 4) & 1 != 0;
                self.irq_counter = self.irq_reload;
                self.irq_pending = false;
                self.mirror = (val >> 7) & 1;
            }
        }
        // Also handle mirroring from certain registers
        self.mirror = (val >> 7) & 1;
    }

    /// Handle register write for mapper 552 (X1017 variant).
    fn write_reg_mapper552(&mut self, addr: u16, val: u8) {
        // Mapper 552 uses slightly different register addressing
        let reg = ((addr >> 13) & 0x03) as usize;
        match reg {
            0 => {
                // $8000-$9FFF: PRG $8000 + CHR 0 + mirror
                self.prg_bank_8000 = val & 0x07;
                self.chr_banks[0] = (val >> 4) & 0x0F;
                self.mirror = (val >> 7) & 1;
            }
            1 => {
                // $A000-$BFFF: PRG $A000 + CHR 1
                self.prg_bank_a000 = val & 0x07;
                self.chr_banks[1] = (val >> 4) & 0x0F;
            }
            2 => {
                // $C000-$DFFF: CHR 2-3
                self.chr_banks[2] = val & 0x0F;
                self.chr_banks[3] = (val >> 4) & 0x0F;
            }
            _ => {
                // $E000-$FFFF: IRQ control + CHR 4-5
                self.chr_banks[4] = val & 0x0F;
                self.chr_banks[5] = (val >> 4) & 0x0F;
                self.irq_reload = val as u16;
                self.irq_counter = self.irq_reload;
                self.irq_enabled = true;
            }
        }
    }
}

impl MapperImpl for X1017 {
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
            if self.is_mapper_552 {
                self.write_reg_mapper552(addr, val);
            } else {
                self.write_reg_mapper82(addr, val);
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            // A12 edge detection for nametable range
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

        // A12 edge detection on CHR address
        let a12 = (a & 0x1000) != 0;
        if a12 && !self.prev_a12 {
            self.clock_irq();
        }
        self.prev_a12 = a12;

        let banks = self.chr_bank_count();
        let slot = (a >> 10) as usize; // 0-7
        let bank = (self.chr_banks[slot % 8] as usize) % banks;
        let off = (a & 0x03FF) as usize;
        self.chr[bank * 0x0400 + off]
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
