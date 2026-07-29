use super::super::MapperImpl;
use alloc::vec::Vec;

/// Sunsoft 93 (mapper 93).
///
/// Simple PRG/CHR banking.
///
/// Registers:
///   $8000-$FFFF write:
///     bits 0-3: selects one of 16 CHR banks (8KB @ PPU $0000)
///     bit 4: selects PRG bank (0 or 1, 16KB at $8000)
pub struct Sunsoft93 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    reg: u8,
}

impl Sunsoft93 {
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
            reg: 0,
        }
    }
}

impl MapperImpl for Sunsoft93 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        match addr {
            0x8000..=0xBFFF => {
                // 16KB switchable bank (bit 4 selects between 2 banks)
                let bank = ((self.reg >> 4) & 1) as usize;
                let idx = bank * 0x4000 + (addr as usize & 0x3FFF);
                self.prg[idx % prg_len]
            }
            0xC000..=0xFFFF => {
                // 16KB fixed last bank
                let bank = (prg_len / 0x4000).saturating_sub(1);
                let idx = bank * 0x4000 + (addr as usize & 0x3FFF);
                self.prg[idx % prg_len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x8000..=0xFFFF = addr {
            self.reg = val;
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
        let bank = (self.reg & 0x0F) as usize;
        let idx = (bank * 0x2000 + a as usize) % self.chr.len();
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
