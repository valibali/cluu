use super::super::MapperImpl;
use crate::mapper::common::mirror;
use alloc::vec::Vec;

/// UNROM-94 (mapper 94).
///
/// Like UNROM but with 6-bit bank register:
/// - 16 KB switchable bank at $8000 (writes to $8000-$FFFF select bank, bits 0-5)
/// - Fixed 16 KB bank at $C000 (last bank)
/// - Mirroring controlled via bit 6 of the bank register
/// - No CHR banking
pub struct UnRom94 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    bank: u8,
}

impl UnRom94 {
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

impl MapperImpl for UnRom94 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF => {
                let off = (addr & 0x3FFF) as usize;
                let bank_idx = (self.bank & 0x3F) as usize;
                self.prg[bank_idx * 0x4000 + off]
            }
            0xC000..=0xFFFF => {
                let off = (addr & 0x3FFF) as usize;
                let last = (self.prg.len() / 0x4000).saturating_sub(1);
                self.prg[last * 0x4000 + off]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.bank = val & 0x7F;
            // Bit 6 controls mirroring: 0 = vertical, 1 = horizontal
            self.mirror = if val & 0x40 != 0 {
                mirror::HORIZONTAL
            } else {
                mirror::VERTICAL
            };
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
