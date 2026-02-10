//! Environment variable stubs for newlib compatibility.
//!
//! MicroPython checks `getenv("MICROPYPATH")`, `getenv("HOME")`, etc.
//! These stubs return NULL/success without actually storing anything.

use super::{c_char, c_int};
use core::ptr::null_mut;

/// Get an environment variable by name.
///
/// Currently returns NULL for all queries (no environment support yet).
#[no_mangle]
pub extern "C" fn getenv(_name: *const c_char) -> *mut c_char {
    null_mut()
}

/// Set an environment variable.
///
/// Currently a no-op stub that reports success.
#[no_mangle]
pub extern "C" fn setenv(_name: *const c_char, _value: *const c_char, _overwrite: c_int) -> c_int {
    0
}

/// Remove an environment variable.
///
/// Currently a no-op stub that reports success.
#[no_mangle]
pub extern "C" fn unsetenv(_name: *const c_char) -> c_int {
    0
}

/// Global environment pointer required by newlib.
///
/// newlib references `environ` for functions like `execve` envp defaults.
/// Set to NULL (no environment variables).
#[no_mangle]
pub static mut environ: *mut *mut c_char = core::ptr::null_mut();
