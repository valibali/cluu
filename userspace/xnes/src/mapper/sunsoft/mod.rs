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
pub mod sunsoft3;
pub mod sunsoft4;
pub mod fme7;
pub mod sunsoft89;
pub mod sunsoft93;
pub mod sunsoft184;

pub use self::sunsoft3::Sunsoft3;
pub use self::sunsoft4::Sunsoft4;
pub use self::fme7::Fme7;
pub use self::sunsoft89::Sunsoft89;
pub use self::sunsoft93::Sunsoft93;
pub use self::sunsoft184::Sunsoft184;
