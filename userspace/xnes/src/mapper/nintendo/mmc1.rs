use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mmc1 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    // internal registers
    shift: u8,
    shift_count: u8,
    control: u8,  // $8000
    chr0: u8,     // $A000
    chr1: u8,     // $C000
    prg_bank: u8, // $E000
    prg_ram: [u8; 0x2000],
    prg_ram_enable: bool,
    has_chr_ram: bool,
}

impl Mmc1 {
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
            shift: 0,
            shift_count: 0,
            control: 0x0C,
            chr0: 0,
            chr1: 0,
            prg_bank: 0,
            prg_ram: [0; 0x2000],
            prg_ram_enable: true,
            has_chr_ram: chr_ram,
        }
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        // MMC1 uses serial loading: shift in bit 0 of val, when bit 4 high -> store
        if val & 0x80 != 0 {
            // Reset shift
            self.shift = 0;
            self.shift_count = 0;
            self.control |= 0x0C;
            return;
        }

        self.shift >>= 1;
        self.shift |= (val & 1) << 4;
        self.shift_count += 1;

        if self.shift_count < 5 {
            return;
        }

        // 5 bits received – write to the target register
        match addr & 0xE000 {
            0x8000 => {
                self.control = self.shift;
                // Ignore mirroring bits from control register;
                // use the iNES header mirroring (set in new())
            }
            0xA000 => self.chr0 = (self.shift) & 0x1F,
            0xC000 => self.chr1 = (self.shift) & 0x1F,
            0xE000 => {
                self.prg_bank = self.shift & 0x0F;
                self.prg_ram_enable = (self.shift & 0x10) == 0;
            }
            _ => {}
        }
        self.shift = 0;
        self.shift_count = 0;
    }

    fn prg_mode(&self) -> u8 {
        (self.control >> 2) & 3
    }

    fn chr_mode(&self) -> bool {
        (self.control & 0x10) != 0
    }
}

impl MapperImpl for Mmc1 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enable {
                    self.prg_ram[(addr & 0x1FFF) as usize]
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => {
                let prg_len = self.prg.len();
                if prg_len == 0 {
                    return 0;
                }
                let prg_mask = prg_len - 1;
                let off = (addr & 0x7FFF) as usize;
                let bank_mode = self.prg_mode();
                match bank_mode {
                    0 | 1 => {
                        // 32 KB switch – ignore low bit of bank number
                        let bank = (self.prg_bank & 0x0E) as usize;
                        self.prg[(bank * 0x8000 + off) & prg_mask]
                    }
                    2 => {
                        // fixed first bank at $8000, switch at $C000
                        if addr < 0xC000 {
                            self.prg[off & prg_mask]
                        } else {
                            let bank = self.prg_bank as usize;
                            self.prg[(bank * 0x4000 + (off & 0x3FFF)) & prg_mask]
                        }
                    }
                    3 => {
                        // switch at $8000, fixed last bank at $C000
                        if addr < 0xC000 {
                            let bank = self.prg_bank as usize;
                            self.prg[(bank * 0x4000 + (off & 0x3FFF)) & prg_mask]
                        } else {
                            self.prg[(prg_len - 0x4000 + (off & 0x3FFF)) & prg_mask]
                        }
                    }
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enable {
                    self.prg_ram[(addr & 0x1FFF) as usize] = val;
                }
            }
            0x8000..=0xFFFF => self.write_register(addr, val),
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        let mode_4k = self.chr_mode();
        if self.has_chr_ram {
            return self.chr[a as usize];
        }
        let ch = &self.chr;
        if ch.is_empty() {
            return 0;
        }
        let ch_mask = ch.len() - 1;
        if mode_4k {
            // Two 4KB banks
            if a < 0x1000 {
                let bank = (self.chr0 as usize) * 0x1000;
                ch[(bank + a as usize) & ch_mask]
            } else {
                let bank = (self.chr1 as usize) * 0x1000;
                ch[(bank + (a as usize & 0xFFF)) & ch_mask]
            }
        } else {
            // One 8KB bank
            let bank = (self.chr0 as usize & 0x1E) * 0x1000;
            ch[(bank + a as usize) & ch_mask]
        }
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.has_chr_ram {
            let a = addr & 0x1FFF;
            self.chr[a as usize] = val;
        }
    }

    fn mirroring(&self) -> u8 {
        // MMC1 control register bits 1-0 control mirroring:
        //   0 = one-screen (lower bank)
        //   1 = one-screen (upper bank)
        //   2 = vertical
        //   3 = horizontal
        // Our mirroring convention: 0 = horizontal, 1 = vertical
        match self.control & 3 {
            2 => 1,           // vertical
            3 => 0,           // horizontal
            _ => self.mirror, // one-screen modes (fallback to header)
        }
    }
    fn irq_pending(&self) -> bool {
        false
    }
    fn ack_irq(&mut self) {}
    fn has_chr_ram(&self) -> bool {
        self.has_chr_ram
    }
}
