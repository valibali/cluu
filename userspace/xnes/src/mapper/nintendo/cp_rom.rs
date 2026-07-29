use super::super::MapperImpl;
use crate::mapper::common;
use alloc::vec::Vec;

/// Nintendo CpROM (mapper 13).
///
/// - 16 KB PRG ROM, fixed at $8000-$BFFF, mirrored at $C000-$FFFF
/// - CHR banking: switches 2 KB banks at PPU $0000-$07FF and $0800-$0FFF
///   via writes to $C000-$FFFF (bits 0-2 select low bank, bits 3-5 select high bank)
/// - No IRQ
pub struct CpRom {
    prg: Vec<u8>,
    chr: common::ChrMem, // bank_size = 0x0800 (2 KB)
    chr_ram: bool,
    mirror: u8,
    chr_bank_lo: u8, // bits 0-2: 2 KB bank for PPU $0000-$07FF
    chr_bank_hi: u8, // bits 3-5: 2 KB bank for PPU $0800-$0FFF
}

impl CpRom {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        Self {
            prg: prg.to_vec(),
            chr: common::ChrMem::new(chr, chr_ram, 0x0800),
            chr_ram,
            mirror,
            chr_bank_lo: 0,
            chr_bank_hi: 0,
        }
    }
}

impl MapperImpl for CpRom {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                if self.prg.is_empty() {
                    return 0;
                }
                // 16 KB PRG fixed, mirrored across $8000-$FFFF
                self.prg[(addr & 0x3FFF) as usize % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0xC000..=0xFFFF = addr {
            self.chr_bank_lo = val & 0x07;
            self.chr_bank_hi = (val >> 3) & 0x07;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.chr.total_size() == 0 {
            return 0;
        }
        if a < 0x0800 {
            // PPU $0000-$07FF: low 2 KB bank
            self.chr.read(self.chr_bank_lo as usize, a as usize)
        } else if a < 0x1000 {
            // PPU $0800-$0FFF: high 2 KB bank
            self.chr
                .read(self.chr_bank_hi as usize, (a - 0x0800) as usize)
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x3FFF;
            if a < 0x0800 {
                self.chr.write(self.chr_bank_lo as usize, a as usize, val);
            } else if a < 0x1000 {
                self.chr
                    .write(self.chr_bank_hi as usize, (a - 0x0800) as usize, val);
            }
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
