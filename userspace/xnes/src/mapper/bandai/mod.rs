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
pub mod bandai_74161_7432;
pub mod fcg;
pub mod karaoke;

pub use self::bandai_74161_7432::Bandai74161_7432;
pub use self::fcg::Fcg;
pub use self::karaoke::Karaoke;
