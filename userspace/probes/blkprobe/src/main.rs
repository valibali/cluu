//! Block-IO smoke tests.
//!
//! Modes (selected by argv[1]):
//!   - "basic" (default): single sector-0 read — l2_blk_basic.
//!   - "concurrent":      4 sessions × 100 reads — l2_blk_concurrent.
//!   - "perf":            64 MB sequential, ≥150 MB/s floor — l2_blk_perf.
//!   - "leak":            open + submit + exit-without-close —
//!                        l2_blk_session_teardown.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_SPACE};
use libcluu::fs::BlkSessionClient;

const BASIC_SCRATCH_VA: usize = 0x4400_0000;
const BASIC_SCRATCH_PAGES: usize = 1;

const CONCURRENT_SCRATCH_VA: usize = 0x4500_0000;
const CONCURRENT_SCRATCH_PAGES_PER_SESSION: usize = 1;
const CONCURRENT_SESSIONS: usize = 4;
const CONCURRENT_READS: usize = 100;

const LEAK_SCRATCH_VA: usize = 0x4700_0000;
const LEAK_SCRATCH_PAGES: usize = 1;

const PERF_SCRATCH_VA: usize = 0x4600_0000;
const PERF_CHUNK_BYTES: usize = 512 * 1024; // 512 KB per request → 128-page chain (130 descs, fits queue_size=256).
const PERF_CHUNK_PAGES: usize = PERF_CHUNK_BYTES / 4096;
const PERF_TARGET_BYTES: usize = 64 * 1024 * 1024;
const PERF_FLOOR_MB_PER_S: u64 = 150;

fn fail(name: &str) -> i32 {
    let _ = libcluu::debug_print(&format!("blkprobe: [FAIL] {}", name));
    1
}

fn ensure_registry() -> Result<(), i32> {
    if libcluu::registry::init("blkprobe").is_err() {
        return Err(fail("registry::init"));
    }
    let _ = libcluu::syscall::yield_cpu();
    Ok(())
}

fn map_scratch(space_token: usize, va: usize, pages: usize) -> Result<(), i32> {
    if libcluu::syscall::space_map_range(space_token, va, 0, 0x03, pages, 0).is_err() {
        return Err(fail("space_map_range scratch"));
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("basic");
    match mode {
        "basic" => run_basic(),
        "concurrent" => run_concurrent(),
        "perf" => run_perf(),
        "leak" => run_leak(),
        _ => fail("unknown mode"),
    }
}

fn run_basic() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    if let Err(rc) = map_scratch(space_token, BASIC_SCRATCH_VA, BASIC_SCRATCH_PAGES) {
        return rc;
    }
    if let Err(rc) = ensure_registry() {
        return rc;
    }

    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe blkdev:main"),
    };

    let mut client = match BlkSessionClient::open(blkdev) {
        Ok(c) => c,
        Err(_) => return fail("BlkSessionClient::open"),
    };

    let buf = unsafe {
        core::slice::from_raw_parts_mut(BASIC_SCRATCH_VA as *mut u8, BASIC_SCRATCH_PAGES * 4096)
    };

    match client.read_blocking(0, buf) {
        // The driver reports `bytes_done` as the device-reported chain
        // total, which includes the 1-byte status descriptor — so a
        // 4096-byte data read returns n=4097 over the wire. Accept any
        // n covering the requested data range.
        Ok(n) if n >= 4096 => {
            if !buf.iter().any(|&b| b != 0) {
                return fail("sector 0 all zeros");
            }
        }
        Ok(n) => return fail(&format!("short read n={}", n)),
        Err(_) => return fail("read_blocking err"),
    }

    let _ = libcluu::debug_print("blkprobe: ALL OK");
    0
}

fn run_concurrent() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    // One scratch page per session, all in a contiguous range.
    let total_pages = CONCURRENT_SESSIONS * CONCURRENT_SCRATCH_PAGES_PER_SESSION;
    if let Err(rc) = map_scratch(space_token, CONCURRENT_SCRATCH_VA, total_pages) {
        return rc;
    }
    if let Err(rc) = ensure_registry() {
        return rc;
    }

    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe blkdev:main"),
    };

    // Open N sessions.
    let mut clients: Vec<BlkSessionClient> = Vec::with_capacity(CONCURRENT_SESSIONS);
    for i in 0..CONCURRENT_SESSIONS {
        match BlkSessionClient::open(blkdev) {
            Ok(c) => clients.push(c),
            Err(_) => return fail(&format!("open session {}", i)),
        }
    }

    // Round-robin reads. Each session uses its own scratch page so
    // concurrent reads don't trample shared memory.
    for k in 0..CONCURRENT_READS {
        let session_idx = k % CONCURRENT_SESSIONS;
        let buf_va =
            CONCURRENT_SCRATCH_VA + session_idx * CONCURRENT_SCRATCH_PAGES_PER_SESSION * 4096;
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                buf_va as *mut u8,
                CONCURRENT_SCRATCH_PAGES_PER_SESSION * 4096,
            )
        };
        // Different LBAs for different reads so we hit a variety of disk
        // blocks. Multiplied to land at sector boundaries within the disk.
        let lba = (k * 16) as u64;
        match clients[session_idx].read_blocking(lba, buf) {
            Ok(n) if n >= 4096 => {}
            Ok(n) => {
                return fail(&format!("read {} short n={}", k, n));
            }
            Err(_) => {
                return fail(&format!("read {} err", k));
            }
        }
    }

    let _ = libcluu::debug_print(&format!("blkprobe: concurrent={} OK", CONCURRENT_READS));
    let _ = libcluu::debug_print("blkprobe: ALL OK");
    0
}

