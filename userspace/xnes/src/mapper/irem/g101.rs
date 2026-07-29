use super::super::MapperImpl;
use alloc::vec::Vec;

/// Irem G-101 (mapper 32).
///
/// PRG: 4 x 8 KB banks:
///   $8000 = switchable (register bits 4-5)
///   $A000 = fixed second-to-last
///   $C000 = switchable (register bit 6)
///   $E000 = fixed last
///
/// CHR: 8 KB bank selected by register bits 0-3.
/// Register write at $8000-$FFFF:
///   bits 0-3 = CHR bank (8 KB)
///   bits 4-5 = PRG bank at $8000
///   bit 6    = PRG bank at $C000
pub struct G101 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    reg: u8,
    prg_bank_count: u8,
}

impl G101 {
    pub fn new(prg: &[u8], chr: &[u8], chr_ram: bool, mirror: u8) -> Self {
        let prg_bank_count = if prg.is_empty() {
            1
        } else {
            (prg.len() / 0x2000).max(1) as u8
        };
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
            prg_bank_count,
        }
    }

    fn prg_addr_8k(&self, bank: u8, addr: u16) -> usize {
        let banks = self.prg_bank_count as usize;
        let b = (bank as usize) % banks;
        (b * 0x2000 + (addr as usize & 0x1FFF)) % self.prg.len()
    }
}

impl MapperImpl for G101 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0x9FFF => {
                let bank = (self.reg >> 4) & 0x03;
                let idx = self.prg_addr_8k(bank, addr);
                self.prg[idx]
            }
            0xA000..=0xBFFF => {
                let bank = self.prg_bank_count.saturating_sub(2);
                let idx = self.prg_addr_8k(bank, addr);
                self.prg[idx]
            }
            0xC000..=0xDFFF => {
                let bank = (self.reg >> 6) & 0x01;
                let idx = self.prg_addr_8k(bank, addr);
                self.prg[idx]
            }
            0xE000..=0xFFFF => {
                let bank = self.prg_bank_count.saturating_sub(1);
                let idx = self.prg_addr_8k(bank, addr);
                self.prg[idx]
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
