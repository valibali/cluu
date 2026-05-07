//! `BuiltinCommand` trait + `BuiltinRegistry` + `BuiltinProvider` trait.
//!
//! Moved from the top of the old monolithic `commands.rs`.  All builtin
//! sub-modules reference this file for the shared trait.

// The canonical definitions live in commands_old during the migration.
// Re-export so that callers can `use crate::commands::builtins::registry::…`.
pub use crate::commands_old::{
    BuiltinCommand, BuiltinRegistry,
};
