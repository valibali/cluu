#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use
)]

use alloc::boxed::Box;

pub mod common;

// Vendor-organized mappers
pub mod bandai;
pub mod camerica;
pub mod irem;
pub mod jaleco;
pub mod konami;
pub mod namco;
pub mod nintendo;
pub mod sunsoft;
pub mod taito;
pub mod unlicensed;

pub trait MapperImpl {
    fn cpu_read(&mut self, addr: u16) -> u8;
    fn cpu_write(&mut self, addr: u16, val: u8);
    fn ppu_read(&mut self, addr: u16) -> u8;
    fn ppu_write(&mut self, addr: u16, val: u8);
    fn mirroring(&self) -> u8;
    fn irq_pending(&self) -> bool;
    fn ack_irq(&mut self);
    fn clock_scanline(&mut self) {}

    fn has_chr_ram(&self) -> bool;

    // MMC5-specific extensions with default no-op implementations:

    /// Called once per scanline by the PPU (for MMC5 scanline IRQ).
    fn notify_scanline(&mut self, _scanline: u16) {}

    /// Returns the nametable mapping register value.
    /// 0xFF = standard mirroring (use `mirroring()`).
    /// Otherwise, each 2-bit pair maps NT0-3 to source (0=`CIRAM_A`, 1=`CIRAM_B`, 2=`ExRAM`, 3=Fill).
    fn nt_mapping(&self) -> u8 {
        0xFF
    }

    /// Read from a non-CIRAM nametable source (`ExRAM` or fill mode).
    fn read_nt_ext(&mut self, _addr: u16, _nt_source: u8) -> u8 {
        0
    }

    /// Write to a non-CIRAM nametable source (`ExRAM`).
    fn write_nt_ext(&mut self, _addr: u16, _nt_source: u8, _val: u8) {}

    /// Set CHR fetch mode to background (for `ExRAM` mode 1).
    fn set_chr_fetch_bg(&mut self) {}

    /// Set CHR fetch mode to sprite (for `ExRAM` mode 1).
    fn set_chr_fetch_sprite(&mut self) {}

    /// Set the extended CHR bank from `ExRAM` byte (for `ExRAM` mode 1).
    fn set_extended_chr_bank(&mut self, _bank: u8) {}

    /// Get the extended CHR bank.
    fn get_extended_chr_bank(&self) -> u8 {
        0
    }

    /// Get the `ExRAM` mode.
    fn get_ex_ram_mode(&self) -> u8 {
        0
    }

    /// Get fill mode tile value.
    fn get_fill_tile(&self) -> u8 {
        0
    }

    /// Get fill mode attribute value.
    fn get_fill_attr(&self) -> u8 {
        0
    }

    /// Read a byte from `ExRAM` at the given offset.
    fn read_ex_ram_byte(&mut self, _offset: u16) -> u8 {
        0
    }
}

pub struct Mapper(pub Box<dyn MapperImpl>);

