use super::super::MapperImpl;
use alloc::vec::Vec;

/// Sunsoft FME-7 (mapper 69).
///
/// Complex mapper with PRG banking, CHR banking, IRQ, and expansion audio.
/// Audio implementation is skipped for now.
///
/// Register interface:
///   $8000-$9FFF: Select register index (A0-A3 from address bits 12-13 or 0-3)
///   $A000-$BFFF: Write data to selected register
///
/// Registers:
///   0: CHR bank 0 (PPU $0000, 1KB)
///   1: CHR bank 1 (PPU $0400, 1KB)
///   2: CHR bank 2 (PPU $0800, 1KB)
///   3: CHR bank 3 (PPU $0C00, 1KB)
///   4: CHR bank 4 (PPU $1000, 1KB)
///   5: CHR bank 5 (PPU $1400, 1KB)
///   6: CHR bank 6 (PPU $1800, 1KB)
///   7: CHR bank 7 (PPU $1C00, 1KB)
///   8: PRG bank 0 ($8000, 8KB)
///   9: PRG bank 1 ($A000, 8KB)
///   A: PRG bank 2 ($C000, 8KB)
///   B: PRG bank 3 ($E000, 8KB)
///   C: Mirroring (bits 0-1: 0=horizontal, 1=vertical, 2=one-screen A, 3=one-screen B)
///   D: IRQ latch (low 8 bits, IRQ counter reload value)
///   E: IRQ control
///   F: Audio (skipped)
pub struct Fme7 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    // Register select
    reg_index: u8,
    // PRG banks (8 KB each)
    prg_banks: [u8; 4],
    // CHR banks (1 KB each)
    chr_banks: [u8; 8],
    // IRQ
    irq_counter: u16,
    irq_reload: u16,
    irq_enabled: bool,
    irq_pending_flag: bool,
    has_chr_ram: bool,
}

impl Fme7 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let prg_count = if prg.is_empty() {
            1
        } else {
            (prg.len() / 0x2000).max(1)
        };
        Self {
            prg: prg.to_vec(),
            chr: if chr_ram {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            chr_ram,
            mirror,
            reg_index: 0,
            prg_banks: [
                0,
                1,
                (prg_count.saturating_sub(2)) as u8,
                (prg_count.saturating_sub(1)) as u8,
            ],
            chr_banks: [0; 8],
            irq_counter: 0,
            irq_reload: 0,
            irq_enabled: false,
            irq_pending_flag: false,
            has_chr_ram: chr_ram,
        }
    }
}

impl MapperImpl for Fme7 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let prg8 = (prg_len / 0x2000).max(1);
        match addr {
            0x8000..=0x9FFF => {
                let bank = (self.prg_banks[0] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xA000..=0xBFFF => {
                let bank = (self.prg_banks[1] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xC000..=0xDFFF => {
                let bank = (self.prg_banks[2] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xE000..=0xFFFF => {
                let bank = (self.prg_banks[3] as usize) % prg8;
                let idx = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x9FFF => {
                // Select register index
                // Uses A0-A3 from address (low bits 0-3) but typically
                // the register index is just the data bus value or address bits.
                // Standard FME-7: $8000 writes the register index from val
                self.reg_index = val & 0x0F;
            }
            0xA000..=0xBFFF => {
                // Write data to selected register
                match self.reg_index {
                    0..=7 => {
                        // CHR bank registers
                        self.chr_banks[self.reg_index as usize] = val;
                    }
                    8 => {
                        // PRG bank 0 ($8000)
                        self.prg_banks[0] = val & 0x3F;
                    }
                    9 => {
                        // PRG bank 1 ($A000)
                        self.prg_banks[1] = val & 0x3F;
                    }
                    10 => {
                        // PRG bank 2 ($C000)
                        self.prg_banks[2] = val & 0x3F;
                    }
                    11 => {
                        // PRG bank 3 ($E000) - typically fixed to last
                        self.prg_banks[3] = val & 0x3F;
                    }
                    12 => {
                        // Mirroring
                        match val & 0x03 {
                            0 => self.mirror = 0, // horizontal
                            1 => self.mirror = 1, // vertical
                            2 => self.mirror = 3, // one-screen A
                            3 => self.mirror = 4, // one-screen B
                            _ => {}
                        }
                    }
                    13 => {
                        // IRQ latch (low 8 bits of reload value)
                        self.irq_reload = (self.irq_reload & 0xFF00) | u16::from(val);
                    }
                    14 => {
                        // IRQ control
                        // bit 6: mode (0=normal, 1=alternate)
                        // bit 7: enable/disable
                        // Writing also acknowledges IRQ
                        self.irq_enabled = (val & 0x80) != 0;
                        self.irq_pending_flag = false;
                        if val & 0x01 != 0 {
                            // bit 0 = reset counter
                            self.irq_counter = self.irq_reload;
                        }
                        if val & 0x02 != 0 {
                            // bit 1 = IRQ acknowledge
                            self.irq_pending_flag = false;
                        }
                    }
                    15 => {
                        // Audio channel select + data — skipped
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.has_chr_ram {
            return self.chr[a as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }
        let slot = (a as usize) / 0x0400;
        let offset = (a as usize) & 0x03FF;
        let bank = self.chr_banks[slot] as usize;
        let idx = (bank * 0x0400 + offset) % self.chr.len();
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.has_chr_ram {
            let a = addr & 0x1FFF;
            self.chr[a as usize] = val;
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending_flag
    }

    fn ack_irq(&mut self) {
        self.irq_pending_flag = false;
    }

    fn clock_scanline(&mut self) {
        // FME-7 scanline IRQ: counter is decremented on each scanline.
        // When counter reaches 0, IRQ fires and counter reloads.
        if self.irq_counter == 0 {
            self.irq_counter = self.irq_reload;
        } else {
            self.irq_counter -= 1;
            if self.irq_counter == 0 && self.irq_enabled {
                self.irq_pending_flag = true;
            }
        }
    }

    fn has_chr_ram(&self) -> bool {
        self.has_chr_ram
    }
}
