/*
 * Binary Loaders
 *
 * This module provides support for loading different binary formats
 * into process address spaces.
 */

pub mod elf;

pub use elf::{load_elf_binary, spawn_elf_process, ElfBinary, ElfLoadError};
