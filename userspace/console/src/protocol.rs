//! IPC protocol helpers for the console service.
//!
//! This keeps parsing and message decoding in one place so rendering code
//! remains focused on drawing and cursor management.

pub use libcluu::ipc::parse_message;
