use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mapper60 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    prg_bank: u8,
    chr_bank: u8,
}

impl Mapper60 {
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
            prg_bank: 0,
            chr_bank: 0,
        }
    }
}

impl MapperImpl for Mapper60 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                let bank = self.prg_bank as usize;
                let off = (addr & 0x7FFF) as usize;
                self.prg[(bank * 0x8000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.prg_bank = (val >> 4) & 0x07;
            self.chr_bank = val & 0x0F;
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
        let bank_size = 0x2000;
        let banks = (self.chr.len() / bank_size).max(1);
        let bank = (self.chr_bank as usize) % banks;
        self.chr[(bank * bank_size + a as usize) % self.chr.len()]
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
