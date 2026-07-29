#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::wildcard_enum_match_arm,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    dead_code,
    unused_imports
)]
// Original flat mappers (moved here for organization)
pub mod axrom;
pub mod cnrom;
pub mod gxrom;
pub mod mmc1;
pub mod mmc2;
pub mod mmc3;
pub mod mmc4;
pub mod mmc5;
pub mod nrom;
pub mod uxrom;

// Additional Nintendo mappers
pub mod cnrom_protect;
pub mod cp_rom;
pub mod mmc1_105;
pub mod mmc1_155;
pub mod tx_srom;
pub mod unrom_180;
pub mod unrom_94;

pub use self::axrom::Axrom;
pub use self::cnrom::Cnrom;
pub use self::cnrom_protect::CnromProtect;
pub use self::cp_rom::CpRom;
pub use self::gxrom::Gxrom;
pub use self::mmc1::Mmc1;
pub use self::mmc1_105::Mmc1_105;
pub use self::mmc1_155::Mmc1_155;
pub use self::mmc2::Mmc2;
pub use self::mmc3::Mmc3;
pub use self::mmc4::Mmc4;
pub use self::mmc5::Mmc5;
pub use self::nrom::Nrom;
pub use self::tx_srom::TxSRom;
pub use self::unrom_94::UnRom94;
pub use self::unrom_180::UnRom180;
pub use self::uxrom::UxRom;
