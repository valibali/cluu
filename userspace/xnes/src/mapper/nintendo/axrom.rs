use crate::mapper::MapperImpl;
use alloc::vec::Vec;

pub struct Axrom {
    prg: Vec<u8>,
    chr: [u8; 0x2000],
    chr_ram: bool,
    mirror: u8,
    bank: u8,
}

impl Axrom {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut c = [0u8; 0x2000];
        if !chr_ram && !chr.is_empty() {
            let clen = core::cmp::min(chr.len(), 0x2000);
            c[..clen].copy_from_slice(&chr[..clen]);
        }
        Self {
            prg: prg.to_vec(),
            chr: c,
            chr_ram,
            mirror,
            bank: 0,
        }
    }
}

impl MapperImpl for Axrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                let bank = self.bank as usize;
                let off = (addr & 0x7FFF) as usize;
                self.prg[(bank * 0x8000 + off) % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.bank = val & 0x07;
            self.mirror = u8::from(val & 0x10 != 0) * 2;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        match addr & 0x3FFF {
            a @ 0x0000..=0x1FFF => self.chr[a as usize],
            _ => 0,
        }
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
