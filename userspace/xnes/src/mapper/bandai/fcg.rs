use super::super::MapperImpl;
use alloc::vec::Vec;

/// Bandai FCG (mapper 16 / 153 / 157 / 159)
///
/// Used by many Bandai-licensed games. Has PRG banking, CHR banking,
/// scanline IRQ, and controller data via reads.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable (internal register $6000)
///   - $A000-$BFFF: switchable (internal register $6001)
///   - $C000-$DFFF: switchable (internal register $6002, or fixed 2nd-to-last)
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots), controlled by registers $7000-$7007
///
/// IRQ: Scanline-based with counter
///   - Control at registers $7008-$700F
///
/// Register layout (varies by sub-mapper):
///   - $6000-$6FFF: PRG banking registers
///   - $7000-$7FFF: CHR banking and IRQ registers
///   - $8000-$FFFF: Some variants also decode register writes here
///
/// Mapper 16: Standard FCG
/// Mapper 153: Slight register address differences
/// Mapper 157: PRG banking only (no CHR banking)
/// Mapper 159: Different register layout
pub struct Fcg {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// CHR 1 KB bank selects (8 slots)
    chr_banks: [u8; 8],
    /// PRG 8 KB bank selects for $8000-$DFFF
    prg_banks: [u8; 3],
    /// IRQ counter
    irq_counter: u16,
    /// IRQ reload value
    irq_reload: u16,
    /// IRQ enabled
    irq_enabled: bool,
    /// IRQ pending flag
    irq_pending: bool,
    /// Sub-mapper variant
    variant: FcgVariant,
    /// Previous A12 state for edge detection
    prev_a12: bool,
    /// PRG RAM (battery-backed)
    prg_ram: [u8; 0x2000],
}

/// Identifies which FCG sub-mapper variant is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FcgVariant {
    /// Mapper 16: Standard FCG
    Standard,
    /// Mapper 153: Slightly different register addresses
    Mapper153,
    /// Mapper 157: PRG banking only
    Mapper157,
    /// Mapper 159: Different register layout
    Mapper159,
}

