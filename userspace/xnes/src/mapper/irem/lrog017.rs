use super::super::MapperImpl;
use alloc::vec::Vec;

/// Irem LROG017 (mapper 77).
///
/// Very simple mapper:
///   - 32 KB PRG fixed at $8000
///   - CHR is 8 KB at PPU $0000, selectable between 2 banks via bit 0
///   - Write at $8000-$FFFF selects CHR bank
///   - No IRQ
pub struct Lrog017 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    chr_bank: u8,
}

impl Lrog017 {
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
            chr_bank: 0,
        }
    }
}

impl MapperImpl for Lrog017 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                let idx = (addr & 0x7FFF) as usize;
                if self.prg.is_empty() {
                    return 0;
                }
                self.prg[idx % self.prg.len()]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.chr_bank = val & 1;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let a = addr & 0x3FFF;
        if a >= 0x2000 {
            return 0;
        }
        if self.chr_ram {
            return self.chr[a as usize];
        }
        if self.chr.is_empty() {
            return 0;
        }
        let bank = (self.chr_bank as usize) * 0x2000;
        let idx = (bank + a as usize) % self.chr.len();
        self.chr[idx]
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
