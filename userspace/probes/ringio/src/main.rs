//! ringio probe: shared-ring VFS read path smoke test.
//! Lifted from RingIoBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    alloc_shared_ring_region, free_shared_ring_region, SharedRing,
    SHARED_RING_DEFAULT_MAP_FLAGS,
};
use libcluu::registry;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let path = args.get(1).map_or("/bin/hello", |s| s.as_str());
    let max_rounds = args
        .get(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(16)
        .max(1);
    let chunk = args
        .get(3)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(16 * 1024)
        .max(512);

    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("ringio: FAIL vfs unavailable {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(client) => client,
        Err(err) => {
            let line = format!("ringio: FAIL client {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    let file = match vfs.open(path) {
        Ok(file) => file,
        Err(err) => {
            let line = format!("ringio: FAIL open {} {:?}", path, err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let space_token = process_info().tokens[TOKEN_SPACE];
    if space_token == 0 {
        let _ = vfs.close(file);
        let _ = libcluu::debug_print("ringio: FAIL missing space token");
        return 1;
    }

    let region = match alloc_shared_ring_region(space_token, 64 * 1024, SHARED_RING_DEFAULT_MAP_FLAGS) {
        Ok(region) => region,
        Err(err) => {
            let _ = vfs.close(file);
            let line = format!("ringio: FAIL alloc_shared_ring {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let ring_meta = match vfs.setup_read_ring(space_token, region.base, region.bytes) {
        Ok(meta) => meta,
        Err(err) => {
            let _ = free_shared_ring_region(space_token, region);
            let _ = vfs.close(file);
            let line = format!("ringio: FAIL ring setup {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    if ring_meta.bytes > region.bytes {
        let _ = free_shared_ring_region(space_token, region);
        let _ = vfs.close(file);
        let _ = libcluu::debug_print("ringio: FAIL invalid ring bytes");
        return 1;
    }

    let backing =
        unsafe { core::slice::from_raw_parts_mut(region.base as *mut u8, ring_meta.bytes) };
    let mut ring = match SharedRing::attach(backing) {
        Ok(ring) => ring,
        Err(err) => {
            let _ = free_shared_ring_region(space_token, region);
            let _ = vfs.close(file);
            let line = format!("ringio: FAIL ring attach {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let mut total = 0usize;
    let mut offset = 0usize;
    let mut rounds = 0usize;
    let mut notify_seq = ring.notify_seq();
    loop {
        if rounds >= max_rounds {
            break;
        }
        let req = chunk.min(ring_meta.capacity.saturating_sub(1));
        if req == 0 {
            break;
        }
        let ring_chunk = match vfs.read_ring(file, offset, req) {
            Ok(chunk) => chunk,
            Err(err) => {
                let line = format!("ringio: FAIL read_ring {:?}", err);
                let _ = libcluu::debug_print(&line);
                let _ = free_shared_ring_region(space_token, region);
                let _ = vfs.close(file);
                return 1;
            }
        };
        if ring_chunk.len == 0 {
            break;
        }

        let mut drain = alloc::vec![0u8; ring_chunk.len];
        let popped = ring.pop(&mut drain);
        if popped != ring_chunk.len {
            let line = format!(
                "ringio: FAIL ring pop mismatch expected={} got={}",
                ring_chunk.len, popped
            );
            let _ = libcluu::debug_print(&line);
            let _ = free_shared_ring_region(space_token, region);
            let _ = vfs.close(file);
            return 1;
        }

        total += popped;
        offset += popped;
        rounds += 1;
        notify_seq = ring_chunk.notify_seq;
        if ring_chunk.eof {
            break;
        }
    }

    let _ = free_shared_ring_region(space_token, region);
    let _ = vfs.close(file);
    let line = format!(
        "ringio: PASS path={} bytes={} rounds={} notify_seq={}",
        path, total, rounds, notify_seq
    );
    let _ = libcluu::debug_print(&line);
    0
}
