use super::super::MapperImpl;
use crate::mapper::common;
use alloc::vec::Vec;

/// Nintendo TxSROM (mapper 118).
///
/// MMC3 clone with CHR RAM:
/// - PRG: 8 KB switchable banks (R6=$8000, R7=$A000, fixed $C000=2nd-to-last, $E000=last)
/// - CHR: 8 KB CHR RAM via ChrBanked (8 x 1 KB slots)
/// - MMC3-style scanline IRQ
pub struct TxSRom {
    prg: Vec<u8>,
    chr: common::ChrBanked, // 8 slots, 1 KB each
    chr_ram: bool,
    mirror: u8,
    // Bank select
    bank_select: u8, // $8000
    // PRG banks: R6, R7
    prg_banks: [u8; 2],
    // MMC3 CHR bank registers (R0-R5)
    // R0/R1: 2KB banks, R2-R5: 1KB banks
    chr_registers: [u8; 6],
    // IRQ (MMC3-style)
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_reload: bool,
    irq_flag: bool,
    // PRG RAM
    prg_ram: [u8; 0x2000],
    prg_ram_enable: bool,
    prg_ram_write: bool,
}

impl TxSRom {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        // TxSROM always has CHR RAM (even if iNES header says otherwise)
        Self {
            prg: prg.to_vec(),
            chr: common::ChrBanked::new(
                if chr_ram || chr.is_empty() {
                    &[0u8; 0x2000]
                } else {
                    chr
                },
                true,   // always writable RAM
                8,      // 8 slots
                0x0400, // slot size = 1 KB
                0x0400, // bank size = 1 KB
            ),
            chr_ram: true,
            mirror,
            bank_select: 0,
            prg_banks: [0; 2],
            chr_registers: [0; 6],
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_reload: false,
            irq_flag: false,
            prg_ram: [0; 0x2000],
            prg_ram_enable: false,
            prg_ram_write: false,
        }
    }

    fn prg_bank_count(&self) -> u8 {
        (self.prg.len() / 0x2000) as u8
    }

    /// Update ChrBanked slots from the MMC3 CHR registers.
    fn update_chr_banks(&mut self) {
        let swap = self.bank_select & 0x80 != 0;
        if swap {
            // Swapped: $0000-$0FFF uses R2-R5 (4x1KB), $1000-$1FFF uses R0/R1 (2x2KB)
            self.chr.set_bank(0, self.chr_registers[2] as usize);
            self.chr.set_bank(1, self.chr_registers[3] as usize);
            self.chr.set_bank(2, self.chr_registers[4] as usize);
            self.chr.set_bank(3, self.chr_registers[5] as usize);
            let b0 = (self.chr_registers[0] as usize) & 0xFE;
            self.chr.set_bank(4, b0);
            self.chr.set_bank(5, b0 | 1);
            let b1 = (self.chr_registers[1] as usize) & 0xFE;
            self.chr.set_bank(6, b1);
            self.chr.set_bank(7, b1 | 1);
        } else {
            // Unswapped: $0000-$0FFF uses R0/R1 (2x2KB), $1000-$1FFF uses R2-R5 (4x1KB)
            let b0 = (self.chr_registers[0] as usize) & 0xFE;
            self.chr.set_bank(0, b0);
            self.chr.set_bank(1, b0 | 1);
            let b1 = (self.chr_registers[1] as usize) & 0xFE;
            self.chr.set_bank(2, b1);
            self.chr.set_bank(3, b1 | 1);
            self.chr.set_bank(4, self.chr_registers[2] as usize);
            self.chr.set_bank(5, self.chr_registers[3] as usize);
            self.chr.set_bank(6, self.chr_registers[4] as usize);
            self.chr.set_bank(7, self.chr_registers[5] as usize);
        }
    }
}

impl MapperImpl for TxSRom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enable {
                    self.prg_ram[(addr & 0x1FFF) as usize]
                } else {
                    0
                }
            }
            0x8000..=0x9FFF => {
                // R6 at $8000 (switchable)
                let bank = self.prg_banks[0] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xA000..=0xBFFF => {
                // R7 at $A000 (switchable)
                let bank = self.prg_banks[1] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xC000..=0xDFFF => {
                // Fixed to 2nd-to-last bank
                let bank = (self.prg_bank_count() - 2) as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xE000..=0xFFFF => {
                // Fixed to last bank
                let bank = (self.prg_bank_count() - 1) as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enable && !self.prg_ram_write {
                    self.prg_ram[(addr & 0x1FFF) as usize] = val;
                }
            }
            0x8000..=0x9FFF => {
                if addr & 1 == 0 {
                    // $8000: Bank select
                    self.bank_select = val & 0xE7;
                    self.update_chr_banks();
                } else {
                    // $8001: Bank data
                    let mode = self.bank_select & 0x07;
                    match mode {
                        0 => {
                            self.chr_registers[0] = val & 0xFE;
                            self.update_chr_banks();
                        }
                        1 => {
                            self.chr_registers[1] = val & 0xFE;
                            self.update_chr_banks();
                        }
                        2 => {
                            self.chr_registers[2] = val;
                            self.update_chr_banks();
                        }
                        3 => {
                            self.chr_registers[3] = val;
                            self.update_chr_banks();
                        }
                        4 => {
                            self.chr_registers[4] = val;
                            self.update_chr_banks();
                        }
                        5 => {
                            self.chr_registers[5] = val;
                            self.update_chr_banks();
                        }
                        6 => {
                            // R6
                            self.prg_banks[0] = val & 0x3F;
                        }
                        7 => {
                            // R7
                            self.prg_banks[1] = val & 0x3F;
                        }
                        _ => {}
                    }
                }
            }
            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    // $A000: Mirroring
                    self.mirror = u8::from(val & 1 == 0);
                } else {
                    // $A001: PRG RAM control
                    self.prg_ram_enable = val & 0x80 != 0;
                    self.prg_ram_write = val & 0x40 != 0;
                }
            }
            0xC000..=0xDFFF => {
                if addr & 1 == 0 {
                    // $C000: IRQ latch
                    self.irq_latch = val;
                } else {
                    // $C001: IRQ reload
                    self.irq_counter = self.irq_latch;
                    self.irq_reload = false;
                }
            }
            0xE000..=0xFFFF => {
                if addr & 1 == 0 {
                    // $E000: IRQ disable + acknowledge
                    self.irq_enabled = false;
                    self.irq_flag = false;
                } else {
                    // $E001: IRQ enable
                    self.irq_enabled = true;
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
        self.chr.read(a)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        let a = addr & 0x3FFF;
        if a < 0x2000 {
            self.chr.write(a, val);
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }

    fn irq_pending(&self) -> bool {
        self.irq_flag
    }

    fn ack_irq(&mut self) {
        self.irq_flag = false;
    }

    fn clock_scanline(&mut self) {
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
            self.irq_reload = false;
        } else {
            self.irq_counter -= 1;
        }
        if self.irq_counter == 0 && self.irq_enabled {
            self.irq_flag = true;
        }
    }

    fn has_chr_ram(&self) -> bool {
        true
    }
}
