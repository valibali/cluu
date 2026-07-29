use alloc::vec::Vec;
use alloc::vec;
use super::super::MapperImpl;
use crate::mapper::common::ScanlineIrq;
use crate::mapper::common::mirror;

/// VRC6 variant selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vrc6Type {
    /// VRC6a (iNES mapper 24) – original register layout.
    Vrc6a,
    /// VRC6b (iNES mapper 26) – slightly different register layout.
    Vrc6b,
}

/// Konami VRC6 (iNES mappers 24, 26)
///
/// - PRG: four 8 KB banks at `$8000`, `$A000`, `$C000`, `$E000`
/// - CHR: eight 1 KB banks
/// - Scanline IRQ
/// - Expansion audio (2 pulse + 1 sawtooth) – **not yet implemented**
/// - Mirroring control
///
/// Variants:
/// - `Vrc6a` (mapper 24): original Konami VRC6
/// - `Vrc6b` (mapper 26): revised register layout
pub struct Vrc6 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    mirror: u8,
    /// VRC6 variant.
    pub variant: Vrc6Type,

    // PRG bank select (8 KB banks)
    prg_banks: [u8; 4],

    // CHR bank select (8 banks of 1 KB each)
    chr_banks: [u8; 8],

    // IRQ
    irq: ScanlineIrq,
    irq_ack_reload: bool,
    prev_a12: bool,
}

impl Vrc6 {
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
            variant: Vrc6Type::Vrc6a,
            prg_banks: [0, 0, 0, 0],
            chr_banks: [0; 8],
            irq: ScanlineIrq::new(),
            irq_ack_reload: false,
            prev_a12: false,
        }
    }

    /// Write to a register at the given CPU address.
    fn write_register(&mut self, addr: u16, val: u8) {
        // VRC6a vs VRC6b differ in which address ranges map to which registers.
        match self.variant {
            Vrc6Type::Vrc6a => {
                match addr {
                    // PRG banks
                    0x8000 => self.prg_banks[0] = val & 0x3F,
                    0x8001 => self.prg_banks[1] = val & 0x3F,
                    0x8002 => self.prg_banks[2] = val & 0x3F,
                    // $E000 is fixed to last bank (bank 3 is unused)
                    0x9000..=0x9003 => {
                        // Mirroring
                        self.mirror = if val & 1 != 0 {
                            mirror::HORIZONTAL
                        } else {
                            mirror::VERTICAL
                        };
                    }
                    // CHR banks (1 KB each)
                    0xB000..=0xB007 => {
                        let idx = (addr & 0x07) as usize;
                        self.chr_banks[idx] = val;
                    }
                    // IRQ control
                    0xC000 => {
                        // IRQ reload value
                        self.irq.write_reload(val);
                    }
                    0xC001 => {
                        // IRQ control
                        self.irq.write_control(val);
                    }
                    0xC002 => {
                        // IRQ acknowledge
                        self.irq.ack();
                    }
                    _ => {}
                }
            }
            Vrc6Type::Vrc6b => {
                match addr {
                    // PRG banks
                    0x8000..=0x8003 => {
                        let idx = (addr & 0x03) as usize;
                        if idx < 3 {
                            self.prg_banks[idx] = val & 0x3F;
                        }
                    }
                    // Mirroring
                    0x9000..=0x9003 => {
                        self.mirror = if val & 1 != 0 {
                            mirror::HORIZONTAL
                        } else {
                            mirror::VERTICAL
                        };
                    }
                    // CHR banks
                    0xB000..=0xB007 => {
                        let idx = (addr & 0x07) as usize;
                        self.chr_banks[idx] = val;
                    }
                    // IRQ control
                    0xC000 => self.irq.write_reload(val),
                    0xC001 => self.irq.write_control(val),
                    0xC002 => self.irq.ack(),
                    _ => {}
                }
            }
        }
    }
}

impl MapperImpl for Vrc6 {
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
                // Typically fixed to last 8 KB bank
                let bank = self.prg_banks[3] as usize;
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
        if (0x8000..=0xC002).contains(&addr) {
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
        let bank = self.chr_banks[bank_idx % 8] as usize ;
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
            self.chr[a as usize] = val;
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

    /// VRC6 clock_scanline is used as an alternative IRQ trigger mechanism
    /// (some VRC6 games use a simpler scanline counting method).
    fn clock_scanline(&mut self) {
        self.irq.clock();
    }

    fn has_chr_ram(&self) -> bool {
        self.chr_ram
    }
}
