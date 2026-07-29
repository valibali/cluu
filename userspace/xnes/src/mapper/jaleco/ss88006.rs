use super::super::MapperImpl;
use alloc::vec::Vec;

/// Jaleco SS88006 (mapper 18)
///
/// Complex mapper used by multiple Jaleco games.
/// Features: PRG banking, CHR banking, scanline IRQ.
///
/// PRG: 8 KB banks
///   - $8000-$9FFF: switchable (register index 0)
///   - $A000-$BFFF: switchable (register index 1)
///   - $C000-$DFFF: fixed to second-to-last bank
///   - $E000-$FFFF: fixed to last bank
///
/// CHR: 1 KB banks (8 slots)
///   - All 8 CHR 1 KB banks are individually selectable
///
/// IRQ: Scanline-based with A12 edge detection
///   - IRQ reload register
///   - IRQ enable/disable
///   - IRQ acknowledge
///
/// Register layout:
///   - Registers are indexed by address bits (typically A0-A2 or A13-A14)
pub struct Ss88006 {
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
    /// IRQ counter
    irq_counter: u16,
    /// IRQ reload value
    irq_reload: u16,
    /// IRQ enabled
    irq_enabled: bool,
    /// IRQ pending flag
    irq_pending: bool,
    /// Previous A12 state for edge detection
    prev_a12: bool,
}

impl Ss88006 {
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
            prev_a12: false,
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
}

impl MapperImpl for Ss88006 {
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
            // SS88006 register selection based on A0-A2
            let reg = (addr & 0x07) as usize;

            match reg {
                0 => {
                    // PRG bank for $8000-$9FFF
                    self.prg_bank_8000 = val & 0x3F;
                }
                1 => {
                    // PRG bank for $A000-$BFFF
                    self.prg_bank_a000 = val & 0x3F;
                }
                2 => {
                    // CHR bank 0-1 (packed)
                    self.chr_banks[0] = val & 0x0F;
                    self.chr_banks[1] = (val >> 4) & 0x0F;
                }
                3 => {
                    // CHR bank 2-3 (packed)
                    self.chr_banks[2] = val & 0x0F;
                    self.chr_banks[3] = (val >> 4) & 0x0F;
                }
                4 => {
                    // CHR bank 4-5 (packed)
                    self.chr_banks[4] = val & 0x0F;
                    self.chr_banks[5] = (val >> 4) & 0x0F;
                }
                5 => {
                    // CHR bank 6-7 (packed)
                    self.chr_banks[6] = val & 0x0F;
                    self.chr_banks[7] = (val >> 4) & 0x0F;
                }
                6 => {
                    // Mirroring + IRQ reload low
                    self.mirror = val & 0x03;
                    self.irq_reload = (self.irq_reload & 0xFF00) | (val as u16);
                }
                _ => {
                    // IRQ control + IRQ reload high
                    self.irq_enabled = (val & 0x02) != 0;
                    if val & 0x01 != 0 {
                        // Acknowledge IRQ
                        self.irq_pending = false;
                    }
                    self.irq_reload = (self.irq_reload & 0x00FF) | ((val as u16 & 0x7F) << 8);
                    if self.irq_enabled {
                        self.irq_counter = self.irq_reload;
                    }
                }
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
