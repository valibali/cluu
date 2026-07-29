use super::super::MapperImpl;
use alloc::vec::Vec;

/// Namco 108 (mapper 76 / 88 / 95 / 154 / 206)
///
/// A family of similar mappers used by numerous Namco (and third-party) games.
/// All variants share the same core: 8 KB PRG banks and 1 KB CHR banks.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable
///   - $A000-$BFFF: switchable
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots)
///
/// Sub-mapper differences:
///   - Mapper 206 ("Namcot 108"): standard, mirroring from iNES header
///   - Mapper 88: adds mirroring control via dedicated register
///   - Mapper 76: simpler, fewer CHR bits
///   - Mapper 95: VRC2-like register layout
///   - Mapper 154: different register addressing
pub struct Namco108 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// CHR 1 KB bank selects (8 slots for PPU $0000-$1FFF)
    chr_banks: [u8; 8],
    /// PRG 8 KB bank select for $8000-$9FFF
    prg_bank_8000: u8,
    /// PRG 8 KB bank select for $A000-$BFFF
    prg_bank_a000: u8,
    /// Sub-mapper variant
    variant: Namco108Variant,
    /// Previous write address for latch-based register selection
    reg_latch: u8,
}

/// Identifies which Namco 108 sub-mapper variant is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Namco108Variant {
    /// Mapper 206: standard Namcot 108
    Standard,
    /// Mapper 88: standard + mirroring control
    Mapper88,
    /// Mapper 76: simpler variant
    Mapper76,
    /// Mapper 95: VRC2-like
    Mapper95,
    /// Mapper 154: different register layout
    Mapper154,
}

