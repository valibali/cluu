#![no_std]

extern crate alloc;

/// cluu_lang provides a no_std parser and AST for the cluu shell language.
pub mod api;
pub mod ast;
pub mod parser;

pub use api::{format_ast, parse_program};
