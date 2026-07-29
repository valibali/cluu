use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Rambo1 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    prg_banks: [u8; 4],
    chr_banks: [u8; 8],
    bank_select: u8,
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_flag: bool,
    prg_ram: [u8; 0x2000],
}

impl Rambo1 {
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
            prg_banks: [0; 4],
            chr_banks: [0; 8],
            bank_select: 0,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
            prg_ram: [0; 0x2000],
        }
    }

    fn prg_bank_count(&self) -> u8 {
        (self.prg.len() / 0x2000).max(1) as u8
    }

    fn chr_bank_count(&self) -> u8 {
        if self.chr.is_empty() {
            return 1;
        }
        (self.chr.len() / 0x400).max(1) as u8
    }
}

impl MapperImpl for Rambo1 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[(addr & 0x1FFF) as usize],
            0x8000..=0x9FFF => {
                let bank = if self.bank_select & 0x40 != 0 {
                    (self.prg_bank_count() - 2) as usize
                } else {
                    self.prg_banks[2] as usize
                };
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xA000..=0xBFFF => {
                let bank = self.prg_banks[3] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xC000..=0xDFFF => {
                let bank = if self.bank_select & 0x40 != 0 {
                    self.prg_banks[2] as usize
                } else {
                    (self.prg_bank_count() - 2) as usize
                };
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xE000..=0xFFFF => {
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
                self.prg_ram[(addr & 0x1FFF) as usize] = val;
            }
            0x8000..=0x9FFF => {
                if addr & 1 == 0 {
                    self.bank_select = val;
                } else {
                    let mode = self.bank_select & 0x07;
                    let bank = val;
                    match mode {
                        0 => self.chr_banks[0] = bank & 0xFE,
                        1 => self.chr_banks[1] = bank & 0xFE,
                        2 => self.chr_banks[2] = bank,
                        3 => self.chr_banks[3] = bank,
                        4 => self.chr_banks[4] = bank,
                        5 => self.chr_banks[5] = bank,
                        6 => self.prg_banks[2] = bank & 0x3F,
                        7 => self.prg_banks[3] = bank & 0x3F,
                        _ => {}
                    }
                }
            }
            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    self.mirror = u8::from(val & 1 == 0);
                }
            }
            0xC000..=0xDFFF => {
                if addr & 1 == 0 {
                    self.irq_latch = val;
                } else {
                    self.irq_counter = self.irq_latch;
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
        if a >= 0x2000 || self.chr.is_empty() {
            return 0;
        }
        if self.chr_ram {
            return self.chr[a as usize];
        }
        let swap = self.bank_select & 0x80 != 0;
        let ch_len = self.chr.len();

        let (bank_base, _) = if a < 0x1000 {
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
        } else if swap {
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
        };

        let chr_idx = (bank_base + (a as usize & 0x3FF)) % ch_len;
        self.chr[chr_idx]
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
        self.irq_flag
    }

    fn ack_irq(&mut self) {
        self.irq_flag = false;
    }

    fn clock_scanline(&mut self) {
        // Rambo1/MMC3-like IRQ: different behavior - counts down with reload
        if self.irq_counter == 0 {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
            if self.irq_counter == 0 && self.irq_enabled {
                self.irq_flag = true;
            }
        }
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
