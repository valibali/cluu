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
pub mod jf16;
pub mod jf13;
pub mod jf17_19;
pub mod ss88006;
pub mod jf11_14;

pub use self::jf16::Jf16;
pub use self::jf13::Jf13;
pub use self::jf17_19::Jf17_19;
pub use self::ss88006::Ss88006;
pub use self::jf11_14::Jf11_14;
