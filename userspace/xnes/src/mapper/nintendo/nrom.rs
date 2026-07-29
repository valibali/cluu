use crate::mapper::MapperImpl;
use core::cmp;

pub struct Nrom {
    prg: [u8; 0x8000],
    chr: [u8; 0x2000],
    chr_ram: bool,
    mirror: u8,
    prg_size: usize,
    prg_ram: [u8; 0x2000],
}

impl Nrom {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let mut p = [0u8; 0x8000];
        let src = if prg.len() > 0x8000 {
            &prg[prg.len() - 0x8000..]
        } else {
            prg
        };
        p[..cmp::min(src.len(), 0x8000)].copy_from_slice(&src[..cmp::min(src.len(), 0x8000)]);
        if prg.len() <= 0x4000 {
            let (lo, hi) = p.split_at_mut(0x4000);
            hi.copy_from_slice(lo);
        }
        let mut c = [0u8; 0x2000];
        if !chr_ram && !chr.is_empty() {
            let clen = cmp::min(chr.len(), 0x2000);
            c[..clen].copy_from_slice(&chr[..clen]);
        }
        Self {
            prg: p,
            chr: c,
            chr_ram,
            mirror,
            prg_size: prg.len(),
            prg_ram: [0; 0x2000],
        }
    }
}

impl MapperImpl for Nrom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.prg_ram[(addr & 0x1FFF) as usize],
            0x8000..=0xFFFF => {
                let idx = (addr & 0x7FFF) as usize;
                if self.prg_size <= 0x4000 && idx >= 0x4000 {
                    self.prg[idx - 0x4000]
                } else {
                    self.prg[idx]
                }
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x6000..=0x7FFF = addr {
            self.prg_ram[(addr & 0x1FFF) as usize] = val;
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