impl Fcg {
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
            prg_banks: [0; 3],
            irq_counter: 0,
            irq_reload: 0,
            irq_enabled: false,
            irq_pending: false,
            variant: FcgVariant::Standard,
            prev_a12: false,
            prg_ram: [0; 0x2000],
        }
    }

    /// Create a mapper 153 variant instance.
    pub fn new_mapper153(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = FcgVariant::Mapper153;
        m
    }

    /// Create a mapper 157 variant instance.
    pub fn new_mapper157(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = FcgVariant::Mapper157;
        m
    }

    /// Create a mapper 159 variant instance.
    pub fn new_mapper159(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut m = Self::new(prg, chr, chr_ram, mirror);
        m.variant = FcgVariant::Mapper159;
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

    /// Handle register write for standard FCG (mapper 16).
    fn write_standard(&mut self, addr: u16, val: u8) {
        match addr & 0xF800 {
            // PRG registers at $6000-$6FFF
            0x6000 | 0x6800 => {
                // PRG bank for $8000
                self.prg_banks[0] = val & 0x3F;
            }
            0x7000 => {
                // CHR bank 0
                self.chr_banks[0] = val;
            }
            0x7800 => {
                if addr & 0x0007 == 0 {
                    // PRG bank for $A000
                    self.prg_banks[1] = val & 0x3F;
                } else if addr & 0x0007 == 1 {
                    // PRG bank for $C000
                    self.prg_banks[2] = val & 0x3F;
                } else if addr & 0x0007 == 2 {
                    // CHR bank 1
                    self.chr_banks[1] = val;
                } else if addr & 0x0007 == 3 {
                    // CHR bank 2
                    self.chr_banks[2] = val;
                } else if addr & 0x0007 == 4 {
                    // CHR bank 3
                    self.chr_banks[3] = val;
                } else if addr & 0x0007 == 5 {
                    // CHR bank 4
                    self.chr_banks[4] = val;
                } else if addr & 0x0007 == 6 {
                    // CHR bank 5
                    self.chr_banks[5] = val;
                } else {
                    // CHR bank 6
                    self.chr_banks[6] = val;
                }
            }
            // Also accept writes at $8000-$FFFF area (used by some games)
            0x8000..=0xF800 => {
                let reg = (addr >> 1) & 0x07;
                match reg {
                    0 => self.prg_banks[0] = val & 0x3F,
                    1 => self.prg_banks[1] = val & 0x3F,
                    2 => self.chr_banks[0] = val,
                    3 => self.chr_banks[1] = val,
                    4 => self.chr_banks[2] = val,
                    5 => self.chr_banks[3] = val,
                    6 => self.chr_banks[4] = val,
                    _ => self.chr_banks[5] = val,
                }
            }
            _ => {}
        }
    }

    /// Handle register write for mapper 153.
    fn write_mapper153(&mut self, addr: u16, val: u8) {
        if let 0x6000..=0x7FFF = addr {
            let reg = (addr & 0x000F) as usize;
            match reg {
                0 => self.prg_banks[0] = val & 0x3F,
                1 => self.prg_banks[1] = val & 0x3F,
                2 => self.prg_banks[2] = val & 0x3F,
                3..=10 => {
                    let chr_slot = reg - 3;
                    if chr_slot < 8 {
                        self.chr_banks[chr_slot] = val;
                    }
                }
                11 => {
                    // Mirroring
                    self.mirror = val & 0x03;
                }
                12 => {
                    // IRQ reload low
                    self.irq_reload = (self.irq_reload & 0xFF00) | val as u16;
                }
                13 => {
                    // IRQ reload high
                    self.irq_reload = (self.irq_reload & 0x00FF) | ((val as u16) << 8);
                }
                14 => {
                    // IRQ enable / disable
                    self.irq_enabled = val != 0;
                    if !self.irq_enabled {
                        self.irq_pending = false;
                    }
                    // Reload counter
                    self.irq_counter = self.irq_reload;
                }
                _ => {
                    // IRQ acknowledge
                    self.irq_pending = false;
                }
            }
        }
    }

    /// Handle register write for mapper 157 (PRG only).
    fn write_mapper157(&mut self, addr: u16, val: u8) {
        // Mapper 157: only PRG banking, no CHR banking
        if let 0x6000..=0x7FFF = addr {
            let reg = addr & 0x0003;
            match reg {
                0 => self.prg_banks[0] = val & 0x3F,
                1 => self.prg_banks[1] = val & 0x3F,
                2 => self.prg_banks[2] = val & 0x3F,
                _ => {}
            }
        }
    }

    /// Handle register write for mapper 159.
    fn write_mapper159(&mut self, addr: u16, val: u8) {
        // Mapper 159: different layout
        match addr & 0xF000 {
            0x8000 => {
                // PRG bank for $8000
                self.prg_banks[0] = val & 0x3F;
            }
            0x9000 => {
                // PRG bank for $A000
                self.prg_banks[1] = val & 0x3F;
            }
            0xA000 => {
                // PRG bank for $C000
                self.prg_banks[2] = val & 0x3F;
            }
            0xB000..=0xF000 => {
                // CHR banks
                let slot = ((addr >> 11) & 0x07) as usize;
                if slot < 8 {
                    self.chr_banks[slot] = val;
                }
            }
            _ => {}
        }
    }
}

impl MapperImpl for Fcg {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                // PRG RAM read
                self.prg_ram[(addr & 0x1FFF) as usize]
            }
            0x8000..=0x9FFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_banks[0] as usize, off)
            }
            0xA000..=0xBFFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_banks[1] as usize, off)
            }
            0xC000..=0xDFFF => {
                let off = (addr & 0x1FFF) as usize;
                self.read_prg(self.prg_banks[2] as usize, off)
            }
            0xE000..=0xFFFF => {
                let off = (addr & 0x1FFF) as usize;
                let banks = self.prg_bank_count();
                self.read_prg(banks.saturating_sub(1), off)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                // PRG RAM write
                self.prg_ram[(addr & 0x1FFF) as usize] = val;
                // Also process register writes for FCG variants that use this range
                match self.variant {
                    FcgVariant::Standard => self.write_standard(addr, val),
                    FcgVariant::Mapper153 => self.write_mapper153(addr, val),
                    FcgVariant::Mapper157 => self.write_mapper157(addr, val),
                    FcgVariant::Mapper159 => {}
                }
            }
            0x8000..=0xFFFF
                // Some variants also decode register writes in this range
                if self.variant == FcgVariant::Mapper159 => {
                    self.write_mapper159(addr, val);
                }
                // For mapper 157, no writes in this range
            _ => {}
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
        self.irq_pending
    }

    fn ack_irq(&mut self) {
        self.irq_pending = false;
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
