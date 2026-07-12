//! Process env helpers available without the `posix` feature.
//!
//! `read_env_var` reads directly from the kernel-provided ProcessInfo env
//! trailer so it works even before `init_env()` has run.

use crate::boot::{process_info, PARAM_ENVC, PARAM_ENV_OFFSET, PROCESS_INFO_ADDR};

/// Read a single environment variable from the ProcessInfo page.
///
/// Walks the kernel-provided env trailer directly rather than the in-process
/// `ENV` copy, so it reflects the envelope-resolved value even before
/// `init_env()` has run. Shared between the shell, cluuterm, and the registry
/// virtual routing for `procmgr:spawn`.
pub fn read_env_var(name: &str) -> Option<alloc::string::String> {
    let info = process_info();
    let envc = info.params[PARAM_ENVC] as usize;
    let env_offset = info.params[PARAM_ENV_OFFSET] as usize;
    if envc == 0 || env_offset == 0 {
        return None;
    }

    let page_base = PROCESS_INFO_ADDR & !(4096 - 1);
    let page_end = page_base + 4096;
    let mut ptr = (page_base + env_offset) as *const u8;
    let prefix_len = name.len();

    for _ in 0..envc {
        if (ptr as usize) >= page_end {
            break;
        }
        let start = ptr;
        let mut len = 0usize;
        unsafe {
            while (start.add(len) as usize) < page_end && *start.add(len) != 0 {
                len += 1;
            }
        }
        if len == 0 {
            break;
        }
        let kv = unsafe { core::slice::from_raw_parts(start, len) };
        if kv.len() > prefix_len
            && kv[prefix_len] == b'='
            && &kv[..prefix_len] == name.as_bytes()
        {
            if let Ok(val) = core::str::from_utf8(&kv[prefix_len + 1..]) {
                return Some(alloc::string::String::from(val));
            }
        }
        ptr = unsafe { start.add(len + 1) };
    }
    None
}
