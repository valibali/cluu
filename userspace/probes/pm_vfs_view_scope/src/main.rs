#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::boot::{process_info, TOKEN_VFS_VIEW_MGR};
use libcluu::syscall::{token_derive_scoped, token_get_info};
use libcluu::{debug_print, Error};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn run() -> libcluu::Result<()> {
    debug_print("pm_vfs_view_scope: start")?;

    let info = process_info();
    let view_mgr_token = info.tokens[TOKEN_VFS_VIEW_MGR];

    if view_mgr_token == 0 {
        debug_print("pm_vfs_view_scope: FAIL no view_mgr_token")?;
        return Err(Error::Unknown);
    }

    debug_print("pm_vfs_view_scope: got view_mgr_token")?;

    // Case A: narrow scope from (0, 0xFFFF) to (1, 0x03).
    let child_a = match token_derive_scoped(view_mgr_token, 0, 0, 1, 0x03) {
        Ok(token) => {
            if let Ok((tag, sid, mask)) = token_get_info(token) {
                if tag == 0x09 && sid == 1 && mask == 0x03 {
                    let _ = debug_print("pm_vfs_view_scope: PASS case_a narrow accepted");
                    token
                } else {
                    let _ = debug_print(&alloc::format!(
                        "pm_vfs_view_scope: FAIL case_a info mismatch tag={} sid={} mask={}",
                        tag, sid, mask
                    ));
                    return Err(Error::Unknown);
                }
            } else {
                debug_print("pm_vfs_view_scope: FAIL case_a get_info failed")?;
                return Err(Error::Unknown);
            }
        }
        Err(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_a derive failed")?;
            return Err(Error::Unknown);
        }
    };

    // Case B: widen mask from 0x03 to 0xFF (should fail).
    match token_derive_scoped(child_a, 0, 0, 1, 0xFF) {
        Ok(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_b widen mask not rejected")?;
            return Err(Error::Unknown);
        }
        Err(Error::PermissionDenied) => {
            let _ = debug_print("pm_vfs_view_scope: PASS case_b widen mask denied");
        }
        Err(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_b wrong error")?;
            return Err(Error::Unknown);
        }
    }

    // Case C: try to change sid from 1 to 2 (should fail).
    match token_derive_scoped(child_a, 0, 0, 2, 0x03) {
        Ok(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_c sid change not rejected")?;
            return Err(Error::Unknown);
        }
        Err(Error::PermissionDenied) => {
            let _ = debug_print("pm_vfs_view_scope: PASS case_c sid change denied");
        }
        Err(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_c wrong error")?;
            return Err(Error::Unknown);
        }
    }

    // Case D: root (sid=0) can mint any sid. Derive from original with sid=42.
    match token_derive_scoped(view_mgr_token, 0, 0, 42, 0x01) {
        Ok(child_d) => {
            if let Ok((tag, sid, mask)) = token_get_info(child_d) {
                if tag == 0x09 && sid == 42 && mask == 0x01 {
                    let _ = debug_print("pm_vfs_view_scope: PASS case_d root mint accepted");
                } else {
                    let _ = debug_print(&alloc::format!(
                        "pm_vfs_view_scope: FAIL case_d info mismatch tag={} sid={} mask={}",
                        tag, sid, mask
                    ));
                    return Err(Error::Unknown);
                }
            } else {
                debug_print("pm_vfs_view_scope: FAIL case_d get_info failed")?;
                return Err(Error::Unknown);
            }
        }
        Err(_) => {
            debug_print("pm_vfs_view_scope: FAIL case_d derive failed")?;
            return Err(Error::Unknown);
        }
    }

    debug_print("pm_vfs_view_scope: PASS all cases")?;
    Ok(())
}
