use super::super::MapperImpl;
use alloc::vec::Vec;

/// Irem TAM-S1 (mapper 97).
///
/// PRG: 8 KB banks, CHR: 1 KB banks.
/// Has IRQ.
///
/// Register layout (MMC3-like with different IRQ timing):
///   $8000-$8001: bank select
///   $A000-$A001: mirroring / misc
///   $C000-$C001: IRQ latch / reload
///   $E000-$E001: IRQ disable / enable
pub struct TamS1 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    // Bank select
    bank_select: u8,
    // PRG banks (8 KB each): 0=R6 ($8000 or $C000), 1=R7 ($A000)
    prg_banks: [u8; 2],
    // CHR banks (1 KB each): 8 slots
    chr_banks: [u8; 8],
    // IRQ
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_reload: bool,
    irq_flag: bool,
    has_chr_ram: bool,
}

impl TamS1 {
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
            bank_select: 0,
            prg_banks: [0; 2],
            chr_banks: [0; 8],
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_reload: false,
            irq_flag: false,
            has_chr_ram: chr_ram,
        }
    }

    fn prg_bank_count(&self) -> u8 {
        if self.prg.is_empty() {
            1
        } else {
            (self.prg.len() / 0x2000).max(1) as u8
        }
    }
}

impl MapperImpl for TamS1 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let prg8 = (prg_len / 0x2000).max(1);
        match addr {
            0x8000..=0x9FFF => {
                let bank = if self.bank_select & 0x40 != 0 {
                    (prg8 - 2) as u8
                } else {
                    self.prg_banks[0]
                };
                let idx = ((bank as usize) % prg8) * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xA000..=0xBFFF => {
                let bank = self.prg_banks[1];
                let idx = ((bank as usize) % prg8) * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xC000..=0xDFFF => {
                let bank = if self.bank_select & 0x40 != 0 {
                    self.prg_banks[0]
                } else {
                    (prg8 - 2) as u8
                };
                let idx = ((bank as usize) % prg8) * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xE000..=0xFFFF => {
                let bank = (prg8 - 1) as u8;
                let idx = ((bank as usize) % prg8) * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x9FFF => {
                if addr & 1 == 0 {
                    self.bank_select = val;
                } else {
                    let mode = self.bank_select & 0x07;
                    match mode {
                        0 => self.chr_banks[0] = val & 0xFE,
                        1 => self.chr_banks[1] = val & 0xFE,
                        2 => self.chr_banks[2] = val,
                        3 => self.chr_banks[3] = val,
                        4 => self.chr_banks[4] = val,
                        5 => self.chr_banks[5] = val,
                        6 => self.prg_banks[0] = val & 0x3F,
                        7 => self.prg_banks[1] = val & 0x3F,
                        _ => {}
                    }
                }
            }
            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    // Mirroring: bit 0 = 0 → vertical, 1 → horizontal
                    self.mirror = u8::from(val & 1 == 0);
                }
            }
            0xC000..=0xDFFF => {
                if addr & 1 == 0 {
                    self.irq_latch = val;
                } else {
                    self.irq_counter = self.irq_latch;
                    self.irq_reload = false;
                }
            }
            0xE000..=0xFFFF => {
                if addr & 1 == 0 {
                    self.irq_enabled = false;
                    self.irq_flag = false;
                } else {
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
        if self.has_chr_ram {
            return self.chr[a as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }
        let ch_len = self.chr.len();
        let swap = self.bank_select & 0x80 != 0;

        let (bank_base, _bank_size) = if a < 0x1000 {
            if swap {
                let idx = if a < 0x0400 {
                    2
                } else if a < 0x0800 {
                    3
                } else if a < 0x0C00 {
                    4
                } else {
                    5
                };
                (self.chr_banks[idx] as usize * 0x400, 0x400)
            } else {
                let b = if a < 0x0800 {
                    self.chr_banks[0] as usize & 0xFE
                } else {
                    self.chr_banks[1] as usize & 0xFE
                };
                let sub = usize::from((a & 0x400) != 0);
                ((b | sub) * 0x400, 0x400)
            }
        } else {
            if swap {
                let b = if a < 0x1800 {
                    self.chr_banks[0] as usize & 0xFE
                } else {
                    self.chr_banks[1] as usize & 0xFE
                };
                let sub = usize::from((a & 0x400) != 0);
                ((b | sub) * 0x400, 0x400)
            } else {
                let idx = match (a >> 10) & 3 {
                    0 => 2,
                    1 => 3,
                    2 => 4,
                    _ => 5,
                };
                (self.chr_banks[idx] as usize * 0x400, 0x400)
            }
        };

        let chr_idx = (bank_base + (a as usize & 0x3FF)) % ch_len;
        self.chr[chr_idx]
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
        self.has_chr_ram
    }
}
