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
    unused_imports,
)]
pub mod vrc1;
pub mod vrc2_4;
pub mod vrc3;
pub mod vrc6;
pub mod vrc7;

pub use self::vrc1::Vrc1;
pub use self::vrc2_4::Vrc2_4;
pub use self::vrc3::Vrc3;
pub use self::vrc6::Vrc6;
pub use self::vrc7::Vrc7;
