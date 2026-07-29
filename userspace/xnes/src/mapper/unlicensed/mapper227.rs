use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mapper227 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    bank: u8,
}

impl Mapper227 {
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
            bank: 0,
        }
    }
}

impl MapperImpl for Mapper227 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                // 1200-in-1: uses bank value to select 16KB PRG bank
                let bank = self.bank as usize;
                let off = (addr & 0x3FFF) as usize;
                self.prg[(bank * 0x4000 + off) % self.prg.len()]
            }
            0xC000..=0xFFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                // Upper 16KB fixed to last bank
                let bank = (self.prg.len() / 0x4000).saturating_sub(1);
                let off = (addr & 0x3FFF) as usize;
                self.prg[bank * 0x4000 + off]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.bank = val;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 || self.chr.is_empty() {
            return 0;
        }
        self.chr[a as usize % self.chr.len()]
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