impl Namco108 {
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
            variant: Namco108Variant::Standard,
            reg_latch: 0,
        }
    }

    /// Create a mapper 88 variant (adds mirroring control).
    pub fn new_mapper88(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = Namco108Variant::Mapper88;
        m
    }

    /// Create a mapper 76 variant (simpler).
    pub fn new_mapper76(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = Namco108Variant::Mapper76;
        m
    }

    /// Create a mapper 95 variant (VRC2-like).
    pub fn new_mapper95(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = Namco108Variant::Mapper95;
        m
    }

    /// Create a mapper 154 variant (different register layout).
    pub fn new_mapper154(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = Namco108Variant::Mapper154;
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

    /// Handle a register write for the standard mapper 206 layout.
    fn write_standard(&mut self, addr: u16, val: u8) {
        match addr & 0xF800 {
            0x8000 => {
                // $8000-$87FF: PRG bank for $8000-$9FFF
                self.prg_bank_8000 = val & 0x07;
            }
            0x8800 => {
                // $8800-$8FFF: PRG bank for $A000-$BFFF (or bank extend)
                self.prg_bank_a000 = val & 0x07;
            }
            0x9000 => {
                // $9000-$97FF: CHR banks 0-1 (packed)
                self.chr_banks[0] = val & 0x0F;
                self.chr_banks[1] = (val >> 4) & 0x0F;
            }
            0x9800 => {
                // $9800-$9FFF: CHR banks 2-3 (packed)
                self.chr_banks[2] = val & 0x0F;
                self.chr_banks[3] = (val >> 4) & 0x0F;
            }
            0xA000 => {
                // $A000-$A7FF: CHR banks 4-5 (packed)
                self.chr_banks[4] = val & 0x0F;
                self.chr_banks[5] = (val >> 4) & 0x0F;
            }
            0xA800 => {
                // $A800-$AFFF: CHR banks 6-7 (packed)
                self.chr_banks[6] = val & 0x0F;
                self.chr_banks[7] = (val >> 4) & 0x0F;
            }
            _ => {
                // Some variants use $B000+ for mirroring or other controls
                if self.variant == Namco108Variant::Mapper88
                    && addr & 0xF000 == 0xB000 {
                        // Mirroring control
                        self.mirror = val & 0x01;
                    }
            }
        }
    }

    /// Handle write for mapper 76 (simpler variant).
    fn write_mapper76(&mut self, addr: u16, val: u8) {
        // Mapper 76 uses address bits to select registers
        match addr & 0xF000 {
            0x8000 => {
                self.prg_bank_8000 = val & 0x07;
            }
            0x9000 => {
                self.prg_bank_a000 = (val >> 4) & 0x07;
                self.chr_banks[0] = val & 0x0F;
            }
            0xA000 => {
                self.chr_banks[1] = val & 0x0F;
            }
            0xB000 => {
                self.chr_banks[2] = val & 0x0F;
            }
            0xC000 => {
                self.chr_banks[3] = val & 0x0F;
            }
            0xD000 => {
                self.chr_banks[4] = val & 0x0F;
            }
            0xE000 => {
                self.chr_banks[5] = val & 0x0F;
            }
            _ => {
                self.chr_banks[6] = val & 0x0F;
                self.chr_banks[7] = (val >> 4) & 0x0F;
            }
        }
    }

    /// Handle write for mapper 95 (VRC2-like).
    fn write_mapper95(&mut self, addr: u16, val: u8) {
        // VRC2-like addressing: address bit A0 selects between low/high nibble
        match addr & 0xF000 {
            0x8000 => {
                if addr & 1 == 0 {
                    self.prg_bank_8000 = (self.prg_bank_8000 & 0xF0) | (val & 0x0F);
                } else {
                    self.prg_bank_8000 = (self.prg_bank_8000 & 0x0F) | ((val & 0x0F) << 4);
                }
            }
            0x9000 => {
                // PRG bank for $A000
                if addr & 1 == 0 {
                    self.prg_bank_a000 = (self.prg_bank_a000 & 0xF0) | (val & 0x0F);
                } else {
                    self.prg_bank_a000 = (self.prg_bank_a000 & 0x0F) | ((val & 0x0F) << 4);
                }
            }
            0xA000..=0xF000 => {
                // CHR banks - address bits select which bank
                let slot = ((addr >> 11) & 0x07) as usize;
                if slot < 8 {
                    if addr & 1 == 0 {
                        self.chr_banks[slot] = (self.chr_banks[slot] & 0xF0) | (val & 0x0F);
                    } else {
                        self.chr_banks[slot] = (self.chr_banks[slot] & 0x0F) | ((val & 0x0F) << 4);
                    }
                }
            }
            _ => {}
        }
    }

    /// Handle write for mapper 154 (different register layout).
    fn write_mapper154(&mut self, addr: u16, val: u8) {
        // Mapper 154 uses A0-A2 for register selection (2 registers)
        let reg = (addr >> 1) & 0x03;
        match reg {
            0 => {
                // PRG bank
                self.prg_bank_8000 = val & 0x07;
            }
            1 => {
                // PRG bank for $A000
                self.prg_bank_a000 = val & 0x07;
            }
            2 => {
                // CHR banks 0-3 (packed)
                self.chr_banks[0] = val & 0x0F;
                self.chr_banks[1] = (val >> 4) & 0x0F;
            }
            _ => {
                // CHR banks 4-7 (packed)
                self.chr_banks[4] = val & 0x0F;
                self.chr_banks[5] = (val >> 4) & 0x0F;
            }
        }
    }
}

impl MapperImpl for Namco108 {
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
            match self.variant {
                Namco108Variant::Standard => {
                    self.write_standard(addr, val);
                }
                Namco108Variant::Mapper88 => {
                    self.write_standard(addr, val);
                    // Extended mirroring handled inside write_standard
                }
                Namco108Variant::Mapper76 => {
                    self.write_mapper76(addr, val);
                }
                Namco108Variant::Mapper95 => {
                    self.write_mapper95(addr, val);
                }
                Namco108Variant::Mapper154 => {
                    self.write_mapper154(addr, val);
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
        let slot = (a >> 10) as usize; // 0-7
        let bank = (self.chr_banks[slot] as usize) % banks;
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
        false
    }

    fn ack_irq(&mut self) {}

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
