use alloc::vec::Vec;
use alloc::vec;
use super::super::MapperImpl;
use crate::mapper::common::mirror;

/// VRC2 / VRC4 variant selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vrc2Type {
    /// VRC2 address decoding (mappers 21, 22, 23, 25).
    /// Uses `A0` for register sub-selection.
    Vrc2,
    /// VRC4 address decoding (mappers 21, 23, 25).
    /// Uses `A0` and `A1` for register sub-selection (different mapping).
    Vrc4,
}

/// Konami VRC2 / VRC4 (iNES mappers 21, 22, 23, 25)
///
/// - PRG: 8 KB banks at `$8000`, `$A000`, `$C000`, last bank fixed at `$E000`
/// - CHR: eight 1 KB banks at PPU `$0000`..`$1FFF`
/// - Mirroring control via a register
/// - No IRQ
///
/// VRC2 and VRC4 share the same banking model but differ in register address
/// decoding.  The `variant` field selects which decoding to use.
pub struct Vrc2_4 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// VRC2 or VRC4 address decoding.
    pub variant: Vrc2Type,

    // --- registers ---
    // PRG bank select (8 KB banks)
    prg_bank_0: u8, // $8000-$9FFF
    prg_bank_1: u8, // $A000-$BFFF
    prg_bank_2: u8, // $C000-$DFFF
    // prg_bank_3 is fixed to the last bank

    // CHR bank select (8 banks of 1 KB each)
    chr_banks: [u8; 8],
}

impl Vrc2_4 {
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
            variant: Vrc2Type::Vrc2,
            prg_bank_0: 0,
            prg_bank_1: 1,
            prg_bank_2: 2,
            chr_banks: [0; 8],
        }
    }

    /// Decode the register index from the CPU address.
    ///
    /// Returns `(group, index)` where `group` selects which register set
    /// and `index` selects which register within that set.
    fn decode_reg(&self, addr: u16) -> (u8, u8) {
        match self.variant {
            Vrc2Type::Vrc2 => {
                let sub = (addr >> 1) as u8 & 0x01;
                let region = (addr >> 12) & 0x0F;
                match region {
                    0x8 => (0, sub),
                    0x9 => (1, sub),
                    0xA => (2, sub),
                    0xB => (3, sub),
                    0xC => (4, sub),
                    0xD => (5, sub),
                    0xE => (6, sub),
                    0xF => (7, sub),
                    _ => (0xFF, 0),
                }
            }
            Vrc2Type::Vrc4 => {
                let sub = (((addr >> 1) & 0x01) | ((addr >> 2) & 0x02)) as u8;
                let region = (addr >> 12) & 0x0F;
                match region {
                    0x8 => (0, sub),
                    0x9 => (1, sub),
                    0xA => (2, sub),
                    0xB => (3, sub),
                    0xC => (4, sub),
                    0xD => (5, sub),
                    0xE => (6, sub),
                    0xF => (7, sub),
                    _ => (0xFF, 0),
                }
            }
        }
    }

    fn write_register(&mut self, group: u8, index: u8, val: u8) {
        match group {
            // PRG banks (8 KB each)
            0 => {
                // $8000 region: PRG bank 0
                if (index & 1) == 0 {
                    self.prg_bank_0 = val & 0x1F;
                }
            }
            1 => {
                // $9000 region: mirroring control
                self.mirror = if val & 1 != 0 {
                    mirror::HORIZONTAL
                } else {
                    mirror::VERTICAL
                };
            }
            2 => {
                // $A000 region: CHR banks 0-1 (low/high nibble)
                let bank_idx = ((index & 1) * 2) as usize;
                if bank_idx < 8 {
                    let nibble = if (index & 1) == 0 {
                        // low nibble -> bank 0 bits 0-3
                        (self.chr_banks[bank_idx] & 0xF0) | (val & 0x0F)
                    } else {
                        // high nibble -> bank 0 bits 4-7
                        (self.chr_banks[bank_idx] & 0x0F) | ((val & 0x0F) << 4)
                    };
                    if bank_idx < 8 {
                        self.chr_banks[bank_idx] = nibble;
                    }
                }
            }
            3 => {
                // $B000 region: CHR banks 2-3
                let base = 2;
                let bank_idx = base + ((index & 1) * 2) as usize;
                if bank_idx < 8 {
                    let nibble = if (index & 1) == 0 {
                        (self.chr_banks[bank_idx] & 0xF0) | (val & 0x0F)
                    } else {
                        (self.chr_banks[bank_idx] & 0x0F) | ((val & 0x0F) << 4)
                    };
                    self.chr_banks[bank_idx] = nibble;
                }
            }
            4 => {
                // $C000 region: CHR banks 4-5
                let base = 4;
                let bank_idx = base + ((index & 1) * 2) as usize;
                if bank_idx < 8 {
                    let nibble = if (index & 1) == 0 {
                        (self.chr_banks[bank_idx] & 0xF0) | (val & 0x0F)
                    } else {
                        (self.chr_banks[bank_idx] & 0x0F) | ((val & 0x0F) << 4)
                    };
                    self.chr_banks[bank_idx] = nibble;
                }
            }
            5 => {
                // $D000 region: CHR banks 6-7
                let base = 6;
                let bank_idx = base + ((index & 1) * 2) as usize;
                if bank_idx < 8 {
                    let nibble = if (index & 1) == 0 {
                        (self.chr_banks[bank_idx] & 0xF0) | (val & 0x0F)
                    } else {
                        (self.chr_banks[bank_idx] & 0x0F) | ((val & 0x0F) << 4)
                    };
                    self.chr_banks[bank_idx] = nibble;
                }
            }
            6 | 7 => {
                // $E000-$FFFF: PRG banks 1 and 2
                if (index & 1) == 0 {
                    // Even -> PRG bank 1 ($A000)
                    self.prg_bank_1 = val & 0x1F;
                } else {
                    // Odd -> PRG bank 2 ($C000)
                    self.prg_bank_2 = val & 0x1F;
                }
            }
            _ => {}
        }
    }
}

impl MapperImpl for Vrc2_4 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let offset = (addr & 0x1FFF) as usize;
        match addr {
            0x8000..=0x9FFF => {
                let bank = self.prg_bank_0 as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xA000..=0xBFFF => {
                let bank = self.prg_bank_1 as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xC000..=0xDFFF => {
                let bank = self.prg_bank_2 as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xE000..=0xFFFF => {
                // Fixed to last 8 KB bank
                let last_bank = (prg_len / 0x2000).saturating_sub(1);
                self.prg[last_bank * 0x2000 + offset]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr >= 0x8000 {
            let (group, index) = self.decode_reg(addr);
            self.write_register(group, index, val);
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
        let bank_idx = (a as usize) / 0x0400;
        let offset = (a as usize) % 0x0400;
        let bank = self.chr_banks[bank_idx % 8] as usize ;
        let idx = (bank * 0x0400 + offset) % self.chr.len();
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x1FFF;
            self.chr[a as usize] = val;
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
