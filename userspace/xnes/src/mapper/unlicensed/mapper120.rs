use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Mapper120 {
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
}

impl Mapper120 {
    pub fn new(_prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        Self {
            chr: if chr_ram || chr.is_empty() {
                alloc::vec![0u8; 0x2000]
            } else {
                chr.to_vec()
            },
            chr_ram: chr_ram || chr.is_empty(),
            mirror,
        }
    }
}

impl MapperImpl for Mapper120 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => 0,
            _ => 0,
        }
    }

    fn cpu_write(&mut self, _addr: u16, _val: u8) {}

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        self.chr[a as usize % self.chr.len()]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        let a = addr & 0x1FFF;
        self.chr[a as usize] = val;
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
