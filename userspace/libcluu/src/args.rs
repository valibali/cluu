//! Decode `argv` from `ProcessInfo.params[PARAM_ARGC / PARAM_ARGV_OFFSET]`.
//!
//! The procmgr writes argv bytes contiguously into the child's ProcessInfo
//! page (at `argv_data_offset`), each string NUL-terminated. `params[6]` holds
//! `argc` and `params[7]` holds the byte offset within the 4 KB page. This
//! module decodes that into an owned `Vec<String>`, called once from
//! `runtime::_start` and cached.
//!
//! C programs use crt0.S for the same decode; this module is the Rust
//! equivalent and shares the wire format verbatim.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::boot::{process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, PROCESS_INFO_ADDR};
use crate::mem::PAGE_SIZE;

static ARGS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Populate the cached argv list from ProcessInfo. Called by `_start` before
/// `main()` runs; safe to call once per process.
pub fn init() {
    let mut slot = ARGS.lock();
    if slot.is_some() {
        return;
    }
    *slot = Some(decode_from_process_info());
}

/// Owned copy of the process's argv, empty if none.
pub fn args() -> Vec<String> {
    ARGS.lock().clone().unwrap_or_default()
}

/// Decode argv bytes from the ProcessInfo page. Returns `Vec<String>` on
/// success, empty Vec on any failure (unmapped page, bogus offsets, non-UTF-8).
///
/// Safety: reads from `PROCESS_INFO_ADDR`'s page. This page is always mapped
/// read-only during the process's lifetime (procmgr guarantees this before
/// jumping to `_start`). Bounds-check every byte offset against `PAGE_SIZE`.
/// Sanity cap on decoded argc. A 4 KB ProcessInfo page cannot hold more than
/// ~2000 single-char args anyway; this bounds allocation against uninitialized
/// `params` slots (e.g., `init`, whose page is not set up by procmgr and may
/// contain debug sentinels like 0xAFAFAFAFAFAFAFAF).
const MAX_ARGC: usize = 256;

fn decode_from_process_info() -> Vec<String> {
    let info = process_info();
    let argc = info.params[PARAM_ARGC] as usize;
    let argv_offset = info.params[PARAM_ARGV_OFFSET] as usize;
    if argc == 0 || argv_offset == 0 {
        return Vec::new();
    }
    // Reject clearly-garbage values without allocating. This is the fast
    // path for processes that boot before procmgr populates their argv
    // (init, procmgr itself).
    if argc > MAX_ARGC || argv_offset >= PAGE_SIZE {
        return Vec::new();
    }

    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
    let page_end = page_base + PAGE_SIZE;
    let mut cursor = page_base + argv_offset;

    let mut out = Vec::with_capacity(argc);
    for _ in 0..argc {
        if cursor >= page_end {
            break;
        }
        // Scan up to the next NUL, bounded by page end.
        let mut len = 0usize;
        while cursor + len < page_end {
            // SAFETY: bounds-checked above against page_end.
            let byte = unsafe { *((cursor + len) as *const u8) };
            if byte == 0 {
                break;
            }
            len += 1;
        }
        if cursor + len >= page_end {
            // Unterminated string — give up rather than read past the page.
            break;
        }
        // SAFETY: bounds-checked; we've scanned `len` in-bounds bytes.
        let slice = unsafe { core::slice::from_raw_parts(cursor as *const u8, len) };
        match core::str::from_utf8(slice) {
            Ok(s) => out.push(String::from(s)),
            Err(_) => {
                out.push(String::new());
            }
        }
        cursor += len + 1; // step past the NUL
    }
    out
}