impl Mapper {
    pub fn from_ines(
        id: u8,
        mirroring: u8,
        prg_data: &[u8],
        chr_data: &[u8],
        chr_ram: bool,
    ) -> Self {
        Self(match id {
            1 => Box::new(nintendo::Mmc1::new(prg_data, chr_data, chr_ram, mirroring)),
            2 => Box::new(nintendo::UxRom::new(prg_data, chr_data, chr_ram, mirroring)),
            3 => Box::new(nintendo::Cnrom::new(prg_data, chr_data, chr_ram, mirroring)),
            4 => Box::new(nintendo::Mmc3::new(prg_data, chr_data, chr_ram, mirroring)),
            5 => Box::new(nintendo::Mmc5::new(prg_data, chr_data, chr_ram, mirroring)),
            7 => Box::new(nintendo::Axrom::new(prg_data, chr_data, chr_ram, mirroring)),
            9 => Box::new(nintendo::Mmc2::new(prg_data, chr_data, chr_ram, mirroring)),
            10 => Box::new(nintendo::Mmc4::new(prg_data, chr_data, chr_ram, mirroring)),
            11 | 144 => Box::new(unlicensed::ColorDreams::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            13 => Box::new(nintendo::CpRom::new(prg_data, chr_data, chr_ram, mirroring)),
            15 => Box::new(unlicensed::Mapper15::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            16 | 153 | 157 | 159 => {
                Box::new(bandai::Fcg::new(prg_data, chr_data, chr_ram, mirroring))
            }
            18 => Box::new(jaleco::Ss88006::new(prg_data, chr_data, chr_ram, mirroring)),
            19 | 210 => Box::new(namco::Namco163::new(prg_data, chr_data, chr_ram, mirroring)),
            21 | 22 | 23 | 25 => {
                Box::new(konami::Vrc2_4::new(prg_data, chr_data, chr_ram, mirroring))
            }
            24 | 26 => Box::new(konami::Vrc6::new(prg_data, chr_data, chr_ram, mirroring)),
            32 => Box::new(irem::G101::new(prg_data, chr_data, chr_ram, mirroring)),
            33 => Box::new(taito::Tc0190::new(prg_data, chr_data, chr_ram, mirroring)),
            35 => Box::new(unlicensed::Mapper35::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            40 => Box::new(unlicensed::Mapper40::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            42 => Box::new(unlicensed::Mapper42::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            43 => Box::new(unlicensed::Mapper43::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            48 => Box::new(taito::Tc0690::new(prg_data, chr_data, chr_ram, mirroring)),
            50 => Box::new(unlicensed::Mapper50::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            57 => Box::new(unlicensed::Mapper57::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            58 => Box::new(unlicensed::Mapper58::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            60 => Box::new(unlicensed::Mapper60::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            61 => Box::new(unlicensed::Mapper61::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            62 => Box::new(unlicensed::Mapper62::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            64 | 158 => Box::new(unlicensed::Rambo1::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            65 => Box::new(irem::H3001::new(prg_data, chr_data, chr_ram, mirroring)),
            66 => Box::new(nintendo::Gxrom::new(prg_data, chr_data, chr_ram, mirroring)),
            67 => Box::new(sunsoft::Sunsoft3::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            68 => Box::new(sunsoft::Sunsoft4::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            69 => Box::new(sunsoft::Fme7::new(prg_data, chr_data, chr_ram, mirroring)),
            70 | 152 => Box::new(bandai::Bandai74161_7432::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            71 => Box::new(camerica::Bf909x::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            72 | 92 => Box::new(jaleco::Jf17_19::new(prg_data, chr_data, chr_ram, mirroring)),
            73 => Box::new(konami::Vrc3::new(prg_data, chr_data, chr_ram, mirroring)),
            74 | 118 | 119 | 191 | 194 | 195 | 192 => Box::new(nintendo::TxSRom::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            75 => Box::new(konami::Vrc1::new(prg_data, chr_data, chr_ram, mirroring)),
            76 | 88 | 95 | 154 | 206 => {
                Box::new(namco::Namco108::new(prg_data, chr_data, chr_ram, mirroring))
            }
            77 => Box::new(irem::Lrog017::new(prg_data, chr_data, chr_ram, mirroring)),
            78 => Box::new(jaleco::Jf16::new(prg_data, chr_data, chr_ram, mirroring)),
            79 | 113 | 146 => Box::new(unlicensed::Mapper79::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            80 | 207 => Box::new(taito::X1005::new(prg_data, chr_data, chr_ram, mirroring)),
            82 => Box::new(taito::X1017::new(prg_data, chr_data, chr_ram, mirroring)),
            83 => Box::new(unlicensed::Mapper83::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            85 => Box::new(konami::Vrc7::new(prg_data, chr_data, chr_ram, mirroring)),
            86 => Box::new(jaleco::Jf13::new(prg_data, chr_data, chr_ram, mirroring)),
            89 => Box::new(sunsoft::Sunsoft89::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            91 => Box::new(unlicensed::Mapper91::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            93 => Box::new(sunsoft::Sunsoft93::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            94 => Box::new(nintendo::UnRom94::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            96 => Box::new(unlicensed::OekaKids::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            97 => Box::new(irem::TamS1::new(prg_data, chr_data, chr_ram, mirroring)),
            103 => Box::new(unlicensed::Mapper103::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            105 => Box::new(nintendo::Mmc1_105::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            106 => Box::new(unlicensed::Mapper106::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            107 => Box::new(unlicensed::Mapper107::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            112 => Box::new(unlicensed::Mapper112::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            116 => Box::new(unlicensed::Mapper116::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            117 => Box::new(unlicensed::Mapper117::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            120 => Box::new(unlicensed::Mapper120::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            140 => Box::new(jaleco::Jf11_14::new(prg_data, chr_data, chr_ram, mirroring)),
            155 => Box::new(nintendo::Mmc1_155::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            170 => Box::new(unlicensed::Mapper170::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            174 => Box::new(unlicensed::Mapper174::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            180 => Box::new(nintendo::UnRom180::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            184 => Box::new(sunsoft::Sunsoft184::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            185 => Box::new(nintendo::CnromProtect::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            188 => Box::new(bandai::Karaoke::new(prg_data, chr_data, chr_ram, mirroring)),
            200 => Box::new(unlicensed::Mapper200::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            202 => Box::new(unlicensed::Mapper202::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            203 => Box::new(unlicensed::Mapper203::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            204 => Box::new(unlicensed::Mapper204::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            212 => Box::new(unlicensed::Mapper212::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            213 => Box::new(unlicensed::Mapper213::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            214 => Box::new(unlicensed::Mapper214::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            216 => Box::new(unlicensed::Mapper216::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            221 => Box::new(unlicensed::Mapper221::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            222 => Box::new(unlicensed::Mapper222::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            225 => Box::new(unlicensed::Mapper225::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            226 => Box::new(unlicensed::Mapper226::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            227 => Box::new(unlicensed::Mapper227::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            229 => Box::new(unlicensed::Mapper229::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            230 => Box::new(unlicensed::Mapper230::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            231 => Box::new(unlicensed::Mapper231::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            232 => Box::new(camerica::Bf9096::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            233 => Box::new(unlicensed::Mapper233::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            234 => Box::new(unlicensed::Mapper234::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            240 => Box::new(unlicensed::Mapper240::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            241 => Box::new(unlicensed::Mapper241::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            242 => Box::new(unlicensed::Mapper242::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            244 => Box::new(unlicensed::Mapper244::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            246 => Box::new(unlicensed::Mapper246::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            253 => Box::new(unlicensed::Mapper253::new(
                prg_data, chr_data, chr_ram, mirroring,
            )),
            _ => Box::new(nintendo::Nrom::new(prg_data, chr_data, chr_ram, mirroring)),
        })
    }
}

impl Mapper {
    #[inline(always)]
    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        self.0.cpu_read(addr)
    }
    #[inline(always)]
    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        self.0.cpu_write(addr, val);
    }
    #[inline(always)]
    pub fn ppu_read(&mut self, addr: u16) -> u8 {
        self.0.ppu_read(addr)
    }
    #[inline(always)]
    pub fn ppu_write(&mut self, addr: u16, val: u8) {
        self.0.ppu_write(addr, val);
    }
    pub fn mirroring(&self) -> u8 {
        self.0.mirroring()
    }
    pub fn irq_pending(&self) -> bool {
        self.0.irq_pending()
    }
    pub fn ack_irq(&mut self) {
        self.0.ack_irq();
    }
    pub fn clock_scanline(&mut self) {
        self.0.clock_scanline();
    }
    pub fn has_chr_ram(&self) -> bool {
        self.0.has_chr_ram()
    }

    pub fn notify_scanline(&mut self, scanline: u16) {
        self.0.notify_scanline(scanline);
    }
    pub fn nt_mapping(&self) -> u8 {
        self.0.nt_mapping()
    }
    pub fn read_nt_ext(&mut self, addr: u16, nt_source: u8) -> u8 {
        self.0.read_nt_ext(addr, nt_source)
    }
    pub fn write_nt_ext(&mut self, addr: u16, nt_source: u8, val: u8) {
        self.0.write_nt_ext(addr, nt_source, val);
    }
    pub fn set_chr_fetch_bg(&mut self) {
        self.0.set_chr_fetch_bg();
    }
    pub fn set_chr_fetch_sprite(&mut self) {
        self.0.set_chr_fetch_sprite();
    }
    pub fn set_extended_chr_bank(&mut self, bank: u8) {
        self.0.set_extended_chr_bank(bank);
    }
    pub fn get_extended_chr_bank(&self) -> u8 {
        self.0.get_extended_chr_bank()
    }
    pub fn get_ex_ram_mode(&self) -> u8 {
        self.0.get_ex_ram_mode()
    }
    pub fn get_fill_tile(&self) -> u8 {
        self.0.get_fill_tile()
    }
    pub fn get_fill_attr(&self) -> u8 {
        self.0.get_fill_attr()
    }
    pub fn read_ex_ram_byte(&mut self, offset: u16) -> u8 {
        self.0.read_ex_ram_byte(offset)
    }
}
