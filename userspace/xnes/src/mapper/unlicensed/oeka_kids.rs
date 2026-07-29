use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct OekaKids {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    prg_banks: [u8; 4],
    chr_banks: [u8; 8],
    irq_counter: u16,
    irq_enabled: bool,
    irq_flag: bool,
}

impl OekaKids {
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
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
        }
    }
}

impl MapperImpl for OekaKids {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => 0,
            0x8000..=0x9FFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                let bank = self.prg_banks[0] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xA000..=0xBFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                let bank = self.prg_banks[1] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xC000..=0xDFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                let bank = self.prg_banks[2] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            0xE000..=0xFFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                let bank = self.prg_banks[3] as usize;
                let off = (addr & 0x1FFF) as usize;
                self.prg[(bank * 0x2000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                self.irq_counter = (self.irq_counter & 0xFF00) | val as u16;
            }
            0x8000..=0x9FFF => {
                self.irq_counter = (self.irq_counter & 0x00FF) | ((val as u16) << 8);
                self.irq_enabled = true;
                self.irq_flag = false;
            }
            0xA000..=0xBFFF => {
                self.prg_banks[0] = val & 0x07;
                self.prg_banks[1] = (val >> 4) & 0x07;
            }
            0xC000..=0xDFFF => {
                self.prg_banks[2] = val & 0x07;
                self.prg_banks[3] = (val >> 4) & 0x07;
            }
            0xE000..=0xFFFF => {
                let idx = (addr >> 10) as usize & 0x07;
                if idx < 8 {
                    self.chr_banks[idx] = val;
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
        if self.chr.is_empty() {
            return 0;
        }
        if self.chr_ram {
            return self.chr[a as usize];
        }
        // 8 KB CHR address space with 1 KB banks
        // $0000-$03FF: chr_banks[0], $0400-$07FF: chr_banks[1], etc.
        let slot = (a as usize) / 0x0400;
        let off = (a as usize) & 0x03FF;
        let bank = if slot < 8 {
            self.chr_banks[slot] as usize
        } else {
            0
        };
        let chr_len = self.chr.len();
        self.chr[(bank * 0x0400 + off) % chr_len]
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
        if self.irq_enabled && !self.irq_flag
            && self.irq_counter > 0 {
                self.irq_counter -= 1;
                if self.irq_counter == 0 {
                    self.irq_flag = true;
                }
            }
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
