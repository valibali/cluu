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
pub mod g101;
pub mod h3001;
pub mod lrog017;
pub mod tam_s1;

pub use self::g101::G101;
pub use self::h3001::H3001;
pub use self::lrog017::Lrog017;
pub use self::tam_s1::TamS1;
