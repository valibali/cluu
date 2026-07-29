use super::super::MapperImpl;
use alloc::vec::Vec;

/// Irem H3001 (mapper 65).
///
/// PRG: 8 KB banks, CHR: 1 KB banks.
/// Has MMC3-style scanline IRQ.
///
/// Registers:
///   $8000-$8001: bank select (MMC3-like)
///   $A000-$A001: mirroring / misc
///   $C000-$C001: IRQ latch / reload
///   $E000-$E001: IRQ disable / enable
pub struct H3001 {
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

impl H3001 {
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
            (self.prg.len() / 0x2000) as u8
        }
    }

    fn chr_bank_count(&self) -> u16 {
        if self.chr.is_empty() {
            1
        } else {
            (self.chr.len() / 0x0400) as u16
        }
    }
}

impl MapperImpl for H3001 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let prg8 = (prg_len / 0x2000).max(1);
        match addr {
            0x8000..=0x9FFF => {
                // Switchable or fixed depending on bank_select bit 6
                let bank = if self.bank_select & 0x40 != 0 {
                    (prg8 - 2) as u8 // fixed second-to-last
                } else {
                    self.prg_banks[0] // switchable
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
                // Switchable or fixed
                let bank = if self.bank_select & 0x40 != 0 {
                    self.prg_banks[0] // switchable
                } else {
                    (prg8 - 2) as u8 // fixed second-to-last
                };
                let idx = ((bank as usize) % prg8) * 0x2000 + (addr as usize & 0x1FFF);
                self.prg[idx % prg_len]
            }
            0xE000..=0xFFFF => {
                // Fixed last
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
                    // $8000: Bank select
                    self.bank_select = val;
                } else {
                    // $8001: Bank data
                    let mode = self.bank_select & 0x07;
                    let bank = val;
                    match mode {
                        0 => self.chr_banks[0] = bank & 0xFE,
                        1 => self.chr_banks[1] = bank & 0xFE,
                        2 => self.chr_banks[2] = bank,
                        3 => self.chr_banks[3] = bank,
                        4 => self.chr_banks[4] = bank,
                        5 => self.chr_banks[5] = bank,
                        6 => self.prg_banks[0] = bank & 0x3F,
                        7 => self.prg_banks[1] = bank & 0x3F,
                        _ => {}
                    }
                }
            }
            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    // $A000: Mirroring (bit 0 = 0 → vertical, 1 → horizontal)
                    self.mirror = u8::from(val & 1 == 0);
                }
                // $A001 unused
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
        if self.has_chr_ram {
            return self.chr[a as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }
        if self.chr.len() == 1 {
            return 0;
        }

        let swap = self.bank_select & 0x80 != 0;
        let ch_len = self.chr.len();

        // Determine 1KB CHR bank
        let (bank_base, _bank_size) = if a < 0x1000 {
            if swap {
                // swapped: $0000-$0FFF uses R2-R5 (modes 2-5)
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
                // $0000-$07FF: chr_banks[0] (2KB select)
                // $0800-$0FFF: chr_banks[1] (2KB select)
                let b = if a < 0x0800 {
                    self.chr_banks[0] as usize & 0xFE
                } else {
                    self.chr_banks[1] as usize & 0xFE
                };
                let sub = usize::from((a & 0x400) != 0);
                ((b | sub) * 0x400, 0x400)
            }
        } else {
            // $1000-$1FFF
            if swap {
                // swapped: $1000-$1FFF uses R0/R1 (2KB pair)
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