fn run_perf() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let clock_token = info.tokens[TOKEN_CLOCK];

    if let Err(rc) = map_scratch(space_token, PERF_SCRATCH_VA, PERF_CHUNK_PAGES) {
        return rc;
    }
    if let Err(rc) = ensure_registry() {
        return rc;
    }

    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe blkdev:main"),
    };

    let mut client = match BlkSessionClient::open(blkdev) {
        Ok(c) => c,
        Err(_) => return fail("BlkSessionClient::open"),
    };

    let buf = unsafe {
        core::slice::from_raw_parts_mut(PERF_SCRATCH_VA as *mut u8, PERF_CHUNK_BYTES)
    };

    let freq_hz = match libcluu::syscall::clock_frequency(clock_token) {
        Ok(f) if f > 0 => f,
        _ => return fail("clock_frequency"),
    };
    let t0 = match libcluu::syscall::clock_now(clock_token) {
        Ok(t) => t,
        Err(_) => return fail("clock_now t0"),
    };

    let mut bytes_done: usize = 0;
    let mut lba: u64 = 0;
    while bytes_done < PERF_TARGET_BYTES {
        match client.read_blocking(lba, buf) {
            // The driver's `n` is the device-reported chain total which
            // includes the 1-byte status descriptor (PERF_CHUNK_BYTES + 1).
            // Anything ≥ PERF_CHUNK_BYTES means the data range was filled.
            Ok(n) if n >= PERF_CHUNK_BYTES => {
                bytes_done += PERF_CHUNK_BYTES;
                lba += (PERF_CHUNK_BYTES / 512) as u64;
            }
            Ok(n) => return fail(&format!("short read n={} chunk={}", n, PERF_CHUNK_BYTES)),
            Err(_) => return fail("read_blocking err"),
        }
    }

    let t1 = match libcluu::syscall::clock_now(clock_token) {
        Ok(t) => t,
        Err(_) => return fail("clock_now t1"),
    };
    let elapsed_ticks = t1.saturating_sub(t0);
    if elapsed_ticks == 0 {
        return fail("zero elapsed");
    }
    // mb_per_s = bytes * freq_hz / elapsed_ticks / (1024*1024).
    // Order matters for u64 precision: bytes_done ≤ 64 MB (2^26),
    // freq_hz ~ 3 GHz (2^32) → product ≤ 2^58, no overflow.
    let mb_per_s = (bytes_done as u64).saturating_mul(freq_hz) / elapsed_ticks / (1024 * 1024);

    let _ = libcluu::debug_print(&format!(
        "blkprobe: perf bytes={} elapsed_ticks={} freq_hz={} mb_per_s={}",
        bytes_done, elapsed_ticks, freq_hz, mb_per_s
    ));

    if mb_per_s < PERF_FLOOR_MB_PER_S {
        let _ = libcluu::debug_print(&format!(
            "blkprobe: [FAIL] perf below {} MB/s floor",
            PERF_FLOOR_MB_PER_S
        ));
        return 1;
    }

    let _ = libcluu::debug_print("blkprobe: ALL OK");
    0
}

/// l2_blk_session_teardown probe: open a session, submit one async read,
/// then exit before draining or closing. The driver should reap the
/// session via procmgr's BLK_TID_CLEANUP broadcast on this process's exit.
fn run_leak() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    if let Err(rc) = map_scratch(space_token, LEAK_SCRATCH_VA, LEAK_SCRATCH_PAGES) {
        return rc;
    }
    if let Err(rc) = ensure_registry() {
        return rc;
    }

    let blkdev = match libcluu::registry::subscribe_output("blkdev", "main") {
        Ok(ep) => ep,
        Err(_) => return fail("subscribe blkdev:main"),
    };

    let mut client = match BlkSessionClient::open(blkdev) {
        Ok(c) => c,
        Err(_) => return fail("BlkSessionClient::open"),
    };

    let buf = unsafe {
        core::slice::from_raw_parts_mut(LEAK_SCRATCH_VA as *mut u8, 4096)
    };

    if client.submit_async(0, buf).is_err() {
        return fail("submit_async");
    }

    let _ = libcluu::debug_print("blkprobe: leak SUBMITTED, exiting");
    // Skip Drop: we want to simulate an unclean exit so procmgr's
    // BLK_TID_CLEANUP path is the only thing that reaps the session.
    core::mem::forget(client);
    0
}
