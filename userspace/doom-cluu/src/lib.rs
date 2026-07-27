//! CLUU WAD loader for doomgeneric SDL2 backend.
//!
//! SDL2 (pinned 2.30.0 with CLUU video/events/audio backends) handles
//! video, input, timer, and audio.  This crate provides:
//!   - cluu_debug: serial diagnostic output for the C engine
//!   - cluu_wad_load: grant-based bulk WAD loader

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

use alloc::format;

use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libcluu::syscall;
use libcluu::{debug_print, Result};

#[no_mangle]
pub extern "C" fn cluu_debug(msg: *const u8) {
    if msg.is_null() { return; }
    let mut len = 0;
    while unsafe { *msg.add(len) } != 0 { len += 1; }
    let s = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(msg, len)) };
    let _ = debug_print(s);
}

// ============================================================================
// WAD bulk loader: grant-based zero-copy file read
// ============================================================================

const WAD_VA: usize = 0xE000_0000;
const WAD_SCRATCH: usize = 0xB000_0000;
const WAD_CHUNK: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn cluu_wad_load(path: *const i8, out_len: *mut u64) -> *mut u8 {
    let path_str = unsafe {
        let mut len = 0;
        while *path.add(len) != 0 {
            len += 1;
        }
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(path as *const u8, len))
    };

    let _ = debug_print(&format!("doom-cluu: bulk-loading WAD {}", path_str));

    match wad_load_inner(path_str) {
        Ok((ptr, len)) => {
            unsafe { *out_len = len as u64; }
            let _ = debug_print(&format!("doom-cluu: WAD loaded {} bytes via grant", len));
            ptr as *mut u8
        }
        Err(e) => {
            let _ = debug_print(&format!("doom-cluu: WAD bulk load failed {:?}", e));
            core::ptr::null_mut()
        }
    }
}

fn wad_load_inner(path: &str) -> Result<(*const u8, usize)> {
    let vfs_ep = registry::subscribe_output("vfs", "main")?;
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_ep, client_id);

    let file = vfs.open(path)?;
    let file_size = file.size;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    let _ = debug_print(&format!("doom-cluu: WAD size {} bytes, reading in 64KB chunks", file_size));

    let num_pages = (file_size + 0xFFF) / 0x1000;
    syscall::space_map_range(space_token, WAD_VA, 0, 0x03, num_pages, 0)?;

    let scratch_pages = (WAD_CHUNK + 0xFFF) / 0x1000;
    syscall::space_map_range(space_token, WAD_SCRATCH, 0, 0x03, scratch_pages, 0)?;

    let mut offset = 0usize;
    while offset < file_size {
        let want = WAD_CHUNK.min(file_size - offset);
        let grant = vfs.read_grant(file, offset, want, space_token, WAD_SCRATCH)?;

        let src = unsafe {
            core::slice::from_raw_parts((WAD_SCRATCH + grant.offset) as *const u8, grant.len)
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                (WAD_VA + offset) as *mut u8,
                grant.len,
            );
        }

        offset += grant.len;
        if (offset / (1024 * 1024)) != ((offset - grant.len) / (1024 * 1024)) {
            let _ = debug_print(&format!("doom-cluu: WAD loaded {}MB / {}MB",
                offset / (1024*1024), file_size / (1024*1024)));
        }
    }

    vfs.close(file)?;

    let ptr = WAD_VA as *const u8;
    Ok((ptr, file_size))
}
