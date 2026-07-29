use alloc::vec::Vec;
use alloc::vec;
use super::super::MapperImpl;
use crate::mapper::common::ScanlineIrq;
use crate::mapper::common::mirror;

/// Konami VRC7 (iNES mapper 85)
///
/// - PRG: eight 8 KB bank slots (`$8000`, `$A000`, `$C000`, `$E000`),
///        but typically two 16 KB or 8 KB banks are switched
/// - CHR: eight 1 KB banks
/// - Scanline IRQ
/// - FM synthesis audio (Yamaha YM2413) – **not yet implemented**
/// - Mirroring control
pub struct Vrc7 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,

    // PRG bank select (8 KB banks)
    prg_banks: [u8; 4],

    // CHR bank select (8 banks of 1 KB each)
    chr_banks: [u8; 8],

    // IRQ
    irq: ScanlineIrq,
    prev_a12: bool,
    irq_ack_reload: bool,
}

impl Vrc7 {
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
            prg_banks: [0, 1, 2, 3],
            chr_banks: [0; 8],
            irq: ScanlineIrq::new(),
            prev_a12: false,
            irq_ack_reload: false,
        }
    }

    fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            // PRG banks (8 KB each)
            0x8000 => self.prg_banks[0] = val & 0x3F,
            0x8010..=0x801F => self.prg_banks[0] = val & 0x3F,
            0xA000 => self.prg_banks[1] = val & 0x3F,
            0xA010..=0xA01F => self.prg_banks[1] = val & 0x3F,
            0xC000 => self.prg_banks[2] = val & 0x3F,
            0xC010..=0xC01F => self.prg_banks[2] = val & 0x3F,
            0xE010..=0xE01F => self.prg_banks[3] = val & 0x3F,

            // CHR banks (1 KB each)
            0xB000 => self.chr_banks[0] = val,
            0xB010..=0xB01F => self.chr_banks[0] = val,
            0xB001 => self.chr_banks[1] = val,
            0xB002 => self.chr_banks[2] = val,
            0xB003 => self.chr_banks[3] = val,
            0xB004 => self.chr_banks[4] = val,
            0xB005 => self.chr_banks[5] = val,
            0xB006 => self.chr_banks[6] = val,
            0xB007 => self.chr_banks[7] = val,

            // Mirroring
            0x9000 => {
                self.mirror = if val & 1 != 0 {
                    mirror::HORIZONTAL
                } else {
                    mirror::VERTICAL
                };
            }

            // IRQ control
            // Note: 0xE010..=0xE01F is handled by the PRG bank arm above
            0xE000 => {
                // IRQ reload
                self.irq.write_reload(val);
            }
            0xE001 => {
                // IRQ acknowledge / enable
                self.irq.write_control(val);
                self.irq.ack();
            }
            // YM2413 audio registers (stub – not yet implemented)
            0x9010..=0x902F => {
                // Audio register select / data – ignored for now
            }

            _ => {}
        }
    }
}

impl MapperImpl for Vrc7 {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.prg.is_empty() {
            return 0;
        }
        let prg_len = self.prg.len();
        let offset = (addr & 0x1FFF) as usize;
        match addr {
            0x8000..=0x9FFF => {
                let bank = self.prg_banks[0] as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xA000..=0xBFFF => {
                let bank = self.prg_banks[1] as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xC000..=0xDFFF => {
                let bank = self.prg_banks[2] as usize;
                self.prg[(bank * 0x2000 + offset) % prg_len]
            }
            0xE000..=0xFFFF => {
                let bank = self.prg_banks[3] as usize;
                // If bank 3 is 0 (default/fallback), use the last bank
                if bank == 0 {
                    let last_bank = (prg_len / 0x2000).saturating_sub(1);
                    self.prg[last_bank * 0x2000 + offset]
                } else {
                    self.prg[(bank * 0x2000 + offset) % prg_len]
                }
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr >= 0x8000 {
            self.write_register(addr, val);
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

        // A12 edge detection for scanline IRQ
        if self.irq.a12_edge(a, &mut self.prev_a12) {
            self.irq.clock();
        }

        let bank_idx = (a as usize) / 0x0400;
        let offset = (a as usize) % 0x0400;
        let bank = self.chr_banks[bank_idx % 8] as usize;
        let idx = (bank * 0x0400 + offset) % self.chr.len();
        self.chr[idx]
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        if self.chr_ram {
            let a = addr & 0x1FFF;
            if self.chr.is_empty() {
                return;
            }

            // A12 edge detection
            if self.irq.a12_edge(a, &mut self.prev_a12) {
                self.irq.clock();
            }

            // CHR RAM write: direct addressing with banking
            let bank_idx = (a as usize) / 0x0400;
            let offset = (a as usize) % 0x0400;
            let bank = self.chr_banks[bank_idx % 8] as usize;
            let idx = (bank * 0x0400 + offset) % self.chr.len();
            self.chr[idx] = val;
        }
    }

    fn mirroring(&self) -> u8 {
        self.mirror
    }

    fn irq_pending(&self) -> bool {
        self.irq.pending
    }

    fn ack_irq(&mut self) {
        self.irq.ack();
    }

    fn clock_scanline(&mut self) {
        self.irq.clock();
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
